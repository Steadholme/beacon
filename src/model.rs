//! Status model: roll up stored probe results into per-component + overall status.
//!
//! This is the pure read-side: given the [`Store`](crate::store::Store) and "now", it
//! computes each component's current status + rolling uptime windows (24h / 7d / 90d), and
//! the worst-of overall status that drives the public banner. Kept free of HTTP/HTML so the
//! uptime math is unit-testable in isolation.

use serde::Serialize;

use crate::store::{Incident, Store};

/// Rolling-uptime windows, in seconds.
pub const WINDOW_24H: i64 = 86_400;
pub const WINDOW_7D: i64 = 604_800;
pub const WINDOW_90D: i64 = 7_776_000;

/// Degraded threshold: a currently-up component whose 24h uptime dipped below this (in %)
/// is flagged "degraded" (recent flapping) rather than fully "operational".
pub const DEGRADED_THRESHOLD_PCT: f64 = 99.0;

/// A component's current status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Operational,
    Degraded,
    Down,
}

impl Status {
    /// Machine token used in the JSON API.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Operational => "operational",
            Status::Degraded => "degraded",
            Status::Down => "down",
        }
    }

    /// Severity ordering for the worst-of overall rollup (higher = worse).
    fn severity(self) -> u8 {
        match self {
            Status::Operational => 0,
            Status::Degraded => 1,
            Status::Down => 2,
        }
    }
}

/// A single component row for the status page / API.
#[derive(Clone, Debug, Serialize)]
pub struct ComponentView {
    pub name: String,
    pub kind: String,
    pub status: &'static str,
    pub uptime_24h: f64,
    pub uptime_7d: f64,
    pub uptime_90d: f64,
    /// Latest measured latency in ms, if the component has ever been probed.
    pub latency_ms: Option<i64>,
    /// Epoch seconds of the most recent probe, if any.
    pub last_checked: Option<i64>,
}

/// The full status snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct StatusView {
    pub overall: &'static str,
    pub updated_at: i64,
    pub components: Vec<ComponentView>,
    pub incidents: Vec<Incident>,
}

/// Uptime percentage from `(total, up)` counts. No data yet is treated as 100% (nominal):
/// the prober populates results within the first sweep, so this only applies pre-first-probe.
pub fn uptime_pct(total: u64, up: u64) -> f64 {
    if total == 0 {
        100.0
    } else {
        // Round to two decimals for stable display + JSON.
        let pct = (up as f64) / (total as f64) * 100.0;
        (pct * 100.0).round() / 100.0
    }
}

/// Derive a component's status from its latest result and 24h uptime.
pub fn component_status(latest_ok: Option<bool>, uptime_24h: f64) -> Status {
    match latest_ok {
        Some(false) => Status::Down,
        Some(true) => {
            if uptime_24h + f64::EPSILON < DEGRADED_THRESHOLD_PCT {
                Status::Degraded
            } else {
                Status::Operational
            }
        }
        // No probe recorded yet — report nominal until the first sweep lands.
        None => Status::Operational,
    }
}

/// Build the full [`StatusView`] from the store as of `now` (epoch seconds). Only enabled
/// checks appear on the public surface.
pub async fn build_status(store: &dyn Store, now: i64) -> StatusView {
    let mut components = Vec::new();
    let mut overall = Status::Operational;

    for check in store.list_checks().await.into_iter().filter(|c| c.enabled) {
        let latest = store.latest_result(&check.name).await;
        let latest_ok = latest.as_ref().map(|r| r.ok);

        let (t24, u24) = store.uptime_counts(&check.name, now - WINDOW_24H).await;
        let (t7, u7) = store.uptime_counts(&check.name, now - WINDOW_7D).await;
        let (t90, u90) = store.uptime_counts(&check.name, now - WINDOW_90D).await;

        let uptime_24h = uptime_pct(t24, u24);
        let status = component_status(latest_ok, uptime_24h);
        if status.severity() > overall.severity() {
            overall = status;
        }

        components.push(ComponentView {
            name: check.name.clone(),
            kind: check.kind.clone(),
            status: status.as_str(),
            uptime_24h,
            uptime_7d: uptime_pct(t7, u7),
            uptime_90d: uptime_pct(t90, u90),
            latency_ms: latest.as_ref().map(|r| r.latency_ms),
            last_checked: latest.as_ref().map(|r| r.ts),
        });
    }

    StatusView {
        overall: overall.as_str(),
        updated_at: now,
        components,
        incidents: store.list_incidents().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_pct_math() {
        assert_eq!(uptime_pct(0, 0), 100.0);
        assert_eq!(uptime_pct(100, 100), 100.0);
        assert_eq!(uptime_pct(100, 99), 99.0);
        assert_eq!(uptime_pct(4, 3), 75.0);
        // 2/3 -> 66.67 (rounded to two decimals).
        assert_eq!(uptime_pct(3, 2), 66.67);
    }

    #[test]
    fn status_derivation() {
        assert_eq!(component_status(Some(true), 100.0), Status::Operational);
        assert_eq!(component_status(Some(true), 99.0), Status::Operational);
        assert_eq!(component_status(Some(true), 98.0), Status::Degraded);
        assert_eq!(component_status(Some(false), 100.0), Status::Down);
        assert_eq!(component_status(None, 100.0), Status::Operational);
    }
}
