//! i18n SHELL pilot contract test (in-memory store).
//!
//! The public subscription notices render through the Odyssey shell with the locale resolved
//! from the request (`__Secure-lang` cookie first, then `Accept-Language`). This drives the
//! real Router and asserts the localized chrome end to end: `<html lang="…">` plus the
//! Odyssey chrome strings — beacon-specific copy stays English (chrome-only pilot).

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
async fn shell_chrome_localizes_from_lang_cookie_and_accept_language() {
    let state = build_dev_state().await;
    let uri = "/subscriptions/unsubscribe?token=sub_nonexistent";

    // A `__Secure-lang=zh` cookie wins: Simplified-Chinese lang tag + localized chrome strings.
    let req = Request::builder()
        .uri(uri)
        .header(header::COOKIE, "__Secure-lang=zh")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let page = text(&body);
    assert!(
        page.contains("<html lang=\"zh-Hans\">"),
        "zh cookie must set <html lang=\"zh-Hans\">, got head: {}",
        &page[..page.len().min(300)]
    );
    assert!(
        page.contains("账户") || page.contains("退出登录"),
        "zh chrome must carry a localized Odyssey chrome string"
    );

    // No cookie: `Accept-Language: ja` negotiates the Japanese shell.
    let req = Request::builder()
        .uri(uri)
        .header(header::ACCEPT_LANGUAGE, "ja")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let page = text(&body);
    assert!(
        page.contains("<html lang=\"ja\">"),
        "Accept-Language: ja must set <html lang=\"ja\">, got head: {}",
        &page[..page.len().min(300)]
    );
}
