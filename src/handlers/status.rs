//! PUBLIC status surface: the server-rendered status page and the machine-readable JSON.
//!
//! Both are unauthenticated by design (placed behind a Sluice `auth=public` route). The
//! page mirrors the HOLDFAST enterprise brand: app-bar, overall hero, an active-incidents
//! section (severity-tinted cards with an expandable update timeline), maintenance notices,
//! compact component rows with rolling uptime + the classic 90-day uptime bar row,
//! and a "Past incidents" section (last 14 days, grouped by day).

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::Json;

use crate::handlers::{
    app_css, esc, fmt_countdown, fmt_date, fmt_datetime, fmt_latency, hv, incident_status_pill,
    rel_time, render_theme_switch, severity_pill, userbox, SHIELD_SVG,
};
use crate::i18n;
use crate::model::{
    affected_names, build_status, day_bucket, day_date, group_rollup, maintenance_ongoing,
    ComponentView, StatusView, DAY_SECS,
};
use crate::store::{Incident, IncidentUpdate};
use crate::{now_secs, vitals, AppState};

const STATUS_HTML: &str = include_str!("../../templates/status.html");

/// `GET /status` — the public status page (no auth).
pub async fn status_page(State(state): State<AppState>, headers: HeaderMap) -> Html<String> {
    let now = now_secs();
    let loc = odyssey::resolve_locale(hv(&headers, "cookie"), hv(&headers, "accept-language"));
    let theme = odyssey::resolve_theme(hv(&headers, "cookie"));
    let mut view = build_status(state.store.as_ref(), now).await;
    attach_infra(&mut view, &state, now);
    Html(render_status(&view, now, loc, theme))
}

/// `GET /api/status` — the public machine-readable status snapshot (no auth).
pub async fn api_status(State(state): State<AppState>, headers: HeaderMap) -> Json<StatusView> {
    let now = now_secs();
    let _loc = odyssey::resolve_locale(hv(&headers, "cookie"), hv(&headers, "accept-language"));
    let mut view = build_status(state.store.as_ref(), now).await;
    attach_infra(&mut view, &state, now);
    Json(view)
}

fn attach_infra(view: &mut StatusView, state: &AppState, now: i64) {
    view.infra = state
        .vitals
        .as_ref()
        .and_then(|v| v.snapshot())
        .and_then(|snap| vitals::public_infra(&snap, now));
}

fn render_lang_switch(loc: odyssey::Locale) -> String {
    let mut out = String::from(r#"<nav class="bc-lang" aria-label="Language">"#);
    for l in odyssey::Locale::all() {
        let active = if l == loc { " is-active" } else { "" };
        out.push_str(&format!(
            r#"<a class="langswitch__opt{active}" href="/_gw/lang?to={code}">{name}</a>"#,
            code = l.code(),
            name = esc(odyssey::t(
                l,
                match l {
                    odyssey::Locale::En => "lang.name.en",
                    odyssey::Locale::Zh => "lang.name.zh",
                    odyssey::Locale::Ja => "lang.name.ja",
                }
            )),
        ));
    }
    out.push_str("</nav>");
    out
}

fn rel_time_l(loc: odyssey::Locale, ts: i64, now: i64) -> String {
    if loc == odyssey::Locale::En {
        return rel_time(ts, now);
    }
    let d = now.saturating_sub(ts);
    if d < 5 {
        i18n::t(loc, "time.just_now").to_string()
    } else if d < 60 {
        i18n::tf(loc, "time.s_ago", &[("n", &d.to_string())])
    } else if d < 3_600 {
        i18n::tf(loc, "time.m_ago", &[("n", &(d / 60).to_string())])
    } else if d < 86_400 {
        i18n::tf(loc, "time.h_ago", &[("n", &(d / 3_600).to_string())])
    } else {
        i18n::tf(loc, "time.d_ago", &[("n", &(d / 86_400).to_string())])
    }
}

fn fmt_countdown_l(loc: odyssey::Locale, secs_until: i64) -> String {
    if loc == odyssey::Locale::En {
        return fmt_countdown(secs_until);
    }
    if secs_until <= 0 {
        return i18n::t(loc, "time.now").to_string();
    }
    let days = secs_until / 86_400;
    let hours = (secs_until % 86_400) / 3_600;
    let mins = (secs_until % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        i18n::t(loc, "time.under_minute").to_string()
    }
}

fn fmt_date_l(loc: odyssey::Locale, secs: i64) -> String {
    if loc == odyssey::Locale::En {
        return fmt_date(secs);
    }
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => odyssey::fmt_date(loc, dt.year(), u8::from(dt.month()), dt.day()),
        Err(_) => secs.to_string(),
    }
}

