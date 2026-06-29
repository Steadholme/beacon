//! PUBLIC status surface: the server-rendered status page and the machine-readable JSON.
//!
//! Both are unauthenticated by design (placed behind a Sluice `auth=public` route). The
//! page mirrors the HOLDFAST enterprise brand: app-bar, overall banner, component cards
//! with status pills + rolling uptime, and an Incidents section.

use axum::extract::State;
use axum::response::Html;
use axum::Json;

use crate::handlers::{
    esc, fmt_latency, overall_banner, rel_time, status_pill, APP_CSS, SHIELD_SVG,
};
use crate::model::{build_status, StatusView};
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
        .replace("{{BANNER}}", &overall_banner(view.overall))
        .replace("{{COMPONENTS}}", &render_components(view, now))
        .replace("{{INCIDENTS}}", &render_incidents(view, now))
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
</div>"#,
            name = esc(&c.name),
            last = last,
            latency = esc(&fmt_latency(c.latency_ms)),
            u24 = c.uptime_24h,
            u7 = c.uptime_7d,
            u90 = c.uptime_90d,
            pill = status_pill(c.status),
        ));
    }
    rows
}

fn render_incidents(view: &StatusView, now: i64) -> String {
    if view.incidents.is_empty() {
        return r#"<div class="empty"><span class="empty__ok" aria-hidden="true"></span>No incidents reported. All clear.</div>"#
            .to_string();
    }
    let mut items = String::new();
    for inc in &view.incidents {
        items.push_str(&format!(
            r#"<article class="incident">
  <div class="incident__head">
    <h3 class="incident__title">{title}</h3>
    <span class="pill pill-state">{status}</span>
  </div>
  <p class="incident__body">{body}</p>
  <div class="incident__time">Posted {ago}</div>
</article>"#,
            title = esc(&inc.title),
            status = esc(&inc.status),
            body = esc(&inc.body),
            ago = esc(&rel_time(inc.created_at, now)),
        ));
    }
    items
}
