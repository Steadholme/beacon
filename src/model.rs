//! Status model: roll up stored probe results into per-component + overall status.
//!
//! This is the pure read-side: given the [`Store`](crate::store::Store) and "now", it
//! computes each component's current status + rolling uptime windows (24h / 7d / the read
//! model's declared evidence window), the per-day evidence bars, and the worst-of overall
//! banner (active incidents and ongoing maintenance fold in with statuspage precedence:
//! critical incident > maintenance > all-ok). Kept free of HTTP/HTML so the uptime and
//! bucketing math is unit-testable in isolation.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::config::PublicComponent;
use crate::store::{Check, ComponentGroup, Incident, IncidentUpdate, Maintenance, Store};
use crate::vitals;

/// Rolling-uptime windows, in seconds.
pub const WINDOW_24H: i64 = 86_400;
pub const WINDOW_7D: i64 = 604_800;
pub const WINDOW_90D: i64 = 7_776_000;

/// Response-time sparkline: hourly average latency across the last [`SPARK_BUCKETS`] hours.
pub const SPARK_BUCKET_SECS: i64 = 3_600;
pub const SPARK_BUCKETS: i64 = 24;
/// The sparkline lookback window, in seconds (24 hourly buckets).
pub const WINDOW_SPARK: i64 = SPARK_BUCKET_SECS * SPARK_BUCKETS;

/// Seconds per day — the uptime-bar bucket width (integer division on the epoch).
pub const DAY_SECS: i64 = 86_400;
/// How many daily bars the status page renders per component.
pub const BAR_DAYS: i64 = 90;
/// Compact history carried by the public read model. The legacy `uptime_90d` JSON field remains
/// for client compatibility, but its value follows this same 30-day evidence window.
pub const PUBLIC_BAR_DAYS: i64 = 30;
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

    /// Parse a machine status token back to a [`Status`] (unknown tokens are nominal).
    fn from_token(token: &str) -> Status {
        match token {
            "down" => Status::Down,
            "degraded" => Status::Degraded,
            "maintenance" => Status::Maintenance,
            _ => Status::Operational,
        }
    }
}

/// Roll a component group's status up from its members' status tokens: the WORST member wins,
/// by the same severity precedence as the overall banner (down > degraded > maintenance >
/// operational). An empty group rolls up as `operational`.
pub fn group_rollup(member_statuses: &[&str]) -> &'static str {
    member_statuses
        .iter()
        .map(|s| Status::from_token(s))
        .max_by_key(|s| s.severity())
        .unwrap_or(Status::Operational)
        .as_str()
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

/// One day of a component's declared evidence window.
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
    /// Uptime over [`StatusView::history_days`]. The field name is retained for wire
    /// compatibility with existing Portal clients; consumers must use `history_days` as the
    /// semantic window rather than assuming 90 days.
    pub uptime_90d: f64,
    /// Latest measured latency in ms, if the component has ever been probed.
    pub latency_ms: Option<i64>,
    /// Mean latency in ms over the sparkline window ([`WINDOW_SPARK`]), if probed in it.
    pub latency_avg_ms: Option<i64>,
    /// Epoch seconds of the most recent probe, if any.
    pub last_checked: Option<i64>,
    /// The last [`BAR_DAYS`] days, oldest first (missing days = `unknown`).
    pub days: Vec<DayStat>,
    /// Per-hour mean latency over the last [`SPARK_BUCKETS`] hours, oldest first (a bucket
    /// with no probe is `None` — a gap in the sparkline).
    pub latency_points: Vec<Option<i64>>,
    /// The [`ComponentGroup`](crate::store::ComponentGroup) id this component sits in, if any.
    pub group_id: Option<String>,
}

/// A component group (public-page section) with its rolled-up status pill.
#[derive(Clone, Debug, Serialize)]
pub struct GroupView {
    pub id: String,
    pub name: String,
    pub position: i64,
    /// Worst-of rollup across the group's visible members (see [`group_rollup`]).
    pub status: &'static str,
}

