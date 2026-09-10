//! End-to-end HTTP contract test against the in-memory store (NO database).
//!
//! Drives the real Router in-process via `tower::oneshot` and asserts the public surface
//! renders without auth (status page, JSON API, RSS feed), the incident/maintenance
//! lifecycle through the CSRF-protected admin POSTs, the banner precedence
//! (critical incident > maintenance > all-ok), the maintenance pill masking, and the
//! compact 30-day public history.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::config::{Config, PublicComponent};
use beacon::store::{Incident, Maintenance};
use beacon::{app, build_dev_state, internal_app, now_secs, state_with, AppState};
use serde_json::Value;
use tower::ServiceExt;

/// Fixed double-submit token: the tests present it as BOTH the cookie and the form field.
const CSRF: &str = "tok_csrf_for_tests";

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn wire_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::ACCEPT, "text/html")
        .header("x-wire", "1")
        .body(Body::empty())
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// An admin POST carrying the CSRF cookie plus optional identity headers; the form body
/// must include the matching `csrf_token` field itself.
fn post_admin(uri: &str, headers: &[(&str, &str)], form: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("__Host-csrf={CSRF}"));
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::from(form.to_string())).unwrap()
}

const OPERATOR: &[(&str, &str)] = &[
    ("x-auth-subject", "u_admin"),
    ("x-auth-email", "admin@steadholme.local"),
];

