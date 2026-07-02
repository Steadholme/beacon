//! End-to-end PUBLIC subscription + webhook fan-out contract test (in-memory store).
//!
//! Exercises the full double-opt-in lifecycle against the real Router: subscribe (unconfirmed)
//! -> confirm via capability token -> a signed webhook delivery on a new incident (captured on
//! a throwaway loopback listener and HMAC-verified) -> unsubscribe by token stops delivery.

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::{app, build_dev_state, AppState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

const CSRF: &str = "tok_csrf_for_tests";
const OPERATOR: &[(&str, &str)] = &[
    ("x-auth-subject", "u_admin"),
    ("x-auth-email", "admin@holdfast.local"),
];

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

fn post_form(uri: &str, headers: &[(&str, &str)], with_csrf: bool, form: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if with_csrf {
        b = b.header(header::COOKIE, format!("__Host-csrf={CSRF}"));
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::from(form.to_string())).unwrap()
}

/// Read one HTTP request (headers + Content-Length body) from an accepted connection, reply
/// with a bare `200`, and return `(raw_headers, body)`.
async fn read_request(mut stream: TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    // Read until we have the full header block.
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut tmp).await.unwrap();
        if n == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_len = headers
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:").or_else(|| l.strip_prefix("content-length:")))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // Read the remaining body bytes.
    while buf.len() < header_end + content_len {
        let n = stream.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    let _ = stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .await;
    let _ = stream.flush().await;
    (headers, body)
}

fn sig_header(raw_headers: &str) -> String {
    raw_headers
        .lines()
        .find_map(|l| l.strip_prefix("X-Beacon-Signature:").or_else(|| l.strip_prefix("x-beacon-signature:")))
        .map(|v| v.trim().to_string())
        .expect("signature header present")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_subscribe_confirm_and_signed_delivery() {
    let state = build_dev_state().await;

    // A throwaway loopback listener stands in for the subscriber's webhook endpoint.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = format!("http://127.0.0.1:{port}/hook");

    // Subscribe (public, no CSRF) -> the endpoint is registered UNCONFIRMED, no delivery yet.
    let (status, body) = call(
        &state,
        post_form("/subscriptions", &[], false, &format!("target={target}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = text(&body);
    assert!(page.contains("Confirm your subscription"));
    assert!(page.contains("/subscriptions/confirm?token=sub_"), "confirm link shown");

    let subs = state.store.list_subscribers().await;
    assert_eq!(subs.len(), 1);
    let sub = subs[0].clone();
    assert!(!sub.confirmed, "starts unconfirmed (double opt-in)");
    assert_eq!(sub.kind, "webhook");

    // An unconfirmed subscriber gets NO delivery: post an incident, expect no connection.
    let (status, _) = call(
        &state,
        post_form(
            "/admin/incidents",
            OPERATOR,
            true,
            &format!("title=Early&severity=minor&body=x&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        tokio::time::timeout(Duration::from_millis(400), listener.accept())
            .await
            .is_err(),
        "no delivery to an unconfirmed subscriber"
    );

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

    // Post an incident -> a signed webhook is delivered to the confirmed endpoint.
    let (status, _) = call(
        &state,
        post_form(
            "/admin/incidents",
            OPERATOR,
            true,
            &format!("title=Gateway+down&severity=critical&affected=Gateway&body=hard+down&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (conn, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("webhook delivered within timeout")
        .unwrap();
    let (headers, wbody) = read_request(conn).await;

    // The signature header verifies against the subscriber's secret over the exact body bytes.
    let sig = sig_header(&headers);
    let expected = format!("sha256={}", beacon::notify::sign(&sub.secret, &wbody));
    assert_eq!(sig, expected, "HMAC-SHA256 signature matches");
    assert!(headers.contains("Content-Type: application/json"));
    assert!(headers.contains("X-Beacon-Event: incident.opened"));

    // The payload is the opened incident.
    let v: serde_json::Value = serde_json::from_str(&wbody).unwrap();
    assert_eq!(v["event"], "incident.opened");
    assert_eq!(v["incident"]["title"], "Gateway down");
    assert_eq!(v["incident"]["severity"], "critical");

    // Unsubscribe by token -> the subscriber is gone and no further deliveries happen.
    let (status, ubody) = call(
        &state,
        get(&format!("/subscriptions/unsubscribe?token={}", sub.id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&ubody).contains("Unsubscribed"));
    assert!(state.store.get_subscriber(&sub.id).await.is_none(), "removed");

    let (status, _) = call(
        &state,
        post_form(
            "/admin/incidents",
            OPERATOR,
            true,
            &format!("title=After&severity=minor&body=y&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        tokio::time::timeout(Duration::from_millis(400), listener.accept())
            .await
            .is_err(),
        "no delivery after unsubscribe"
    );
}

#[tokio::test]
async fn subscribe_form_public_and_rejects_bad_url() {
    let state = build_dev_state().await;

    // The subscribe form is rendered on the public status page.
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("Subscribe to updates"));
    assert!(html.contains(r#"action="/subscriptions""#));

    // A non-http(s) target is rejected with a notice and creates no subscriber.
    let (status, body) = call(
        &state,
        post_form("/subscriptions", &[], false, "target=ftp://nope"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&body).contains("Invalid webhook URL"));
    assert!(state.store.list_subscribers().await.is_empty(), "no subscriber created");

    // An unknown unsubscribe token is idempotent (neutral notice, no error).
    let (status, body) = call(&state, get("/subscriptions/unsubscribe?token=sub_nope")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text(&body).contains("Unsubscribed"));
}
