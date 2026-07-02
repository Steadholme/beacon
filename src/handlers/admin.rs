//! SSO-gated operator dashboard: incident lifecycle + maintenance windows.
//!
//! Mounted behind a Sluice `auth=sso` route: the gateway authenticates the operator and
//! injects `X-Auth-Subject` / `X-Auth-Email`, which we trust (Beacon is internal-only).
//! Every state-changing POST additionally requires the double-submit CSRF token minted on
//! the dashboard render (defense in depth behind the gateway, mirroring lodestar), and
//! emits the crate's audit line (`tracing::info!`) after the mutation.

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth;
use crate::error::AppError;
use crate::handlers::{
    esc, fmt_datetime, incident_status_pill, rel_time, severity_pill, status_pill, userbox,
    APP_CSS, SHIELD_SVG,
};
use crate::model::{affected_names, maintenance_ongoing};
use crate::store::{Incident, IncidentUpdate, Maintenance};
use crate::{now_nanos, now_secs, AppState};

const ADMIN_HTML: &str = include_str!("../../templates/admin.html");

/// Normalize an incident lifecycle status to the allowlist (default `investigating`).
fn normalize_status(raw: &str) -> &'static str {
    match raw.trim() {
        "identified" => "identified",
        "monitoring" => "monitoring",
        "resolved" => "resolved",
        _ => "investigating",
    }
}

/// Normalize an incident severity to the allowlist (default `minor`).
fn normalize_severity(raw: &str) -> &'static str {
    match raw.trim() {
        "major" => "major",
        "critical" => "critical",
        _ => "minor",
    }
}

/// Normalize a comma-separated affected list: trim entries, drop empties, rejoin.
fn normalize_affected(raw: &str) -> String {
    affected_names(raw).join(", ")
}

/// `GET /admin` — operator dashboard (checks + incident/maintenance control). Renders for
/// any request the gateway forwards; the signed-in email comes from the injected
/// `X-Auth-Email`. Mints/reuses the CSRF token its forms embed.
pub async fn admin_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let email = auth::admin_email(&headers).unwrap_or_else(|| "operator".to_string());
    let (csrf, set_cookie) = auth::ensure_csrf(&headers);
    let now = now_secs();
    html_with_cookie(render_admin(&state, &email, &csrf, now).await, set_cookie)
}

// ---------------------------------------------------------------------------
// POST /admin/incidents — open an incident
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IncidentForm {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub affected: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub csrf_token: String,
}

/// `POST /admin/incidents` — open an incident, then bounce back to the dashboard.
/// Requires a gateway-injected identity + the double-submit CSRF token.
pub async fn create_incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<IncidentForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;
    auth::verify_csrf(&headers, &form.csrf_token)?;

    let title = form.title.trim();
    if title.is_empty() {
        return Err(AppError::InvalidRequest("incident title is required".to_string()));
    }
    let status = normalize_status(&form.status);
    let severity = normalize_severity(&form.severity);
    let now = now_secs();
    let incident = Incident {
        id: format!("inc_{}", now_nanos()),
        title: title.to_string(),
        status: status.to_string(),
        severity: severity.to_string(),
        affected: normalize_affected(&form.affected),
        body: form.body.trim().to_string(),
        created_at: now,
        updated_at: now,
        resolved_at: if status == "resolved" { now } else { 0 },
    };
    state.store.insert_incident(&incident).await;
    tracing::info!(
        id = incident.id,
        title = incident.title,
        severity = incident.severity,
        "incident posted"
    );

    // 303 -> GET /admin (post/redirect/get).
    Ok(see_other())
}

// ---------------------------------------------------------------------------
// POST /admin/incidents/update + /admin/incidents/resolve — timeline updates
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UpdateForm {
    #[serde(default)]
    pub incident_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub csrf_token: String,
}

/// `POST /admin/incidents/update` — append a timeline update to an incident, moving it to
/// the submitted status (a `resolved` update resolves it; a non-resolved update on a
/// resolved incident reopens it).
pub async fn post_incident_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpdateForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;
    auth::verify_csrf(&headers, &form.csrf_token)?;

    let body = form.body.trim();
    if body.is_empty() {
        return Err(AppError::InvalidRequest("update body is required".to_string()));
    }
    append_update(&state, &form.incident_id, normalize_status(&form.status), body).await?;
    Ok(see_other())
}