fn status_label_l(loc: odyssey::Locale, status: &str) -> String {
    let key = match status {
        "down" => "status.state.down",
        "degraded" => "status.state.degraded",
        "maintenance" => "status.state.maintenance",
        _ => "status.state.operational",
    };
    i18n::t(loc, key).to_string()
}

fn status_pill_l(loc: odyssey::Locale, status: &str) -> String {
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        cls = crate::handlers::status_pill_class(status),
        label = esc(&status_label_l(loc, status)),
    )
}

fn incident_status_label_l(loc: odyssey::Locale, status: &str) -> String {
    if loc == odyssey::Locale::En {
        return crate::handlers::incident_status_label(status);
    }
    match status {
        "investigating" => "Investigating".to_string(),
        "identified" => "Identified".to_string(),
        "monitoring" => "Monitoring".to_string(),
        "resolved" => "Resolved".to_string(),
        other => other.to_string(),
    }
}

fn incident_status_pill_l(loc: odyssey::Locale, status: &str) -> String {
    if loc == odyssey::Locale::En {
        return incident_status_pill(status);
    }
    let cls = match status {
        "investigating" | "identified" => "pill-warn",
        "monitoring" => "pill-info",
        "resolved" => "pill-ok",
        _ => "pill-state",
    };
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        label = esc(&incident_status_label_l(loc, status)),
    )
}

fn severity_pill_l(loc: odyssey::Locale, severity: &str) -> String {
    if loc == odyssey::Locale::En {
        return severity_pill(severity);
    }
    let cls = match severity {
        "critical" => "pill-down",
        "major" => "pill-warn",
        _ => "pill-state",
    };
    let key = match severity {
        "critical" => "status.severity.critical",
        "major" => "status.severity.major",
        _ => "status.severity.minor",
    };
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        label = esc(i18n::t(loc, key))
    )
}

fn render_status(view: &StatusView, now: i64, loc: odyssey::Locale, theme: &str) -> String {
    let updated = rel_time_l(loc, view.updated_at, now);
    STATUS_HTML
        .replace("{{CSS}}", app_css())
        .replace("{{LANG}}", loc.bcp47())
        .replace("{{THEME}}", odyssey::html_theme_attr(theme))
        .replace("{{COLOR_SCHEME}}", odyssey::color_scheme_meta(theme))
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{THEMESWITCH}}", &render_theme_switch(theme))
        .replace("{{USERBOX}}", &userbox(i18n::t(loc, "status.topbar"), None))
        .replace("{{LANGSWITCH}}", &render_lang_switch(loc))
        .replace("{{STATUS_TITLE}}", i18n::t(loc, "status.title"))
        .replace("{{STATUS_SUB}}", i18n::t(loc, "status.sub"))
        .replace("{{GET_UPDATES}}", i18n::t(loc, "status.get_updates"))
        .replace("{{WEBHOOK}}", i18n::t(loc, "status.webhook"))
        .replace("{{RSS_FEED}}", i18n::t(loc, "status.rss_feed"))
        .replace("{{JSON_API}}", i18n::t(loc, "status.json_api"))
        .replace("{{BANNER}}", &render_hero(view, now, loc))
        .replace(
            "{{ACTIVE_INCIDENTS}}",
            &render_active_incidents(view, now, loc),
        )
        .replace("{{MAINTENANCE}}", &render_maintenances(view, now, loc))
        .replace("{{COMP_COUNT}}", &render_component_count(view))
        .replace("{{COMPONENTS_TITLE}}", i18n::t(loc, "status.components"))
        .replace("{{COMPONENTS}}", &render_components(view, now, loc))
        .replace("{{INFRA}}", &render_infra(loc, view.infra.as_ref(), now))
        .replace("{{PAST_TITLE}}", i18n::t(loc, "status.past"))
        .replace("{{HISTORY_NOTE}}", i18n::t(loc, "status.history_note"))
        .replace("{{PAST_INCIDENTS}}", &render_past_incidents(view, now, loc))
        .replace("{{SUBSCRIBE}}", &render_subscribe(loc))
        .replace("{{FOOTER}}", i18n::t(loc, "status.footer"))
        .replace("{{RSS}}", i18n::t(loc, "status.rss"))
        .replace(
            "{{UPDATED_LABEL}}",
            &i18n::tf(loc, "status.updated", &[("time", &updated)]),
        )
}

