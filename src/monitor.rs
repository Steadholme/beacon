//! The background prober: every `CHECK_INTERVAL`, probe each enabled check and record the
//! result. Fans the checks out concurrently (one task each) so one slow target never delays
//! the others, then records `(ok, latency_ms, ts)` via the [`Store`](crate::store::Store).

use crate::{now_secs, probe, AppState};

/// Run one full sweep of all enabled checks, concurrently, recording each outcome.
pub async fn run_all_once(state: &AppState) {
    let checks: Vec<_> = state
        .store
        .list_checks()
        .await
        .into_iter()
        .filter(|c| c.enabled)
        .collect();

    let mut handles = Vec::with_capacity(checks.len());
    for check in checks {
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            let outcome = probe::probe(&check.kind, &check.target, state.config.probe_timeout).await;
            let ts = now_secs();
            state
                .store
                .insert_result(&check.name, outcome.ok, outcome.latency_ms, ts)
                .await;
            tracing::debug!(
                check = check.name,
                ok = outcome.ok,
                latency_ms = outcome.latency_ms,
                "probe recorded"
            );
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

/// The monitor loop: sweep immediately, then every `CHECK_INTERVAL`. Runs for the life of
/// the process (spawned from `main`).
pub async fn run_monitor(state: AppState) {
    let interval = state.config.check_interval;
    let checks = state.store.count_checks().await;
    tracing::info!(
        interval_secs = interval.as_secs(),
        checks,
        "Beacon monitor started"
    );
    loop {
        run_all_once(&state).await;
        tokio::time::sleep(interval).await;
    }
}
