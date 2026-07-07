use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::{app, build_dev_state, AppState};
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[tokio::test]
async fn public_status_resolves_locale_and_stays_zero_js() {
    let state = build_dev_state().await;
    let req = Request::builder()
        .uri("/status")
        .header(header::COOKIE, "__Secure-lang=zh")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains(r#"<html lang="zh-Hans">"#));
    assert!(html.contains("系统状态"));
    assert!(html.contains(r#"/_gw/lang?to=en"#));
    assert!(!html.contains("<script"), "public status remains zero-JS");
}
