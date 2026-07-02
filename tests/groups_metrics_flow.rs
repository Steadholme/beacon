//! End-to-end component-groups + response-time-metrics contract test (in-memory store).
//!
//! Drives the real Router: an operator creates groups and assigns components (CSRF-protected),
//! the public page then renders grouped sections with a rolled-up status pill (worst member
//! wins) plus a "Other" section for ungrouped components, and each component carries a
//! response-time sparkline derived from the `check_results` latency aggregate.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::{app, build_dev_state, now_secs, AppState};
use serde_json::Value;
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

fn post_admin(uri: &str, form: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("__Host-csrf={CSRF}"))
        .header("x-auth-subject", OPERATOR[0].1)
        .header("x-auth-email", OPERATOR[1].1)
        .body(Body::from(form.to_string()))
        .unwrap()
}

#[tokio::test]
async fn groups_render_sections_with_rollup_pill() {
    let state = build_dev_state().await; // seeds Gateway, Identity, CA.

    // Create a group.
    let (status, _) = call(&state, post_admin("/admin/groups", &format!("name=Apps&position=0&csrf_token={CSRF}"))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let groups = state.store.list_component_groups().await;
    assert_eq!(groups.len(), 1);
    let apps_id = groups[0].id.clone();

    // Assign Gateway + Identity to "Apps"; leave CA ungrouped.
    for comp in ["Gateway", "Identity"] {
        let (status, _) = call(
            &state,
            post_admin(
                "/admin/groups/assign",
                &format!("check_name={comp}&group_id={apps_id}&csrf_token={CSRF}"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }

    // Gateway is down (latest failing) while Identity is up -> group rollup is worst = down.
    let now = now_secs();
    state.store.insert_result("Gateway", false, 40, now - 5).await;
    state.store.insert_result("Identity", true, 12, now - 5).await;

    // Public page: grouped section "Apps" with a down rollup pill + an "Other" section for CA.
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains(r#"class="group__name">Apps"#), "Apps section header");
    assert!(html.contains(r#"class="group__name">Other"#), "ungrouped -> Other section");
    // The Apps rollup pill is Down (worst of a down + an up member).
    assert!(html.contains(r#"<span class="pill pill-down">Down</span>"#), "group rollup pill down");

    // JSON API: groups array present with the rollup, components carry their group_id.
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let grp = v["groups"].as_array().unwrap();
    assert_eq!(grp.len(), 1);
    assert_eq!(grp[0]["name"], "Apps");
    assert_eq!(grp[0]["status"], "down", "worst-member rollup");
    let gw = v["components"].as_array().unwrap().iter().find(|c| c["name"] == "Gateway").unwrap();
    assert_eq!(gw["group_id"], apps_id);
    let ca = v["components"].as_array().unwrap().iter().find(|c| c["name"] == "CA").unwrap();
    assert!(ca["group_id"].is_null(), "CA stays ungrouped");
}

#[tokio::test]
async fn latency_aggregate_drives_sparkline() {
    let state = build_dev_state().await;
    let now = now_secs();

    // Two probes for Gateway in the current hour (avg 20ms) and one an hour earlier (30ms).
    let hour = 3_600;
    let this_hour = (now / hour) * hour;
    state.store.insert_result("Gateway", true, 10, this_hour + 10).await;
    state.store.insert_result("Gateway", true, 30, this_hour + 20).await;
    state.store.insert_result("Gateway", true, 30, this_hour - hour + 10).await;

    // JSON: the aggregate yields a window average + a per-hour points series (oldest first).
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let gw = v["components"].as_array().unwrap().iter().find(|c| c["name"] == "Gateway").unwrap();
    // (10 + 30 + 30) / 3 = 23.33 -> 23.
    assert_eq!(gw["latency_avg_ms"], 23, "window-mean latency");
    let points = gw["latency_points"].as_array().unwrap();
    assert_eq!(points.len(), 24, "24 hourly buckets");
    assert_eq!(points[23], 20, "current hour mean (10,30) -> 20");
    assert_eq!(points[22], 30, "previous hour mean");
    assert!(points[0].is_null(), "oldest hour has no data");

    // HTML: a pure-SVG sparkline renders for a component with >= 2 data points.
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(html.contains(r#"<svg class="spark""#), "sparkline SVG present");
    assert!(html.contains("<polyline"), "trend polyline present");
    assert!(html.contains("now 30 ms"), "current latency figure");
    assert!(html.contains("avg 23 ms"), "average latency figure");
}
