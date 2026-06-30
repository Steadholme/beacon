//! End-to-end HTTP contract test against the in-memory store (NO database).
//!
//! Drives the real Router in-process via `tower::oneshot` and asserts the public surface
//! renders without auth, the JSON API shape, and that a posted incident shows on the public
//! status page — plus that the admin POST requires a gateway-injected identity.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::{app, build_dev_state, AppState};
use serde_json::Value;
use tower::ServiceExt;

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
    // No data yet -> nominal banner.
    assert!(html.contains("All systems operational"));
    // Inlined CSS (embedded design system).
    assert!(html.contains("--accent: #546be7"), "design tokens inlined");
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
    assert!(v["incidents"].as_array().unwrap().is_empty());
}

fn post_incident(headers: &[(&str, &str)], form: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/admin/incidents")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::from(form.to_string())).unwrap()
}

#[tokio::test]
async fn admin_post_requires_gateway_identity() {
    let state = build_dev_state().await;
    // No X-Auth-* headers -> 401.
    let (status, _) = call(
        &state,
        post_incident(&[], "title=Outage&status=investigating&body=down"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no SSO identity -> 401");
}

#[tokio::test]
async fn incident_create_shows_on_public_status() {
    let state = build_dev_state().await;

    // Operator (gateway-injected identity) posts an incident.
    let (status, _) = call(
        &state,
        post_incident(
            &[
                ("x-auth-subject", "u_admin"),
                ("x-auth-email", "admin@holdfast.local"),
            ],
            "title=Identity+provider+degraded&status=investigating&body=Investigating+elevated+errors",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "post/redirect/get -> 303");

    // It now appears on the PUBLIC status page (no auth).
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(
        html.contains("Identity provider degraded"),
        "incident title shows on public page"
    );
    assert!(html.contains("investigating"), "incident status shows");

    // And in the JSON API.
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let incidents = v["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0]["title"], "Identity provider degraded");
}

#[tokio::test]
async fn admin_page_renders_with_email() {
    let state = build_dev_state().await;
    let req = Request::builder()
        .uri("/admin")
        .header("x-auth-email", "ops@holdfast.local")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("Beacon admin"));
    assert!(html.contains("ops@holdfast.local"), "signed-in email shown");
    assert!(html.contains("/_gw/auth/logout"), "logout link present");
    assert!(html.contains("Post an incident"));
}

#[tokio::test]
async fn empty_incident_title_rejected() {
    let state = build_dev_state().await;
    let (status, _) = call(
        &state,
        post_incident(
            &[("x-auth-subject", "u_admin")],
            "title=&status=investigating&body=x",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty title -> 400");
}
