//! PUBLIC status surface: the server-rendered status page and the machine-readable JSON.
//!
//! Both are unauthenticated by design (placed behind a Sluice `auth=public` route). The
//! page mirrors the HOLDFAST enterprise brand: app-bar, overall hero, an active-incidents
//! section (severity-tinted cards with an expandable update timeline), maintenance notices,
//! compact component rows with rolling uptime + the classic 90-day uptime bar row,
//! and a "Past incidents" section (last 14 days, grouped by day).

use axum::extract::State;
use axum::response::Html;
use axum::Json;

use crate::handlers::{
    app_css, esc, fmt_countdown, fmt_date, fmt_datetime, fmt_latency, incident_status_pill,
    rel_time, severity_pill, status_label, status_pill, userbox, SHIELD_SVG,
};
use crate::model::{
    affected_names, build_status, day_bucket, group_rollup, maintenance_ongoing, ComponentView,
    StatusView, DAY_SECS,
};
use crate::store::{Incident, IncidentUpdate};
use crate::{now_secs, AppState};

const STATUS_HTML: &str = include_str!("../../templates/status.html");

/// `GET /status` — the public status page (no auth).
pub async fn status_page(State(state): State<AppState>) -> Html<String> {
    let now = now_secs();
    let view = build_status(state.store.as_ref(), now).await;
    Html(render_status(&view, now))
}

/// `GET /api/status` — the public machine-readable status snapshot (no auth).
pub async fn api_status(State(state): State<AppState>) -> Json<StatusView> {
    let now = now_secs();
    Json(build_status(state.store.as_ref(), now).await)
}

fn render_status(view: &StatusView, now: i64) -> String {
    STATUS_HTML
        .replace("{{CSS}}", app_css())
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{USERBOX}}", &userbox("System Status", None))
        .replace("{{BANNER}}", &render_hero(view, now))
        .replace("{{ACTIVE_INCIDENTS}}", &render_active_incidents(view, now))
        .replace("{{MAINTENANCE}}", &render_maintenances(view, now))
        .replace("{{COMP_COUNT}}", &render_component_count(view))
        .replace("{{COMPONENTS}}", &render_components(view, now))
        .replace("{{PAST_INCIDENTS}}", &render_past_incidents(view, now))
        .replace("{{SUBSCRIBE}}", &render_subscribe())
        .replace("{{UPDATED}}", &rel_time(view.updated_at, now))
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

