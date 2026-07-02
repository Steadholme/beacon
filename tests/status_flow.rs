//! End-to-end HTTP contract test against the in-memory store (NO database).
//!
//! Drives the real Router in-process via `tower::oneshot` and asserts the public surface
//! renders without auth (status page, JSON API, RSS feed), the incident/maintenance
//! lifecycle through the CSRF-protected admin POSTs, the banner precedence
//! (critical incident > maintenance > all-ok), the maintenance pill masking, and the
//! 90-day uptime bars.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::{app, build_dev_state, now_secs, AppState};
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
    ("x-auth-email", "admin@holdfast.local"),
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
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK, "public status page is open");
    let html = text(&body);
    assert!(html.contains("HOLDFAST"), "brand present");
    assert!(html.contains("System status"), "page heading present");
    // The default seed components show up.
    assert!(html.contains("Gateway"));
    assert!(html.contains("Identity"));
    assert!(html.contains("CA"));
    // No data yet -> nominal banner, no active-incident section, no maintenance section.
    assert!(html.contains("All systems operational"));
    assert!(!html.contains("Active incidents"));
    assert!(html.contains("Past incidents"));
    assert!(html.contains("No incidents in the last 14 days"));
    // Inlined CSS (embedded design system).
    assert!(html.contains("--accent: #546be7"), "design tokens inlined");
    // 90-day bars render one span per day per component: 3 components x 90 days, all
    // unknown (no probe data yet).
    assert_eq!(html.matches(r#"class="bar bar-unknown""#).count(), 270);
    assert!(html.contains("no data"), "unknown days carry a no-data hover title");
    assert!(html.contains("90 days ago"), "bar legend present");
}

#[tokio::test]
async fn api_status_json_shape() {
    let state = build_dev_state().await;
    let (status, body) = call(&state, get("/api/status")).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["overall"], "operational");
    assert!(v["components"].as_array().unwrap().len() >= 3);
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    assert_eq!(gw["status"], "operational");
    assert_eq!(gw["uptime_24h"], 100.0);
    assert_eq!(gw["days"].as_array().unwrap().len(), 90, "90 daily bars");
    assert_eq!(gw["days"][0]["status"], "unknown", "no data yet -> unknown");
    assert!(v["incidents"].as_array().unwrap().is_empty());
    assert!(v["maintenances"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn daily_bars_reflect_probe_results() {
    let state = build_dev_state().await;
    let now = now_secs();
    // Day-aligned timestamps so the buckets are deterministic even right after midnight:
    // today 3 ok of 4 (75% -> down tint), yesterday all ok.
    let day_start = (now / 86_400) * 86_400;
    state.store.insert_result("Gateway", true, 10, day_start + 1).await;
    state.store.insert_result("Gateway", true, 10, day_start + 2).await;
    state.store.insert_result("Gateway", true, 10, day_start + 3).await;
    state.store.insert_result("Gateway", false, 10, day_start + 4).await;
    state.store.insert_result("Gateway", true, 10, day_start - 10).await;

    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    let days = gw["days"].as_array().unwrap();
    assert_eq!(days.len(), 90);
    assert_eq!(days[89]["status"], "down", "75% today -> down tint");
    assert_eq!(days[89]["uptime"], 75.0);
    assert_eq!(days[0]["status"], "unknown", "90 days ago -> no data");
    assert_eq!(days[0]["uptime"], Value::Null);

    // The HTML page renders the same bars with date + percent hover titles.
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(html.contains(r#"class="bar bar-down""#));
    assert!(html.contains("75.00%"), "hover title carries the day percent");
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
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}: no identity -> 401");
        let (status, _) = call(&state, post_admin(uri, OPERATOR, "csrf_token=WRONG")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}: CSRF mismatch -> 401");
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
    assert!(html.contains("sev-major"), "severity-tinted card");
    assert!(html.contains("Partial degradation"), "major incident -> degraded banner");

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
    assert!(html.contains("Fix deployed"), "latest update leads the card");
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
    assert!(html.contains("All systems operational"), "resolved -> banner clears");
    assert!(!html.contains("Active incidents"), "active section gone");
    assert!(html.contains("incident--resolved"), "past incident muted");
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
    assert!(html.contains("Scheduled maintenance underway"), "info banner");
    assert!(html.contains("DB upgrade"));
    assert!(html.contains("in progress"), "ongoing window pill");

    // An active critical incident outranks the maintenance banner.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/incidents",
            OPERATOR,
            &format!("title=Total+outage&severity=critical&body=x&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["overall"], "down", "critical incident beats maintenance");
    let (_, body) = call(&state, get("/status")).await;
    assert!(text(&body).contains("Service disruption"), "down banner wording");

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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
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
                "title=Cache+%26+CDN+%3Cdegraded%3E&severity=minor&body=5%25+errors+%26+retries\
                 &csrf_token={CSRF}"
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
        .header("x-auth-email", "ops@holdfast.local")
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
    assert!(set_cookie.starts_with("__Host-csrf="), "double-submit cookie set");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let html = text(&bytes);
    assert!(html.contains("Beacon admin"));
    assert!(html.contains("ops@holdfast.local"), "signed-in email shown");
    assert!(html.contains("/_gw/auth/logout"), "logout link present");
    assert!(html.contains("Post an incident"));
    assert!(html.contains("Schedule maintenance"));
    // New admin surfaces: incident templates, component groups, and the subscribers panel.
    assert!(html.contains(r#"data-tpl="#), "insert-template buttons present");
    assert!(html.contains("Component groups"), "group management card");
    assert!(html.contains(r#"action="/admin/groups""#), "create-group form");
    assert!(html.contains("Status subscribers"), "subscribers panel");
    assert!(html.contains(r#"name="csrf_token""#), "forms embed the CSRF token");
    // The rendered token matches the minted cookie (double-submit pair).
    let token = set_cookie
        .trim_start_matches("__Host-csrf=")
        .split(';')
        .next()
        .unwrap();
    assert!(html.contains(token), "hidden field carries the cookie token");
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
