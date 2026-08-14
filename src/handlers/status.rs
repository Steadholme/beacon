//! PUBLIC status surface: the server-rendered status page and the machine-readable JSON.
//!
//! Both are unauthenticated by design (placed behind a Sluice `auth=public` route). The
//! page mirrors the Steadholme publication identity: app-bar, factual overall state, an
//! operational snapshot strip, the active-incident ledger with expandable update timelines,
//! scheduled maintenance, native category disclosures over compact component rows with rolling
//! uptime + a read-model-declared evidence window, infra vitals when available, and a
//! "Past incidents" section (last 14 days, grouped by day).

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

use crate::handlers::{
    app_css, dynamic_js, esc, fmt_countdown, fmt_date, fmt_datetime, fmt_latency, hv,
    incident_status_pill, rel_time, render_theme_switch, severity_pill, userbox, SHIELD_SVG,
};
use crate::i18n;
use crate::model::{
    affected_names, build_public_status, build_status, day_bucket, day_date, group_rollup,
    maintenance_ongoing, ComponentView, StatusView, DAY_SECS,
};
use crate::store::{Incident, IncidentUpdate};
use crate::{now_secs, vitals, AppState};

const STATUS_HTML: &str = include_str!("../../templates/status.html");
const STATUS_LIVE_ID: &str = "status-live";
const STATUS_LIVE_SELECTOR: &str = "#status-live";

/// `GET /status` — the public status page (no auth).
///
/// An Odyssey Wire request (`X-Wire: 1`) receives only the shared live region. A normal request,
/// including a no-JavaScript activation of the refresh link, always receives the complete SSR
/// document. The snapshot is explicitly non-cacheable: it varies by representation, locale, and
/// theme while also carrying live operational data. Locale comes from the estate-wide
/// `__Secure-lang` cookie / `Accept-Language` chain only — never from query parameters.
pub async fn status_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let now = now_secs();
    let loc = odyssey::resolve_locale(hv(&headers, "cookie"), hv(&headers, "accept-language"));
    let theme = odyssey::resolve_theme(hv(&headers, "cookie"));
    let mut view =
        build_public_status(state.store.as_ref(), now, &state.config.public_catalog).await;
    attach_infra(&mut view, &state, now);

    let body = if is_wire_request(&headers) {
        render_status_live(&view, now, loc)
    } else {
        render_status(&view, now, loc, theme, state.config.public_webhooks_enabled)
    };
    let mut response = Html(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("X-Wire"));
    response
}

