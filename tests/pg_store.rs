//! PostgreSQL `Store` integration test.
//!
//! Runs ONLY when `TEST_DATABASE_URL` is set (it needs an external Postgres). When unset the
//! test prints a note and returns early — it never fails the default `cargo test` run, which
//! stays database-free. Spin up a throwaway Postgres and run:
//!
//! ```text
//! TEST_DATABASE_URL=postgres://postgres:pw@127.0.0.1:55441/beacon \
//!   cargo test --test pg_store -- --nocapture
//! ```
//!
//! Requires a multi-threaded runtime: the synchronous `Store` trait bridges to async sqlx
//! via `block_in_place`, which only works on the multi_thread scheduler.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::store::{Check, PgStore, Store};
use beacon::{app, build_dev_state, now_secs, AppState};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_store_full_integration() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "NOTE: TEST_DATABASE_URL not set — skipping Postgres integration test \
             (needs external Postgres). This is expected for the default test run."
        );
        return;
    };

    // --- connect / migrate (idempotent: run twice) -------------------------
    let pg = PgStore::connect(&url).await.expect("connect TEST_DATABASE_URL");
    pg.migrate().await.expect("migrate");
    pg.migrate().await.expect("migrate is idempotent");
    let pg = Arc::new(pg);

    // --- direct Store-trait round-trip (sync over async sqlx) --------------
    // Seed checks (ON CONFLICT DO NOTHING — re-insert is a no-op, not an error).
    let check = Check {
        name: "PgGateway".to_string(),
        kind: "http".to_string(),
        target: "https://id.w33d.xyz/healthz".to_string(),
        enabled: true,
    };
    pg.insert_check(&check);
    pg.insert_check(&check);
    assert!(pg.count_checks() >= 1);
    assert!(pg.list_checks().iter().any(|c| c.name == "PgGateway"));

    // Record results spanning ok/down and compute uptime.
    let now = now_secs();
    pg.insert_result("PgGateway", true, 12, now - 30);
    pg.insert_result("PgGateway", true, 12, now - 30); // dup (name,ts) -> no-op
    pg.insert_result("PgGateway", false, 30, now - 20);
    pg.insert_result("PgGateway", true, 15, now - 10);

    let latest = pg.latest_result("PgGateway").expect("latest exists");
    assert_eq!(latest.ts, now - 10);
    assert!(latest.ok);

    let (total, up) = pg.uptime_counts("PgGateway", now - 86_400);
    assert_eq!(total, 3, "dup (name,ts) not double-counted");
    assert_eq!(up, 2);

    // Incident persistence + ordering (newest first).
    pg.insert_incident(&beacon::store::Incident {
        id: "inc_a".to_string(),
        title: "older".to_string(),
        status: "resolved".to_string(),
        body: "b".to_string(),
        created_at: now - 100,
        updated_at: now - 100,
    });
    pg.insert_incident(&beacon::store::Incident {
        id: "inc_b".to_string(),
        title: "newer".to_string(),
        status: "investigating".to_string(),
        body: "b2".to_string(),
        created_at: now,
        updated_at: now,
    });
    let incidents = pg.list_incidents();
    assert_eq!(incidents.len(), 2);
    assert_eq!(incidents[0].id, "inc_b", "newest first");

    // --- full HTTP flow through the PG-backed app --------------------------
    let mut state: AppState = build_dev_state();
    state.store = pg.clone();

    // Public status renders against Postgres-backed data.
    let (status, body) = raw_call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("PgGateway"));

    // Post an incident via the SSO admin path, then it shows on the public JSON.
    let (status, _) = raw_call(
        &state,
        Request::builder()
            .method("POST")
            .uri("/admin/incidents")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-auth-subject", "u_admin")
            .header("x-auth-email", "admin@holdfast.local")
            .body(Body::from("title=PG+incident&status=monitoring&body=via+pg"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (_, body) = raw_call(&state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["title"] == "PG incident"));

    println!(
        "PG STORE INTEGRATION OK: migrate (idempotent) + checks/results/uptime/incidents \
         round-trip + full status/admin HTTP flow against real Postgres"
    );
}

async fn raw_call(state: &AppState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
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
