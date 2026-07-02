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
    esc, fmt_countdown, fmt_date, fmt_datetime, fmt_latency, incident_status_pill, overall_banner,
    rel_time, severity_pill, status_pill, userbox, APP_CSS, SHIELD_SVG,
};
use crate::model::{
    affected_names, build_status, day_bucket, maintenance_ongoing, ComponentView, StatusView,
    WINDOW_14D,
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
        .replace("{{SUBSCRIBE}}", &render_subscribe())
        .replace("{{UPDATED}}", &rel_time(view.updated_at, now))
}

/// The Components body. With NO groups configured this renders the flat component list
/// exactly as before (backward-compatible byte-for-byte). With groups, components are
/// rendered under their group section (each with a rolled-up status pill), and any ungrouped
/// components fall into a trailing "Other" section.
fn render_components(view: &StatusView, now: i64) -> String {
    if view.components.is_empty() {
        return r#"<div class="empty">No components are being monitored yet.</div>"#.to_string();
    }
    if view.groups.is_empty() {
        // Flat list — unchanged legacy output.
        let mut rows = String::new();
        for c in &view.components {
            rows.push_str(&render_component_row(c, now));
        }
        return rows;
    }

    // Grouped: a section per group (ordered by the view), then ungrouped components last.
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
        out.push_str(&format!(
            r#"<div class="group"><div class="group__head"><h3 class="group__name">{name}</h3>{pill}</div>"#,
            name = esc(&g.name),
            pill = status_pill(g.status),
        ));
        for c in members {
            out.push_str(&render_component_row(c, now));
        }
        out.push_str("</div>");
    }

    // Ungrouped components (group_id None, or pointing at a group that no longer exists).
    let known: std::collections::HashSet<&str> = view.groups.iter().map(|g| g.id.as_str()).collect();
    let ungrouped: Vec<&ComponentView> = view
        .components
        .iter()
        .filter(|c| c.group_id.as_deref().is_none_or(|id| !known.contains(id)))
        .collect();
    if !ungrouped.is_empty() {
        out.push_str(r#"<div class="group"><div class="group__head"><h3 class="group__name">Other</h3></div>"#);
        for c in ungrouped {
            out.push_str(&render_component_row(c, now));
        }
        out.push_str("</div>");
    }
    out
}

/// Render a single component row: name + meta, a response-time sparkline, rolling uptime, the
/// status pill, and the classic 90-day uptime bar row.
fn render_component_row(c: &ComponentView, now: i64) -> String {
    let last = match c.last_checked {
        Some(ts) => format!("checked {}", esc(&rel_time(ts, now))),
        None => "awaiting first check".to_string(),
    };
    // The classic statuspage bar row: one 4px bar per day, oldest first, hover title carrying
    // the date + up-ratio (pure HTML/CSS — no script).
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
    format!(
        r#"<div class="component">
  <div class="component__main">
    <div class="component__name">{name}</div>
    <div class="component__meta">{last} · {latency}</div>
    {spark}
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
        spark = render_sparkline(c),
        u24 = c.uptime_24h,
        u7 = c.uptime_7d,
        u90 = c.uptime_90d,
        pill = status_pill(c.status),
        bars = bars,
    )
}

/// A pure-SVG response-time sparkline (24h hourly means) plus current + average figures. All
/// values are numeric, so nothing here needs escaping. When fewer than two hours have data
/// the trend line is omitted (just the figures), keeping the row clean for fresh components.
fn render_sparkline(c: &ComponentView) -> String {
    let known: Vec<(usize, i64)> = c
        .latency_points
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| (i, v)))
        .collect();
    let figures = format!(
        r#"<span class="spark__fig">now {now}</span><span class="spark__fig">avg {avg}</span>"#,
        now = fmt_latency(c.latency_ms),
        avg = fmt_latency(c.latency_avg_ms),
    );
    if known.len() < 2 {
        return format!(r#"<div class="component__spark">{figures}</div>"#);
    }

    // Normalize into a 140x26 viewBox (a flat mid-line when every value is equal).
    const W: f64 = 140.0;
    const H: f64 = 26.0;
    const PAD: f64 = 3.0;
    let n = c.latency_points.len().max(2) as f64;
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for &(_, v) in &known {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let span = (hi - lo).max(1) as f64;
    let mut pts = String::new();
    for &(i, v) in &known {
        let x = PAD + (i as f64) / (n - 1.0) * (W - 2.0 * PAD);
        let y = if hi == lo {
            H / 2.0
        } else {
            H - PAD - (v - lo) as f64 / span * (H - 2.0 * PAD)
        };
        if !pts.is_empty() {
            pts.push(' ');
        }
        pts.push_str(&format!("{x:.1},{y:.1}"));
    }
    format!(
        r#"<div class="component__spark"><svg class="spark" viewBox="0 0 {w} {h}" preserveAspectRatio="none" aria-hidden="true"><polyline points="{pts}"/></svg>{figures}</div>"#,
        w = W as i64,
        h = H as i64,
        pts = pts,
        figures = figures,
    )
}

/// The public "Subscribe to updates" card: a webhook URL form posting to `/subscriptions`.
/// Public (no CSRF); double opt-in confirmation gates any delivery.
fn render_subscribe() -> String {
    r#"<section class="card">
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
                format!(r#"<span class="countdown">ends in {}</span>"#, esc(&fmt_countdown(m.ends_at - now))),
            )
        } else {
            (
                r#"<span class="pill pill-state">scheduled</span>"#,
                format!(r#"<span class="countdown">starts in {}</span>"#, esc(&fmt_countdown(m.starts_at - now))),
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