fn checked_uptime_avg<'a, I>(components: I) -> Option<f64>
where
    I: IntoIterator<Item = &'a ComponentView>,
{
    let (sum, count) = components
        .into_iter()
        .filter(|c| c.last_checked.is_some())
        .fold((0.0, 0usize), |(sum, count), c| {
            (sum + c.uptime_90d, count + 1)
        });
    (count > 0).then_some(sum / count as f64)
}

fn render_hero(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let (cls, headline_key, sub_key) = match view.overall {
        "down" => (
            "status-hero--down",
            "status.hero.down.title",
            "status.hero.down.sub",
        ),
        "degraded" => (
            "status-hero--warn",
            "status.hero.warn.title",
            "status.hero.warn.sub",
        ),
        "maintenance" => (
            "status-hero--info",
            "status.hero.maint.title",
            "status.hero.maint.sub",
        ),
        _ => (
            "status-hero--ok",
            "status.hero.ok.title",
            "status.hero.ok.sub",
        ),
    };
    let uptime = checked_uptime_avg(view.components.iter())
        .map(|avg| {
            format!(
                r#"<span class="status-hero__uptime" title="Average 90-day uptime across all components"><strong>{avg:.2}%</strong> uptime · 90 days</span>"#
            )
        })
        .unwrap_or_default();
    format!(
        r#"<section class="status-hero {cls}">
  <div class="status-hero__text">
    <h2 class="status-hero__headline">{headline}</h2>
    <p class="status-hero__sub">{sub}</p>
  </div>
  <div class="status-hero__meta">
    {uptime}
    <span class="status-hero__updated">Updated {updated}</span>
  </div>
</section>"#,
        headline = esc(i18n::t(loc, headline_key)),
        sub = esc(i18n::t(loc, sub_key)),
        updated = esc(&i18n::tf(
            loc,
            "status.updated",
            &[("time", &rel_time_l(loc, view.updated_at, now))]
        )),
    )
}

fn render_component_count(view: &StatusView) -> String {
    if view.components.is_empty() {
        String::new()
    } else {
        format!(
            r#"<span class="card__head-meta">{} monitored</span>"#,
            view.components.len()
        )
    }
}

