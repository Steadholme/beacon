//! End-to-end component-groups + response-time-metrics contract test (in-memory store).
//!
//! Drives the real Router: database groups remain an internal operator concern while the
//! explicit public catalog owns stable public sections and rollups.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::{app, build_dev_state, now_secs, AppState};
use serde_json::Value;
use tower::ServiceExt;

const CSRF: &str = "tok_csrf_for_tests";
const OPERATOR: &[(&str, &str)] = &[
    ("x-auth-subject", "u_admin"),
    ("x-auth-email", "admin@steadholme.local"),
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

#[tokio::test]
async fn operational_catalog_renders_one_group_of_tiles() {
    let state = build_dev_state().await;
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    // The catalog-owned Core group renders as one section with two component tiles and no
    // member count.
    assert_eq!(
        html.matches(r#"<section class="group group--"#).count(),
        1,
        "the single catalog group renders one section"
    );
    assert!(html.contains(r#"<h2 class="group__name">Core</h2>"#));
    assert_eq!(html.matches(r#"<details class="tile tile--"#).count(), 2);
    assert!(!html.contains("2 components"));
    assert!(!html.contains(r#"class="group__state""#));
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
async fn groups_render_sections_with_the_worst_member_rollup() {
    let state = build_dev_state().await; // seeds Gateway, Identity, CA.

    // Create a raw database group and place the internal CA probe in it. This must not affect
    // or leak into the catalog-owned public grouping.
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/groups",
            &format!("name=Secret+operators&position=0&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let groups = state.store.list_component_groups().await;
    assert_eq!(groups.len(), 1);
    let secret_id = groups[0].id.clone();
    let (status, _) = call(
        &state,
        post_admin(
            "/admin/groups/assign",
            &format!("check_name=CA&group_id={secret_id}&csrf_token={CSRF}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Gateway is down (latest failing) while Identity is up -> group rollup is worst = down.
    let now = now_secs();
    state
        .store
        .insert_result("Gateway", false, 40, now - 5)
        .await;
    state
        .store
        .insert_result("Identity", true, 12, now - 5)
        .await;

    // Public page: the default catalog's Core group rolls up Gateway + Identity. Neither the
    // raw group nor CA crosses the public projection boundary.
    let (status, body) = call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(
        html.contains(r#"<section class="group group--down">"#),
        "catalog-owned Core section carries the worst-member rollup"
    );
    assert!(html.contains(r#"<h2 class="group__name">Core</h2>"#));
    assert!(html.contains(r#"<span class="group__state">Down</span>"#));
    assert!(!html.contains("Secret operators"));
    assert!(!html.contains(r#"<span class="tile__name">CA</span>"#));
    assert!(html.contains(r#"<details class="tile tile--down" name="tile">"#));
    assert!(html.contains(r#"<details class="tile tile--operational" name="tile">"#));
    assert_eq!(
        html.matches(r#"<span class="tile__state">Down</span>"#).count(),
        1,
        "only the down tile carries a state word"
    );

    // JSON API: groups array present with the rollup, components carry their group_id.
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let grp = v["groups"].as_array().unwrap();
    assert_eq!(grp.len(), 1);
    assert_eq!(grp[0]["name"], "Core");
    assert_eq!(grp[0]["status"], "down", "worst-member rollup");
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    assert_eq!(gw["group_id"], "public-group-1");
    assert!(v["components"]
        .as_array()
        .unwrap()
        .iter()
        .all(|component| component["name"] != "CA"));
}

#[tokio::test]
async fn latency_aggregate_drives_sparkline() {
    let state = build_dev_state().await;
    let now = now_secs();

    // Two probes for Gateway in the current hour (avg 20ms) and one an hour earlier (30ms).
    let hour = 3_600;
    let this_hour = (now / hour) * hour;
    state
        .store
        .insert_result("Gateway", true, 10, this_hour + 10)
        .await;
    state
        .store
        .insert_result("Gateway", true, 30, this_hour + 20)
        .await;
    state
        .store
        .insert_result("Gateway", true, 30, this_hour - hour + 10)
        .await;

    // JSON: the aggregate yields a window average + a per-hour points series (oldest first).
    let (_, body) = call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let gw = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Gateway")
        .unwrap();
    // (10 + 30 + 30) / 3 = 23.33 -> 23.
    assert_eq!(gw["latency_avg_ms"], 23, "window-mean latency");
    let points = gw["latency_points"].as_array().unwrap();
    assert_eq!(points.len(), 24, "24 hourly buckets");
    assert_eq!(points[23], 20, "current hour mean (10,30) -> 20");
    assert_eq!(points[22], 30, "previous hour mean");
    assert!(points[0].is_null(), "oldest hour has no data");

    // HTML: the tile face shows the window average; the tile detail carries the hourly spark.
    let (_, body) = call(&state, get("/status")).await;
    let html = text(&body);
    assert!(html.contains(
        r#"<span class="tile__lat" title="24h average · latest 30 ms">~23 ms</span>"#
    ));
    assert!(
        html.contains(r#"<svg class="spark" viewBox="0 0 200 40""#),
        "the 24-hour latency spark renders inside the tile detail"
    );
    assert!(html.contains(r#"<b>~23 ms</b><small>24h average · latest 30 ms</small>"#));
}