/// The full status snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct StatusView {
    pub overall: &'static str,
    pub updated_at: i64,
    /// Number of daily buckets carried in each component's `days` array.
    pub history_days: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub infra: Option<vitals::InfraPublic>,
    pub components: Vec<ComponentView>,
    /// Component groups (sections), ordered by position then name. Empty when none configured
    /// — the page then renders the flat component list exactly as before.
    pub groups: Vec<GroupView>,
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
    build_days_for(buckets, today, BAR_DAYS)
}

/// Build a caller-selected daily history window. Public status uses 30 days while the raw
/// compatibility model retains the original 90-day window.
pub fn build_days_for(
    buckets: &HashMap<i64, (i64, i64)>,
    today: i64,
    history_days: i64,
) -> Vec<DayStat> {
    let history_days = history_days.max(1);
    let mut days = Vec::with_capacity(history_days as usize);
    for day in (today - history_days + 1)..=today {
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

/// Mean latency (ms, rounded) from `(sum, count)`; `None` when there were no samples.
pub fn latency_avg(sum: i64, count: i64) -> Option<i64> {
    if count <= 0 {
        None
    } else {
        Some((sum as f64 / count as f64).round() as i64)
    }
}

/// Build one component's sparkline row: the mean latency of each of the last `num` buckets,
/// oldest first, `None` for buckets with no probe (mirrors [`build_days`] over time buckets).
pub fn build_latency_points(
    buckets: &HashMap<i64, (i64, i64)>,
    now: i64,
    bucket_secs: i64,
    num: i64,
) -> Vec<Option<i64>> {
    let width = bucket_secs.max(1);
    let current = now / width;
    let mut points = Vec::with_capacity(num as usize);
    for b in (current - num + 1)..=current {
        points.push(
            buckets
                .get(&b)
                .and_then(|&(sum, count)| latency_avg(sum, count)),
        );
    }
    points
}

#[derive(Clone, Debug)]
struct ComponentProjection {
    name: String,
    kind: String,
    raw_names: Vec<String>,
    group_id: Option<String>,
}

/// Build the original operator-oriented model: every enabled check appears 1:1 and database
/// groups are preserved. Kept for monitor/admin compatibility and unit tests. Public handlers
/// MUST use [`build_public_status`] instead.
pub async fn build_status(store: &dyn Store, now: i64) -> StatusView {
    let projections: Vec<_> = store
        .list_checks()
        .await
        .into_iter()
        .filter(|check| check.enabled)
        .map(|check| ComponentProjection {
            name: check.name.clone(),
            kind: check.kind,
            raw_names: vec![check.name],
            group_id: check.group_id,
        })
        .collect();
    let groups: Vec<_> = store
        .list_component_groups()
        .await
        .into_iter()
        .map(|g: ComponentGroup| GroupView {
            id: g.id,
            name: g.name,
            position: g.position,
            status: Status::Operational.as_str(),
        })
        .collect();
    build_projected_status(
        store,
        now,
        projections,
        groups,
        store.list_incidents().await,
        store.list_incident_updates().await,
        store.list_maintenances().await,
        BAR_DAYS,
    )
    .await
}

/// Build the only model allowed on anonymous surfaces. The projection is explicit and
/// fail-closed: adding/enabling a raw check never publishes it. Each public component may roll
/// up one or more raw probes; the public `name` remains stable for Manifest/Portal joins.
pub async fn build_public_status(
    store: &dyn Store,
    now: i64,
    catalog: &[PublicComponent],
) -> StatusView {
    let checks = store.list_checks().await;
    let (projections, groups) = public_projections(&checks, catalog);
    let incidents = store.list_incidents().await;
    let updates = store.list_incident_updates().await;
    let maintenances = store.list_maintenances().await;
    let (incidents, updates) = project_public_timeline_from(&projections, incidents, updates);
    let maintenances = project_public_maintenances_from(&projections, maintenances);
    build_projected_status(
        store,
        now,
        projections,
        groups,
        incidents,
        updates,
        maintenances,
        PUBLIC_BAR_DAYS,
    )
    .await
}

/// Project incidents and updates without building uptime metrics. RSS uses this seam so it
/// applies the exact same no-internal-name boundary as HTML/JSON.
pub fn project_public_timeline(
    checks: &[Check],
    catalog: &[PublicComponent],
    incidents: Vec<Incident>,
    updates: Vec<IncidentUpdate>,
) -> (Vec<Incident>, Vec<IncidentUpdate>) {
    let (projections, _) = public_projections(checks, catalog);
    project_public_timeline_from(&projections, incidents, updates)
}

fn public_projections(
    checks: &[Check],
    catalog: &[PublicComponent],
) -> (Vec<ComponentProjection>, Vec<GroupView>) {
    let enabled: HashSet<&str> = checks
        .iter()
        .filter(|check| check.enabled)
        .map(|check| check.name.as_str())
        .collect();
    let mut projections = Vec::new();
    let mut groups = Vec::new();
    let mut group_ids: HashMap<&str, String> = HashMap::new();

    for entry in catalog {
        let raw_names: Vec<String> = entry
            .checks
            .iter()
            .filter(|name| enabled.contains(name.as_str()))
            .cloned()
            .collect();
        // A configured component with no enabled backing probe is omitted, not reported as a
        // misleading 100% nominal service.
        if raw_names.is_empty() {
            continue;
        }
        let group_id = match group_ids.get(entry.group.as_str()) {
            Some(id) => id.clone(),
            None => {
                let id = format!("public-group-{}", groups.len() + 1);
                group_ids.insert(entry.group.as_str(), id.clone());
                groups.push(GroupView {
                    id: id.clone(),
                    name: entry.group.clone(),
                    position: groups.len() as i64,
                    status: Status::Operational.as_str(),
                });
                id
            }
        };
        projections.push(ComponentProjection {
            name: entry.name.clone(),
            kind: "service".to_string(),
            raw_names,
            group_id: Some(group_id),
        });
    }
    (projections, groups)
}

fn project_affected(affected: &str, projections: &[ComponentProjection]) -> Option<String> {
    let raw: HashSet<String> = affected_names(affected).into_iter().collect();
    if raw.is_empty() {
        return None;
    }
    let public: Vec<&str> = projections
        .iter()
        .filter(|projection| {
            raw.contains(&projection.name)
                || projection.raw_names.iter().any(|name| raw.contains(name))
        })
        .map(|projection| projection.name.as_str())
        .collect();
    (!public.is_empty()).then(|| public.join(", "))
}

fn project_public_timeline_from(
    projections: &[ComponentProjection],
    incidents: Vec<Incident>,
    updates: Vec<IncidentUpdate>,
) -> (Vec<Incident>, Vec<IncidentUpdate>) {
    let incidents: Vec<_> = incidents
        .into_iter()
        .filter_map(|mut incident| {
            incident.affected = project_affected(&incident.affected, projections)?;
            Some(incident)
        })
        .collect();
    let ids: HashSet<&str> = incidents
        .iter()
        .map(|incident| incident.id.as_str())
        .collect();
    let updates = updates
        .into_iter()
        .filter(|update| ids.contains(update.incident_id.as_str()))
        .collect();
    (incidents, updates)
}

fn project_public_maintenances_from(
    projections: &[ComponentProjection],
    maintenances: Vec<Maintenance>,
) -> Vec<Maintenance> {
    maintenances
        .into_iter()
        .filter_map(|mut maintenance| {
            maintenance.affected = project_affected(&maintenance.affected, projections)?;
            Some(maintenance)
        })
        .collect()
}

/// Shared rollup engine used by both the raw operator model and the explicit public model.
///
/// Overall-banner precedence (worst wins): a projected component that is down / an active
/// critical incident force "down"; degraded / major-or-minor force "degraded"; ongoing
/// maintenance forces "maintenance"; otherwise "operational".
#[allow(clippy::too_many_arguments)]
async fn build_projected_status(
    store: &dyn Store,
    now: i64,
    projections: Vec<ComponentProjection>,
    mut groups: Vec<GroupView>,
    incidents: Vec<Incident>,
    updates: Vec<IncidentUpdate>,
    maintenances: Vec<Maintenance>,
    history_days: i64,
) -> StatusView {
    let mut components = Vec::new();
    let mut overall = Status::Operational;

    let under_maintenance: HashSet<String> = maintenances
        .iter()
        .filter(|m| maintenance_ongoing(m, now))
        .flat_map(|m| affected_names(&m.affected))
        .collect();

    // Daily aggregate for every raw probe in one query; only projected buckets are serialized.
    let today = day_bucket(now);
    let bar_since = (today - history_days.max(1) + 1) * DAY_SECS;
    let evidence_window_secs = history_days.max(1).saturating_mul(DAY_SECS);
    let mut daily: HashMap<String, HashMap<i64, (i64, i64)>> = HashMap::new();
    for row in store.daily_uptime(bar_since).await {
        daily
            .entry(row.name)
            .or_default()
            .insert(row.day, (row.total, row.up));
    }
    // The response-time sparkline grid for EVERY component in ONE aggregate query, keyed
    // (name, hour-bucket) -> (sum_latency, count).
    let mut latency: HashMap<String, HashMap<i64, (i64, i64)>> = HashMap::new();
    for row in store
        .latency_series(now - WINDOW_SPARK, SPARK_BUCKET_SECS)
        .await
    {
        latency
            .entry(row.name)
            .or_default()
            .insert(row.bucket, (row.sum_latency_ms, row.count));
    }
    // Per-group rollup input: each group's visible members' status tokens.
    let mut group_members: HashMap<String, Vec<&'static str>> = HashMap::new();

    for projection in projections {
        let mut latest_ok = None;
        let mut latest_latency = None;
        let mut last_checked = None;
        let (mut t24, mut u24, mut t7, mut u7, mut evidence_total, mut evidence_up) =
            (0, 0, 0, 0, 0, 0);
        let mut comp_days: HashMap<i64, (i64, i64)> = HashMap::new();
        let mut comp_latency: HashMap<i64, (i64, i64)> = HashMap::new();

        for raw_name in &projection.raw_names {
            if let Some(latest) = store.latest_result(raw_name).await {
                latest_ok = match (latest_ok, latest.ok) {
                    (Some(false), _) | (_, false) => Some(false),
                    _ => Some(true),
                };
                latest_latency = Some(
                    latest_latency
                        .map_or(latest.latency_ms, |value: i64| value.max(latest.latency_ms)),
                );
                last_checked =
                    Some(last_checked.map_or(latest.ts, |value: i64| value.max(latest.ts)));
            }
            let (total, up) = store.uptime_counts(raw_name, now - WINDOW_24H).await;
            t24 += total;
            u24 += up;
            let (total, up) = store.uptime_counts(raw_name, now - WINDOW_7D).await;
            t7 += total;
            u7 += up;
            let (total, up) = store
                .uptime_counts(raw_name, now - evidence_window_secs)
                .await;
            evidence_total += total;
            evidence_up += up;

            if let Some(raw_days) = daily.get(raw_name) {
                for (&day, &(total, up)) in raw_days {
                    let bucket = comp_days.entry(day).or_insert((0, 0));
                    bucket.0 += total;
                    bucket.1 += up;
                }
            }
            if let Some(raw_latency) = latency.get(raw_name) {
                for (&bucket_id, &(sum, count)) in raw_latency {
                    let bucket = comp_latency.entry(bucket_id).or_insert((0, 0));
                    bucket.0 += sum;
                    bucket.1 += count;
                }
            }
        }

        let uptime_24h = uptime_pct(t24, u24);
        let status = if under_maintenance.contains(&projection.name) {
            Status::Maintenance
        } else {
            component_status(latest_ok, uptime_24h)
        };
        if status.severity() > overall.severity() {
            overall = status;
        }
        if let Some(gid) = &projection.group_id {
            group_members
                .entry(gid.clone())
                .or_default()
                .push(status.as_str());
        }

        // Window-mean latency: total sum / total count across this component's hour buckets.
        let (lat_sum, lat_count) = comp_latency
            .values()
            .fold((0i64, 0i64), |(s, c), &(bs, bc)| (s + bs, c + bc));

        components.push(ComponentView {
            name: projection.name,
            kind: projection.kind,
            status: status.as_str(),
            uptime_24h,
            uptime_7d: uptime_pct(t7, u7),
            uptime_90d: uptime_pct(evidence_total, evidence_up),
            latency_ms: latest_latency,
            latency_avg_ms: latency_avg(lat_sum, lat_count),
            last_checked,
            days: build_days_for(&comp_days, today, history_days),
            latency_points: build_latency_points(
                &comp_latency,
                now,
                SPARK_BUCKET_SECS,
                SPARK_BUCKETS,
            ),
            group_id: projection.group_id,
        });
    }

    // Group sections with their worst-of rollup pill.
    for group in &mut groups {
        group.status = group_members
            .get(&group.id)
            .map_or_else(|| group_rollup(&[]), |members| group_rollup(members));
    }

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
        history_days,
        infra: None,
        components,
        groups,
        incidents,
        updates,
        // Only upcoming/ongoing windows are public surface (ended ones drop off).
        maintenances: maintenances
            .into_iter()
            .filter(|m| m.ends_at > now)
            .collect(),
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
    fn group_rollup_precedence() {
        // Empty group -> nominal.
        assert_eq!(group_rollup(&[]), "operational");
        // All operational -> operational.
        assert_eq!(group_rollup(&["operational", "operational"]), "operational");
        // Worst-of wins, by the banner precedence (down > degraded > maintenance > ok).
        assert_eq!(group_rollup(&["operational", "maintenance"]), "maintenance");
        assert_eq!(group_rollup(&["maintenance", "degraded"]), "degraded");
        assert_eq!(group_rollup(&["degraded", "down"]), "down");
        assert_eq!(
            group_rollup(&["operational", "degraded", "maintenance"]),
            "degraded"
        );
        // Unknown tokens are treated as nominal, never dragging the pill down.
        assert_eq!(group_rollup(&["operational", "bogus"]), "operational");
    }

    #[test]
    fn latency_average_rounds_and_guards_empty() {
        assert_eq!(latency_avg(0, 0), None, "no samples -> none");
        assert_eq!(latency_avg(100, 4), Some(25));
        assert_eq!(latency_avg(10, 3), Some(3), "3.33 rounds down");
        assert_eq!(latency_avg(11, 3), Some(4), "3.67 rounds up");
        assert_eq!(latency_avg(50, 1), Some(50));
    }

    #[test]
    fn latency_points_bucket_window_oldest_first() {
        // now sits in hour bucket `h`; fill this hour and 2 hours ago, leave 1 hour ago empty.
        let now = 100 * SPARK_BUCKET_SECS + 42;
        let h = now / SPARK_BUCKET_SECS;
        let mut buckets = HashMap::new();
        buckets.insert(h, (40, 2)); // this hour avg 20
        buckets.insert(h - 2, (30, 1)); // two hours ago avg 30
        let points = build_latency_points(&buckets, now, SPARK_BUCKET_SECS, SPARK_BUCKETS);
        assert_eq!(points.len(), SPARK_BUCKETS as usize);
        // Oldest first: last three entries are [h-2, h-1, h].
        assert_eq!(points[SPARK_BUCKETS as usize - 1], Some(20), "current hour");
        assert_eq!(
            points[SPARK_BUCKETS as usize - 2],
            None,
            "empty hour is a gap"
        );
        assert_eq!(
            points[SPARK_BUCKETS as usize - 3],
            Some(30),
            "two hours ago"
        );
        assert_eq!(points[0], None, "oldest hour had no probes");
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

    #[test]
    fn public_maintenance_projection_hides_internal_only_windows() {
        let checks = vec![
            Check {
                name: "Gateway".to_string(),
                kind: "http".to_string(),
                target: "http://gateway".to_string(),
                enabled: true,
                group_id: None,
            },
            Check {
                name: "CA".to_string(),
                kind: "http".to_string(),
                target: "http://keyward".to_string(),
                enabled: true,
                group_id: None,
            },
        ];
        let catalog = vec![PublicComponent {
            name: "Public edge".to_string(),
            group: "Core".to_string(),
            checks: vec!["Gateway".to_string()],
        }];
        let (projections, _) = public_projections(&checks, &catalog);
        let maintenances = vec![
            Maintenance {
                id: "internal".to_string(),
                title: "CA rotation".to_string(),
                body: "internal".to_string(),
                starts_at: 100,
                ends_at: 200,
                affected: "CA".to_string(),
            },
            Maintenance {
                id: "public".to_string(),
                title: "Edge work".to_string(),
                body: "public".to_string(),
                starts_at: 100,
                ends_at: 200,
                affected: "CA, Gateway".to_string(),
            },
        ];
        let projected = project_public_maintenances_from(&projections, maintenances);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].id, "public");
        assert_eq!(projected[0].affected, "Public edge");
    }
}
