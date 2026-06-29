//! SSO-gated operator dashboard + manual incident posting.
//!
//! Mounted behind a Sluice `auth=sso` route: the gateway authenticates the operator and
//! injects `X-Auth-Subject` / `X-Auth-Email`, which we trust (Beacon is internal-only).
//! The dashboard lists configured checks and offers a form to post an incident that then
//! shows on the PUBLIC status page.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth;
use crate::error::AppError;
use crate::handlers::{esc, status_pill, APP_CSS, SHIELD_SVG};
use crate::store::Incident;
use crate::{now_nanos, now_secs, AppState};

const ADMIN_HTML: &str = include_str!("../../templates/admin.html");

/// `GET /admin` — operator dashboard (checks + incident form). Renders for any request the
/// gateway forwards; the signed-in email comes from the injected `X-Auth-Email`.
pub async fn admin_page(State(state): State<AppState>, headers: HeaderMap) -> Html<String> {
    let email = auth::admin_email(&headers).unwrap_or_else(|| "operator".to_string());
    let now = now_secs();
    Html(render_admin(&state, &email, now).await)
}

#[derive(Debug, Deserialize)]
pub struct IncidentForm {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub body: String,
}

/// `POST /admin/incidents` — store a manual incident, then bounce back to the dashboard.
/// Requires a gateway-injected identity (defense in depth behind the `auth=sso` route).
pub async fn create_incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<IncidentForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;

    let title = form.title.trim();
    if title.is_empty() {
        return Err(AppError::InvalidRequest("incident title is required".to_string()));
    }
    let status = match form.status.trim() {
        "" => "investigating",
        s => s,
    };
    let now = now_secs();
    let incident = Incident {
        id: format!("inc_{}", now_nanos()),
        title: title.to_string(),
        status: status.to_string(),
        body: form.body.trim().to_string(),
        created_at: now,
        updated_at: now,
    };
    state.store.insert_incident(&incident).await;
    tracing::info!(id = incident.id, title = incident.title, "incident posted");

    // 303 -> GET /admin (post/redirect/get).
    Ok((StatusCode::SEE_OTHER, [(header::LOCATION, "/admin")]).into_response())
}

async fn render_admin(state: &AppState, email: &str, now: i64) -> String {
    ADMIN_HTML
        .replace("{{CSS}}", APP_CSS)
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{EMAIL}}", &esc(email))
        .replace("{{CHECKS}}", &render_checks(state).await)
        .replace("{{INCIDENTS}}", &render_incidents(state, now).await)
}

async fn render_checks(state: &AppState) -> String {
    let checks = state.store.list_checks().await;
    if checks.is_empty() {
        return r#"<tr><td colspan="4" class="empty">No checks configured.</td></tr>"#.to_string();
    }
    let mut rows = String::new();
    for c in &checks {
        let latest = state.store.latest_result(&c.name).await;
        let status = match latest.as_ref().map(|r| r.ok) {
            Some(true) => "operational",
            Some(false) => "down",
            None => "degraded",
        };
        let enabled = if c.enabled { "enabled" } else { "disabled" };
        rows.push_str(&format!(
            r#"<tr><td><strong>{name}</strong> <span class="muted">({enabled})</span></td><td><code>{kind}</code></td><td><code>{target}</code></td><td>{pill}</td></tr>"#,
            name = esc(&c.name),
            enabled = enabled,
            kind = esc(&c.kind),
            target = esc(&c.target),
            pill = if latest.is_some() { status_pill(status) } else { r#"<span class="pill pill-state">pending</span>"#.to_string() },
        ));
    }
    rows
}

async fn render_incidents(state: &AppState, now: i64) -> String {
    let incidents = state.store.list_incidents().await;
    if incidents.is_empty() {
        return r#"<div class="empty">No incidents posted yet.</div>"#.to_string();
    }
    let mut items = String::new();
    for inc in &incidents {
        items.push_str(&format!(
            r#"<article class="incident"><div class="incident__head"><h3 class="incident__title">{title}</h3><span class="pill pill-state">{status}</span></div><p class="incident__body">{body}</p><div class="incident__time">Posted {ago}</div></article>"#,
            title = esc(&inc.title),
            status = esc(&inc.status),
            body = esc(&inc.body),
            ago = esc(&crate::handlers::rel_time(inc.created_at, now)),
        ));
    }
    items
}