/// The Components body. With no groups configured this renders a flat list of compact rows.
/// With groups, components are rendered under collapsible group sections, and any ungrouped
/// components fall into a trailing "Other" section.
fn render_components(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    if view.components.is_empty() {
        return format!(
            r#"<div class="empty">{}</div>"#,
            esc(i18n::t(loc, "status.no_components"))
        );
    }
    let incident_by_day = incident_titles_by_day(view);
    let team_first_day = team_first_data_day(view, now);
    if view.groups.is_empty() {
        let mut rows = String::new();
        for c in &view.components {
            rows.push_str(&render_component_row(
                c,
                now,
                loc,
                &incident_by_day,
                team_first_day,
            ));
        }
        rows.push_str(&render_barlegend(view, loc, team_first_day));
        return rows;
    }

    // Grouped: one collapsible section per group (ordered by the view), then ungrouped last.
    let mut out = String::new();
    for g in &view.groups {
        let members: Vec<&ComponentView> = view
            .components
            .iter()
            .filter(|c| c.group_id.as_deref() == Some(g.id.as_str()))
            .collect();
        if members.is_empty() {
            continue;
        }
        out.push_str(&render_group_section(
            &g.name,
            &members,
            g.status,
            now,
            loc,
            &incident_by_day,
            team_first_day,
        ));
    }

    // Ungrouped components (group_id None, or pointing at a group that no longer exists).
    let known: std::collections::HashSet<&str> =
        view.groups.iter().map(|g| g.id.as_str()).collect();
    let ungrouped: Vec<&ComponentView> = view
        .components
        .iter()
        .filter(|c| c.group_id.as_deref().is_none_or(|id| !known.contains(id)))
        .collect();
    if !ungrouped.is_empty() {
        let statuses: Vec<&str> = ungrouped.iter().map(|c| c.status).collect();
        out.push_str(&render_group_section(
            "Other",
            &ungrouped,
            group_rollup(&statuses),
            now,
            loc,
            &incident_by_day,
            team_first_day,
        ));
    }
    out.push_str(&render_barlegend(view, loc, team_first_day));
    out
}

fn render_group_section(
    name: &str,
    members: &[&ComponentView],
    rollup: &str,
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
    team_first_day: Option<i64>,
) -> String {
    let open = if rollup != "operational" { " open" } else { "" };
    let uptime = checked_uptime_avg(members.iter().copied())
        .map(|avg| {
            format!(
                r#"<span class="cgroup__uptime" title="Average 90-day uptime across this group">{avg:.2}%</span>"#
            )
        })
        .unwrap_or_default();
    let mut out = format!(
        r#"<details class="cgroup"{open}>
  <summary class="cgroup__head">
    <span class="cgroup__chev" aria-hidden="true"></span>
    <h3 class="cgroup__name">{name}</h3>
    <span class="cgroup__count">{count} components</span>
    {uptime}
    {pill}
  </summary>
  <div class="cgroup__body">"#,
        name = esc(name),
        count = members.len(),
        pill = status_pill_l(loc, rollup),
    );
    for c in members {
        out.push_str(&render_component_row(
            c,
            now,
            loc,
            incident_by_day,
            team_first_day,
        ));
    }
    out.push_str("</div></details>");
    out
}

fn state_mod(status: &str) -> &'static str {
    match status {
        "down" => "down",
        "degraded" => "warn",
        "maintenance" => "info",
        _ => "ok",
    }
}

