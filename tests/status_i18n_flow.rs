use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use beacon::store::{Incident, Maintenance};
use beacon::{app, build_dev_state, now_secs, AppState};
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
async fn public_status_resolves_locale_and_keeps_the_ssr_floor() {
    let state = build_dev_state().await;
    let req = Request::builder()
        .uri("/status")
        .header(header::COOKIE, "__Secure-lang=zh")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);
    assert!(html.contains(r#"<html lang="zh-Hans""#));
    assert!(html.contains(r#"data-ody-profile="public""#));
    assert!(html.contains("系统状态"));
    assert!(html.contains("公开 · 只读"));
    assert!(html.contains("刷新状态"));
    assert!(html.contains(r#"/_gw/lang?to=en"#));
    assert!(
        html.contains("<script"),
        "public status opts into Odyssey Wire"
    );
    assert!(
        html.contains(r#"href="/status" role="button""#),
        "the localized refresh control retains native navigation"
    );

    let req = Request::builder()
        .uri("/status")
        .header(header::COOKIE, "__Secure-lang=zh")
        .header("x-wire", "1")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let fragment = text(&body);
    assert!(fragment.starts_with(r#"<div id="status-live""#));
    assert!(fragment.contains("所有系统运行正常"));
    assert!(!fragment.contains("<script"));
}

#[tokio::test]
async fn evidence_window_copy_is_dynamic_and_localized() {
    let state = build_dev_state().await;
    state
        .store
        .insert_result("Gateway", true, 12, now_secs())
        .await;

    for (lang, hero, title, ago, today) in [
        (
            "en",
            "<strong>100.00%</strong> uptime · 30 days",
            "Uptime over 30 days",
            "30 days ago",
            "Today",
        ),
        (
            "zh",
            "<strong>100.00%</strong> 可用率 · 30 天",
            "最近 30 天可用率",
            "30 天前",
            "今天",
        ),
        (
            "ja",
            "<strong>100.00%</strong> 稼働率 · 30 日間",
            "過去 30 日間の稼働率",
            "30 日前",
            "今日",
        ),
    ] {
        let req = Request::builder()
            .uri("/status")
            .header(header::COOKIE, format!("__Secure-lang={lang}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK);
        let html = text(&body);
        assert!(html.contains(hero), "{lang}: localized hero window");
        assert!(html.contains(&format!(r#"title="{title}""#)));
        assert!(html.contains(ago), "{lang}: localized evidence start");
        assert!(html.contains(today), "{lang}: localized evidence end");
        assert!(!html.contains("90 days"), "{lang}: no stale 90-day copy");
    }
}

/// A populated zh page localizes every previously hardcoded micro-copy path — incident
/// stamps, timeline, maintenance countdown, group/component counts, uptime-row footnotes —
/// with no English residue, and the active language option is announced via `aria-current`.
#[tokio::test]
async fn zh_page_localizes_micro_copy_without_english_residue() {
    let state = build_dev_state().await;
    let now = now_secs();
    // Gateway gets one healthy probe (bars with data + latency title); Identity stays
    // unchecked (awaiting-first-check path). One live incident, one resolved incident,
    // and one ongoing window exercise every localized renderer at once.
    state.store.insert_result("Gateway", true, 12, now).await;
    state
        .store
        .insert_incident(&Incident {
            id: "inc_live".to_string(),
            title: "Edge flap".to_string(),
            status: "investigating".to_string(),
            severity: "critical".to_string(),
            affected: "Gateway".to_string(),
            body: "Tracing packet loss".to_string(),
            created_at: now - 600,
            updated_at: now - 600,
            resolved_at: 0,
        })
        .await;
    state
        .store
        .insert_incident(&Incident {
            id: "inc_past".to_string(),
            title: "Past blip".to_string(),
            status: "resolved".to_string(),
            severity: "minor".to_string(),
            affected: "Gateway".to_string(),
            body: "Fixed".to_string(),
            created_at: now - 7_200,
            updated_at: now - 3_600,
            resolved_at: now - 3_600,
        })
        .await;
    state
        .store
        .insert_maintenance(&Maintenance {
            id: "mw_live".to_string(),
            title: "Rack move".to_string(),
            body: "Relocating the edge pair".to_string(),
            starts_at: now - 600,
            ends_at: now + 3_630,
            affected: "Identity".to_string(),
        })
        .await;

    let req = Request::builder()
        .uri("/status")
        .header(header::COOKIE, "__Secure-lang=zh")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    // Localized micro-copy from each render path.
    assert!(html.contains("调查中"), "incident lifecycle pill");
    assert!(
        html.contains(r#"<span class="pill pill-down">严重</span>"#),
        "severity pill"
    );
    assert!(html.contains("监控 2 个组件"), "component count meta");
    assert!(
        html.contains(r#"<span class="cgroup__count">2 个组件</span>"#),
        "group member count"
    );
    assert!(
        html.contains(r#"<span class="pill pill-info">进行中</span>"#),
        "ongoing maintenance pill"
    );
    assert!(
        html.contains("1 小时 0 分钟 后结束"),
        "maintenance countdown"
    );
    assert!(
        html.contains(r#"<details class="timeline"><summary>时间线 (1)</summary>"#),
        "timeline summary"
    );
    assert!(
        html.contains(r#"<span class="pill pill-state">已报告</span>"#),
        "timeline reported pill"
    );
    assert!(
        html.contains(r#"<div class="incident__time">创建于 10 分钟前 · 最后更新 10 分钟前</div>"#),
        "active incident stamp"
    );
    assert!(
        html.contains(
            r#"<div class="incident__time">创建于 2 小时前 · 解决于 1 小时前 · 持续 1 小时 0 分钟</div>"#
        ),
        "past incident stamp with duration"
    );
    assert!(html.contains(r#"data-uptime="暂无数据""#), "empty bars");
    assert!(
        html.contains(r#"data-uptime="暂无数据 — 监控始于 "#),
        "dated empty bars"
    );
    assert!(
        html.contains("等待首次检查"),
        "unchecked component placeholder"
    );
    assert!(
        html.contains(r#"title="24 小时平均 · 最新 12 ms""#),
        "latency title"
    );

    // The switch label is localized and only the active option is marked current.
    assert!(html.contains(r#"<nav class="bc-lang" aria-label="语言">"#));
    assert!(html.contains(r#"href="/_gw/lang?to=zh" aria-current="true""#));
    assert!(!html.contains(r#"to=en" aria-current"#));
    assert!(!html.contains(r#"to=ja" aria-current"#));

    // No English residue from any previously hardcoded string.
    for residue in [
        "Opened ",
        "Last update",
        "Resolved",
        "Investigating",
        "· lasted",
        "Timeline (",
        "reported</span>",
        "monitoring since",
        "awaiting first check",
        "no data",
        "24h average",
        " monitored</span>",
        " components</span>",
        "in progress",
        "ends in ",
    ] {
        assert!(
            !html.contains(residue),
            "zh page leaks English: {residue:?}"
        );
    }
}

/// The ja page renders the corrected Japanese severity labels (previously raw English
/// tokens), localized lifecycle stamps, and the `aria-current` language marker.
#[tokio::test]
async fn ja_page_translates_severity_and_lifecycle_micro_copy() {
    let state = build_dev_state().await;
    let now = now_secs();
    state
        .store
        .insert_incident(&Incident {
            id: "inc_crit".to_string(),
            title: "Edge flap".to_string(),
            status: "investigating".to_string(),
            severity: "critical".to_string(),
            affected: "Gateway".to_string(),
            body: "Tracing packet loss".to_string(),
            created_at: now - 600,
            updated_at: now - 600,
            resolved_at: 0,
        })
        .await;
    state
        .store
        .insert_incident(&Incident {
            id: "inc_major".to_string(),
            title: "Slow lookups".to_string(),
            status: "monitoring".to_string(),
            severity: "major".to_string(),
            affected: "Identity".to_string(),
            body: "Watching latency".to_string(),
            created_at: now - 1_200,
            updated_at: now - 1_200,
            resolved_at: 0,
        })
        .await;
    state
        .store
        .insert_incident(&Incident {
            id: "inc_minor".to_string(),
            title: "Past blip".to_string(),
            status: "resolved".to_string(),
            severity: "minor".to_string(),
            affected: "Gateway".to_string(),
            body: "Fixed".to_string(),
            created_at: now - 7_200,
            updated_at: now - 3_600,
            resolved_at: now - 3_600,
        })
        .await;

    let req = Request::builder()
        .uri("/status")
        .header(header::COOKIE, "__Secure-lang=ja")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = text(&body);

    // All three severity values read Japanese, not the former raw English tokens.
    assert!(html.contains(r#"<span class="pill pill-down">重大</span>"#));
    assert!(html.contains(r#"<span class="pill pill-warn">主要</span>"#));
    assert!(html.contains(r#"<span class="pill pill-state">軽微</span>"#));

    // Lifecycle pills, timeline, and stamps localize through the shared renderer.
    assert!(html.contains("調査中"));
    assert!(html.contains("監視中"));
    assert!(html.contains("解決済み"));
    assert!(html.contains(r#"<details class="timeline"><summary>タイムライン (1)</summary>"#));
    assert!(html.contains(r#"<span class="pill pill-state">報告</span>"#));
    assert!(html.contains(r#"<div class="incident__time">作成 10分前 · 最終更新 10分前</div>"#));
    assert!(html.contains(
        r#"<div class="incident__time">作成 2時間前 · 解決 1時間前 · 継続時間 1時間0分</div>"#
    ));
    assert!(html.contains(r#"data-uptime="データなし""#));
    assert!(html.contains("初回チェック待ち"));

    assert!(html.contains(r#"<nav class="bc-lang" aria-label="言語">"#));
    assert!(html.contains(r#"href="/_gw/lang?to=ja" aria-current="true""#));
    assert!(!html.contains(r#"to=en" aria-current"#));

    for residue in [
        ">critical</span>",
        ">major</span>",
        ">minor</span>",
        "Investigating",
        "Monitoring",
        "Resolved",
        "Opened ",
        "Timeline (",
        "reported</span>",
        "· lasted",
        "no data",
        "awaiting first check",
    ] {
        assert!(
            !html.contains(residue),
            "ja page leaks English: {residue:?}"
        );
    }
}
