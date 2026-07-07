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
    app_css, dynamic_js, esc, fmt_countdown, fmt_datetime, fmt_latency, incident_status_pill,
    rel_time, severity_pill, status_pill, userbox, SHIELD_SVG,
};
use crate::model::{affected_names, maintenance_ongoing};
use crate::notify;
use crate::store::{ComponentGroup, Incident, IncidentUpdate, Maintenance};
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
        return Err(AppError::InvalidRequest(
            "incident title is required".to_string(),
        ));
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

    // Fan the open out to confirmed webhook subscribers (best-effort, off the response path).
    spawn_fan_out(&state, notify::EVENT_OPENED, &incident, None, now);

    // 303 -> GET /admin (post/redirect/get).
    Ok(see_other())
}

/// Spawn the (bounded, best-effort) webhook fan-out for an incident event so subscriber
/// reachability never blocks the operator's 303. Builds the signed-payload body once; each
/// recipient gets its own signature inside [`notify::fan_out`].
fn spawn_fan_out(
    state: &AppState,
    event: &'static str,
    incident: &Incident,
    update: Option<&IncidentUpdate>,
    now: i64,
) {
    let body = notify::incident_body(event, incident, update, now);
    let store = state.store.clone();
    let timeout = state.config.probe_timeout;
    tokio::spawn(async move {
        notify::fan_out(store, event, body, timeout).await;
    });
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
        return Err(AppError::InvalidRequest(
            "update body is required".to_string(),
        ));
    }
    append_update(
        &state,
        &form.incident_id,
        normalize_status(&form.status),
        body,
    )
    .await?;
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

    // Fan the update out with the incident reflecting its NEW status/timestamps + the update.
    let updated_incident = Incident {
        status: status.to_string(),
        updated_at: now,
        resolved_at,
        ..incident
    };
    spawn_fan_out(
        state,
        notify::EVENT_UPDATED,
        &updated_incident,
        Some(&update),
        now,
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
        return Err(AppError::InvalidRequest(
            "maintenance title is required".to_string(),
        ));
    }
    let starts_in_mins = form
        .starts_in_mins
        .trim()
        .parse::<i64>()
        .unwrap_or(0)
        .max(0);
    let duration_mins = form
        .duration_mins
        .trim()
        .parse::<i64>()
        .unwrap_or(60)
        .max(1);

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
// POST /admin/groups + /admin/groups/assign — component groups
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GroupForm {
    #[serde(default)]
    pub name: String,
    /// Sort position among sections (lower first); parsed leniently, default 0.
    #[serde(default)]
    pub position: String,
    #[serde(default)]
    pub csrf_token: String,
}

/// `POST /admin/groups` — create a component group (public-page section).
pub async fn create_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<GroupForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;
    auth::verify_csrf(&headers, &form.csrf_token)?;

    let name = form.name.trim();
    if name.is_empty() {
        return Err(AppError::InvalidRequest(
            "group name is required".to_string(),
        ));
    }
    let position = form.position.trim().parse::<i64>().unwrap_or(0);
    let group = ComponentGroup {
        id: format!("grp_{}", now_nanos()),
        name: name.to_string(),
        position,
    };
    state.store.insert_component_group(&group).await;
    tracing::info!(id = group.id, name = group.name, "component group created");
    Ok(see_other())
}

#[derive(Debug, Deserialize)]
pub struct AssignForm {
    #[serde(default)]
    pub check_name: String,
    /// Target group id, or empty to clear the assignment (ungroup).
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub csrf_token: String,
}

/// `POST /admin/groups/assign` — assign a component to a group (empty `group_id` = ungroup).
pub async fn assign_component(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AssignForm>,
) -> Result<Response, AppError> {
    auth::require_admin(&headers)?;
    auth::verify_csrf(&headers, &form.csrf_token)?;

    let check_name = form.check_name.trim();
    if check_name.is_empty() {
        return Err(AppError::InvalidRequest(
            "component name is required".to_string(),
        ));
    }
    let group_id = form.group_id.trim();
    let target = if group_id.is_empty() {
        None
    } else {
        Some(group_id)
    };
    state.store.set_check_group(check_name, target).await;
    tracing::info!(
        check = check_name,
        group = group_id,
        "component group assignment updated"
    );
    Ok(see_other())
}