/// Render a compact component row with latest latency, 90-day uptime, and the 90 daily bars.
fn render_component_row(
    c: &ComponentView,
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
    team_first_day: Option<i64>,
) -> String {
    let first_data_idx = c.days.iter().position(|d| d.uptime.is_some());
    let first_day = first_data_idx.map(|idx| day_bucket(now) - 89 + idx as i64);
    let monitoring_since = first_day.map(|day| fmt_date_l(loc, day * DAY_SECS));

    let mut bars = String::new();
    for d in &c.days {
        let data_uptime = match d.uptime {
            Some(pct) => format!("{pct:.2}%"),
            None => match &monitoring_since {
                Some(date) => format!("no data — monitoring began {date}"),
                None => "no data".to_string(),
            },
        };
        let inc_attr = if matches!(d.status, "warn" | "down") {
            incident_by_day
                .get(&d.date)
                .map(|s| format!(r#" data-inc="{}""#, esc(s)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        bars.push_str(&format!(
            r#"<span class="bar bar-{cls}" data-date="{date}" data-status="{status}" data-uptime="{uptime}"{inc_attr}></span>"#,
            cls = d.status,
            date = esc(&d.date),
            status = esc(d.status),
            uptime = esc(&data_uptime),
        ));
    }
    let since = if c.days.first().is_some_and(|d| d.uptime.is_none()) {
        match (first_day, monitoring_since) {
            (Some(day), Some(_)) if team_first_day == Some(day) => String::new(),
            (_, Some(date)) => format!(
                r#"<span class="crow__since">monitoring since {}</span>"#,
                esc(&date)
            ),
            _ => r#"<span class="crow__since">awaiting first check</span>"#.to_string(),
        }
    } else {
        String::new()
    };
    let latest_latency = fmt_latency(c.latency_ms);
    let latency = match c.latency_avg_ms {
        Some(avg) => format!("~{avg} ms"),
        None => "—".to_string(),
    };
    let spark = render_latency_spark(&c.latency_points);
    let pct_title = if c.last_checked.is_some() {
        "90-day uptime"
    } else {
        "awaiting first check"
    };
    let pct = if c.last_checked.is_some() {
        format!("{:.2}%", c.uptime_90d)
    } else {
        "—".to_string()
    };
    let state = state_mod(c.status);
    format!(
        r#"<div class="crow">
  <span class="crow__id">
    <span class="crow__dot crow__dot--{state}" aria-hidden="true"></span>
    <span class="crow__name" title="{name}">{name}</span>
  </span>
  <span class="bc-lat">{spark}<span class="crow__lat" title="24h average · latest {latest_latency}">{latency}</span></span>
  <div class="crow__track" aria-hidden="true"><div class="bars">{bars}</div></div>
  <span class="crow__pct" title="{pct_title}">{pct}</span>
  <span class="crow__state crow__state--{state}">{label}</span>
  {since}
</div>"#,
        name = esc(&c.name),
        latest_latency = esc(&latest_latency),
        latency = esc(&latency),
        spark = spark,
        bars = bars,
        pct_title = pct_title,
        pct = esc(&pct),
        label = esc(&status_label_l(loc, c.status)),
    )
}

fn render_latency_spark(points: &[Option<i64>]) -> String {
    let vals: Vec<(usize, i64)> = points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.map(|v| (i, v.max(0))))
        .collect();
    if vals.len() < 2 {
        return String::new();
    }
    let max = vals.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1) as f64;
    let denom = (points.len().saturating_sub(1)).max(1) as f64;
    let coords: Vec<String> = vals
        .iter()
        .map(|(i, v)| {
            let x = *i as f64 / denom * 44.0;
            let y = 13.0 - (*v as f64 / max * 12.0);
            format!("{x:.1},{y:.1}")
        })
        .collect();
    format!(
        r#"<svg class="bc-lat-spark" viewBox="0 0 44 14" preserveAspectRatio="none" aria-hidden="true"><polyline fill="none" stroke="currentColor" stroke-width="1.5" vector-effect="non-scaling-stroke" points="{}"/></svg>"#,
        coords.join(" ")
    )
}

fn component_first_data_day(c: &ComponentView, now: i64) -> Option<i64> {
    c.days
        .iter()
        .position(|d| d.uptime.is_some())
        .map(|idx| day_bucket(now) - 89 + idx as i64)
}

fn team_first_data_day(view: &StatusView, now: i64) -> Option<i64> {
    view.components
        .iter()
        .filter_map(|c| component_first_data_day(c, now))
        .min()
}

fn incident_titles_by_day(view: &StatusView) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for inc in &view.incidents {
        out.entry(day_date(day_bucket(inc.created_at)))
            .or_default()
            .push(inc.title.clone());
    }
    out.into_iter()
        .map(|(day, titles)| (day, titles.join("; ")))
        .collect()
}

fn render_barlegend(
    view: &StatusView,
    loc: odyssey::Locale,
    team_first_day: Option<i64>,
) -> String {
    let mid = checked_uptime_avg(view.components.iter())
        .map(|avg| format!("{avg:.2}% uptime · All times UTC"))
        .unwrap_or_else(|| "No uptime data yet · All times UTC".to_string());
    let since = team_first_day
        .map(|day| format!(" · Monitoring since {}", fmt_date_l(loc, day * DAY_SECS)))
        .unwrap_or_default();
    format!(
        r#"<div class="bc-barlegend"><span>90 days ago</span><span class="bc-barlegend__mid">{mid}{since}</span><span>Today</span></div>"#,
        mid = esc(&mid),
        since = esc(&since),
    )
}

