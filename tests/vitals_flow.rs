//! Vitals client/cache integration tests.
//!
//! These use a tiny raw TCP JSON server so the beacon-side client exercises its real
//! HTTP/1.0 transport without adding test dependencies.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use beacon::config::Config;
use beacon::{app, now_secs, state_with, AppState};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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

async fn spawn_vitals(now: i64, max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..max_requests {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle_vitals(stream, now));
        }
    });
    format!("http://{addr}")
}

async fn handle_vitals(mut stream: TcpStream, now: i64) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        let Ok(n) = stream.read(&mut tmp).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let req = String::from_utf8_lossy(&buf);
    let body = if req.contains("/api/anomalies") {
        format!(
            r#"{{"z_threshold":3.0,"window":30,"detect_secs":60,"anomalies":[{{"host":"edge-01","metric":"cpu_pct","ts":{now},"value":94.2,"score":4.1,"note":"hot"}}]}}"#
        )
    } else if req.contains("metric=cpu_pct") {
        format!(
            r#"{{"samples":[{{"host":"edge-01","metric":"cpu_pct","value":94.2,"ts":{now_minus_20}}},{{"host":"edge-02","metric":"cpu_pct","value":42.0,"ts":{now_minus_20}}}]}}"#,
            now_minus_20 = now - 20,
        )
    } else if req.contains("metric=mem_pct") {
        format!(
            r#"{{"samples":[{{"host":"edge-01","metric":"mem_pct","value":71.0,"ts":{now_minus_10}}},{{"host":"edge-02","metric":"mem_pct","value":63.0,"ts":{now_minus_10}}}]}}"#,
            now_minus_10 = now - 10,
        )
    } else if req.contains("metric=disk_pct") {
        format!(
            r#"{{"samples":[{{"host":"edge-01","metric":"disk_pct","value":55.0,"ts":{now_minus_5}}},{{"host":"edge-02","metric":"disk_pct","value":88.0,"ts":{now_minus_5}}}]}}"#,
            now_minus_5 = now - 5,
        )
    } else {
        format!(
            r#"{{"samples":[
              {{"host":"edge-01","metric":"cpu_pct","value":94.2,"ts":{now}}},
              {{"host":"edge-01","metric":"mem_pct","value":71.0,"ts":{now}}},
              {{"host":"edge-01","metric":"disk_pct","value":55.0,"ts":{now}}},
              {{"host":"edge-01","metric":"load1","value":0.42,"ts":{now}}},
              {{"host":"edge-01","metric":"mem_used_bytes","value":1024.0,"ts":{now}}},
              {{"host":"edge-01","metric":"mem_total_bytes","value":2048.0,"ts":{now}}},
              {{"host":"edge-02","metric":"cpu_pct","value":42.0,"ts":{now}}},
              {{"host":"edge-02","metric":"mem_pct","value":63.0,"ts":{now}}},
              {{"host":"edge-02","metric":"disk_pct","value":88.0,"ts":{now}}}
            ]}}"#
        )
    };
    let resp = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.flush().await;
}

async fn state_with_vitals(base: String) -> AppState {
    let mut config = Config::dev();
    config.vitals_url = Some(base);
    state_with(config).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_builds_snapshot_and_api_is_leak_safe() {
    let now = now_secs();
    let base = spawn_vitals(now, 5).await;
    let state = state_with_vitals(base).await;
    let vitals = state.vitals.as_ref().expect("vitals configured");

    vitals.refresh(Duration::from_secs(2), now).await;
    let snap = vitals.snapshot().expect("snapshot computed");
    assert_eq!(snap.overall, "high");
    assert_eq!(snap.bands[0].metric, "cpu");
    assert_eq!(snap.bands[0].band, "high");
    assert_eq!(snap.bands[1].band, "elevated");
    assert_eq!(snap.bands[2].band, "elevated");
    assert_eq!(snap.trend.len(), 24);
    assert!(snap.hosts.iter().any(|h| h.host == "edge-01"));
    assert_eq!(snap.anomalies.len(), 1);

    let (status, body) = call(&state, get("/api/status")).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains(r#""infra""#));
    assert!(
        !text.contains("edge-01") && !text.contains("edge-02"),
        "public API must not expose host ids"
    );
    assert!(
        !text.contains("_bytes"),
        "public API must not expose byte metrics"
    );
    assert!(
        !text.contains("anomalies"),
        "public API must not expose anomalies"
    );

    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["infra"]["overall"], "high");
    assert_eq!(
        v["overall"], "operational",
        "infra does not floor availability"
    );
    assert!(v["infra"]["bands"].as_array().unwrap().len() == 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_vitals_keeps_old_snapshot_and_ttl_hides_public_infra() {
    let now = now_secs();
    let base = spawn_vitals(now, 5).await;
    let state = state_with_vitals(base).await;
    let vitals = state.vitals.as_ref().expect("vitals configured");

    vitals.refresh(Duration::from_secs(2), now).await;
    let before = vitals.snapshot().expect("snapshot computed");
    assert!(beacon::vitals::public_infra(&before, now).is_some());

    vitals.refresh(Duration::from_millis(100), now + 1).await;
    let after = vitals.snapshot().expect("old snapshot retained");
    assert_eq!(after.fetched_at, before.fetched_at);
    assert!(beacon::vitals::public_infra(&after, now + 181).is_none());

    let stale_base = spawn_vitals(now - 300, 5).await;
    let stale_state = state_with_vitals(stale_base).await;
    let stale = stale_state.vitals.as_ref().unwrap();
    stale.refresh(Duration::from_secs(2), now - 300).await;
    let (_, body) = call(&stale_state, get("/api/status")).await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        v.get("infra").is_none(),
        "stale snapshot omits infra from public API"
    );
}

#[tokio::test]
async fn vitals_unset_leaves_public_api_compatible() {
    let state = state_with(Config::dev()).await;
    assert!(state.vitals.is_none());
    let (status, body) = call(&state, get("/api/status")).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v.get("infra").is_none());
}
