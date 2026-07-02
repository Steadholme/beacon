//! Status model: roll up stored probe results into per-component + overall status.
//!
//! This is the pure read-side: given the [`Store`](crate::store::Store) and "now", it
//! computes each component's current status + rolling uptime windows (24h / 7d / 90d), the
//! per-day 90-day uptime bars, and the worst-of overall status that drives the public
//! banner (active incidents and ongoing maintenance fold in with statuspage precedence:
//! critical incident > maintenance > all-ok). Kept free of HTTP/HTML so the uptime and
//! bucketing math is unit-testable in isolation.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::store::{Incident, IncidentUpdate, Maintenance, Store};

/// Rolling-uptime windows, in seconds.
pub const WINDOW_24H: i64 = 86_400;
pub const WINDOW_7D: i64 = 604_800;
pub const WINDOW_90D: i64 = 7_776_000;

/// Seconds per day — the uptime-bar bucket width (integer division on the epoch).
pub const DAY_SECS: i64 = 86_400;
/// How many daily bars the status page renders per component.
pub const BAR_DAYS: i64 = 90;
/// "Past incidents" horizon on the public page, in seconds (14 days).
pub const WINDOW_14D: i64 = 1_209_600;

/// Degraded threshold: a currently-up component whose 24h uptime dipped below this (in %)
/// is flagged "degraded" (recent flapping) rather than fully "operational".
pub const DEGRADED_THRESHOLD_PCT: f64 = 99.0;

/// A component's current status. `Maintenance` masks a component that sits inside an
/// ongoing maintenance window (it renders a "maintenance" pill instead of down and never
/// drags the overall banner below "maintenance").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Operational,
    Maintenance,
    Degraded,
    Down,
}

impl Status {
    /// Machine token used in the JSON API.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Operational => "operational",
            Status::Maintenance => "maintenance",
            Status::Degraded => "degraded",
            Status::Down => "down",
        }
    }

    /// Severity ordering for the worst-of overall rollup (higher = worse). This IS the
    /// banner precedence: critical incident (down) > degraded > maintenance > all-ok.
    fn severity(self) -> u8 {
        match self {
            Status::Operational => 0,
            Status::Maintenance => 1,
            Status::Degraded => 2,
            Status::Down => 3,
        }
    }
}

/// The overall-banner floor an ACTIVE (non-resolved) incident imposes: a critical incident
/// forces "down" wording; major/minor force at least "degraded".
pub fn incident_floor(severity: &str) -> Status {
    if severity == "critical" {
        Status::Down
    } else {
        Status::Degraded
    }
}

/// One day of a component's 90-day uptime bar.
#[derive(Clone, Debug, Serialize)]
pub struct DayStat {
    /// Calendar date of the bucket, `YYYY-MM-DD` (UTC).
    pub date: String,
    /// `ok` | `warn` | `down` | `unknown` (no results recorded that day).
    pub status: &'static str,
    /// Up-ratio percent for the day; `None` when there is no data.
    pub uptime: Option<f64>,
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
    /// The last [`BAR_DAYS`] days, oldest first (missing days = `unknown`).
    pub days: Vec<DayStat>,
}

/// The full status snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct StatusView {
    pub overall: &'static str,
    pub updated_at: i64,
    pub components: Vec<ComponentView>,
    pub incidents: Vec<Incident>,
    /// ALL incident updates, newest first (grouped per incident by the renderers).
    pub updates: Vec<IncidentUpdate>,
    /// Upcoming + ongoing maintenance windows (past ones are not public surface).
    pub maintenances: Vec<Maintenance>,
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

/// Epoch-day bucket for a timestamp — integer division, matching the portable SQL
/// aggregate (`ts / 86400`).
pub fn day_bucket(ts: i64) -> i64 {
    ts / DAY_SECS
}

/// Calendar date (`YYYY-MM-DD`, UTC) of an epoch-day bucket.
pub fn day_date(day: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(day * DAY_SECS) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02}",
            dt.year(),
            u8::from(dt.month()),
            dt.day()
        ),
        Err(_) => day.to_string(),
    }
}

