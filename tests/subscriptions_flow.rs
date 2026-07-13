//! End-to-end PUBLIC subscription + webhook fan-out contract test (in-memory store).
//!
//! Exercises the feature-gated double-opt-in lifecycle and the SSRF-safe registration boundary.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::config::Config;
use beacon::{app, build_dev_state, state_with, AppState};
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

fn post_form(uri: &str, form: &str) -> Request<Body> {
    let b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    b.body(Body::from(form.to_string())).unwrap()
}

#[tokio::test]
async fn explicitly_enabled_webhook_subscribe_confirm_and_unsubscribe() {
    let mut config = Config::dev();
    config.public_webhooks_enabled = true;
    let state = state_with(config).await;
    // A literal public address makes registration validation deterministic and does not require
    // an outbound connection (delivery is independently covered by notify unit tests).
    let target = "https://1.1.1.1/hook";

    // Subscribe (public, no CSRF) -> the endpoint is registered UNCONFIRMED, no delivery yet.
    let (status, body) = call(
        &state,
        post_form("/subscriptions", &format!("target={target}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = text(&body);
    assert!(page.contains("Confirm your subscription"));
    assert!(
        page.contains("/subscriptions/confirm?token=sub_"),
        "confirm link shown"
    );

    let subs = state.store.list_subscribers().await;
    assert_eq!(subs.len(), 1);
    let sub = subs[0].clone();
    assert!(!sub.confirmed, "starts unconfirmed (double opt-in)");
    assert_eq!(sub.kind, "webhook");

    // Confirm via the capability token (the subscriber id).
    let (status, cbody) = call(
        &state,
        get(&format!("/subscriptions/confirm?token={}", sub.id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&cbody).contains("Subscription confirmed"));
    assert!(
        state.store.get_subscriber(&sub.id).await.unwrap().confirmed,
        "confirmed in the store"
    );

    // Unsubscribe by token -> the subscriber is gone.
    let (status, ubody) = call(
        &state,
        get(&format!("/subscriptions/unsubscribe?token={}", sub.id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&ubody).contains("Unsubscribed"));
    assert!(
        state.store.get_subscriber(&sub.id).await.is_none(),
        "removed"
    );
}

#[tokio::test]
async fn webhooks_default_off_and_enabled_mode_rejects_non_public_targets() {
    let state = build_dev_state().await;

    // RSS + JSON remain public, but no anonymous webhook form is advertised by default.
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("Subscribe to updates"));
    assert!(!html.contains(r#"action="/subscriptions""#));
    assert!(html.contains("RSS feed"));
    assert!(html.contains("JSON API"));

    let response = app(state.clone())
        .oneshot(post_form("/subscriptions", "target=https://1.1.1.1/hook"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert!(state.store.list_subscribers().await.is_empty());

    let mut config = Config::dev();
    config.public_webhooks_enabled = true;
    let enabled = state_with(config).await;

    // Syntax and SSRF policy are both enforced before a subscriber is stored.
    let (status, body) = call(&enabled, post_form("/subscriptions", "target=ftp://nope")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&body).contains("Invalid webhook URL"));
    let (status, body) = call(
        &enabled,
        post_form("/subscriptions", "target=http://127.0.0.1:9000/hook"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&body).contains("Webhook target unavailable"));
    assert!(enabled.store.list_subscribers().await.is_empty());

    // An unknown unsubscribe token is idempotent (neutral notice, no error).
    let (status, body) = call(&enabled, get("/subscriptions/unsubscribe?token=sub_nope")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&body).contains("Unsubscribed"));
}