/// The public "Subscribe to updates" card: a webhook URL form posting to `/subscriptions`.
/// Public (no CSRF); double opt-in confirmation gates any delivery.
fn render_subscribe(loc: odyssey::Locale) -> String {
    format!(
        r#"<section class="card" id="subscribe">
  <div class="card__head"><h2>{title}</h2></div>
  <div class="card__body">
    <p class="hint">{body}</p>
    <form method="post" action="/subscriptions" class="subscribe-form">
      <div class="field">
        <label for="sub-target">{field}</label>
        <input type="url" id="sub-target" name="target" required placeholder="https://example.com/hooks/holdfast-status">
      </div>
      <button class="btn btn-primary" type="submit">{button}</button>
    </form>
    <p class="hint--muted">{note}</p>
  </div>
</section>"#,
        title = esc(i18n::t(loc, "status.subscribe.title")),
        body = esc(i18n::t(loc, "status.subscribe.body")),
        field = esc(i18n::t(loc, "status.subscribe.field")),
        button = esc(i18n::t(loc, "status.subscribe.button")),
        note = esc(i18n::t(loc, "status.subscribe.note")),
    )
}

fn render_infra(loc: odyssey::Locale, infra: Option<&vitals::InfraPublic>, now: i64) -> String {
    let Some(infra) = infra else {
        return String::new();
    };
    let pill_cls = match infra.overall {
        "high" => "pill-down",
        "elevated" => "pill-warn",
        "ok" => "pill-ok",
        _ => "pill-state",
    };
    let mut meters = String::new();
    for band in &infra.bands {
        let pct_text = band
            .worst_pct
            .map(|pct| format!("{:.0}%", pct.round()))
            .unwrap_or_else(|| "—".to_string());
        let width = band
            .worst_pct
            .map(|pct| pct.clamp(0.0, 100.0))
            .unwrap_or(0.0);
        meters.push_str(&format!(
            r#"<div class="bc-infra__meter"><span class="bc-infra__label">{label}</span><div class="bc-infra__gauge"><span class="bc-infra__fill bc-infra__fill--{band}" style="width:{width:.0}%"></span></div><span class="bc-infra__pct">{pct}</span><span class="bc-infra__band bc-infra__band--{band}">{band_label}</span></div>"#,
            label = esc(infra_metric_label(loc, band.metric)),
            band = esc(band.band),
            pct = esc(&pct_text),
            band_label = esc(infra_band_label(loc, band.band)),
        ));
    }

    let current = now / vitals::TREND_BUCKET_SECS;
    let mut cells = String::new();
    for (idx, band) in infra.trend.iter().enumerate() {
        let bucket = current - infra.trend.len() as i64 + 1 + idx as i64;
        cells.push_str(&format!(
            r#"<span class="bc-infra__cell bc-infra__cell--{band}" data-hour="{hour}" data-band="{band_label}"></span>"#,
            band = esc(band),
            hour = esc(&hour_label(bucket)),
            band_label = esc(infra_band_label(loc, band)),
        ));
    }

    format!(
        r#"<section class="card bc-infra"><div class="card__head card__head--split"><h2>{title}</h2><span class="pill {pill_cls}">{state}</span></div><div class="card__body"><div class="bc-infra__meters">{meters}</div><p class="bc-infra__trend-label">{trend}</p><div class="bc-infra__trend" role="img" aria-label="{trend}">{cells}</div><p class="bc-infra__note">{note}</p></div></section>"#,
        title = esc(i18n::t(loc, "infra.title")),
        pill_cls = pill_cls,
        state = esc(infra_state_label(loc, infra.overall)),
        meters = meters,
        trend = esc(i18n::t(loc, "infra.trend")),
        cells = cells,
        note = esc(i18n::t(loc, "infra.note")),
    )
}

fn infra_metric_label<'a>(loc: odyssey::Locale, metric: &'a str) -> &'a str {
    match metric {
        "cpu" => i18n::t(loc, "infra.metric.cpu"),
        "memory" => i18n::t(loc, "infra.metric.memory"),
        "disk" => i18n::t(loc, "infra.metric.disk"),
        _ => metric,
    }
}

