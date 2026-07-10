use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use beacon::config::Config;
use beacon::{app, now_secs, state_with, AppState};
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

fn admin_get() -> Request<Body> {
    Request::builder()
        .uri("/admin")
        .header("x-auth-subject", "u_admin")
        .header("x-auth-email", "ops@holdfast.local")
        .body(Body::empty())
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

async fn state_with_vitals(base: String) -> AppState {
    let mut config = Config::dev();
    config.vitals_url = Some(base);
    state_with(config).await
}

async fn spawn_admin_vitals(now: i64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..5 {
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
    let host = "edge<script>";
    let body = if req.contains("/api/anomalies") {
        format!(
            r#"{{"anomalies":[{{"host":"{host}","metric":"cpu_pct","ts":{now},"value":96.0,"score":5.0,"note":"hot <cpu>"}}]}}"#
        )
    } else if req.contains("metric=cpu_pct") {
        format!(r#"{{"samples":[{{"host":"{host}","metric":"cpu_pct","value":96.0,"ts":{now}}}]}}"#)
    } else if req.contains("metric=mem_pct") {
        format!(r#"{{"samples":[{{"host":"{host}","metric":"mem_pct","value":72.0,"ts":{now}}}]}}"#)
    } else if req.contains("metric=disk_pct") {
        format!(
            r#"{{"samples":[{{"host":"{host}","metric":"disk_pct","value":51.0,"ts":{now}}}]}}"#
        )
    } else {
        format!(
            r#"{{"samples":[
              {{"host":"{host}","metric":"cpu_pct","value":96.0,"ts":{now}}},
              {{"host":"{host}","metric":"mem_pct","value":72.0,"ts":{now}}},
              {{"host":"{host}","metric":"disk_pct","value":51.0,"ts":{now}}},
              {{"host":"{host}","metric":"load1","value":0.42,"ts":{now}}},
              {{"host":"{host}","metric":"load5","value":0.38,"ts":{now}}},
              {{"host":"{host}","metric":"load15","value":0.31,"ts":{now}}},
              {{"host":"{host}","metric":"net_rx_bps","value":1200.0,"ts":{now}}},
              {{"host":"{host}","metric":"net_tx_bps","value":3200.0,"ts":{now}}},
              {{"host":"{host}","metric":"uptime_secs","value":93600.0,"ts":{now}}}
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_renders_host_vitals_and_w7_hooks() {
    let now = now_secs();
    let base = spawn_admin_vitals(now).await;
    let state = state_with_vitals(base).await;
    state
        .vitals
        .as_ref()
        .unwrap()
        .refresh(Duration::from_secs(2), now)
        .await;
    state
        .store
        .insert_incident(&beacon::store::Incident {
            id: "inc_admin_vitals".to_string(),
            title: "Admin check".to_string(),
            status: "investigating".to_string(),
            severity: "major".to_string(),
            affected: "Gateway".to_string(),
            body: "checking".to_string(),
            created_at: now,
            updated_at: now,
            resolved_at: 0,
        })
        .await;

    let (status, body) = call(&state, admin_get()).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("Infrastructure"));
    assert!(html.contains("bc-host"));
    assert!(html.contains("edge&lt;script&gt;"));
    assert!(!html.contains("edge<script>"));
    assert!(html.contains("progress--down"));
    assert!(html.contains("bc-host__spark"));
    assert!(html.contains("1 anomalies"));
    assert!(html.contains(r##"data-wire-target="#bc-incidents""##));
    assert!(html.contains(r##"data-wire-target="#bc-groups""##));
    assert!(html.contains(r#"data-spark="confirm:false""#));
    assert!(html.contains("odyssey-wire v1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_vitals_unavailable_is_fail_quiet() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let state = state_with_vitals(format!("http://{addr}")).await;
    state
        .vitals
        .as_ref()
        .unwrap()
        .refresh(Duration::from_millis(100), now_secs())
        .await;

    let (status, body) = call(&state, admin_get()).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains("Host metrics unavailable."));
    assert!(!html.contains("connection refused"));
}