fn render_hero(view: &StatusView, now: i64) -> String {
    let (cls, headline, sub) = match view.overall {
        "down" => (
            "status-hero--down",
            "Service disruption",
            "One or more components are down. We are on it.",
        ),
        "degraded" => (
            "status-hero--warn",
            "Partial degradation",
            "Some components are degraded; service may be slower than usual.",
        ),
        "maintenance" => (
            "status-hero--info",
            "Scheduled maintenance underway",
            "Planned maintenance is in progress; affected components may be briefly unavailable.",
        ),
        _ => (
            "status-hero--ok",
            "All systems operational",
            "Every monitored component is up and healthy.",
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
        updated = esc(&rel_time(view.updated_at, now)),
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
fn render_components(view: &StatusView, now: i64) -> String {
    if view.components.is_empty() {
        return r#"<div class="empty">No components are being monitored yet.</div>"#.to_string();
    }
    if view.groups.is_empty() {
        let mut rows = String::new();
        for c in &view.components {
            rows.push_str(&render_component_row(c, now));
        }
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
        out.push_str(&render_group_section(&g.name, &members, g.status, now));
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
        ));
    }
    out
}

fn render_group_section(name: &str, members: &[&ComponentView], rollup: &str, now: i64) -> String {
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
        pill = status_pill(rollup),
    );
    for c in members {
        out.push_str(&render_component_row(c, now));
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
fn render_component_row(c: &ComponentView, now: i64) -> String {
    let first_data_idx = c.days.iter().position(|d| d.uptime.is_some());
    let monitoring_since =
        first_data_idx.map(|idx| fmt_date((day_bucket(now) - 89 + idx as i64) * DAY_SECS));

    let mut bars = String::new();
    for d in &c.days {
        let data_uptime = match d.uptime {
            Some(pct) => format!("{pct:.2}%"),
            None => match &monitoring_since {
                Some(date) => format!("no data — monitoring began {date}"),
                None => "no data".to_string(),
            },
        };
        bars.push_str(&format!(
            r#"<span class="bar bar-{cls}" data-date="{date}" data-status="{status}" data-uptime="{uptime}"></span>"#,
            cls = d.status,
            date = esc(&d.date),
            status = esc(d.status),
            uptime = esc(&data_uptime),
        ));
    }
    let since = if c.days.first().is_some_and(|d| d.uptime.is_none()) {
        match monitoring_since {
            Some(date) => format!(
                r#"<span class="crow__since">monitoring since {}</span>"#,
                esc(&date)
            ),
            None => r#"<span class="crow__since">awaiting first check</span>"#.to_string(),
        }
    } else {
        String::new()
    };
    let latest_latency = fmt_latency(c.latency_ms);
    let latency = match c.latency_avg_ms {
        Some(avg) => format!("~{avg} ms"),
        None => "—".to_string(),
    };
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
  <span class="crow__lat" title="24h average · latest {latest_latency}">{latency}</span>
  <div class="crow__track" aria-hidden="true"><div class="bars">{bars}</div></div>
  <span class="crow__pct" title="{pct_title}">{pct}</span>
  <span class="crow__state crow__state--{state}">{label}</span>
  {since}
</div>"#,
        name = esc(&c.name),
        latest_latency = esc(&latest_latency),
        latency = esc(&latency),
        bars = bars,
        pct_title = pct_title,
        pct = esc(&pct),
        label = status_label(c.status),
    )
}

/// The public "Subscribe to updates" card: a webhook URL form posting to `/subscriptions`.
/// Public (no CSRF); double opt-in confirmation gates any delivery.
fn render_subscribe() -> String {
    r#"<section class="card" id="subscribe">
  <div class="card__head"><h2>Subscribe to updates</h2></div>
  <div class="card__body">
    <p class="hint">Get a signed JSON webhook POST whenever an incident is opened or updated.</p>
    <form method="post" action="/subscriptions" class="subscribe-form">
      <div class="field">
        <label for="sub-target">Webhook URL</label>
        <input type="url" id="sub-target" name="target" required placeholder="https://example.com/hooks/holdfast-status">
      </div>
      <button class="btn btn-primary" type="submit">Subscribe</button>
    </form>
    <p class="hint--muted">Webhook-only — Beacon has no outbound mail path. You will confirm before any delivery.</p>
  </div>
</section>"#
        .to_string()
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
fn render_timeline(inc: &Incident, updates: &[&IncidentUpdate], now: i64) -> String {
    let mut items = String::new();
    for u in updates {
        items.push_str(&format!(
            r#"<li class="timeline__item">{pill} <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
            pill = incident_status_pill(&u.status),
            ago = esc(&rel_time(u.created_at, now)),
            body = esc(&u.body),
        ));
    }
    items.push_str(&format!(
        r#"<li class="timeline__item"><span class="pill pill-state">reported</span> <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
        ago = esc(&rel_time(inc.created_at, now)),
        body = esc(&inc.body),
    ));
    format!(
        r#"<details class="timeline"><summary>Timeline ({n})</summary><ol>{items}</ol></details>"#,
        n = updates.len() + 1,
    )
}

/// "Affects: a, b" pills row, empty string when the incident names no components.
fn render_affected(affected: &str) -> String {
    let names = affected_names(affected);
    if names.is_empty() {
        return String::new();
    }
    let pills: String = names
        .iter()
        .map(|n| format!(r#"<span class="pill pill-state">{}</span>"#, esc(n)))
        .collect();
    format!(r#"<div class="affected">Affects{pills}</div>"#)
}

/// The "Active incidents" banner section above the components — one severity-tinted card
/// per non-resolved incident. Empty string (section omitted) when everything is resolved.
fn render_active_incidents(view: &StatusView, now: i64) -> String {
    let active: Vec<&Incident> = view
        .incidents
        .iter()
        .filter(|i| i.status != "resolved")
        .collect();
    if active.is_empty() {
        return String::new();
    }
    let mut out = String::from(r#"<h2 class="section-title">Active incidents</h2>"#);
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
            sev_pill = severity_pill(&inc.severity),
            status_pill = incident_status_pill(&inc.status),
            affected = render_affected(&inc.affected),
            latest = esc(latest),
            opened = esc(&rel_time(inc.created_at, now)),
            updated = esc(&rel_time(inc.updated_at, now)),
            timeline = render_timeline(inc, &updates, now),
        ));
    }
    out
}

/// Upcoming/ongoing maintenance windows as info-tinted cards. Empty string when none.
fn render_maintenances(view: &StatusView, now: i64) -> String {
    if view.maintenances.is_empty() {
        return String::new();
    }
    let mut out = String::from(r#"<h2 class="section-title">Maintenance</h2>"#);
    for m in &view.maintenances {
        // Surface a prominent countdown: ongoing windows show time-to-end, upcoming ones
        // time-to-start (upcoming/ongoing windows are the only ones on the public surface).
        let (state_pill, countdown) = if maintenance_ongoing(m, now) {
            (
                r#"<span class="pill pill-info">in progress</span>"#,
                format!(
                    r#"<span class="countdown">ends in {}</span>"#,
                    esc(&fmt_countdown(m.ends_at - now))
                ),
            )
        } else {
            (
                r#"<span class="pill pill-state">scheduled</span>"#,
                format!(
                    r#"<span class="countdown">starts in {}</span>"#,
                    esc(&fmt_countdown(m.starts_at - now))
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
            affected = render_affected(&m.affected),
            body = esc(&m.body),
            starts = esc(&fmt_datetime(m.starts_at)),
            ends = esc(&fmt_datetime(m.ends_at)),
        ));
    }
    out
}

/// "Past incidents": fixed calendar buckets for the last 14 days, newest day first.
fn render_past_incidents(view: &StatusView, now: i64) -> String {
    let mut out = String::new();
    let today = day_bucket(now);
    for offset in 0..14 {
        let day = today - offset;
        out.push_str(&format!(
            r#"<div class="day-group"><h3 class="day-group__date">{}</h3>"#,
            esc(&fmt_date(day * DAY_SECS)),
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
                    rel_time(inc.created_at, now),
                    rel_time(inc.resolved_at, now)
                )
            } else {
                format!("Opened {}", rel_time(inc.created_at, now))
            };
            if resolved && inc.resolved_at > 0 {
                when.push_str(&format!(
                    " · lasted {}",
                    fmt_countdown(inc.resolved_at - inc.created_at)
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
                sev_pill = severity_pill(&inc.severity),
                status_pill = incident_status_pill(&inc.status),
                body = esc(&inc.body),
                when = esc(&when),
            ));
        }
        if count == 0 {
            out.push_str(r#"<p class="day-group__none">No incidents reported.</p>"#);
        }
        out.push_str("</div>");
    }
    out
}
