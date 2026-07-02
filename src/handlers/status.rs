//! PUBLIC status surface: the server-rendered status page and the machine-readable JSON.
//!
//! Both are unauthenticated by design (placed behind a Sluice `auth=public` route). The
//! page mirrors the HOLDFAST enterprise brand: app-bar, overall banner, an active-incidents
//! section (severity-tinted cards with an expandable update timeline), maintenance notices,
//! component cards with status pills + rolling uptime + the classic 90-day uptime bar row,
//! and a "Past incidents" section (last 14 days, grouped by day).

use axum::extract::State;
use axum::response::Html;
use axum::Json;

use crate::handlers::{
    esc, fmt_date, fmt_datetime, fmt_latency, incident_status_pill, overall_banner, rel_time,
    severity_pill, status_pill, userbox, APP_CSS, SHIELD_SVG,
};
use crate::model::{
    affected_names, build_status, day_bucket, maintenance_ongoing, StatusView, WINDOW_14D,
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
        .replace("{{CSS}}", APP_CSS)
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{USERBOX}}", &userbox("System Status", None))
        .replace("{{BANNER}}", &overall_banner(view.overall))
        .replace("{{ACTIVE_INCIDENTS}}", &render_active_incidents(view, now))
        .replace("{{MAINTENANCE}}", &render_maintenances(view, now))
        .replace("{{COMPONENTS}}", &render_components(view, now))
        .replace("{{PAST_INCIDENTS}}", &render_past_incidents(view, now))
        .replace("{{UPDATED}}", &rel_time(view.updated_at, now))
}

fn render_components(view: &StatusView, now: i64) -> String {
    if view.components.is_empty() {
        return r#"<div class="empty">No components are being monitored yet.</div>"#.to_string();
    }
    let mut rows = String::new();
    for c in &view.components {
        let last = match c.last_checked {
            Some(ts) => format!("checked {}", esc(&rel_time(ts, now))),
            None => "awaiting first check".to_string(),
        };
        // The classic statuspage bar row: one 4px bar per day, oldest first, hover title
        // carrying the date + up-ratio (pure HTML/CSS — no script).
        let mut bars = String::new();
        for d in &c.days {
            let title = match d.uptime {
                Some(pct) => format!("{} · {pct:.2}%", d.date),
                None => format!("{} · no data", d.date),
            };
            bars.push_str(&format!(
                r#"<span class="bar bar-{cls}" title="{title}"></span>"#,
                cls = d.status,
                title = esc(&title),
            ));
        }
        rows.push_str(&format!(
            r#"<div class="component">
  <div class="component__main">
    <div class="component__name">{name}</div>
    <div class="component__meta">{last} · {latency}</div>
  </div>
  <div class="component__uptime">
    <div class="uptime-cell"><span class="uptime-val">{u24:.2}%</span><span class="uptime-lab">24h</span></div>
    <div class="uptime-cell"><span class="uptime-val">{u7:.2}%</span><span class="uptime-lab">7d</span></div>
    <div class="uptime-cell"><span class="uptime-val">{u90:.2}%</span><span class="uptime-lab">90d</span></div>
  </div>
  <div class="component__status">{pill}</div>
  <div class="component__bars">
    <div class="bars">{bars}</div>
    <div class="bars__legend"><span>90 days ago</span><span>{u90:.2}% uptime</span><span>Today</span></div>
  </div>
</div>"#,
            name = esc(&c.name),
            last = last,
            latency = esc(&fmt_latency(c.latency_ms)),
            u24 = c.uptime_24h,
            u7 = c.uptime_7d,
            u90 = c.uptime_90d,
            pill = status_pill(c.status),
            bars = bars,
        ));
    }
    rows
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
        let state_pill = if maintenance_ongoing(m, now) {
            r#"<span class="pill pill-info">in progress</span>"#
        } else {
            r#"<span class="pill pill-state">scheduled</span>"#
        };
        out.push_str(&format!(
            r#"<section class="card incident-card sev-maintenance"><div class="card__body"><article class="incident">
  <div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{state_pill}</span></div>
  {affected}
  <p class="incident__body">{body}</p>
  <div class="incident__time">{starts} → {ends}</div>
</article></div></section>"#,
            title = esc(&m.title),
            state_pill = state_pill,
            affected = render_affected(&m.affected),
            body = esc(&m.body),
            starts = esc(&fmt_datetime(m.starts_at)),
            ends = esc(&fmt_datetime(m.ends_at)),
        ));
    }
    out
}

/// "Past incidents": the last 14 days of incident history, grouped by calendar day (newest
/// day first), resolved incidents muted. The incidents are already newest-first.
fn render_past_incidents(view: &StatusView, now: i64) -> String {
    let recent: Vec<&Incident> = view
        .incidents
        .iter()
        .filter(|i| i.created_at >= now - WINDOW_14D)
        .collect();
    if recent.is_empty() {
        return r#"<div class="empty"><span class="empty__ok" aria-hidden="true"></span>No incidents in the last 14 days. All clear.</div>"#
            .to_string();
    }
    let mut out = String::new();
    let mut current_day = i64::MIN;
    for inc in recent {
        let day = day_bucket(inc.created_at);
        if day != current_day {
            if current_day != i64::MIN {
                out.push_str("</div>");
            }
            current_day = day;
            out.push_str(&format!(
                r#"<div class="day-group"><h3 class="day-group__date">{}</h3>"#,
                esc(&fmt_date(inc.created_at)),
            ));
        }
        let resolved = inc.status == "resolved";
        let when = if resolved && inc.resolved_at > 0 {
            format!(
                "Opened {} · Resolved {}",
                rel_time(inc.created_at, now),
                rel_time(inc.resolved_at, now)
            )
        } else {
            format!("Opened {}", rel_time(inc.created_at, now))
        };
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
    out.push_str("</div>");
    out
}