/// Classify one day's `(total, up)` counts for the uptime bar: no data is `unknown`,
/// at/above the degraded threshold is `ok`, above 90% is `warn`, else `down`.
pub fn day_class(total: i64, up: i64) -> &'static str {
    if total <= 0 {
        return "unknown";
    }
    let pct = up as f64 / total as f64 * 100.0;
    if pct + f64::EPSILON >= DEGRADED_THRESHOLD_PCT {
        "ok"
    } else if pct >= 90.0 {
        "warn"
    } else {
        "down"
    }
}

/// Split a comma-separated `affected` list into trimmed, non-empty component names.
pub fn affected_names(affected: &str) -> Vec<String> {
    affected
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether a maintenance window is ongoing at `now` (`starts_at <= now < ends_at`).
pub fn maintenance_ongoing(m: &Maintenance, now: i64) -> bool {
    m.starts_at <= now && now < m.ends_at
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

/// Build one component's [`BAR_DAYS`]-long bar row (oldest first) from the daily buckets,
/// filling days with no recorded results as `unknown`.
pub fn build_days(buckets: &HashMap<i64, (i64, i64)>, today: i64) -> Vec<DayStat> {
    let mut days = Vec::with_capacity(BAR_DAYS as usize);
    for day in (today - BAR_DAYS + 1)..=today {
        let (total, up) = buckets.get(&day).copied().unwrap_or((0, 0));
        days.push(DayStat {
            date: day_date(day),
            status: day_class(total, up),
            uptime: if total > 0 {
                Some(uptime_pct(total as u64, up as u64))
            } else {
                None
            },
        });
    }
    days
}

/// Build the full [`StatusView`] from the store as of `now` (epoch seconds). Only enabled
/// checks appear on the public surface.
///
/// Overall-banner precedence (worst wins): a component that is down / an active critical
/// incident force "down"; a degraded component / an active major-or-minor incident force
/// "degraded"; an ongoing maintenance forces "maintenance"; otherwise "operational".
/// Components named in an ongoing maintenance window are MASKED to `maintenance` — they
/// render a maintenance pill instead of down and never drag the banner below maintenance.
pub async fn build_status(store: &dyn Store, now: i64) -> StatusView {
    let mut components = Vec::new();
    let mut overall = Status::Operational;

    let maintenances = store.list_maintenances().await;
    let under_maintenance: HashSet<String> = maintenances
        .iter()
        .filter(|m| maintenance_ongoing(m, now))
        .flat_map(|m| affected_names(&m.affected))
        .collect();

    // The 90-day bar grid for EVERY component in ONE aggregate query, keyed (name, day).
    let today = day_bucket(now);
    let bar_since = (today - BAR_DAYS + 1) * DAY_SECS;
    let mut daily: HashMap<String, HashMap<i64, (i64, i64)>> = HashMap::new();
    for row in store.daily_uptime(bar_since).await {
        daily
            .entry(row.name)
            .or_default()
            .insert(row.day, (row.total, row.up));
    }
    let empty_days: HashMap<i64, (i64, i64)> = HashMap::new();

    for check in store.list_checks().await.into_iter().filter(|c| c.enabled) {
        let latest = store.latest_result(&check.name).await;
        let latest_ok = latest.as_ref().map(|r| r.ok);

        let (t24, u24) = store.uptime_counts(&check.name, now - WINDOW_24H).await;
        let (t7, u7) = store.uptime_counts(&check.name, now - WINDOW_7D).await;
        let (t90, u90) = store.uptime_counts(&check.name, now - WINDOW_90D).await;

        let uptime_24h = uptime_pct(t24, u24);
        let status = if under_maintenance.contains(&check.name) {
            Status::Maintenance
        } else {
            component_status(latest_ok, uptime_24h)
        };
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
            days: build_days(daily.get(&check.name).unwrap_or(&empty_days), today),
        });
    }

    let incidents = store.list_incidents().await;
    // Active (non-resolved) incidents floor the banner: critical -> down, else degraded.
    for inc in incidents.iter().filter(|i| i.status != "resolved") {
        let floor = incident_floor(&inc.severity);
        if floor.severity() > overall.severity() {
            overall = floor;
        }
    }
    // Ongoing maintenance floors the banner to "maintenance" (never past it).
    if maintenances.iter().any(|m| maintenance_ongoing(m, now))
        && Status::Maintenance.severity() > overall.severity()
    {
        overall = Status::Maintenance;
    }

    StatusView {
        overall: overall.as_str(),
        updated_at: now,
        components,
        incidents,
        updates: store.list_incident_updates().await,
        // Only upcoming/ongoing windows are public surface (ended ones drop off).
        maintenances: maintenances.into_iter().filter(|m| m.ends_at > now).collect(),
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

    #[test]
    fn day_bucketing_math() {
        // Integer division on the epoch: second 0..86399 -> day 0, 86400 -> day 1.
        assert_eq!(day_bucket(0), 0);
        assert_eq!(day_bucket(86_399), 0);
        assert_eq!(day_bucket(86_400), 1);
        assert_eq!(day_bucket(1_750_000_000), 1_750_000_000 / 86_400);
        // Bucket dates are UTC calendar days.
        assert_eq!(day_date(0), "1970-01-01");
        assert_eq!(day_date(1), "1970-01-02");
        assert_eq!(day_date(day_bucket(1_750_000_000)), "2025-06-15");
    }

    #[test]
    fn day_classification() {
        assert_eq!(day_class(0, 0), "unknown");
        assert_eq!(day_class(100, 100), "ok");
        assert_eq!(day_class(100, 99), "ok"); // exactly at the 99% threshold
        assert_eq!(day_class(100, 95), "warn");
        assert_eq!(day_class(100, 89), "down");
        assert_eq!(day_class(2, 0), "down");
    }

    #[test]
    fn build_days_fills_missing_as_unknown() {
        let today = day_bucket(1_750_000_000);
        let mut buckets = HashMap::new();
        buckets.insert(today, (10, 10)); // today all up
        buckets.insert(today - 1, (10, 5)); // yesterday half down
        let days = build_days(&buckets, today);
        assert_eq!(days.len(), BAR_DAYS as usize);
        // Oldest first: day 0 of the row is `today - 89`, unknown (no data).
        assert_eq!(days[0].status, "unknown");
        assert_eq!(days[0].uptime, None);
        assert_eq!(days[0].date, day_date(today - BAR_DAYS + 1));
        // Yesterday and today carry their aggregates.
        assert_eq!(days[88].status, "down");
        assert_eq!(days[88].uptime, Some(50.0));
        assert_eq!(days[89].status, "ok");
        assert_eq!(days[89].uptime, Some(100.0));
        assert_eq!(days[89].date, day_date(today));
    }

    #[test]
    fn affected_list_parsing() {
        assert_eq!(affected_names(""), Vec::<String>::new());
        assert_eq!(affected_names("Gateway"), vec!["Gateway"]);
        assert_eq!(
            affected_names(" Gateway , Identity ,,CA"),
            vec!["Gateway", "Identity", "CA"]
        );
    }

    #[test]
    fn incident_floor_by_severity() {
        assert_eq!(incident_floor("critical"), Status::Down);
        assert_eq!(incident_floor("major"), Status::Degraded);
        assert_eq!(incident_floor("minor"), Status::Degraded);
    }

    #[test]
    fn banner_precedence_ordering() {
        // critical incident (down) > degraded > maintenance > all-ok.
        assert!(Status::Down.severity() > Status::Degraded.severity());
        assert!(Status::Degraded.severity() > Status::Maintenance.severity());
        assert!(Status::Maintenance.severity() > Status::Operational.severity());
    }

    #[test]
    fn maintenance_window_bounds() {
        let m = Maintenance {
            id: "mw_1".to_string(),
            title: "t".to_string(),
            body: "b".to_string(),
            starts_at: 100,
            ends_at: 200,
            affected: "Gateway".to_string(),
        };
        assert!(!maintenance_ongoing(&m, 99), "not started yet");
        assert!(maintenance_ongoing(&m, 100), "inclusive start");
        assert!(maintenance_ongoing(&m, 199));
        assert!(!maintenance_ongoing(&m, 200), "exclusive end");
    }
}