#[tokio::test]
async fn healthz_ok() {
    let state = build_dev_state().await;
    let (status, body) = call(&state, get("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body, b"ok");
}

#[tokio::test]
async fn public_status_renders_without_auth() {
    let state = build_dev_state().await;
    let (root_status, root_body) = call(&state, get("/")).await;
    assert_eq!(root_status, StatusCode::OK, "public host root is open");
    assert!(
        text(&root_body).starts_with("<!DOCTYPE html>"),
        "public host root keeps the complete SSR document"
    );

    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK, "public status page is open");
    let html = text(&body);
    let skip = html
        .find(r##"<a class="skip-link" href="#status-main">"##)
        .expect("first-focus skip link");
    let chrome = html.find(r#"<header class="topbar">"#).unwrap();
    assert!(skip < chrome, "skip link precedes application chrome");
    assert!(html.contains(
        r#"<main class="console" id="status-main" tabindex="-1" aria-labelledby="status-title">"#
    ));
    assert!(html.contains(r#"content="width=device-width, initial-scale=1, viewport-fit=cover""#));
    assert!(html.contains(r#"<body class="page-console page-status" data-ody-shell="1.3">"#));
    assert!(html.contains("beacon status-page v1"), "page script embedded");
    assert!(
        html.contains(r#"<time class="clock" datetime=""#) && html.contains(" UTC</time>"),
        "UTC clock value in the app bar"
    );
    assert!(
        !html.contains("status-stage") && !html.contains("status-window"),
        "the fixed reading window is retired"
    );
    assert!(html.contains("Steadholme"), "brand present");
    // The state IS the heading: no "System status" title, no operational-snapshot counts,
    // no repeated state words on an all-nominal page.
    assert!(html.contains(
        r#"<h1 id="status-title" class="mast__headline">All systems operational</h1>"#
    ));
    assert!(html.contains(r#"<section class="mast mast--operational">"#));
    assert!(!html.contains("System status"));
    assert!(!html.contains("bc-snapshot"));
    assert!(!html.contains("monitored"));
    assert!(!html.contains("components</span>"));
    assert!(!html.contains("statushead__signal"));
    assert!(!html.contains("sovereign infrastructure"));
    // The fail-safe public catalog shows only explicitly listed components. CA remains an
    // internal raw probe even though it is present in the default seed.
    assert!(html.contains(r#"<span class="tile__name">Gateway</span>"#));
    assert!(html.contains(r#"<span class="tile__name">Identity</span>"#));
    assert!(!html.contains(r#"<span class="tile__name">CA</span>"#));
    assert_eq!(html.matches(r#"<details class="tile tile--"#).count(), 2);
    // No data yet -> pending tiles, no evidence average, no active-incident section.
    assert_eq!(
        html.matches(r#"<details class="tile tile--pending" name="tile">"#).count(),
        2
    );
    assert!(
        !html.contains(r#"class="mast__uptime""#),
        "no nominal evidence-window average before first check"
    );
    assert!(!html.contains("Active incidents"));
    assert!(html.contains(r#"<h2 class="history__title">Past incidents</h2>"#));
    assert!(html.contains("No incidents in the last 14 days"));
    assert!(html.contains(r#"<section class="catalog" aria-label="Components">"#));
    assert!(html.contains(r#"<section class="estate">"#));
    // Estate strip (30) + two mini strips (60) + two detail strips (60) of unknown days.
    assert_eq!(html.matches(r#"class="cell cell--unknown""#).count(), 150);
    assert!(
        html.contains(r#"data-uptime="no data""#),
        "unknown days carry no-data metadata"
    );
    assert!(
        html.contains(r#"data-date=""#),
        "cell dates render as data attributes"
    );
    // Channels are actions only; the webhook form stays off by default.
    assert!(html.contains(r#"<section class="channels" id="subscribe">"#));
    assert!(!html.contains(r#"action="/subscriptions""#));
    assert!(!html.contains("Webhook registration"));
    assert!(html.contains(r#"<link rel="stylesheet" href="/assets/beacon-20260907.css">"#));
    assert!(!html.contains("<style>"), "shared CSS stays out of HTML");
}

#[tokio::test]
async fn public_wire_fragment_matches_the_full_ssr_live_region() {
    let state = build_dev_state().await;

    let full_response = app(state.clone()).oneshot(get("/status")).await.unwrap();
    assert_eq!(full_response.status(), StatusCode::OK);
    assert_eq!(
        full_response.headers().get(header::VARY).unwrap(),
        "X-Wire",
        "representation caches must vary on the Wire handshake"
    );
    assert_eq!(
        full_response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store",
        "the public live snapshot must not enter a shared cache"
    );
    let full = text(
        &axum::body::to_bytes(full_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    let fragment_response = app(state.clone())
        .oneshot(wire_get("/status"))
        .await
        .unwrap();
    assert_eq!(
        fragment_response.status(),
        StatusCode::OK,
        "Wire refresh remains anonymous"
    );
    assert_eq!(
        fragment_response.headers().get(header::VARY).unwrap(),
        "X-Wire"
    );
    assert_eq!(
        fragment_response
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap(),
        "no-store"
    );
    assert!(fragment_response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let fragment = text(
        &axum::body::to_bytes(fragment_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    let live_start = full.find(r#"<div id="status-live""#).unwrap();
    let subscribe_id = full[live_start..]
        .find(r#" id="subscribe">"#)
        .map(|offset| live_start + offset)
        .unwrap();
    let subscribe_start = full[..subscribe_id].rfind("<section").unwrap();
    assert_eq!(
        fragment,
        full[live_start..subscribe_start].trim_end(),
        "full and fragment responses must share one renderer"
    );
    assert!(fragment.starts_with(r#"<div id="status-live""#));
    assert!(!fragment.contains("<!DOCTYPE html>"));
    assert!(!fragment.contains("<script"));
    assert!(!fragment.contains(r#"action="/subscriptions""#));
}

#[tokio::test]
async fn public_refresh_keeps_a_complete_no_js_floor_and_declares_error_recovery() {
    let state = build_dev_state().await;
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains(r#"data-ody-profile="public""#));
    assert!(html.contains(r#"data-ody-shell="1.3""#));
    assert!(html.contains(r#"<meta http-equiv="refresh" content="300">"#));
    assert!(!html.contains("Public · read only"));
    assert!(html.contains(r#"role="region" aria-label="Live system status""#));
    assert!(
        !html.contains(r#"aria-label="Live system status" aria-live="#),
        "the large live region must not be announced wholesale"
    );

    let refresh_start = html
        .find(r#"<a class="btn btn-ghost btn-sm" href="/status" role="button""#)
        .expect("typed Odyssey refresh link");
    let refresh_end = html[refresh_start..]
        .find("</a>")
        .map(|offset| refresh_start + offset + 4)
        .unwrap();
    let refresh = &html[refresh_start..refresh_end];
    for contract in [
        r#"href="/status""#,
        r#"data-wire="get""#,
        r##"data-wire-target="#status-live""##,
        r##"data-wire-select="#status-live""##,
        r#"data-wire-swap="outer""#,
        r#"data-wire-busy="Refreshing…""#,
        r#"data-wire-ok="Live status refreshed.""#,
        r#"data-wire-err="Live refresh failed — use the link to reload.""#,
    ] {
        assert!(
            refresh.contains(contract),
            "missing refresh contract: {contract}"
        );
    }
    assert!(!refresh.contains("data-wire-push"));
    assert!(!refresh.contains("data-wire-optimistic"));
    assert!(
        html.contains("window.OdysseyWire"),
        "audited runtime embedded"
    );
    assert!(html.contains(r#"href="/feed.xml""#));

    let invalid_wire = Request::builder()
        .uri("/status")
        .header("x-wire", "unexpected")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, invalid_wire).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text(&body).starts_with("<!DOCTYPE html>"),
        "an invalid handshake safely falls back to full SSR"
    );
}

#[tokio::test]
async fn api_status_json_shape() {
    let state = build_dev_state().await;
    let response = app(state).oneshot(get("/api/status")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["overall"], "operational");
    assert_eq!(v["history_days"], 30);
    assert_eq!(v["components"].as_array().unwrap().len(), 2);
    assert!(v["components"]
        .as_array()
        .unwrap()
        .iter()
        .all(|component| component["name"] != "CA"));
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    assert_eq!(gw["status"], "operational");
    assert_eq!(gw["uptime_24h"], 100.0);
    assert_eq!(gw["days"].as_array().unwrap().len(), 30, "30 daily bars");
    assert_eq!(gw["days"][0]["status"], "unknown", "no data yet -> unknown");
    assert!(v["incidents"].as_array().unwrap().is_empty());
    assert!(v["maintenances"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn raw_status_exists_only_on_the_dedicated_internal_router() {
    let state = build_dev_state().await;

    let public = app(state.clone())
        .oneshot(get("/api/internal/status"))
        .await
        .unwrap();
    assert_eq!(
        public.status(),
        StatusCode::UNAUTHORIZED,
        "public router has no raw status route; fallback remains admin-authenticated"
    );

    let response = internal_app(state)
        .oneshot(get("/api/status"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let view: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(view["history_days"], 90);
    assert_eq!(view["components"].as_array().unwrap().len(), 3);
    assert!(view["components"]
        .as_array()
        .unwrap()
        .iter()
        .any(|component| component["name"] == "CA"));
}

#[tokio::test]
async fn compatibility_uptime_field_follows_each_read_models_evidence_window() {
    let state = build_dev_state().await;
    let now = now_secs();
    // A 45-day-old failure belongs to the operator's 90-day evidence, but must not affect the
    // public projection's declared 30-day evidence window.
    state
        .store
        .insert_result("Gateway", false, 80, now - 45 * 86_400)
        .await;
    state.store.insert_result("Gateway", true, 12, now).await;

    let (_, body) = call(&state, get("/api/status")).await;
    let public: Value = serde_json::from_slice(&body).unwrap();
    let public_gateway = public["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["name"] == "Gateway")
        .unwrap();
    assert_eq!(public["history_days"], 30);
    assert_eq!(
        public_gateway["uptime_90d"], 100.0,
        "legacy field follows the public 30-day evidence window"
    );

    let response = internal_app(state)
        .oneshot(get("/api/status"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let internal: Value = serde_json::from_slice(&body).unwrap();
    let internal_gateway = internal["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["name"] == "Gateway")
        .unwrap();
    assert_eq!(internal["history_days"], 90);
    assert_eq!(
        internal_gateway["uptime_90d"], 50.0,
        "operator model retains its 90-day evidence window"
    );
}

#[tokio::test]
async fn public_catalog_aggregates_raw_checks_without_leaking_member_names() {
    let mut config = Config::dev();
    config.public_catalog = vec![PublicComponent {
        name: "Public edge".to_string(),
        group: "Delivery".to_string(),
        checks: vec!["Gateway".to_string(), "CA".to_string()],
    }];
    let state = state_with(config).await;
    let now = now_secs();
    state.store.insert_result("Gateway", true, 12, now).await;
    state.store.insert_result("CA", false, 44, now).await;

    let (_, body) = call(&state, get("/api/status")).await;
    let view: Value = serde_json::from_slice(&body).unwrap();
    let components = view["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0]["name"], "Public edge");
    assert_eq!(components[0]["kind"], "service");
    assert_eq!(components[0]["status"], "down", "worst raw member wins");
    assert_eq!(components[0]["latency_ms"], 44);
    assert_eq!(view["groups"][0]["name"], "Delivery");
    let json = text(&body);
    assert!(!json.contains(r#""name":"Gateway""#));
    assert!(!json.contains(r#""name":"CA""#));
}

#[tokio::test]
async fn daily_bars_reflect_probe_results() {
    let state = build_dev_state().await;
    let now = now_secs();
    // Day-aligned timestamps so the buckets are deterministic even right after midnight:
    // today 3 ok of 4 (75% -> down tint), yesterday all ok.
    let day_start = (now / 86_400) * 86_400;
    state
        .store
        .insert_result("Gateway", true, 10, day_start + 1)
        .await;
    state
        .store
        .insert_result("Gateway", true, 10, day_start + 2)
        .await;
    state
        .store
        .insert_result("Gateway", true, 10, day_start + 3)
        .await;
    state
        .store
        .insert_result("Gateway", false, 10, day_start + 4)
        .await;
    state
        .store
        .insert_result("Gateway", true, 10, day_start - 10)
        .await;

    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    let days = gw["days"].as_array().unwrap();
    assert_eq!(days.len(), 30);
    assert_eq!(days[29]["status"], "down", "75% today -> down tint");
    assert_eq!(days[29]["uptime"], 75.0);
    assert_eq!(days[0]["status"], "unknown", "30 days ago -> no data");
    assert_eq!(days[0]["uptime"], Value::Null);

    // The HTML page renders the same bars with date + percent metadata.
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(html.contains(r#"class="cell cell--down""#));
    assert!(
        html.contains(r#"data-uptime="75.00%""#),
        "bar metadata carries the day percent"
    );
}

#[tokio::test]
async fn admin_post_requires_gateway_identity_and_csrf() {
    let state = build_dev_state().await;
    // No X-Auth-* headers -> 401 (even with a valid CSRF pair).
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            &[],
            &format!("title=Outage&status=investigating&body=down&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no SSO identity -> 401");

    // Identity present but the form token does not match the cookie -> 401.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            OPERATOR,
            "title=Outage&status=investigating&body=down&csrf_token=WRONG",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "CSRF mismatch -> 401");

    // The same guards protect every write route.
    for uri in [
        "/admin/incidents/update",
        "/admin/incidents/resolve",
        "/admin/maintenances",
    ] {
        let (status, _) = call(&state, post_admin(uri, &[], &format!("csrf_token={CSRF}"))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri}: no identity -> 401"
        );
        let (status, _) = call(&state, post_admin(uri, OPERATOR, "csrf_token=WRONG")).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri}: CSRF mismatch -> 401"
        );
    }

    // Nothing leaked onto the public page.
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v["incidents"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn incident_lifecycle_shows_on_public_status() {
    let state = build_dev_state().await;

    // Operator opens a major incident against the Gateway.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            OPERATOR,
            &format!(
                "title=Identity+provider+degraded&status=investigating&severity=major\
                 &affected=Gateway&body=Investigating+elevated+errors&csrf_token={CSRF}"
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "post/redirect/get -> 303");

    // It now appears on the PUBLIC status page (no auth), in the active section, and the
    // banner reflects the major severity (degraded wording).
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("Active incidents"), "active section renders");
    assert!(html.contains("Identity provider degraded"));
    assert!(html.contains("Investigating"), "status pill shows");
    assert!(
        html.contains(r#"<article class="incident incident--major">"#),
        "severity-railed card"
    );
    assert!(
        html.contains(r#"<span class="stage stage--now" aria-current="step">"#),
        "the stage track marks the current stage"
    );
    assert!(
        html.contains("Partial degradation"),
        "major incident -> degraded banner"
    );
    let estate = html.find(r#"<section class="estate">"#).unwrap();
    let active = html.find("Active incidents").unwrap();
    assert!(
        estate < active,
        "the estate strip precedes the active ledger"
    );

    // And in the JSON API.
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let incidents = v["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0]["title"], "Identity provider degraded");
    assert_eq!(incidents[0]["severity"], "major");
    assert_eq!(incidents[0]["affected"], "Gateway");
    assert_eq!(v["overall"], "degraded");
    let id = incidents[0]["id"].as_str().unwrap().to_string();

    // Post a monitoring update: the timeline grows and the status moves.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents/update",
            OPERATOR,
            &format!("incident_id={id}&status=monitoring&body=Fix+deployed&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(html.contains("Monitoring"), "moved to monitoring");
    assert!(
        html.contains("Fix deployed"),
        "latest update leads the card"
    );
    assert!(html.contains("Timeline (2)"), "opening report + one update");

    // Resolve it: the banner returns to all-ok and the incident moves to Past, muted.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents/resolve",
            OPERATOR,
            &format!("incident_id={id}&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(
        html.contains("All systems operational"),
        "resolved -> banner clears"
    );
    assert!(!html.contains("Active incidents"), "active section gone");
    assert!(html.contains("hrow--resolved"), "past incident muted");
    assert!(html.contains("Resolved"), "resolved pill in past section");

    // Updating an unknown incident is a 400.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents/update",
            OPERATOR,
            &format!("incident_id=inc_nope&status=monitoring&body=x&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn public_incidents_and_feed_fail_closed_on_internal_affected_names() {
    let state = build_dev_state().await;

    for form in [
        format!(
            "title=Internal+CA+rotation&severity=critical&affected=CA&body=keyward%3A8200&csrf_token={CSRF}"
        ),
        format!(
            "title=Gateway+and+internal+dependency&severity=minor&affected=CA%2CGateway&body=public+impact&csrf_token={CSRF}"
        ),
    ] {
        let (status, _) = call(
            &state,
            post_admin("/admin/incidents", OPERATOR, &form),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }

    let (_, body) = call(&state, get("/api/status")).await;
    let view: Value = serde_json::from_slice(&body).unwrap();
    let incidents = view["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 1, "internal-only incident is not public");
    assert_eq!(incidents[0]["title"], "Gateway and internal dependency");
    assert_eq!(incidents[0]["affected"], "Gateway");
    let public_json = text(&body);
    assert!(!public_json.contains("Internal CA rotation"));
    assert!(!public_json.contains("keyward:8200"));

    let (_, feed) = call(&state, get("/feed.xml")).await;
    let feed = text(&feed);
    assert!(feed.contains("Gateway and internal dependency"));
    assert!(!feed.contains("Internal CA rotation"));
    assert!(!feed.contains("keyward:8200"));

    let response = internal_app(state)
        .oneshot(get("/api/status"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let raw: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(raw["incidents"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn banner_precedence_critical_over_maintenance_over_ok() {
    let state = build_dev_state().await;

    // Ongoing maintenance alone -> maintenance banner + Gateway masked to maintenance pill.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/maintenances",
            OPERATOR,
            &format!(
                "title=DB+upgrade&body=Planned+work&starts_in_mins=0&duration_mins=60\
                 &affected=Gateway&csrf_token={CSRF}"
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["overall"], "maintenance", "maintenance beats all-ok");
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    assert_eq!(gw["status"], "maintenance", "affected component masked");
    assert_eq!(v["maintenances"].as_array().unwrap().len(), 1);
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(
        html.contains("Scheduled maintenance underway"),
        "info banner"
    );
    assert!(html.contains("DB upgrade"));
    assert!(html.contains("in progress"), "ongoing window pill");

    // An active critical incident outranks the maintenance banner.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            OPERATOR,
            &format!(
                "title=Total+outage&severity=critical&affected=Gateway&body=x&csrf_token={CSRF}"
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["overall"], "down", "critical incident beats maintenance");
    let (_, body) = call(&state, get("/status")).await;
    assert!(
        text(&body).contains("Service disruption"),
        "down banner wording"
    );

    // Resolving the incident falls back to the maintenance banner (still ongoing).
    let id = v["incidents"][0]["id"].as_str().unwrap().to_string();
    let (_, _) = call(
        &state,
        post_admin(
            "/admin/incidents/resolve",
            OPERATOR,
            &format!("incident_id={id}&csrf_token={CSRF}"),
        ),
    )
    .await;
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["overall"], "maintenance", "falls back to maintenance");
}

#[tokio::test]
async fn rss_feed_is_well_formed_and_escaped() {
    let state = build_dev_state().await;

    // Empty history: still a valid, well-formed channel.
    let resp = app(state.clone()).oneshot(get("/feed.xml")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "feed is public (no auth)");
    assert!(resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/rss+xml"));
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let xml = text(&bytes);
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(xml.contains("<rss version=\"2.0\""));
    assert!(xml.ends_with("</rss>\n"));
    assert!(!xml.contains("<item>"), "no incidents -> no items");

    // Open an incident with XML-hostile characters, then resolve it.
    let (_, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            OPERATOR,
            &format!(
                "title=Cache+%26+CDN+%3Cdegraded%3E&severity=minor&affected=Gateway\
                 &body=5%25+errors+%26+retries&csrf_token={CSRF}"
            ),
        ),
    )
    .await;
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let id = v["incidents"][0]["id"].as_str().unwrap().to_string();
    let (_, _) = call(
        &state,
        post_admin(
            "/admin/incidents/resolve",
            OPERATOR,
            &format!("incident_id={id}&csrf_token={CSRF}"),
        ),
    )
    .await;

    let (_, bytes) = call(&state, get("/feed.xml")).await;
    let xml = text(&bytes);
    // Two items: the open and the resolve update, both escaped.
    assert_eq!(xml.matches("<item>").count(), 2);
    assert_eq!(xml.matches("</item>").count(), 2);
    assert!(xml.contains("Cache &amp; CDN &lt;degraded&gt; — new minor incident"));
    assert!(xml.contains("Cache &amp; CDN &lt;degraded&gt; — Resolved"));
    assert!(xml.contains("5% errors &amp; retries"));
    assert!(!xml.contains("<degraded>"), "raw user markup never emitted");
    assert!(xml.contains("<guid isPermaLink=\"false\">inc_"));
    assert!(xml.contains("<guid isPermaLink=\"false\">upd_"));
    assert!(xml.contains("GMT</pubDate>"));
    // Every angle bracket is part of balanced markup: parseable tag soup check.
    assert_eq!(xml.matches('<').count(), xml.matches('>').count());
}

#[tokio::test]
async fn admin_page_renders_with_email() {
    let state = build_dev_state().await;
    let req = Request::builder()
        .uri("/admin")
        .header("x-auth-subject", "u_admin")
        .header("x-auth-email", "ops@steadholme.local")
        .body(Body::empty())
        .unwrap();
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("first render mints the CSRF cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        set_cookie.starts_with("__Host-csrf="),
        "double-submit cookie set"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = text(&bytes);
    assert!(html.contains("Beacon admin"));
    assert!(
        html.contains("ops@steadholme.local"),
        "signed-in email shown"
    );
    assert!(html.contains("/_gw/auth/logout"), "logout link present");
    assert!(html.contains("Post an incident"));
    assert!(html.contains("Schedule maintenance"));
    // New admin surfaces: incident templates, component groups, and the subscribers panel.
    assert!(
        html.contains(r#"data-tpl="#),
        "insert-template buttons present"
    );
    assert!(html.contains("Component groups"), "group management card");
    assert!(
        html.contains(r#"action="/admin/groups""#),
        "create-group form"
    );
    assert!(html.contains("Status subscribers"), "subscribers panel");
    assert!(
        html.contains(r#"name="csrf_token""#),
        "forms embed the CSRF token"
    );
    // The rendered token matches the minted cookie (double-submit pair).
    let token = set_cookie
        .trim_start_matches("__Host-csrf=")
        .split(';')
        .next()
        .unwrap();
    assert!(
        html.contains(token),
        "hidden field carries the cookie token"
    );
    // Desk structure: skip link, bc-desk class, slate, attention-first summary order.
    assert!(html.contains(r#"class="skip-link""#), "skip link present");
    assert!(
        html.contains(r##"href="#desk-main""##),
        "skip link targets desk main"
    );
    assert!(
        html.contains(r#"class="page-console bc-desk""#),
        "bc-desk class present"
    );
    assert!(html.contains(r#"class="bc-slate"#), "slate section present");
    let checks_pos = html.find(r#"id="checks""#).unwrap_or(usize::MAX);
    let slate_pos = html.find(r#"class="bc-slate"#).unwrap_or(usize::MAX);
    assert!(
        slate_pos < checks_pos,
        "slate appears before checks section"
    );
    // Summary tiles: Down tile appears before Operational.
    let down_tile = html.find("Down").unwrap_or(usize::MAX);
    let operational_tile = html.find("Operational").unwrap_or(usize::MAX);
    assert!(
        down_tile < operational_tile,
        "Down tile precedes Operational tile"
    );
}

#[tokio::test]
async fn admin_page_rejects_anonymous_reads() {
    let state = build_dev_state().await;
    for request in [get("/admin"), wire_get("/admin")] {
        let (status, body) = call(&state, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!text(&body).contains("Post an incident"));
        assert!(!text(&body).contains("Status subscribers"));
    }
}

#[tokio::test]
async fn admin_slate_reflects_attention_states() {
    let state = build_dev_state().await;
    let now = crate::now_secs();
    state.store.insert_result("Gateway", false, 0, now).await;
    state
        .store
        .insert_incident(&beacon::store::Incident {
            id: "inc_test".to_string(),
            title: "Test incident".to_string(),
            status: "investigating".to_string(),
            severity: "major".to_string(),
            affected: "Gateway".to_string(),
            body: "Something is wrong".to_string(),
            created_at: now,
            updated_at: now,
            resolved_at: 0,
        })
        .await;

    let req = Request::builder()
        .uri("/admin")
        .header("x-auth-subject", "u_admin")
        .header("x-auth-email", "ops@steadholme.local")
        .body(Body::empty())
        .unwrap();
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = text(&bytes);

    assert!(
        html.contains(r#"class="bc-slate bc-slate--alert""#),
        "slate is in alert state"
    );
    assert!(
        html.contains("COMPONENT") && html.contains("DOWN"),
        "slate lead mentions down components"
    );
    assert!(
        html.contains("OPEN INCIDENT"),
        "slate lead mentions open incidents"
    );
    assert!(html.contains("Gateway"), "down component name shown");
    assert!(
        html.contains(r##"href="#bc-incidents""##),
        "slate links to incidents"
    );

    // Resolve incident and mark Gateway as ok.
    // Use a timestamp that is strictly greater than now to ensure latest_result picks it up
    let now2 = now + 10;
    state
        .store
        .set_incident_status("inc_test", "resolved", now2, now2)
        .await;
    state.store.insert_result("Gateway", true, 0, now2).await;

    let req2 = Request::builder()
        .uri("/admin")
        .header("x-auth-subject", "u_admin")
        .header("x-auth-email", "ops@steadholme.local")
        .body(Body::empty())
        .unwrap();
    let resp2 = app(state.clone()).oneshot(req2).await.unwrap();
    let bytes2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    let html2 = text(&bytes2);

    assert!(
        html2.contains(r#"class="bc-slate bc-slate--quiet""#),
        "slate is in quiet state"
    );
    assert!(
        html2.contains("All quiet"),
        "quiet slate shows all-quiet message"
    );
    assert!(
        !html2.contains(r#"class="bc-slate bc-slate--alert""#),
        "alert class is absent"
    );
}

#[tokio::test]
async fn admin_open_incidents_render_before_resolved() {
    let state = build_dev_state().await;
    let now = crate::now_secs();
    state
        .store
        .insert_incident(&beacon::store::Incident {
            id: "inc_open".to_string(),
            title: "Open incident".to_string(),
            status: "investigating".to_string(),
            severity: "minor".to_string(),
            affected: "".to_string(),
            body: "Still investigating".to_string(),
            created_at: now - 100,
            updated_at: now - 100,
            resolved_at: 0,
        })
        .await;
    state
        .store
        .insert_incident(&beacon::store::Incident {
            id: "inc_resolved".to_string(),
            title: "Resolved incident".to_string(),
            status: "resolved".to_string(),
            severity: "major".to_string(),
            affected: "".to_string(),
            body: "Fixed".to_string(),
            created_at: now - 200,
            updated_at: now - 50,
            resolved_at: now - 50,
        })
        .await;

    let req = Request::builder()
        .uri("/admin")
        .header("x-auth-subject", "u_admin")
        .header("x-auth-email", "ops@steadholme.local")
        .body(Body::empty())
        .unwrap();
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = text(&bytes);

    let open_pos = html.find("Open incident").unwrap_or(usize::MAX);
    let resolved_pos = html.find("Resolved incident").unwrap_or(usize::MAX);
    assert!(
        open_pos < resolved_pos,
        "open incident appears before resolved incident"
    );
}

#[tokio::test]
async fn empty_incident_title_rejected() {
    let state = build_dev_state().await;
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            &[("x-auth-subject", "u_admin")],
            &format!("title=&status=investigating&body=x&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty title -> 400");
}

/// Served-CSS regression guard: the operator desk layer is a `.bc-desk`/`.bc-slate`-scoped
/// block tail-appended after the byte-stable publication baseline. `app_css()` assembles
/// unlayered Odyssey CSS before `service.css`, so wrapping service CSS in a named `@layer`
/// demotes every Beacon override below Odyssey. If cascade layers are ever introduced
/// legitimately, the same stylesheet must first declare the `@layer odyssey, beacon;`
/// order and this assertion must be updated alongside it.
#[tokio::test]
async fn service_css_stays_unlayered_and_keeps_shared_chrome() {
    let state = build_dev_state().await;
    let css_response = app(state.clone())
        .oneshot(get("/assets/beacon-20260907.css"))
        .await
        .unwrap();
    assert_eq!(css_response.status(), StatusCode::OK);
    assert_eq!(
        css_response.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        css_response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/css; charset=utf-8"
    );
    let css = text(
        &axum::body::to_bytes(css_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    );
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    // Cascade guard: the served stylesheet carries no cascade layer.
    assert!(!css.contains("@layer"), "service CSS stays unlayered");

    // Menu guard: the public updates menu stays native-<details> driven. The `open`
    // attribute lives on <details>, never on the <nav>, so an author rule keyed on
    // `.updates-pop__menu[open]` pins the menu shut.
    assert!(
        !css.contains(".updates-pop__menu[open]"),
        "no author rule may key the updates menu on [open]"
    );
    assert!(
        html.contains(r#"<details class="updates-pop">"#),
        "updates menu markup stays a native <details>"
    );
    assert!(
        css.contains(".updates-pop__btn::-webkit-details-marker"),
        "baseline marker-hiding rule for the native summary stays served"
    );

    // Chrome guard: the topbar chrome actually emitted by `userbox()` keeps its base
    // rules; the orphan `.userbox*` family stays gone.
    assert!(
        css.contains(".userchip {"),
        "userchip base rule stays served"
    );
    assert!(
        css.contains(".userchip__avatar {"),
        "userchip avatar base rule stays served"
    );
    assert!(css.contains(".allapps {"), "allapps base rule stays served");
    assert!(
        !css.contains(".userbox {"),
        "orphan .userbox rules stay gone"
    );

    // Order guard: the desk layer stays appended after the publication layer.
    let publication = css
        .find("Public status · Beacon publication layer")
        .expect("publication layer marker present");
    let desk = css
        .find("Operator desk layer")
        .expect("operator desk layer marker present");
    assert!(
        publication < desk,
        "desk layer stays appended after the publication layer"
    );
}

/// A two-group public catalog over the seeded dev checks — "Edge" carries Gateway and
/// "Accounts" carries Identity — so the section-focus rank rules are observable over HTTP.
/// CA stays an internal, unlisted probe exactly as in the dev catalog.
fn two_group_config() -> Config {
    let mut config = Config::dev();
    config.public_catalog = vec![
        PublicComponent {
            name: "Gateway".to_string(),
            group: "Edge".to_string(),
            checks: vec!["Gateway".to_string()],
        },
        PublicComponent {
            name: "Identity".to_string(),
            group: "Accounts".to_string(),
            checks: vec!["Identity".to_string()],
        },
    ];
    config
}

/// Ordered `(group name, rollup state)` pairs for every rendered catalog group.
fn group_states(html: &str) -> Vec<(String, String)> {
    let name_marker = r#"<h2 class="group__name">"#;
    html.split(r#"<section class="group group--"#)
        .skip(1)
        .map(|chunk| {
            let state_end = chunk.find('"').expect("group state");
            let start = chunk.find(name_marker).expect("group name") + name_marker.len();
            let end = chunk[start..].find('<').expect("group name close") + start;
            (chunk[start..end].to_string(), chunk[..state_end].to_string())
        })
        .collect()
}

#[tokio::test]
async fn all_operational_catalog_shows_no_state_words() {
    // Single-group dev catalog: nothing on the page repeats "Operational".
    let state = build_dev_state().await;
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", true, 10, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 60)
        .await;
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("All systems operational"));
    assert_eq!(
        group_states(&html),
        vec![("Core".to_string(), "operational".to_string())]
    );
    assert!(
        !html.contains(r#"class="group__state""#),
        "no group state word when every rollup is operational"
    );
    assert!(
        !html.contains(r#"class="tile__state""#),
        "no tile state word on an all-operational page"
    );
    assert_eq!(
        html.matches(r#"<details class="tile tile--operational" name="tile">"#).count(),
        2
    );

    // Multi-group catalog: configuration order, no affected line.
    let state = state_with(two_group_config()).await;
    state
        .store
        .insert_result("Gateway", true, 10, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 60)
        .await;
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert_eq!(
        group_states(&html),
        vec![
            ("Edge".to_string(), "operational".to_string()),
            ("Accounts".to_string(), "operational".to_string())
        ],
        "catalog order is configuration order"
    );
    assert!(
        !html.contains(r#"class="mast__affected""#),
        "an all-operational page never shows the affected line"
    );
}

#[tokio::test]
async fn group_rollups_follow_the_worst_member() {
    let state = state_with(two_group_config()).await;
    let now = now_secs();
    // Edge/Gateway: one failure inside the 24h window with a healthy latest probe -> degraded.
    state
        .store
        .insert_result("Gateway", false, 0, now - 900)
        .await;
    state
        .store
        .insert_result("Gateway", true, 10, now - 300)
        .await;
    // Accounts/Identity: latest probe failing -> down.
    state
        .store
        .insert_result("Identity", false, 0, now - 60)
        .await;

    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert_eq!(
        group_states(&html),
        vec![
            ("Edge".to_string(), "degraded".to_string()),
            ("Accounts".to_string(), "down".to_string())
        ]
    );
    assert!(html.contains(r#"<span class="group__state">Degraded</span>"#));
    assert!(html.contains(r#"<span class="group__state">Down</span>"#));
    assert!(html.contains(r#"<details class="tile tile--degraded" name="tile">"#));
    assert!(html.contains(r#"<details class="tile tile--down" name="tile">"#));
    assert!(html.contains(r#"<span class="tile__state">Degraded</span>"#));
    assert!(html.contains(r#"<span class="tile__state">Down</span>"#));
    assert!(html.contains("2 of 2 components affected"));
}

#[tokio::test]
async fn maintenance_masks_the_group_and_the_tile() {
    let state = state_with(two_group_config()).await;
    let now = now_secs();
    // An ongoing window masks Gateway, so the Edge rollup reads "maintenance".
    state
        .store
        .insert_maintenance(&Maintenance {
            id: "mw_rank".to_string(),
            title: "Edge relocation".to_string(),
            body: "Racking the edge pair".to_string(),
            starts_at: now - 600,
            ends_at: now + 3_600,
            affected: "Gateway".to_string(),
        })
        .await;
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert_eq!(
        group_states(&html),
        vec![
            ("Edge".to_string(), "maintenance".to_string()),
            ("Accounts".to_string(), "operational".to_string())
        ]
    );
    assert!(html.contains(r#"<details class="tile tile--maintenance" name="tile">"#));
    assert!(html.contains(r#"<span class="tile__state">Maintenance</span>"#));
    assert!(html.contains(r#"<article class="incident incident--maintenance is-live">"#));
    assert!(
        html.contains(r#"<span class="chip chip--maintenance">"#),
        "affected chips carry the component's masked state"
    );

    // Identity degrades (failure in the window, healthy latest probe): its own group reads
    // degraded while the masked group keeps reading maintenance.
    state
        .store
        .insert_result("Identity", false, 0, now - 900)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 300)
        .await;
    let (_, body) = call(&state, get("/status")).await;
    assert_eq!(
        group_states(&text(&body)),
        vec![
            ("Edge".to_string(), "maintenance".to_string()),
            ("Accounts".to_string(), "degraded".to_string())
        ]
    );
}

#[tokio::test]
async fn catalog_keeps_configuration_order_whatever_the_states() {
    let state = state_with(two_group_config()).await;
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", false, 0, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", false, 0, now - 60)
        .await;
    let (_, body) = call(&state, get("/status")).await;
    assert_eq!(
        group_states(&text(&body)),
        vec![
            ("Edge".to_string(), "down".to_string()),
            ("Accounts".to_string(), "down".to_string())
        ],
        "groups never reorder by state"
    );
}

#[tokio::test]
async fn affected_line_derives_from_component_states_without_incidents() {
    let state = build_dev_state().await;
    let now = now_secs();
    // A failing probe with NO incident on file still yields a truthful affected line.
    state
        .store
        .insert_result("Gateway", false, 0, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 60)
        .await;
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(
        html.contains(r#"<span class="mast__affected">"#),
        "affected line appears without any incident"
    );
    assert!(html.contains("1 of 2 components affected"));
    assert!(
        !html.contains("Active incidents"),
        "no incident ledger is involved"
    );
}

#[tokio::test]
async fn maintenance_masking_counts_toward_affected_without_an_incident() {
    let state = build_dev_state().await;
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", true, 10, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 60)
        .await;
    state
        .store
        .insert_maintenance(&Maintenance {
            id: "mw_mask".to_string(),
            title: "Identity re-key".to_string(),
            body: "Rotating signing keys".to_string(),
            starts_at: now - 300,
            ends_at: now + 1_800,
            affected: "Identity".to_string(),
        })
        .await;
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(html.contains(r#"<h2 class="ledger__title">Maintenance</h2>"#));
    assert!(
        html.contains("1 of 2 components affected"),
        "a maintenance-masked component is not operational right now"
    );
}

#[tokio::test]
async fn incident_names_never_inflate_the_affected_line() {
    let state = build_dev_state().await;
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", true, 10, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 60)
        .await;
    // An active incident naming two PUBLIC components that both measure operational.
    state
        .store
        .insert_incident(&Incident {
            id: "inc_pub".to_string(),
            title: "Gateway flapping".to_string(),
            status: "monitoring".to_string(),
            severity: "minor".to_string(),
            affected: "Gateway,Identity".to_string(),
            body: "Watching recovery".to_string(),
            created_at: now,
            updated_at: now,
            resolved_at: 0,
        })
        .await;
    // An active incident naming ONLY the internal CA probe (never public surface).
    state
        .store
        .insert_incident(&Incident {
            id: "inc_int".to_string(),
            title: "Authority backplane fault".to_string(),
            status: "investigating".to_string(),
            severity: "critical".to_string(),
            affected: "CA".to_string(),
            body: "Internal only".to_string(),
            created_at: now,
            updated_at: now,
            resolved_at: 0,
        })
        .await;

    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(
        html.contains("Gateway flapping"),
        "the public incident stays on the ledger"
    );
    assert!(
        !html.contains("Authority backplane fault"),
        "internal-only incidents stay fail-closed off the public page"
    );
    assert!(
        !html.contains(r#"class="mast__affected""#),
        "affected derives from component states, so incident name lists cannot inflate it"
    );
}

#[tokio::test]
async fn affected_line_localizes_via_cookie_and_accept_language() {
    let state = build_dev_state().await;
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", false, 0, now - 60)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 60)
        .await;

    // Default: English.
    let (_, body) = call(&state, get("/status")).await;
    assert!(text(&body).contains("1 of 2 components affected"));

    // The estate-wide `__Secure-lang` cookie steers the locale.
    let zh = Request::builder()
        .uri("/status")
        .header(header::COOKIE, "__Secure-lang=zh")
        .body(Body::empty())
        .unwrap();
    let (_, body) = call(&state, zh).await;
    assert!(text(&body).contains("2 个组件中有 1 个受影响"));

    // Accept-Language negotiation still works without a cookie.
    let ja = Request::builder()
        .uri("/status")
        .header(header::ACCEPT_LANGUAGE, "ja-JP,ja;q=0.9,en;q=0.3")
        .body(Body::empty())
        .unwrap();
    let (_, body) = call(&state, ja).await;
    assert!(text(&body).contains("2 個中 1 個のコンポーネントが影響を受けています"));

    // The retired `?locale` query parameter is inert: resolution stays cookie/header-only.
    let (status, body) = call(&state, get("/status?locale=zh")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        text(&body).contains("1 of 2 components affected"),
        "query strings never pick the locale"
    );
}

#[tokio::test]
async fn active_sits_before_maintenance_before_components_in_the_ia() {
    let state = build_dev_state().await;
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", true, 10, now - 60)
        .await;
    state
        .store
        .insert_incident(&Incident {
            id: "inc_ia".to_string(),
            title: "Elevated error rate".to_string(),
            status: "investigating".to_string(),
            severity: "major".to_string(),
            affected: "Gateway".to_string(),
            body: "Tracing the spike".to_string(),
            created_at: now,
            updated_at: now,
            resolved_at: 0,
        })
        .await;
    state
        .store
        .insert_maintenance(&Maintenance {
            id: "mw_ia".to_string(),
            title: "Scheduled upgrade".to_string(),
            body: "Brief restarts expected".to_string(),
            starts_at: now + 3_600,
            ends_at: now + 7_200,
            affected: "Gateway".to_string(),
        })
        .await;

    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    let hero = html.find(r#"<section class="mast "#).expect("masthead");
    let estate = html
        .find(r#"<section class="estate">"#)
        .expect("estate strip");
    let active = html
        .find(r#"<h2 class="ledger__title">Active incidents</h2>"#)
        .expect("active ledger");
    let maintenance = html
        .find(r#"<h2 class="ledger__title">Maintenance</h2>"#)
        .expect("maintenance section");
    let components = html
        .find(r#"<section class="catalog""#)
        .expect("catalog");
    let history = html
        .find(r#"<section class="history" id="past-incidents">"#)
        .expect("history section");
    assert!(hero < estate, "masthead leads");
    assert!(estate < active, "the estate strip precedes the active ledger");
    assert!(active < maintenance, "active incidents precede maintenance");
    assert!(maintenance < components, "maintenance precedes the catalog");
    assert!(components < history, "history closes the region");
    // The infra slot between components and history is covered by
    // `infra_block_renders_between_components_and_history` (no vitals poller runs here).
}

#[tokio::test]
async fn lang_switch_marks_only_the_active_locale_for_assistive_tech() {
    let state = build_dev_state().await;
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    assert!(
        html.contains(
            r#"<a class="langswitch__opt is-active" href="/_gw/lang?to=en" aria-current="true">"#
        ),
        "the active EN option is announced via aria-current"
    );
    assert!(
        html.contains(r#"<nav class="bc-lang" aria-label="Language">"#),
        "the EN switch keeps its English group label"
    );
    assert!(
        !html.contains(r#"to=zh" aria-current"#),
        "inactive zh option stays unmarked"
    );
    assert!(
        !html.contains(r#"to=ja" aria-current"#),
        "inactive ja option stays unmarked"
    );
}

/// The publication layer ships in the served CSS: scoped to `.page-status`, it re-points the
/// Odyssey palette names, carries both dark-theme hooks, keeps a plain-token fallback ahead of
/// every color-mix deepening on a soft background, drops every retired public rule, and sits
/// inside the publication layer ahead of the operator desk layer.
#[tokio::test]
async fn service_css_scopes_the_status_layer_to_the_public_page() {
    let state = build_dev_state().await;
    let response = app(state)
        .oneshot(get("/assets/beacon-20260907.css"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let css = text(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    for selector in [
        ".page-status {",
        r#"html[data-theme="dark"] .page-status {"#,
        r#"html:not([data-theme="light"]) .page-status {"#,
        ".page-status .mast {",
        ".page-status .tiles {",
        ".page-status .tile__detail {",
        ".page-status .stages {",
        ".page-status .cells {",
        ".page-status .heat__cells {",
        ".page-status .hrow {",
    ] {
        assert!(
            css.contains(selector),
            "served CSS keeps the public-scoped rule: {selector}"
        );
    }
    for pair in [
        ".page-status .tile--degraded .tile__state { color: var(--warn-ink); color: color-mix(in srgb, var(--warn-ink) 80%, var(--ink)); }",
        ".page-status .tile--down .tile__state { color: var(--down-ink); color: color-mix(in srgb, var(--down-ink) 80%, var(--ink)); }",
        ".page-status .tile--maintenance .tile__state { color: var(--info-ink); color: color-mix(in srgb, var(--info-ink) 80%, var(--ink)); }",
    ] {
        assert!(
            css.contains(pair),
            "fallback declaration precedes color-mix: {pair}"
        );
    }
    assert!(
        css.contains("--bg: var(--st-canvas);"),
        "Odyssey palette names are re-pointed under .page-status"
    );
    for residue in [
        ".status-hero",
        ".cgroup",
        ".crow {",
        ".bc-snapshot",
        ".status-stage",
        ".bc-infra",
        ".bars {",
        "status-window",
    ] {
        assert!(!css.contains(residue), "retired public rule stays gone: {residue}");
    }
    assert!(css.contains("@media (forced-colors: active)"));

    let publication = css
        .find("Public status · Beacon publication layer")
        .expect("publication marker");
    let scope = css.find(".page-status {").expect("scope rule");
    let desk = css.find("Operator desk layer").expect("desk marker");
    assert!(
        publication < scope && scope < desk,
        "the status layer sits inside the publication layer, before the desk layer"
    );
}
