//! Prober + uptime-math integration test (NO database).
//!
//! Spins up a tiny local HTTP server (an "up" 200 route and a "down" 500 route), then drives
//! the real probe + monitor sweep against it and asserts the recorded `(ok, latency_ms)` and
//! the rolling-uptime math.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use beacon::config::Config;
use beacon::model::{build_status, uptime_pct};
use beacon::probe::probe;
use beacon::store::Check;
use beacon::{monitor, now_secs, state_with};

/// Start a local test server on an ephemeral port; returns its address.
async fn spawn_test_server() -> SocketAddr {
    let app = Router::new()
        .route("/", get(|| async { "ok" }))
        .route("/down", get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "bad") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Give the listener a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_probe_up_and_down() {
    let addr = spawn_test_server().await;
    let timeout = Duration::from_secs(3);

    // UP: 200 -> ok, with a real (>=0) latency.
    let up = probe("http", &format!("http://{addr}/"), timeout).await;
    assert!(up.ok, "200 should be ok");
    assert!(up.latency_ms >= 0, "latency recorded");

    // DOWN: 500 -> not ok.
    let down = probe("http", &format!("http://{addr}/down"), timeout).await;
    assert!(!down.ok, "500 should be down");

    // DOWN: connection refused on a closed port -> not ok.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        a
    };
    let refused = probe("http", &format!("http://{closed}/"), timeout).await;
    assert!(!refused.ok, "refused connection should be down");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_probe_up_and_down() {
    let addr = spawn_test_server().await;
    let timeout = Duration::from_secs(3);

    let up = probe("tcp", &addr.to_string(), timeout).await;
    assert!(up.ok, "open port -> tcp ok");

    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        a
    };
    let down = probe("tcp", &closed.to_string(), timeout).await;
    assert!(!down.ok, "closed port -> tcp down");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn monitor_sweep_records_results() {
    let addr = spawn_test_server().await;

    // Build dev state whose only check targets the local "up" server.
    let mut config = Config::dev();
    config.probe_timeout = Duration::from_secs(3);
    config.seed = vec![Check {
        name: "Local".to_string(),
        kind: "http".to_string(),
        target: format!("http://{addr}/"),
        enabled: true,
    }];
    let state = state_with(config);

    // One sweep should record exactly one (ok) result for "Local".
    monitor::run_all_once(&state).await;
    let latest = state.store.latest_result("Local").expect("a result was recorded");
    assert!(latest.ok, "local up server records ok");
    assert!(latest.latency_ms >= 0);

    // The status view rolls this up to Operational at 100% uptime.
    let view = build_status(state.store.as_ref(), now_secs());
    assert_eq!(view.overall, "operational");
    let comp = view.components.iter().find(|c| c.name == "Local").unwrap();
    assert_eq!(comp.status, "operational");
    assert_eq!(comp.uptime_24h, 100.0);
}

#[test]
fn uptime_percentage_math() {
    assert_eq!(uptime_pct(0, 0), 100.0);
    assert_eq!(uptime_pct(10, 10), 100.0);
    assert_eq!(uptime_pct(10, 9), 90.0);
    assert_eq!(uptime_pct(8, 6), 75.0);
    assert_eq!(uptime_pct(3, 2), 66.67);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_results_drive_degraded_status() {
    let state = state_with({
        let mut c = Config::dev();
        c.seed = vec![Check {
            name: "Flappy".to_string(),
            kind: "http".to_string(),
            target: "http://unused/".to_string(),
            enabled: true,
        }];
        c
    });
    let now = now_secs();
    // 99 ok + 2 down within the 24h window -> ~98.04% -> degraded (currently up).
    for i in 0..99 {
        state.store.insert_result("Flappy", true, 10, now - 1000 + i);
    }
    state.store.insert_result("Flappy", false, 10, now - 50);
    state.store.insert_result("Flappy", false, 10, now - 40);
    state.store.insert_result("Flappy", true, 10, now); // currently up

    let view = build_status(state.store.as_ref(), now);
    let comp = view.components.iter().find(|c| c.name == "Flappy").unwrap();
    assert_eq!(comp.status, "degraded", "recent flapping -> degraded");
    assert_eq!(view.overall, "degraded");
}