/// `POST /admin/incidents/resolve` — resolve an incident with a closing timeline update
/// (the submitted body, or a stock closing line when omitted).
pub async fn resolve_incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<UpdateForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;
    auth::verify_csrf(&headers, &form.csrf_token)?;

    let body = match form.body.trim() {
        "" => "This incident has been resolved.",
        b => b,
    };
    append_update(&state, &form.incident_id, "resolved", body).await?;
    Ok(see_other())
}

/// Shared write path for update/resolve: verify the incident exists, append the timeline
/// row, move the incident, audit.
async fn append_update(
    state: &AppState,
    incident_id: &str,
    status: &'static str,
    body: &str,
) -> Result<(), AppError> {
    let incident = state
        .store
        .get_incident(incident_id)
        .await
        .ok_or_else(|| AppError::InvalidRequest("unknown incident id".to_string()))?;

    let now = now_secs();
    let update = IncidentUpdate {
        id: format!("upd_{}", now_nanos()),
        incident_id: incident.id.clone(),
        status: status.to_string(),
        body: body.to_string(),
        created_at: now,
    };
    state.store.insert_incident_update(&update).await;
    let resolved_at = if status == "resolved" { now } else { 0 };
    state
        .store
        .set_incident_status(&incident.id, status, now, resolved_at)
        .await;
    tracing::info!(
        id = update.id,
        incident = incident.id,
        status = status,
        "incident update posted"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /admin/maintenances — schedule a maintenance window
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MaintenanceForm {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Minutes from now until the window opens (`0` = starts immediately).
    #[serde(default)]
    pub starts_in_mins: String,
    /// Window length in minutes (min 1, default 60).
    #[serde(default)]
    pub duration_mins: String,
    #[serde(default)]
    pub affected: String,
    #[serde(default)]
    pub csrf_token: String,
}

/// `POST /admin/maintenances` — schedule a maintenance window. Upcoming/ongoing windows
/// show on the public page; components inside an ongoing window render a maintenance pill.
pub async fn create_maintenance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<MaintenanceForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;
    auth::verify_csrf(&headers, &form.csrf_token)?;

    let title = form.title.trim();
    if title.is_empty() {
        return Err(AppError::InvalidRequest("maintenance title is required".to_string()));
    }
    let starts_in_mins = form.starts_in_mins.trim().parse::<i64>().unwrap_or(0).max(0);
    let duration_mins = form.duration_mins.trim().parse::<i64>().unwrap_or(60).max(1);

    let now = now_secs();
    let starts_at = now + starts_in_mins * 60;
    let maintenance = Maintenance {
        id: format!("mw_{}", now_nanos()),
        title: title.to_string(),
        body: form.body.trim().to_string(),
        starts_at,
        ends_at: starts_at + duration_mins * 60,
        affected: normalize_affected(&form.affected),
    };
    state.store.insert_maintenance(&maintenance).await;
    tracing::info!(
        id = maintenance.id,
        title = maintenance.title,
        starts_at = maintenance.starts_at,
        ends_at = maintenance.ends_at,
        "maintenance scheduled"
    );
    Ok(see_other())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

async fn render_admin(state: &AppState, email: &str, csrf: &str, now: i64) -> String {
    ADMIN_HTML
        .replace("{{CSS}}", APP_CSS)
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{USERBOX}}", &userbox("Beacon admin", Some(email)))
        .replace("{{EMAIL}}", &esc(email))
        .replace("{{CHECKS}}", &render_checks(state).await)
        .replace("{{INCIDENTS}}", &render_incidents(state, csrf, now).await)
        .replace("{{MAINTENANCES}}", &render_maintenances(state, now).await)
        .replace("{{CSRF}}", &esc(csrf))
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

async fn render_incidents(state: &AppState, csrf: &str, now: i64) -> String {
    let incidents = state.store.list_incidents().await;
    if incidents.is_empty() {
        return r#"<div class="empty">No incidents posted yet.</div>"#.to_string();
    }
    let updates = state.store.list_incident_updates().await;
    let mut items = String::new();
    for inc in &incidents {
        let mut timeline = String::new();
        for u in updates.iter().filter(|u| u.incident_id == inc.id) {
            timeline.push_str(&format!(
                r#"<li class="timeline__item">{pill} <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
                pill = incident_status_pill(&u.status),
                ago = esc(&rel_time(u.created_at, now)),
                body = esc(&u.body),
            ));
        }
        // Update/resolve controls only while the incident is open.
        let controls = if inc.status == "resolved" {
            String::new()
        } else {
            format!(
                r#"<form method="post" action="/admin/incidents/update" class="incident__form">
  <input type="hidden" name="csrf_token" value="{csrf}">
  <input type="hidden" name="incident_id" value="{id}">
  <div class="field"><label>Update</label><textarea name="body" required placeholder="What changed."></textarea></div>
  <div class="field"><label>Move to</label><select name="status">
    <option value="investigating"{s_inv}>Investigating</option>
    <option value="identified"{s_ide}>Identified</option>
    <option value="monitoring"{s_mon}>Monitoring</option>
    <option value="resolved">Resolved</option>
  </select></div>
  <button class="btn btn-secondary btn-sm" type="submit">Post update</button>
</form>
<form method="post" action="/admin/incidents/resolve" class="incident__form">
  <input type="hidden" name="csrf_token" value="{csrf}">
  <input type="hidden" name="incident_id" value="{id}">
  <button class="btn btn-primary btn-sm" type="submit">Resolve</button>
</form>"#,
                csrf = esc(csrf),
                id = esc(&inc.id),
                s_inv = selected(&inc.status, "investigating"),
                s_ide = selected(&inc.status, "identified"),
                s_mon = selected(&inc.status, "monitoring"),
            )
        };
        let affected = if inc.affected.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="affected">Affects <span class="muted">{}</span></div>"#,
                esc(&inc.affected)
            )
        };
        items.push_str(&format!(
            r#"<article class="incident{muted}"><div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{sev}{status}</span></div>{affected}<p class="incident__body">{body}</p><div class="incident__time">Posted {ago}</div><ol class="timeline__list">{timeline}</ol>{controls}</article>"#,
            muted = if inc.status == "resolved" { " incident--resolved" } else { "" },
            title = esc(&inc.title),
            sev = severity_pill(&inc.severity),
            status = incident_status_pill(&inc.status),
            affected = affected,
            body = esc(&inc.body),
            ago = esc(&rel_time(inc.created_at, now)),
            timeline = timeline,
            controls = controls,
        ));
    }
    items
}