// ---------------------------------------------------------------------------
// Incident templates (compiled-in boilerplate the update forms can insert)
// ---------------------------------------------------------------------------

/// The compiled-in incident-update templates: `(status value, label, boilerplate)`. Static
/// text (no remote data), so it is safe to embed directly in the admin forms/JS.
pub fn incident_templates() -> [(&'static str, &'static str, &'static str); 4] {
    [
        (
            "investigating",
            "Investigating",
            "We are currently investigating an issue affecting this service and will provide an update shortly.",
        ),
        (
            "identified",
            "Identified",
            "We have identified the root cause and are working on a fix.",
        ),
        (
            "monitoring",
            "Monitoring",
            "A fix has been deployed and we are monitoring the results.",
        ),
        (
            "resolved",
            "Resolved",
            "This incident has been resolved and all systems are operating normally.",
        ),
    ]
}

/// A row of "insert template" buttons. A tiny delegated click handler (embedded once in the
/// admin page) copies `data-tpl` into the enclosing form's textarea via `.value` (never
/// `innerHTML`), so the static boilerplate is inserted without touching remote strings.
fn render_template_buttons() -> String {
    let mut out = String::from(r#"<div class="tpl-row">"#);
    for (_status, label, text) in incident_templates() {
        out.push_str(&format!(
            r#"<button type="button" class="btn btn-ghost btn-sm" data-tpl="{tpl}">{label}</button>"#,
            tpl = esc(text),
            label = esc(label),
        ));
    }
    out.push_str("</div>");
    out
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

async fn render_admin(state: &AppState, email: &str, csrf: &str, now: i64) -> String {
    let vitals = state.vitals.as_ref().and_then(|v| v.snapshot());
    ADMIN_HTML
        .replace("{{CSS}}", app_css())
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{USERBOX}}", &userbox("Beacon admin", Some(email)))
        .replace("{{EMAIL}}", &esc(email))
        .replace("{{SUMMARY}}", &render_summary(state, now).await)
        .replace("{{ADMINNAV}}", &render_admin_nav())
        .replace("{{CHECKS}}", &render_checks(state).await)
        .replace("{{INFRA_META}}", &render_infra_meta(vitals.as_deref(), now))
        .replace("{{INFRA}}", &render_admin_infra(vitals.as_deref(), now))
        .replace("{{TEMPLATES_CREATE}}", &render_template_buttons())
        .replace("{{INCIDENTS}}", &render_incidents(state, csrf, now).await)
        .replace("{{MAINTENANCES}}", &render_maintenances(state, now).await)
        .replace("{{GROUPS}}", &render_groups(state, csrf).await)
        .replace("{{SUBSCRIBERS}}", &render_subscribers(state).await)
        .replace("{{CSRF}}", &esc(csrf))
        .replace("{{SCRIPTS}}", dynamic_js())
}

async fn render_summary(state: &AppState, now: i64) -> String {
    let checks = state.store.list_checks().await;
    let mut operational = 0usize;
    let degraded = 0usize;
    let mut down = 0usize;
    let mut pending = 0usize;
    for c in &checks {
        match state.store.latest_result(&c.name).await.map(|r| r.ok) {
            Some(true) => operational += 1,
            Some(false) => down += 1,
            None => pending += 1,
        }
    }
    let incidents = state.store.list_incidents().await;
    let open_incidents = incidents.iter().filter(|i| i.status != "resolved").count();
    let maintenances = state.store.list_maintenances().await;
    let visible_maint = maintenances.iter().filter(|m| m.ends_at > now).count();
    let subs = state.store.list_subscribers().await;
    let confirmed = subs.iter().filter(|s| s.confirmed).count();
    let hosts = state
        .vitals
        .as_ref()
        .and_then(|v| v.snapshot())
        .map(|s| s.hosts.len())
        .unwrap_or(0);

    format!(
        r#"<div class="bc-sum">
  {op}{deg}{down}{pending}{inc}{mw}{subs}{hosts}
</div>"#,
        op = summary_tile("Operational", operational, ""),
        deg = summary_tile("Degraded", degraded, " bc-sum__n--warn"),
        down = summary_tile("Down", down, " bc-sum__n--down"),
        pending = summary_tile("Pending", pending, " bc-sum__n--warn"),
        inc = summary_tile("Open incidents", open_incidents, ""),
        mw = summary_tile("Maintenance", visible_maint, ""),
        subs = summary_tile("Confirmed subs", confirmed, ""),
        hosts = summary_tile("Hosts", hosts, ""),
    )
}

fn summary_tile(label: &str, n: usize, cls: &str) -> String {
    format!(
        r#"<div class="bc-sum__tile"><div class="bc-sum__n{cls}">{n}</div><div class="bc-sum__k">{label}</div></div>"#,
        label = esc(label),
    )
}

fn render_admin_nav() -> String {
    r##"<nav class="bc-adminnav" aria-label="Admin sections">
  <a href="#checks">Checks</a>
  <a href="#infra">Infrastructure</a>
  <a href="#post-incident">Post incident</a>
  <a href="#bc-incidents">Incidents</a>
  <a href="#maintenance">Maintenance</a>
  <a href="#bc-groups">Groups</a>
  <a href="#subscribers">Subscribers</a>
</nav>"##
        .to_string()
}

fn render_infra_meta(snap: Option<&crate::vitals::VitalsSnapshot>, now: i64) -> String {
    let Some(snap) = snap else {
        return String::new();
    };
    let stale = if now.saturating_sub(snap.fetched_at) > crate::vitals::SNAPSHOT_TTL_SECS {
        r#"<span class="pill pill-warn">stale</span>"#
    } else {
        ""
    };
    format!(
        r#"{hosts} hosts · refreshed {ago}{stale}"#,
        hosts = snap.hosts.len(),
        ago = esc(&rel_time(snap.fetched_at, now)),
        stale = stale,
    )
}

fn render_admin_infra(snap: Option<&crate::vitals::VitalsSnapshot>, now: i64) -> String {
    let Some(snap) = snap else {
        return r#"<div class="empty">Host metrics unavailable.</div>"#.to_string();
    };
    if snap.hosts.is_empty() {
        return r#"<div class="empty">Host metrics unavailable.</div>"#.to_string();
    }
    let mut out = String::new();
    for host in &snap.hosts {
        out.push_str(&render_host_vitals(host, now));
    }
    out
}

fn render_host_vitals(host: &crate::vitals::HostVitals, now: i64) -> String {
    let stale = host.last_ts < now - 300;
    let tone = host_tone(host);
    let dot = if stale { "bc-dot--stale" } else { tone };
    let up = host
        .uptime_secs
        .map(|s| {
            format!(
                r#"<span class="bc-host__up">up {}</span>"#,
                esc(&fmt_countdown(s as i64))
            )
        })
        .unwrap_or_default();
    let stale_cls = if stale { " bc-host--stale" } else { "" };
    format!(
        r#"<div class="bc-host{stale_cls}">
  <div class="bc-host__id"><span class="crow__dot crow__dot--{dot}" aria-hidden="true"></span><code class="bc-host__name">{host}</code>{up}</div>
  <div class="bc-host__meters">{cpu}{mem}{disk}</div>
  <div class="bc-host__sparkwrap">{spark}</div>
  <div class="bc-host__load" title="Load 1 / 5 / 15">{load}</div>
  <div class="bc-host__net">{net}</div>
  <div class="bc-host__anom">{anom}</div>
</div>"#,
        stale_cls = stale_cls,
        dot = dot,
        host = esc(&host.host),
        up = up,
        cpu = render_meter("CPU", host.cpu_pct),
        mem = render_meter("MEM", host.mem_pct),
        disk = render_meter("DISK", host.disk_pct),
        spark = bc_spark(&host.cpu_series, 100.0),
        load = esc(&fmt_load(host.load1, host.load5, host.load15)),
        net = esc(&format!(
            "↓{} ↑{}",
            fmt_bps(host.net_rx_bps),
            fmt_bps(host.net_tx_bps)
        )),
        anom = if host.anomalies_24h > 0 {
            format!(
                r#"<span class="pill pill-warn">{} anomalies</span>"#,
                host.anomalies_24h
            )
        } else {
            r#"<span class="pill pill-state">0 anomalies</span>"#.to_string()
        },
    )
}

fn render_meter(label: &str, pct: Option<f64>) -> String {
    let pct_u8 = pct.map(|p| p.round().clamp(0.0, 100.0) as u8).unwrap_or(0);
    let tone = match pct {
        Some(p) if p >= 90.0 => odyssey::Tone::Down,
        Some(p) if p >= 70.0 => odyssey::Tone::Warn,
        Some(_) => odyssey::Tone::Ok,
        None => odyssey::Tone::Neutral,
    };
    let val = pct
        .map(|p| format!("{:.0}%", p.round()))
        .unwrap_or_else(|| "—".to_string());
    format!(
        r#"<div class="bc-meter"><span class="bc-meter__k">{label}</span>{progress}<span class="bc-meter__v">{val}</span></div>"#,
        label = esc(label),
        progress = odyssey::progress(pct_u8, tone).0,
        val = esc(&val),
    )
}

fn host_tone(host: &crate::vitals::HostVitals) -> &'static str {
    let worst = [host.cpu_pct, host.mem_pct, host.disk_pct]
        .into_iter()
        .flatten()
        .fold(None, |acc: Option<f64>, p| {
            Some(acc.map_or(p, |a| a.max(p)))
        });
    match worst {
        Some(p) if p >= 90.0 => "down",
        Some(p) if p >= 70.0 => "warn",
        _ => "ok",
    }
}

fn fmt_load(load1: Option<f64>, load5: Option<f64>, load15: Option<f64>) -> String {
    match (load1, load5, load15) {
        (None, None, None) => "—".to_string(),
        _ => format!(
            "{:.2} · {:.2} · {:.2}",
            load1.unwrap_or(0.0),
            load5.unwrap_or(0.0),
            load15.unwrap_or(0.0)
        ),
    }
}

fn fmt_bps(value: Option<f64>) -> String {
    let Some(v) = value else {
        return "—".to_string();
    };
    if v >= 1_000_000.0 {
        format!("{:.1} MB/s", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1} KB/s", v / 1_000.0)
    } else {
        format!("{:.0} B/s", v)
    }
}

fn bc_spark(values: &[f64], max: f64) -> String {
    if values.len() < 2 {
        return r#"<svg class="bc-host__spark" viewBox="0 0 120 28" preserveAspectRatio="none" aria-hidden="true"><line x1="0" y1="14" x2="120" y2="14" stroke="var(--border)" stroke-width="1"/></svg>"#.to_string();
    }
    let max = max.max(1.0);
    let denom = (values.len() - 1) as f64;
    let points: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = i as f64 / denom * 120.0;
            let y = 26.0 - (v.clamp(0.0, max) / max * 24.0);
            format!("{x:.1},{y:.1}")
        })
        .collect();
    let mut area = String::from("0,28 ");
    area.push_str(&points.join(" "));
    area.push_str(" 120,28");
    format!(
        r#"<svg class="bc-host__spark" viewBox="0 0 120 28" preserveAspectRatio="none" aria-hidden="true"><polygon points="{area}" fill="var(--accent)" fill-opacity=".10"></polygon><polyline points="{line}" fill="none" stroke="var(--accent)" stroke-width="1.5" vector-effect="non-scaling-stroke"></polyline></svg>"#,
        area = area,
        line = points.join(" "),
    )
}