fn infra_band_label(loc: odyssey::Locale, band: &str) -> &'static str {
    match band {
        "ok" => i18n::t(loc, "infra.band.ok"),
        "elevated" => i18n::t(loc, "infra.band.elevated"),
        "high" => i18n::t(loc, "infra.band.high"),
        _ => i18n::t(loc, "infra.band.unknown"),
    }
}

fn infra_state_label(loc: odyssey::Locale, band: &str) -> &'static str {
    match band {
        "ok" => i18n::t(loc, "infra.state.ok"),
        "elevated" => i18n::t(loc, "infra.state.elevated"),
        "high" => i18n::t(loc, "infra.state.high"),
        _ => i18n::t(loc, "infra.state.unknown"),
    }
}

fn hour_label(bucket: i64) -> String {
    let secs = bucket * vitals::TREND_BUCKET_SECS;
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => format!("{:02}:00 UTC", dt.hour()),
        Err(_) => "00:00 UTC".to_string(),
    }
}

/// Updates belonging to one incident, newest first (`view.updates` is already newest-first).
fn updates_for<'a>(view: &'a StatusView, incident_id: &str) -> Vec<&'a IncidentUpdate> {
    view.updates
        .iter()
        .filter(|u| u.incident_id == incident_id)
        .collect()
}

/// The expandable update timeline for one incident: its updates newest first, closing with
/// the opening report (the incident row itself).
fn render_timeline(
    inc: &Incident,
    updates: &[&IncidentUpdate],
    now: i64,
    loc: odyssey::Locale,
) -> String {
    let mut items = String::new();
    for u in updates {
        items.push_str(&format!(
            r#"<li class="timeline__item">{pill} <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
            pill = incident_status_pill_l(loc, &u.status),
            ago = esc(&rel_time_l(loc, u.created_at, now)),
            body = esc(&u.body),
        ));
    }
    items.push_str(&format!(
        r#"<li class="timeline__item"><span class="pill pill-state">reported</span> <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
        ago = esc(&rel_time_l(loc, inc.created_at, now)),
        body = esc(&inc.body),
    ));
    format!(
        r#"<details class="timeline"><summary>Timeline ({n})</summary><ol>{items}</ol></details>"#,
        n = updates.len() + 1,
    )
}