/// `GET /api/status` — the public machine-readable status snapshot (no auth).
pub async fn api_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let now = now_secs();
    let _loc = odyssey::resolve_locale(hv(&headers, "cookie"), hv(&headers, "accept-language"));
    let mut view =
        build_public_status(state.store.as_ref(), now, &state.config.public_catalog).await;
    attach_infra(&mut view, &state, now);
    let mut response = Json(view).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /api/status` on the dedicated internal listener — the complete raw check model used by
/// Portal Estate's server-side joins. This handler is intentionally absent from the public
/// router and remains non-cacheable because it carries live operational data.
pub async fn api_internal_status(State(state): State<AppState>) -> Response {
    let now = now_secs();
    let mut view = build_status(state.store.as_ref(), now).await;
    attach_infra(&mut view, &state, now);
    let mut response = Json(view).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn attach_infra(view: &mut StatusView, state: &AppState, now: i64) {
    view.infra = state
        .vitals
        .as_ref()
        .and_then(|v| v.snapshot())
        .and_then(|snap| vitals::public_infra(&snap, now));
}

fn is_wire_request(headers: &HeaderMap) -> bool {
    hv(headers, "x-wire").is_some_and(|value| value.trim() == "1")
}

fn render_lang_switch(loc: odyssey::Locale) -> String {
    let mut out = format!(
        r#"<nav class="bc-lang" aria-label="{}">"#,
        esc(i18n::t(loc, "status.lang_label"))
    );
    for l in odyssey::Locale::all() {
        let (active, current) = if l == loc {
            (" is-active", r#" aria-current="true""#)
        } else {
            ("", "")
        };
        out.push_str(&format!(
            r#"<a class="langswitch__opt{active}" href="/_gw/lang?to={code}"{current}>{name}</a>"#,
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
        i18n::tf(
            loc,
            "time.duration.dh",
            &[("d", &days.to_string()), ("h", &hours.to_string())],
        )
    } else if hours > 0 {
        i18n::tf(
            loc,
            "time.duration.hm",
            &[("h", &hours.to_string()), ("m", &mins.to_string())],
        )
    } else if mins > 0 {
        i18n::tf(loc, "time.duration.m", &[("m", &mins.to_string())])
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
        "investigating" => i18n::t(loc, "status.incident.investigating").to_string(),
        "identified" => i18n::t(loc, "status.incident.identified").to_string(),
        "monitoring" => i18n::t(loc, "status.incident.monitoring").to_string(),
        "resolved" => i18n::t(loc, "status.incident.resolved").to_string(),
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

fn render_refresh(loc: odyssey::Locale) -> String {
    odyssey::link_button_with_wire(
        "/status",
        i18n::t(loc, "status.refresh"),
        odyssey::Variant::Secondary,
        odyssey::BtnOpts {
            small: true,
            ..Default::default()
        },
        odyssey::WireOpts::new(STATUS_LIVE_SELECTOR)
            .select(STATUS_LIVE_SELECTOR)
            .swap(odyssey::WireSwap::Outer)
            .busy_label(i18n::t(loc, "status.refresh_busy"))
            .success_message(i18n::t(loc, "status.refresh_success"))
            .error_message(i18n::t(loc, "status.refresh_error")),
    )
    .0
}

fn render_status(
    view: &StatusView,
    now: i64,
    loc: odyssey::Locale,
    theme: &str,
    webhooks_enabled: bool,
) -> String {
    let updated = rel_time_l(loc, view.updated_at, now);
    let webhook_link = if webhooks_enabled {
        format!(
            r##"<a class="updates-pop__item" href="#subscribe">{}</a>"##,
            esc(i18n::t(loc, "status.webhook"))
        )
    } else {
        String::new()
    };
    STATUS_HTML
        .replace("{{CSS}}", app_css())
        .replace("{{LANG}}", loc.bcp47())
        .replace("{{THEME}}", odyssey::html_theme_attr(theme))
        .replace("{{COLOR_SCHEME}}", odyssey::color_scheme_meta(theme))
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{THEMESWITCH}}", &render_theme_switch(theme))
        .replace("{{USERBOX}}", &userbox(i18n::t(loc, "status.topbar"), None))
        .replace("{{LANGSWITCH}}", &render_lang_switch(loc))
        .replace("{{SKIP_TO_STATUS}}", i18n::t(loc, "status.skip_to_content"))
        .replace("{{STATUS_TITLE}}", i18n::t(loc, "status.title"))
        .replace("{{STATUS_SUB}}", i18n::t(loc, "status.sub"))
        .replace(
            "{{PUBLIC_SIGNAL}}",
            &format!(
                r#"<span class="statushead__signal"><span aria-hidden="true"></span>{}</span>"#,
                esc(i18n::t(loc, "status.public_read_only"))
            ),
        )
        .replace("{{REFRESH}}", &render_refresh(loc))
        .replace("{{GET_UPDATES}}", i18n::t(loc, "status.get_updates"))
        .replace("{{WEBHOOK_LINK}}", &webhook_link)
        .replace("{{RSS_FEED}}", i18n::t(loc, "status.rss_feed"))
        .replace("{{JSON_API}}", i18n::t(loc, "status.json_api"))
        .replace("{{STATUS_LIVE}}", &render_status_live(view, now, loc))
        .replace("{{SUBSCRIBE}}", &render_subscribe(loc, webhooks_enabled))
        .replace("{{FOOTER}}", i18n::t(loc, "status.footer"))
        .replace("{{RSS}}", i18n::t(loc, "status.rss"))
        .replace(
            "{{UPDATED_LABEL}}",
            &i18n::tf(loc, "status.updated", &[("time", &updated)]),
        )
        .replace("{{SCRIPTS}}", dynamic_js())
}

/// Render the one replaceable public status region. Full-page SSR and Wire fragments call this
/// exact function, so a dynamic refresh cannot drift from the no-JavaScript representation.
fn render_status_live(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    format!(
        r#"<div id="{id}" class="status-live" role="region" aria-label="{label}">
{banner}
{snapshot}
{active}
{maintenance}
<section class="card status-components">
  <div class="card__head card__head--split"><h2>{components_title}</h2>{component_count}</div>
  <div class="card__body">{components}</div>
</section>
{infra}
<section class="status-history">
  <h2 class="section-title">{past_title}</h2>
  <p class="sub">{history_note} <a href="/feed.xml">{rss_feed}</a>.</p>
  {past_incidents}
</section>
</div>"#,
        id = STATUS_LIVE_ID,
        label = esc(i18n::t(loc, "status.live_region")),
        banner = render_hero(view, now, loc),
        snapshot = render_snapshot(view, loc),
        active = render_active_incidents(view, now, loc),
        maintenance = render_maintenances(view, now, loc),
        components_title = esc(i18n::t(loc, "status.components")),
        component_count = render_component_count(view, loc),
        components = render_components(view, now, loc),
        infra = render_infra(loc, view.infra.as_ref(), now),
        past_title = esc(i18n::t(loc, "status.past")),
        history_note = esc(i18n::t(loc, "status.history_note")),
        rss_feed = esc(i18n::t(loc, "status.rss_feed")),
        past_incidents = render_past_incidents(view, now, loc),
    )
}

fn render_snapshot(view: &StatusView, loc: odyssey::Locale) -> String {
    let active = view
        .incidents
        .iter()
        .filter(|incident| incident.status != "resolved")
        .count();
    let elevated = if active > 0 {
        " bc-snapshot__n--warn"
    } else {
        ""
    };
    format!(
        r#"<section class="bc-snapshot" aria-label="{label}">
  <div class="bc-snapshot__item"><strong class="bc-snapshot__n">{services}</strong><span>{services_label}</span></div>
  <div class="bc-snapshot__item"><strong class="bc-snapshot__n{elevated}">{active}</strong><span>{incidents_label}</span></div>
  <div class="bc-snapshot__item"><strong class="bc-snapshot__n">{maintenance}</strong><span>{maintenance_label}</span></div>
  <div class="bc-snapshot__item"><strong class="bc-snapshot__n">{days}</strong><span>{evidence_label}</span></div>
</section>"#,
        label = esc(i18n::t(loc, "status.snapshot.label")),
        services = view.components.len(),
        services_label = esc(i18n::t(loc, "status.snapshot.services")),
        active = active,
        incidents_label = esc(i18n::t(loc, "status.snapshot.incidents")),
        maintenance = view.maintenances.len(),
        maintenance_label = esc(i18n::t(loc, "status.snapshot.maintenance")),
        days = view.history_days,
        evidence_label = esc(i18n::t(loc, "status.snapshot.evidence")),
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
    // Affected is derived from the projected component states themselves — never from incident
    // affected-name lists, which may name internal checks or lag behind the probes. A
    // maintenance-masked component counts as affected: it is not operational right now.
    let affected_total = view.components.len();
    let affected_count = view
        .components
        .iter()
        .filter(|c| c.status != "operational")
        .count();
    let uptime = checked_uptime_avg(view.components.iter())
        .map(|avg| {
            let days = view.history_days.to_string();
            let title = i18n::tf(loc, "status.uptime.average_all", &[("days", &days)]);
            let window = i18n::tf(loc, "status.uptime.window", &[("days", &days)]);
            format!(
                r#"<span class="status-hero__uptime" title="{title}"><strong>{avg:.2}%</strong> {window}</span>"#,
                title = esc(&title),
                window = esc(&window),
            )
        })
        .unwrap_or_default();
    let affected = if affected_count > 0 {
        format!(
            r#"<span class="status-hero__affected">{}</span>"#,
            esc(&i18n::tf(
                loc,
                "status.hero.affected",
                &[
                    ("affected", &affected_count.to_string()),
                    ("total", &affected_total.to_string())
                ]
            ))
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="status-hero {cls}">
  <span class="status-hero__mark status-hero__mark--{state}" aria-hidden="true"></span>
  <div class="status-hero__text">
    <h2 class="status-hero__headline">{headline}</h2>
    <p class="status-hero__sub">{sub}</p>
  </div>
  <div class="status-hero__meta">
    {uptime}
    {affected}
    <span class="status-hero__updated">{updated}</span>
  </div>
</section>"#,
        headline = esc(i18n::t(loc, headline_key)),
        state = state_mod(view.overall),
        sub = esc(i18n::t(loc, sub_key)),
        updated = esc(&i18n::tf(
            loc,
            "status.updated",
            &[("time", &rel_time_l(loc, view.updated_at, now))]
        )),
    )
}

fn render_component_count(view: &StatusView, loc: odyssey::Locale) -> String {
    if view.components.is_empty() {
        return String::new();
    }
    if loc == odyssey::Locale::En {
        return format!(
            r#"<span class="card__head-meta">{} monitored</span>"#,
            view.components.len()
        );
    }
    format!(
        r#"<span class="card__head-meta">{}</span>"#,
        esc(&i18n::tf(
            loc,
            "status.components.monitored",
            &[("n", &view.components.len().to_string())]
        ))
    )
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

    // Materialize non-empty sections in view order and determine each rollup; exactly one
    // section starts open (see `section_open_index`).
    let mut sections = Vec::new();
    for g in &view.groups {
        let members: Vec<&ComponentView> = view
            .components
            .iter()
            .filter(|c| c.group_id.as_deref() == Some(g.id.as_str()))
            .collect();
        if !members.is_empty() {
            let statuses: Vec<&str> = members.iter().map(|c| c.status).collect();
            let rollup = group_rollup(&statuses);
            sections.push((g.name.as_str(), members, rollup));
        }
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
        let other_label = if loc == odyssey::Locale::En {
            "Other"
        } else {
            i18n::t(loc, "status.group.other")
        };
        sections.push((other_label, ungrouped, group_rollup(&statuses)));
    }

    let rollups: Vec<&str> = sections.iter().map(|(_, _, rollup)| *rollup).collect();
    let open_idx = section_open_index(&rollups);

    let mut out = String::new();
    for (idx, (name, members, rollup)) in sections.into_iter().enumerate() {
        let is_open = open_idx == Some(idx);
        out.push_str(&render_group_section(
            name,
            &members,
            rollup,
            is_open,
            now,
            loc,
            &incident_by_day,
            team_first_day,
        ));
    }
    out.push_str(&render_barlegend(view, loc, team_first_day));
    out
}

/// Severity rank used to pick which category section starts expanded. Higher is worse.
fn section_rank(rollup: &str) -> u8 {
    match rollup {
        "down" => 3,
        "degraded" => 2,
        "maintenance" => 1,
        _ => 0,
    }
}

/// Index of the single category section that starts expanded: the worst rollup wins, and rank
/// ties keep the earliest section (strict-max, first-wins). An all-operational page still
/// focuses the first section, so a grouped page always opens exactly one fold. `None` only
/// when there are no sections at all (empty and flat layouts render no folds).
fn section_open_index(rollups: &[&str]) -> Option<usize> {
    if rollups.is_empty() {
        return None;
    }
    let mut open_idx = 0;
    let mut best_rank = 0;
    for (idx, rollup) in rollups.iter().enumerate() {
        let rank = section_rank(rollup);
        if rank > best_rank {
            best_rank = rank;
            open_idx = idx;
        }
    }
    Some(open_idx)
}

#[allow(clippy::too_many_arguments)]
fn render_group_section(
    name: &str,
    members: &[&ComponentView],
    rollup: &str,
    is_open: bool,
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
    team_first_day: Option<i64>,
) -> String {
    let open = if is_open { " open" } else { "" };
    let uptime = checked_uptime_avg(members.iter().copied())
        .map(|avg| {
            let days = members
                .first()
                .map_or(0, |component| component.days.len())
                .to_string();
            let title = i18n::tf(loc, "status.uptime.average_group", &[("days", &days)]);
            format!(
                r#"<span class="cgroup__uptime" title="{}">{avg:.2}%</span>"#,
                esc(&title),
            )
        })
        .unwrap_or_default();
    let count_label = if loc == odyssey::Locale::En {
        format!("{} components", members.len())
    } else {
        i18n::tf(
            loc,
            "status.group.count",
            &[("n", &members.len().to_string())],
        )
    };
    let mut out = format!(
        r#"<details class="cgroup"{open}>
  <summary class="cgroup__head">
    <span class="cgroup__chev" aria-hidden="true"></span>
    <h3 class="cgroup__name">{name}</h3>
    <span class="cgroup__count">{count}</span>
    {uptime}
    {pill}
  </summary>
  <div class="cgroup__body">"#,
        name = esc(name),
        count = esc(&count_label),
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

/// Render a compact component row with latest latency and the declared evidence window.
fn render_component_row(
    c: &ComponentView,
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
    team_first_day: Option<i64>,
) -> String {
    let first_data_idx = c.days.iter().position(|d| d.uptime.is_some());
    let first_day = first_data_idx
        .map(|idx| day_bucket(now) - c.days.len().saturating_sub(1) as i64 + idx as i64);
    let monitoring_since = first_day.map(|day| fmt_date_l(loc, day * DAY_SECS));
    let no_data_text = match &monitoring_since {
        Some(date) if loc == odyssey::Locale::En => {
            format!("no data — monitoring began {date}")
        }
        Some(date) => i18n::tf(loc, "status.bar.no_data_since", &[("date", date)]),
        None if loc == odyssey::Locale::En => "no data".to_string(),
        None => i18n::t(loc, "status.bar.no_data").to_string(),
    };

    let mut bars = String::new();
    for d in &c.days {
        let data_uptime = match d.uptime {
            Some(pct) => format!("{pct:.2}%"),
            None => no_data_text.clone(),
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
            (_, Some(date)) => {
                let text = if loc == odyssey::Locale::En {
                    format!("monitoring since {date}")
                } else {
                    i18n::tf(loc, "status.row.monitoring_since", &[("date", &date)])
                };
                format!(r#"<span class="crow__since">{}</span>"#, esc(&text))
            }
            _ => {
                let text = if loc == odyssey::Locale::En {
                    "awaiting first check"
                } else {
                    i18n::t(loc, "status.row.awaiting_first_check")
                };
                format!(r#"<span class="crow__since">{}</span>"#, esc(text))
            }
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
        i18n::tf(
            loc,
            "status.uptime.component_title",
            &[("days", &c.days.len().to_string())],
        )
    } else if loc == odyssey::Locale::En {
        "awaiting first check".to_string()
    } else {
        i18n::t(loc, "status.row.awaiting_first_check").to_string()
    };
    let pct = if c.last_checked.is_some() {
        format!("{:.2}%", c.uptime_90d)
    } else {
        "—".to_string()
    };
    let state = state_mod(c.status);
    let state_label = status_label_l(loc, c.status);
    let evidence_label = format!("{} · {} · {} · {}", c.name, state_label, pct, pct_title);
    let latency_title = if loc == odyssey::Locale::En {
        format!("24h average · latest {latest_latency}")
    } else {
        i18n::tf(
            loc,
            "status.row.latency_title",
            &[("latest", &latest_latency)],
        )
    };
    format!(
        r#"<div class="crow">
  <span class="crow__id">
    <span class="crow__dot crow__dot--{state}" aria-hidden="true"></span>
    <span class="crow__name" title="{name}">{name}</span>
  </span>
  <span class="bc-lat">{spark}<span class="crow__lat" title="{latency_title}">{latency}</span></span>
  <div class="crow__track" role="img" aria-label="{evidence_label}"><div class="bars" aria-hidden="true">{bars}</div></div>
  <span class="crow__pct" title="{pct_title}">{pct}</span>
  <span class="crow__state crow__state--{state}">{label}</span>
  {since}
</div>"#,
        name = esc(&c.name),
        latency_title = esc(&latency_title),
        latency = esc(&latency),
        spark = spark,
        bars = bars,
        pct_title = esc(&pct_title),
        pct = esc(&pct),
        evidence_label = esc(&evidence_label),
        label = esc(&state_label),
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
        .map(|idx| day_bucket(now) - c.days.len().saturating_sub(1) as i64 + idx as i64)
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
        .map(|avg| {
            i18n::tf(
                loc,
                "status.uptime.legend_summary",
                &[("uptime", &format!("{avg:.2}"))],
            )
        })
        .unwrap_or_else(|| i18n::t(loc, "status.uptime.legend_empty").to_string());
    let since = team_first_day
        .map(|day| {
            i18n::tf(
                loc,
                "status.uptime.monitoring_since",
                &[("date", &fmt_date_l(loc, day * DAY_SECS))],
            )
        })
        .unwrap_or_default();
    let days = view.history_days.to_string();
    format!(
        r#"<div class="bc-barlegend"><span>{days_ago}</span><span class="bc-barlegend__mid">{mid}{since}</span><span>{today}</span></div>"#,
        days_ago = esc(&i18n::tf(loc, "status.uptime.days_ago", &[("days", &days)])),
        mid = esc(&mid),
        since = esc(&since),
        today = esc(i18n::t(loc, "status.uptime.today")),
    )
}

/// Public update channels. RSS and JSON are always available. Anonymous webhook registration
/// only appears when the operator explicitly enables the hardened egress path.
fn render_subscribe(loc: odyssey::Locale, webhooks_enabled: bool) -> String {
    if !webhooks_enabled {
        return format!(
            r#"<section class="card bc-channels" id="subscribe">
  <div class="card__head"><h2>{title}</h2></div>
  <div class="card__body">
    <p class="hint">{body}</p>
    <div class="bc-channels__links"><a class="btn btn-secondary" href="/feed.xml">{rss}</a><a class="btn btn-secondary" href="/api/status">{json}</a></div>
    <p class="hint--muted">{note}</p>
  </div>
</section>"#,
            title = esc(i18n::t(loc, "status.subscribe.title")),
            body = esc(i18n::t(loc, "status.channels.body")),
            rss = esc(i18n::t(loc, "status.rss_feed")),
            json = esc(i18n::t(loc, "status.json_api")),
            note = esc(i18n::t(loc, "status.channels.webhook_unavailable")),
        );
    }
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

fn infra_metric_label(loc: odyssey::Locale, metric: &str) -> &str {
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
    let reported_label = if loc == odyssey::Locale::En {
        "reported"
    } else {
        i18n::t(loc, "status.incident.reported")
    };
    items.push_str(&format!(
        r#"<li class="timeline__item"><span class="pill pill-state">{reported}</span> <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
        reported = esc(reported_label),
        ago = esc(&rel_time_l(loc, inc.created_at, now)),
        body = esc(&inc.body),
    ));
    let summary = if loc == odyssey::Locale::En {
        format!("Timeline ({})", updates.len() + 1)
    } else {
        i18n::tf(
            loc,
            "status.timeline",
            &[("n", &(updates.len() + 1).to_string())],
        )
    };
    format!(
        r#"<details class="timeline"><summary>{summary}</summary><ol>{items}</ol></details>"#,
        summary = esc(&summary),
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

/// The "Active incidents" ledger above the evidence summary and components — one entry per
/// non-resolved incident. Empty string (section omitted) when everything is resolved.
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
        let opened_ago = rel_time_l(loc, inc.created_at, now);
        let updated_ago = rel_time_l(loc, inc.updated_at, now);
        let time_line = if loc == odyssey::Locale::En {
            format!("Opened {opened_ago} · Last update {updated_ago}")
        } else {
            i18n::tf(
                loc,
                "status.incident.opened_updated",
                &[("opened", &opened_ago), ("updated", &updated_ago)],
            )
        };
        out.push_str(&format!(
            r#"<section class="card incident-card sev-{sev}"><div class="card__body"><article class="incident">
  <div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{sev_pill}{status_pill}</span></div>
  {affected}
  <p class="incident__body">{latest}</p>
  <div class="incident__time">{time_line}</div>
  {timeline}
</article></div></section>"#,
            sev = esc(&inc.severity),
            title = esc(&inc.title),
            sev_pill = severity_pill_l(loc, &inc.severity),
            status_pill = incident_status_pill_l(loc, &inc.status),
            affected = render_affected_l(loc, &inc.affected),
            latest = esc(latest),
            time_line = esc(&time_line),
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
            let label = if loc == odyssey::Locale::En {
                "in progress"
            } else {
                i18n::t(loc, "status.maint.in_progress")
            };
            let ends = fmt_countdown_l(loc, m.ends_at - now);
            let cd = if loc == odyssey::Locale::En {
                format!("ends in {ends}")
            } else {
                i18n::tf(loc, "status.maint.ends_in", &[("t", &ends)])
            };
            (
                format!(r#"<span class="pill pill-info">{}</span>"#, esc(label)),
                format!(r#"<span class="countdown">{}</span>"#, esc(&cd)),
            )
        } else {
            let label = if loc == odyssey::Locale::En {
                "scheduled"
            } else {
                i18n::t(loc, "status.maint.scheduled")
            };
            let starts = fmt_countdown_l(loc, m.starts_at - now);
            let cd = if loc == odyssey::Locale::En {
                format!("starts in {starts}")
            } else {
                i18n::tf(loc, "status.maint.starts_in", &[("t", &starts)])
            };
            (
                format!(r#"<span class="pill pill-state">{}</span>"#, esc(label)),
                format!(r#"<span class="countdown">{}</span>"#, esc(&cd)),
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

/// "Past incidents": only days with public incidents are rendered. A single all-clear row
/// replaces fourteen repetitive empty buckets, keeping the incident evidence scannable.
fn render_past_incidents(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let mut out = String::new();
    let today = day_bucket(now);
    let mut rendered_days = 0usize;
    for offset in 0..14 {
        let day = today - offset;
        let incidents: Vec<_> = view
            .incidents
            .iter()
            .filter(|inc| day_bucket(inc.created_at) == day)
            .collect();
        if incidents.is_empty() {
            continue;
        }
        rendered_days += 1;
        out.push_str(&format!(
            r#"<div class="day-group"><h3 class="day-group__date">{}</h3>"#,
            esc(&fmt_date_l(loc, day * DAY_SECS)),
        ));
        for inc in incidents {
            let resolved = inc.status == "resolved";
            let opened_ago = rel_time_l(loc, inc.created_at, now);
            let mut when = if resolved && inc.resolved_at > 0 {
                let resolved_ago = rel_time_l(loc, inc.resolved_at, now);
                if loc == odyssey::Locale::En {
                    format!("Opened {opened_ago} · Resolved {resolved_ago}")
                } else {
                    i18n::tf(
                        loc,
                        "status.incident.opened_resolved",
                        &[("opened", &opened_ago), ("resolved", &resolved_ago)],
                    )
                }
            } else if loc == odyssey::Locale::En {
                format!("Opened {opened_ago}")
            } else {
                i18n::tf(loc, "status.incident.opened_at", &[("ago", &opened_ago)])
            };
            if resolved && inc.resolved_at > 0 {
                let lasted = fmt_countdown_l(loc, inc.resolved_at - inc.created_at);
                let lasted_text = if loc == odyssey::Locale::En {
                    format!("· lasted {lasted}")
                } else {
                    i18n::tf(loc, "status.incident.lasted", &[("duration", &lasted)])
                };
                when.push(' ');
                when.push_str(&lasted_text);
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
        out.push_str("</div>");
    }
    if rendered_days == 0 {
        out.push_str(&format!(
            r#"<div class="bc-history-clear"><span class="empty__ok" aria-hidden="true"></span><p>{}</p></div>"#,
            esc(i18n::t(loc, "status.none_history"))
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ComponentView, DayStat, GroupView, StatusView};
    use crate::vitals::InfraPublic;
    use odyssey::Locale;

    /// Synthetic trailing "Other" section: ungrouped components when groups exist.
    #[test]
    fn render_status_live_with_trailing_other_section() {
        let view = StatusView {
            overall: "operational",
            updated_at: 1700000000,
            history_days: 30,
            components: vec![
                ComponentView {
                    name: "Gateway".to_string(),
                    kind: "http".to_string(),
                    status: "operational",
                    group_id: Some("g_core".to_string()),
                    uptime_24h: 100.0,
                    uptime_7d: 99.98,
                    uptime_90d: 99.95,
                    latency_ms: Some(42),
                    latency_avg_ms: Some(38),
                    latency_points: vec![],
                    last_checked: Some(1700000000),
                    days: vec![DayStat {
                        date: "2023-11-14".to_string(),
                        status: "ok",
                        uptime: Some(100.0),
                    }],
                },
                ComponentView {
                    name: "Orphan".to_string(),
                    kind: "http".to_string(),
                    status: "operational",
                    group_id: None,
                    uptime_24h: 99.90,
                    uptime_7d: 99.85,
                    uptime_90d: 99.80,
                    latency_ms: Some(55),
                    latency_avg_ms: Some(50),
                    latency_points: vec![],
                    last_checked: Some(1700000000),
                    days: vec![DayStat {
                        date: "2023-11-14".to_string(),
                        status: "ok",
                        uptime: Some(100.0),
                    }],
                },
            ],
            groups: vec![GroupView {
                id: "g_core".to_string(),
                name: "Core".to_string(),
                position: 0,
                status: "operational",
            }],
            incidents: vec![],
            updates: vec![],
            maintenances: vec![],
            infra: None,
        };

        let html = render_status_live(&view, 1700000000, Locale::En);

        // Must render exactly two sections: the named group "Core" and the trailing "Other".
        assert_eq!(
            html.matches(r#"<details class="cgroup""#).count(),
            2,
            "one named group plus one trailing Other section"
        );
        assert!(
            html.contains(r#"class="cgroup__name">Core"#),
            "Core group present"
        );
        assert!(
            html.contains(r#"class="cgroup__name">Other"#),
            "trailing Other section present"
        );
        assert!(html.contains(r#"title="Gateway""#));
        assert!(html.contains(r#"title="Orphan""#));
    }

    /// The trailing "Other" section label and the member counts localize with the page
    /// (the catalog path always assigns a group id, so this is only reachable synthetically).
    #[test]
    fn render_status_live_localizes_the_trailing_other_section() {
        let component = |name: &str, group_id: Option<&str>| ComponentView {
            name: name.to_string(),
            kind: "http".to_string(),
            status: "operational",
            group_id: group_id.map(str::to_string),
            uptime_24h: 100.0,
            uptime_7d: 99.98,
            uptime_90d: 99.95,
            latency_ms: Some(42),
            latency_avg_ms: Some(38),
            latency_points: vec![],
            last_checked: Some(1700000000),
            days: vec![DayStat {
                date: "2023-11-14".to_string(),
                status: "ok",
                uptime: Some(100.0),
            }],
        };
        let view = StatusView {
            overall: "operational",
            updated_at: 1700000000,
            history_days: 30,
            components: vec![
                component("Gateway", Some("g_core")),
                component("Orphan", None),
            ],
            groups: vec![GroupView {
                id: "g_core".to_string(),
                name: "Core".to_string(),
                position: 0,
                status: "operational",
            }],
            incidents: vec![],
            updates: vec![],
            maintenances: vec![],
            infra: None,
        };

        let zh = render_status_live(&view, 1700000000, Locale::Zh);
        assert!(
            zh.contains(r#"class="cgroup__name">其他"#),
            "zh Other label"
        );
        assert!(
            zh.contains(r#"<span class="cgroup__count">1 个组件</span>"#),
            "zh member count"
        );
        assert!(
            !zh.contains(r#"class="cgroup__name">Other"#),
            "no English Other on the zh page"
        );

        let ja = render_status_live(&view, 1700000000, Locale::Ja);
        assert!(
            ja.contains(r#"class="cgroup__name">その他"#),
            "ja Other label"
        );
        assert!(
            ja.contains(r#"<span class="cgroup__count">コンポーネント 1 件</span>"#),
            "ja member count"
        );
    }

    /// Synthetic infra block without a live vitals poller.
    #[test]
    fn render_status_live_with_synthetic_infra() {
        let view = StatusView {
            overall: "operational",
            updated_at: 1700000000,
            history_days: 30,
            components: vec![],
            groups: vec![],
            incidents: vec![],
            updates: vec![],
            maintenances: vec![],
            infra: Some(InfraPublic {
                overall: "ok",
                bands: vec![],
                trend: vec!["ok"; 24],
            }),
        };

        let html = render_status_live(&view, 1700000000, Locale::En);

        assert!(
            html.contains(r#"class="card bc-infra""#),
            "infra section renders when present"
        );
        assert!(
            html.contains(r#"class="bc-infra__trend""#),
            "24-hour trend present"
        );
    }
}