async fn render_checks(state: &AppState) -> String {
    let checks = state.store.list_checks().await;
    if checks.is_empty() {
        return r#"<tr><td colspan="5" class="empty">No checks configured.</td></tr>"#.to_string();
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
            r#"<tr><td><strong>{name}</strong> <span class="muted">({enabled})</span></td><td><code>{kind}</code></td><td><code>{target}</code></td><td>{pill}</td><td><span class="mono">{latency}</span></td></tr>"#,
            name = esc(&c.name),
            enabled = enabled,
            kind = esc(&c.kind),
            target = esc(&c.target),
            pill = if latest.is_some() { status_pill(status) } else { r#"<span class="pill pill-state">pending</span>"#.to_string() },
            latency = esc(&fmt_latency(latest.as_ref().map(|r| r.latency_ms))),
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
                r##"<form method="post" action="/admin/incidents/update" class="incident__form" data-wire data-wire-target="#bc-incidents" data-wire-select="#bc-incidents" data-wire-busy="Posting…" data-wire-ok="Update posted">
  <input type="hidden" name="csrf_token" value="{csrf}">
  <input type="hidden" name="incident_id" value="{id}">
  <div class="field"><label>Update</label>{templates}<textarea name="body" required placeholder="What changed."></textarea></div>
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
  <span class="bc-confirm" data-spark="confirm:false">
    <button class="btn btn-ghost btn-sm" type="button" data-spark-click="set:confirm=true">Resolve</button>
    <button class="btn btn-primary btn-sm" type="submit" data-spark-show="confirm=true">Yes, resolve</button>
    <button class="btn btn-ghost btn-sm" type="button" data-spark-click="set:confirm=false" data-spark-show="confirm=true">Cancel</button>
  </span>
</form>"##,
                csrf = esc(csrf),
                id = esc(&inc.id),
                templates = render_template_buttons(),
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
        let (state_pill, countdown) = if maintenance_ongoing(m, now) {
            (
                r#"<span class="pill pill-info">in progress</span>"#,
                format!(" · ends in {}", fmt_countdown(m.ends_at - now)),
            )
        } else if m.ends_at <= now {
            (
                r#"<span class="pill pill-state">ended</span>"#,
                String::new(),
            )
        } else {
            (
                r#"<span class="pill pill-state">scheduled</span>"#,
                format!(" · starts in {}", fmt_countdown(m.starts_at - now)),
            )
        };
        items.push_str(&format!(
            r#"<article class="incident{muted}"><div class="incident__head"><h3 class="incident__title">{title}</h3><span class="incident__pills">{state_pill}</span></div><p class="incident__body">{body}</p><div class="incident__time">{starts} → {ends}{countdown}{affected}</div></article>"#,
            muted = if m.ends_at <= now { " incident--resolved" } else { "" },
            title = esc(&m.title),
            state_pill = state_pill,
            body = esc(&m.body),
            starts = esc(&fmt_datetime(m.starts_at)),
            ends = esc(&fmt_datetime(m.ends_at)),
            countdown = esc(&countdown),
            affected = if m.affected.is_empty() {
                String::new()
            } else {
                format!(" · affects {}", esc(&m.affected))
            },
        ));
    }
    items
}

/// The "Component groups" card body: a create-group form, the existing groups (with their
/// position), and a per-component group assignment control. Assignment writes go through the
/// CSRF-protected `/admin/groups/assign`.
async fn render_groups(state: &AppState, csrf: &str) -> String {
    let groups = state.store.list_component_groups().await;
    let checks = state.store.list_checks().await;

    // The <select> options: "Ungrouped" plus one per group (marking the current assignment).
    let option_list = |current: Option<&str>| -> String {
        let mut opts = format!(
            r#"<option value=""{sel}>Ungrouped</option>"#,
            sel = if current.is_none() { " selected" } else { "" },
        );
        for g in &groups {
            opts.push_str(&format!(
                r#"<option value="{id}"{sel}>{name}</option>"#,
                id = esc(&g.id),
                sel = if current == Some(g.id.as_str()) {
                    " selected"
                } else {
                    ""
                },
                name = esc(&g.name),
            ));
        }
        opts
    };

    let mut group_list = String::new();
    if groups.is_empty() {
        group_list.push_str(
            r#"<div class="empty">No groups yet. Components render in a single flat list.</div>"#,
        );
    } else {
        group_list.push_str(r#"<ul class="group-list">"#);
        for g in &groups {
            group_list.push_str(&format!(
                r#"<li><strong>{name}</strong> <span class="muted">position {pos}</span></li>"#,
                name = esc(&g.name),
                pos = g.position,
            ));
        }
        group_list.push_str("</ul>");
    }

    let mut assign_rows = String::new();
    if checks.is_empty() {
        assign_rows
            .push_str(r#"<tr><td colspan="2" class="empty">No components configured.</td></tr>"#);
    } else {
        for c in &checks {
            assign_rows.push_str(&format!(
                r##"<tr><td><strong>{name}</strong></td><td><form method="post" action="/admin/groups/assign" class="assign-form" data-wire data-wire-target="#bc-groups" data-wire-select="#bc-groups" data-wire-ok="Assigned">
  <input type="hidden" name="csrf_token" value="{csrf}">
  <input type="hidden" name="check_name" value="{name}">
  <select name="group_id">{options}</select>
  <button class="btn btn-secondary btn-sm" type="submit">Assign</button>
</form></td></tr>"##,
                name = esc(&c.name),
                csrf = esc(csrf),
                options = option_list(c.group_id.as_deref()),
            ));
        }
    }

    format!(
        r#"<form method="post" action="/admin/groups" class="group-create">
  <input type="hidden" name="csrf_token" value="{csrf}">
  <div class="field"><label for="g-name">New group</label><input type="text" id="g-name" name="name" required placeholder="e.g. Apps"></div>
  <div class="field"><label for="g-pos">Position</label><input type="number" id="g-pos" name="position" value="0"></div>
  <button class="btn btn-primary" type="submit">Create group</button>
</form>
{group_list}
<h3 class="section-title">Assign components</h3>
<table class="table"><thead><tr><th>Component</th><th>Group</th></tr></thead><tbody>{assign_rows}</tbody></table>"#,
        csrf = esc(csrf),
        group_list = group_list,
        assign_rows = assign_rows,
    )
}

/// The "Subscribers" card body: a read-only summary of public webhook subscriptions (count +
/// confirmation state + target host). Targets are escaped.
async fn render_subscribers(state: &AppState) -> String {
    let subs = state.store.list_subscribers().await;
    if subs.is_empty() {
        return r#"<div class="empty">No status subscribers yet.</div>"#.to_string();
    }
    let confirmed = subs.iter().filter(|s| s.confirmed).count();
    let mut rows = String::new();
    for s in &subs {
        let pill = if s.confirmed {
            r#"<span class="pill pill-ok">confirmed</span>"#
        } else {
            r#"<span class="pill pill-state">pending</span>"#
        };
        rows.push_str(&format!(
            r#"<tr><td><code>{kind}</code></td><td><code>{target}</code></td><td>{pill}</td></tr>"#,
            kind = esc(&s.kind),
            target = esc(&s.target),
            pill = pill,
        ));
    }
    format!(
        r#"<p class="hint--muted">{confirmed} confirmed of {total} total.</p>
<table class="table"><thead><tr><th>Kind</th><th>Target</th><th>State</th></tr></thead><tbody>{rows}</tbody></table>"#,
        confirmed = confirmed,
        total = subs.len(),
        rows = rows,
    )
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