fn render_affected_l(loc: odyssey::Locale, affected: &str) -> String {
    let names = affected_names(affected);
    if names.is_empty() {
        return String::new();
    }
    let pills: String = names
        .iter()
        .map(|n| format!(r#"<span class="pill pill-state">{}</span>"#, esc(n)))
        .collect();
    format!(
        r#"<div class="affected">{}{pills}</div>"#,
        esc(i18n::t(loc, "status.affects"))
    )
}

/// The "Active incidents" banner section above the components — one severity-tinted card
/// per non-resolved incident. Empty string (section omitted) when everything is resolved.
fn render_active_incidents(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let active: Vec<&Incident> = view
        .incidents
        .iter()
        .filter(|i| i.status != "resolved")
        .collect();
    if active.is_empty() {
        return String::new();
    }
    let mut out = format!(
        r#"<h2 class="section-title">{}</h2>"#,
        esc(i18n::t(loc, "status.active"))
    );
    for inc in active {
        let updates = updates_for(view, &inc.id);
        // The card leads with the LATEST update (or the opening report when none yet).
        let latest = updates
            .first()
            .map(|u| u.body.as_str())
            .unwrap_or(inc.body.as_str());
        out.push_str(&format!(
            r#"<section class="card incident-card sev-{sev}"><div class="card__body"><article class="incident">
  <div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{sev_pill}{status_pill}</span></div>
  {affected}
  <p class="incident__body">{latest}</p>
  <div class="incident__time">Opened {opened} · Last update {updated}</div>
  {timeline}
</article></div></section>"#,
            sev = esc(&inc.severity),
            title = esc(&inc.title),
            sev_pill = severity_pill_l(loc, &inc.severity),
            status_pill = incident_status_pill_l(loc, &inc.status),
            affected = render_affected_l(loc, &inc.affected),
            latest = esc(latest),
            opened = esc(&rel_time_l(loc, inc.created_at, now)),
            updated = esc(&rel_time_l(loc, inc.updated_at, now)),
            timeline = render_timeline(inc, &updates, now, loc),
        ));
    }
    out
}

/// Upcoming/ongoing maintenance windows as info-tinted cards. Empty string when none.
fn render_maintenances(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    if view.maintenances.is_empty() {
        return String::new();
    }
    let mut out = format!(
        r#"<h2 class="section-title">{}</h2>"#,
        esc(i18n::t(loc, "status.maintenance"))
    );
    for m in &view.maintenances {
        // Surface a prominent countdown: ongoing windows show time-to-end, upcoming ones
        // time-to-start (upcoming/ongoing windows are the only ones on the public surface).
        let (state_pill, countdown) = if maintenance_ongoing(m, now) {
            (
                r#"<span class="pill pill-info">in progress</span>"#,
                format!(
                    r#"<span class="countdown">ends in {}</span>"#,
                    esc(&fmt_countdown_l(loc, m.ends_at - now))
                ),
            )
        } else {
            (
                r#"<span class="pill pill-state">scheduled</span>"#,
                format!(
                    r#"<span class="countdown">starts in {}</span>"#,
                    esc(&fmt_countdown_l(loc, m.starts_at - now))
                ),
            )
        };
        out.push_str(&format!(
            r#"<section class="card incident-card sev-maintenance"><div class="card__body"><article class="incident">
  <div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{state_pill}{countdown}</span></div>
  {affected}
  <p class="incident__body">{body}</p>
  <div class="incident__time">{starts} → {ends}</div>
</article></div></section>"#,
            title = esc(&m.title),
            state_pill = state_pill,
            countdown = countdown,
            affected = render_affected_l(loc, &m.affected),
            body = esc(&m.body),
            starts = esc(&fmt_datetime(m.starts_at)),
            ends = esc(&fmt_datetime(m.ends_at)),
        ));
    }
    out
}

/// "Past incidents": fixed calendar buckets for the last 14 days, newest day first.
fn render_past_incidents(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let mut out = String::new();
    let today = day_bucket(now);
    for offset in 0..14 {
        let day = today - offset;
        out.push_str(&format!(
            r#"<div class="day-group"><h3 class="day-group__date">{}</h3>"#,
            esc(&fmt_date_l(loc, day * DAY_SECS)),
        ));
        let mut count = 0usize;
        for inc in view
            .incidents
            .iter()
            .filter(|inc| day_bucket(inc.created_at) == day)
        {
            count += 1;
            let resolved = inc.status == "resolved";
            let mut when = if resolved && inc.resolved_at > 0 {
                format!(
                    "Opened {} · Resolved {}",
                    rel_time_l(loc, inc.created_at, now),
                    rel_time_l(loc, inc.resolved_at, now)
                )
            } else {
                format!("Opened {}", rel_time_l(loc, inc.created_at, now))
            };
            if resolved && inc.resolved_at > 0 {
                when.push_str(&format!(
                    " · lasted {}",
                    fmt_countdown_l(loc, inc.resolved_at - inc.created_at)
                ));
            }
            out.push_str(&format!(
                r#"<article class="incident{muted}">
  <div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{sev_pill}{status_pill}</span></div>
  <p class="incident__body">{body}</p>
  <div class="incident__time">{when}</div>
</article>"#,
                muted = if resolved { " incident--resolved" } else { "" },
                title = esc(&inc.title),
                sev_pill = severity_pill_l(loc, &inc.severity),
                status_pill = incident_status_pill_l(loc, &inc.status),
                body = esc(&inc.body),
                when = esc(&when),
            ));
        }
        if count == 0 {
            out.push_str(&format!(
                r#"<p class="day-group__none">{}</p>"#,
                esc(i18n::t(loc, "status.none_day"))
            ));
        }
        out.push_str("</div>");
    }
    out
}
