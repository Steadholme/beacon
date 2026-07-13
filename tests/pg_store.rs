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
//! The `Store` trait is async: each method `.await`s sqlx natively (no `block_in_place`), so
//! it runs on any Tokio scheduler — this test stays on `multi_thread` for parallel queries.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::config::PublicComponent;
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
    let pg = PgStore::connect(&url)
        .await
        .expect("connect TEST_DATABASE_URL");
    pg.migrate().await.expect("migrate");
    pg.migrate().await.expect("migrate is idempotent");
    let pg = Arc::new(pg);

    // --- direct Store-trait round-trip (sync over async sqlx) --------------
    // Seed checks (ON CONFLICT DO NOTHING — re-insert is a no-op, not an error).
    let check = Check {
        name: "PgGateway".to_string(),
        kind: "http".to_string(),
        target: "https://sso.w33d.xyz/healthz".to_string(),
        enabled: true,
        group_id: None,
    };
    pg.insert_check(&check).await;
    pg.insert_check(&check).await;
    assert!(pg.count_checks().await >= 1);
    assert!(pg.list_checks().await.iter().any(|c| c.name == "PgGateway"));

    // Record results spanning ok/down and compute uptime.
    let now = now_secs();
    pg.insert_result("PgGateway", true, 12, now - 30).await;
    pg.insert_result("PgGateway", true, 12, now - 30).await; // dup (name,ts) -> no-op
    pg.insert_result("PgGateway", false, 30, now - 20).await;
    pg.insert_result("PgGateway", true, 15, now - 10).await;

    let latest = pg.latest_result("PgGateway").await.expect("latest exists");
    assert_eq!(latest.ts, now - 10);
    assert!(latest.ok);

    let (total, up) = pg.uptime_counts("PgGateway", now - 86_400).await;
    assert_eq!(total, 3, "dup (name,ts) not double-counted");
    assert_eq!(up, 2);

    // The daily bar aggregate groups the same rows by (name, epoch-day) in ONE query.
    let daily = pg.daily_uptime(now - 86_400).await;
    let today = daily
        .iter()
        .find(|d| d.name == "PgGateway" && d.day == (now - 30) / 86_400)
        .expect("today's bucket present");
    assert!(
        today.total >= 3 && today.up >= 2,
        "bucket aggregates results"
    );

    // Incident persistence + ordering (newest first) with severity/affected/resolved_at.
    pg.insert_incident(&beacon::store::Incident {
        id: "inc_a".to_string(),
        title: "older".to_string(),
        status: "resolved".to_string(),
        severity: "minor".to_string(),
        affected: "PgGateway".to_string(),
        body: "b".to_string(),
        created_at: now - 100,
        updated_at: now - 100,
        resolved_at: now - 50,
    })
    .await;
    pg.insert_incident(&beacon::store::Incident {
        id: "inc_b".to_string(),
        title: "newer".to_string(),
        status: "investigating".to_string(),
        severity: "critical".to_string(),
        affected: String::new(),
        body: "b2".to_string(),
        created_at: now,
        updated_at: now,
        resolved_at: 0,
    })
    .await;
    let incidents = pg.list_incidents().await;
    assert_eq!(incidents.len(), 2);
    assert_eq!(incidents[0].id, "inc_b", "newest first");
    assert_eq!(incidents[0].severity, "critical");
    let got = pg.get_incident("inc_a").await.expect("get by id");
    assert_eq!(got.affected, "PgGateway");
    assert_eq!(got.resolved_at, now - 50);

    // Timeline updates + status transition round-trip.
    pg.insert_incident_update(&beacon::store::IncidentUpdate {
        id: "upd_1".to_string(),
        incident_id: "inc_b".to_string(),
        status: "monitoring".to_string(),
        body: "fix deployed".to_string(),
        created_at: now + 1,
    })
    .await;
    pg.set_incident_status("inc_b", "monitoring", now + 1, 0)
        .await;
    let updates = pg.list_incident_updates().await;
    assert!(updates
        .iter()
        .any(|u| u.id == "upd_1" && u.incident_id == "inc_b"));
    assert_eq!(pg.get_incident("inc_b").await.unwrap().status, "monitoring");
    pg.set_incident_status("inc_b", "resolved", now + 2, now + 2)
        .await;
    assert_eq!(pg.get_incident("inc_b").await.unwrap().resolved_at, now + 2);

    // Maintenance windows round-trip, ordered by starts_at.
    pg.insert_maintenance(&beacon::store::Maintenance {
        id: "mw_1".to_string(),
        title: "db upgrade".to_string(),
        body: "planned".to_string(),
        starts_at: now - 60,
        ends_at: now + 3_600,
        affected: "PgGateway".to_string(),
    })
    .await;
    let maintenances = pg.list_maintenances().await;
    assert!(maintenances
        .iter()
        .any(|m| m.id == "mw_1" && m.affected == "PgGateway"));

    // --- full HTTP flow through the PG-backed app --------------------------
    let mut state: AppState = build_dev_state().await;
    state.store = pg.clone();
    let mut config = (*state.config).clone();
    config.public_catalog = vec![PublicComponent {
        name: "PgGateway".to_string(),
        group: "Integration".to_string(),
        checks: vec!["PgGateway".to_string()],
    }];
    state.config = Arc::new(config);

    // Public status renders against Postgres-backed data.
    let (status, body) = raw_call(&state, get("/status")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("PgGateway"));

    // Post an incident via the SSO admin path (identity + double-submit CSRF), then it
    // shows on the public JSON.
    let (status, _) = raw_call(
        &state,
        Request::builder()
            .method("POST")
            .uri("/admin/incidents")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, "__Host-csrf=tok_csrf_for_tests")
            .header("x-auth-subject", "u_admin")
            .header("x-auth-email", "admin@holdfast.local")
            .body(Body::from(
                "title=PG+incident&status=monitoring&severity=major&affected=PgGateway&body=via+pg\
                 &csrf_token=tok_csrf_for_tests",
            ))
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
        "PG STORE INTEGRATION OK: migrate (idempotent) + checks/results/uptime/daily-bars/\
         incidents/updates/maintenances round-trip + full status/admin HTTP flow against \
         real Postgres"
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