async fn render_maintenances(state: &AppState, now: i64) -> String {
    let maintenances = state.store.list_maintenances().await;
    if maintenances.is_empty() {
        return r#"<div class="empty">No maintenance windows scheduled.</div>"#.to_string();
    }
    let mut items = String::new();
    for m in &maintenances {
        let state_pill = if maintenance_ongoing(m, now) {
            r#"<span class="pill pill-info">in progress</span>"#
        } else if m.ends_at <= now {
            r#"<span class="pill pill-state">ended</span>"#
        } else {
            r#"<span class="pill pill-state">scheduled</span>"#
        };
        items.push_str(&format!(
            r#"<article class="incident{muted}"><div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{state_pill}</span></div><p class="incident__body">{body}</p><div class="incident__time">{starts} → {ends}{affected}</div></article>"#,
            muted = if m.ends_at <= now { " incident--resolved" } else { "" },
            title = esc(&m.title),
            state_pill = state_pill,
            body = esc(&m.body),
            starts = esc(&fmt_datetime(m.starts_at)),
            ends = esc(&fmt_datetime(m.ends_at)),
            affected = if m.affected.is_empty() {
                String::new()
            } else {
                format!(" · affects {}", esc(&m.affected))
            },
        ));
    }
    items
}

/// ` selected` when the incident's current status matches the option value.
fn selected(current: &str, option: &str) -> &'static str {
    if current == option {
        " selected"
    } else {
        ""
    }
}

/// 303 -> GET /admin (post/redirect/get).
fn see_other() -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, "/admin")]).into_response()
}

/// An HTML response, optionally attaching a freshly-minted CSRF `Set-Cookie`.
fn html_with_cookie(body: String, set_cookie: Option<String>) -> Response {
    let mut resp = Html(body).into_response();
    if let Some(c) = set_cookie {
        if let Ok(value) = HeaderValue::from_str(&c) {
            resp.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    resp
}
