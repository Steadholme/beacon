//! Monitoring storage.
//!
//! `Store` is a small trait with an in-memory and a PostgreSQL implementation, mirroring
//! keystone/keyward's seam: handlers and the prober depend only on the trait, so a
//! FusionDB-backed store can drop in later. The PostgreSQL layer uses ONLY portable
//! standard SQL (TEXT/BIGINT/BOOLEAN, PK/UNIQUE/NOT NULL/DEFAULT, parameterized queries,
//! INSERT .. ON CONFLICT, `SUM(CASE WHEN ...)` aggregates) and runtime queries (no
//! compile-time macros), so the build needs NO database and the same statements later run
//! unchanged on FusionDB over pgwire.
//!
//! Tables (all standard SQL):
//! - `checks(name TEXT PK, kind TEXT, target TEXT, enabled BOOLEAN)`
//! - `check_results(name TEXT, ok BOOLEAN, latency_ms BIGINT, ts BIGINT, PK(name, ts))`
//! - `incidents(id TEXT PK, title TEXT, status TEXT, severity TEXT, affected TEXT,
//!    body TEXT, created_at BIGINT, updated_at BIGINT, resolved_at BIGINT)`
//! - `incident_updates(id TEXT PK, incident_id TEXT, status TEXT, body TEXT, created_at BIGINT)`
//! - `maintenances(id TEXT PK, title TEXT, body TEXT, starts_at BIGINT, ends_at BIGINT, affected TEXT)`
//! - `component_groups(id TEXT PK, name TEXT, position BIGINT)` + nullable `checks.group_id`
//! - `subscribers(id TEXT PK, kind TEXT, target TEXT, secret TEXT, confirmed BOOLEAN, created_at BIGINT)`

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A configured monitoring check (maps 1:1 to a `checks` row).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    /// `"http"` (GET, expect 2xx) or `"tcp"` (connect succeeds).
    pub kind: String,
    /// `http(s)://host[:port]/path` for http, or `host:port` for tcp.
    pub target: String,
    pub enabled: bool,
    /// Optional [`ComponentGroup`] id this component sits in (`None` = ungrouped). Backed by
    /// the nullable `checks.group_id` column added by an idempotent ALTER, so old rows and
    /// the default seed read back as `None`.
    #[serde(default)]
    pub group_id: Option<String>,
}

/// One recorded probe outcome (maps 1:1 to a `check_results` row).
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub latency_ms: i64,
    pub ts: i64,
}

/// A manually posted incident (maps 1:1 to an `incidents` row). The incident row itself is
/// the OPENING event of its timeline (`body` at `created_at`); follow-ups live in
/// `incident_updates`. `resolved_at == 0` means "not resolved" (the 0 sentinel keeps the
/// column NOT NULL and the SQL portable — no nullable-column special cases).
#[derive(Clone, Debug, Serialize)]
pub struct Incident {
    pub id: String,
    pub title: String,
    /// `investigating` | `identified` | `monitoring` | `resolved`.
    pub status: String,
    /// `minor` | `major` | `critical`.
    pub severity: String,
    /// Comma-separated affected component names (matches `checks.name`), possibly empty.
    pub affected: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Epoch seconds when the incident was resolved; `0` while active.
    pub resolved_at: i64,
}

/// One timeline entry appended to an incident (maps 1:1 to an `incident_updates` row).
#[derive(Clone, Debug, Serialize)]
pub struct IncidentUpdate {
    pub id: String,
    pub incident_id: String,
    /// The incident status this update moved to.
    pub status: String,
    pub body: String,
    pub created_at: i64,
}

/// A scheduled maintenance window (maps 1:1 to a `maintenances` row).
#[derive(Clone, Debug, Serialize)]
pub struct Maintenance {
    pub id: String,
    pub title: String,
    pub body: String,
    pub starts_at: i64,
    pub ends_at: i64,
    /// Comma-separated affected component names (matches `checks.name`), possibly empty.
    pub affected: String,
}

/// One `(component, day)` aggregate over `check_results` — the 90-day uptime-bar input.
/// `day` is the epoch-day bucket (`ts / 86400`, integer division).
#[derive(Clone, Debug)]
pub struct DailyUptime {
    pub name: String,
    pub day: i64,
    pub total: i64,
    pub up: i64,
}

/// One `(component, time-bucket)` latency aggregate over `check_results` — the response-time
/// sparkline input. `bucket` is `ts / bucket_secs` (integer division); the average is
/// `sum_latency_ms / count` computed in Rust (mirroring the `SUM(...)` uptime aggregate, so
/// the SQL stays the most portable form — no `AVG`, FusionDB-safe over pgwire).
#[derive(Clone, Debug)]
pub struct LatencyPoint {
    pub name: String,
    pub bucket: i64,
    pub sum_latency_ms: i64,
    pub count: i64,
}

/// A section grouping components on the public page (maps 1:1 to a `component_groups` row).
/// `position` orders the sections (ascending); ties break by name.
#[derive(Clone, Debug, Serialize)]
pub struct ComponentGroup {
    pub id: String,
    pub name: String,
    pub position: i64,
}

/// A public status-update subscriber (maps 1:1 to a `subscribers` row). `kind` is currently
/// always `"webhook"` (Beacon has no outbound mail path). `secret` is the per-subscriber
/// HMAC-SHA256 signing key shared with the endpoint owner; `id` doubles as the unguessable
/// confirm/unsubscribe capability token (random hex, so it is safe to place in a link).
#[derive(Clone, Debug, Serialize)]
pub struct Subscriber {
    pub id: String,
    /// Delivery channel — `"webhook"`.
    pub kind: String,
    /// Delivery target (a webhook URL).
    pub target: String,
    /// Per-subscriber HMAC-SHA256 signing key (hex).
    pub secret: String,
    /// Whether the subscription has been confirmed (double opt-in).
    pub confirmed: bool,
    pub created_at: i64,
}

/// Pluggable monitoring store. Methods are `async`: the axum handlers and the background
/// prober `.await` them directly on the serving runtime, so a DB round-trip never blocks a
/// worker thread. The in-memory store never holds a lock across an `.await`.
#[async_trait]
pub trait Store: Send + Sync {
    /// Insert a check if its name is new (seed-safe; never clobbers operator edits).
    async fn insert_check(&self, check: &Check);
    /// All configured checks, ordered by name.
    async fn list_checks(&self) -> Vec<Check>;
    /// Number of configured checks (drives "seed only when empty").
    async fn count_checks(&self) -> usize;

    /// Record a probe outcome (idempotent on `(name, ts)`).
    async fn insert_result(&self, name: &str, ok: bool, latency_ms: i64, ts: i64);
    /// The most recent result for a check, if any.
    async fn latest_result(&self, name: &str) -> Option<CheckResult>;
    /// `(total, up)` result counts for a check at/after `since_ts` — the rolling-uptime input.
    async fn uptime_counts(&self, name: &str, since_ts: i64) -> (u64, u64);

    /// Store a manually posted incident.
    async fn insert_incident(&self, incident: &Incident);
    /// All incidents, newest first.
    async fn list_incidents(&self) -> Vec<Incident>;
    /// One incident by id, if it exists.
    async fn get_incident(&self, id: &str) -> Option<Incident>;
    /// Move an incident to `status`, bumping `updated_at` and setting `resolved_at`
    /// (`0` clears it — posting a non-resolved update reopens a resolved incident).
    async fn set_incident_status(&self, id: &str, status: &str, updated_at: i64, resolved_at: i64);

    /// Append a timeline update to an incident (idempotent on `id`).
    async fn insert_incident_update(&self, update: &IncidentUpdate);
    /// ALL incident updates, newest first (status-page scale; callers group by incident).
    async fn list_incident_updates(&self) -> Vec<IncidentUpdate>;

    /// Store a maintenance window (idempotent on `id`).
    async fn insert_maintenance(&self, maintenance: &Maintenance);
    /// All maintenance windows, soonest `starts_at` first.
    async fn list_maintenances(&self) -> Vec<Maintenance>;

    /// `(name, day, total, up)` aggregates over `check_results` at/after `since_ts`, for
    /// EVERY component in ONE query (`GROUP BY name, ts / 86400`) — the 90-day bar input.
    async fn daily_uptime(&self, since_ts: i64) -> Vec<DailyUptime>;

    /// `(name, bucket, sum_latency_ms, count)` latency aggregates over `check_results` at/after
    /// `since_ts`, for EVERY component in ONE query (`GROUP BY name, ts / bucket_secs`) — the
    /// response-time sparkline input. `bucket_secs` is the sparkline bucket width (e.g. 3600).
    async fn latency_series(&self, since_ts: i64, bucket_secs: i64) -> Vec<LatencyPoint>;

    // --- component groups ---------------------------------------------------

    /// Create a component group if its id is new (idempotent on `id`).
    async fn insert_component_group(&self, group: &ComponentGroup);
    /// All component groups, ordered by `position` then `name`.
    async fn list_component_groups(&self) -> Vec<ComponentGroup>;
    /// Assign a check to a group (`Some(id)`) or clear its group (`None`). No-op for an
    /// unknown check name.
    async fn set_check_group(&self, name: &str, group_id: Option<&str>);

    // --- public subscribers -------------------------------------------------

    /// Store a subscriber (idempotent on `id`).
    async fn insert_subscriber(&self, subscriber: &Subscriber);
    /// One subscriber by id (the confirm/unsubscribe token), if it exists.
    async fn get_subscriber(&self, id: &str) -> Option<Subscriber>;
    /// Mark a subscriber confirmed. No-op for an unknown id.
    async fn confirm_subscriber(&self, id: &str);
    /// Remove a subscriber (unsubscribe). No-op for an unknown id.
    async fn delete_subscriber(&self, id: &str);
    /// All subscribers, newest first (admin visibility + fan-out source).
    async fn list_subscribers(&self) -> Vec<Subscriber>;
}

// --------------------------------------------------------------------------------------
// In-memory `Store` (the default; keeps the whole service database-free for dev + tests).
// --------------------------------------------------------------------------------------

#[derive(Default)]
pub struct InMemoryStore {
    checks: Mutex<HashMap<String, Check>>,
    results: Mutex<Vec<CheckResult>>,
    incidents: Mutex<Vec<Incident>>,
    incident_updates: Mutex<Vec<IncidentUpdate>>,
    maintenances: Mutex<Vec<Maintenance>>,
    component_groups: Mutex<Vec<ComponentGroup>>,
    subscribers: Mutex<Vec<Subscriber>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for InMemoryStore {
    // The std `Mutex` is fine throughout: each critical section is fully synchronous (no
    // `.await` inside), so a guard is never held across a yield point.
    async fn insert_check(&self, check: &Check) {
        self.checks
            .lock()
            .expect("checks lock poisoned")
            .entry(check.name.clone())
            .or_insert_with(|| check.clone());
    }

    async fn list_checks(&self) -> Vec<Check> {
        let mut v: Vec<Check> = self
            .checks
            .lock()
            .expect("checks lock poisoned")
            .values()
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    async fn count_checks(&self) -> usize {
        self.checks.lock().expect("checks lock poisoned").len()
    }

    async fn insert_result(&self, name: &str, ok: bool, latency_ms: i64, ts: i64) {
        let mut results = self.results.lock().expect("results lock poisoned");
        // PRIMARY KEY(name, ts): a duplicate (name, ts) is a no-op, like ON CONFLICT DO NOTHING.
        if results.iter().any(|r| r.name == name && r.ts == ts) {
            return;
        }
        results.push(CheckResult {
            name: name.to_string(),
            ok,
            latency_ms,
            ts,
        });
    }

    async fn latest_result(&self, name: &str) -> Option<CheckResult> {
        self.results
            .lock()
            .expect("results lock poisoned")
            .iter()
            .filter(|r| r.name == name)
            .max_by_key(|r| r.ts)
            .cloned()
    }

    async fn uptime_counts(&self, name: &str, since_ts: i64) -> (u64, u64) {
        let results = self.results.lock().expect("results lock poisoned");
        let mut total = 0u64;
        let mut up = 0u64;
        for r in results.iter() {
            if r.name == name && r.ts >= since_ts {
                total += 1;
                if r.ok {
                    up += 1;
                }
            }
        }
        (total, up)
    }

    async fn insert_incident(&self, incident: &Incident) {
        let mut incidents = self.incidents.lock().expect("incidents lock poisoned");
        if incidents.iter().any(|i| i.id == incident.id) {
            return;
        }
        incidents.push(incident.clone());
    }

    async fn list_incidents(&self) -> Vec<Incident> {
        let mut v: Vec<Incident> = self
            .incidents
            .lock()
            .expect("incidents lock poisoned")
            .clone();
        v.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        v
    }

    async fn get_incident(&self, id: &str) -> Option<Incident> {
        self.incidents
            .lock()
            .expect("incidents lock poisoned")
            .iter()
            .find(|i| i.id == id)
            .cloned()
    }

    async fn set_incident_status(&self, id: &str, status: &str, updated_at: i64, resolved_at: i64) {
        let mut incidents = self.incidents.lock().expect("incidents lock poisoned");
        if let Some(inc) = incidents.iter_mut().find(|i| i.id == id) {
            inc.status = status.to_string();
            inc.updated_at = updated_at;
            inc.resolved_at = resolved_at;
        }
    }

    async fn insert_incident_update(&self, update: &IncidentUpdate) {
        let mut updates = self
            .incident_updates
            .lock()
            .expect("incident_updates lock poisoned");
        if updates.iter().any(|u| u.id == update.id) {
            return;
        }
        updates.push(update.clone());
    }

    async fn list_incident_updates(&self) -> Vec<IncidentUpdate> {
        let mut v: Vec<IncidentUpdate> = self
            .incident_updates
            .lock()
            .expect("incident_updates lock poisoned")
            .clone();
        v.sort_by_key(|u| std::cmp::Reverse(u.created_at));
        v
    }

    async fn insert_maintenance(&self, maintenance: &Maintenance) {
        let mut maintenances = self
            .maintenances
            .lock()
            .expect("maintenances lock poisoned");
        if maintenances.iter().any(|m| m.id == maintenance.id) {
            return;
        }
        maintenances.push(maintenance.clone());
    }

    async fn list_maintenances(&self) -> Vec<Maintenance> {
        let mut v: Vec<Maintenance> = self
            .maintenances
            .lock()
            .expect("maintenances lock poisoned")
            .clone();
        v.sort_by_key(|m| m.starts_at);
        v
    }

    async fn daily_uptime(&self, since_ts: i64) -> Vec<DailyUptime> {
        // Same aggregate the SQL runs: GROUP BY (name, ts / 86400) over ts >= since.
        let mut buckets: HashMap<(String, i64), (i64, i64)> = HashMap::new();
        for r in self.results.lock().expect("results lock poisoned").iter() {
            if r.ts >= since_ts {
                let entry = buckets
                    .entry((r.name.clone(), r.ts / 86_400))
                    .or_insert((0, 0));
                entry.0 += 1;
                if r.ok {
                    entry.1 += 1;
                }
            }
        }
        let mut v: Vec<DailyUptime> = buckets
            .into_iter()
            .map(|((name, day), (total, up))| DailyUptime {
                name,
                day,
                total,
                up,
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then(a.day.cmp(&b.day)));
        v
    }

    async fn latency_series(&self, since_ts: i64, bucket_secs: i64) -> Vec<LatencyPoint> {
        // Same aggregate the SQL runs: GROUP BY (name, ts / bucket_secs) over ts >= since,
        // summing latency + counting so the average is derived identically in both stores.
        let width = bucket_secs.max(1);
        let mut buckets: HashMap<(String, i64), (i64, i64)> = HashMap::new();
        for r in self.results.lock().expect("results lock poisoned").iter() {
            if r.ts >= since_ts {
                let entry = buckets
                    .entry((r.name.clone(), r.ts / width))
                    .or_insert((0, 0));
                entry.0 += r.latency_ms;
                entry.1 += 1;
            }
        }
        let mut v: Vec<LatencyPoint> = buckets
            .into_iter()
            .map(|((name, bucket), (sum_latency_ms, count))| LatencyPoint {
                name,
                bucket,
                sum_latency_ms,
                count,
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then(a.bucket.cmp(&b.bucket)));
        v
    }

    async fn insert_component_group(&self, group: &ComponentGroup) {
        let mut groups = self
            .component_groups
            .lock()
            .expect("component_groups lock poisoned");
        if groups.iter().any(|g| g.id == group.id) {
            return;
        }
        groups.push(group.clone());
    }

    async fn list_component_groups(&self) -> Vec<ComponentGroup> {
        let mut v: Vec<ComponentGroup> = self
            .component_groups
            .lock()
            .expect("component_groups lock poisoned")
            .clone();
        v.sort_by(|a, b| a.position.cmp(&b.position).then(a.name.cmp(&b.name)));
        v
    }

    async fn set_check_group(&self, name: &str, group_id: Option<&str>) {
        let mut checks = self.checks.lock().expect("checks lock poisoned");
        if let Some(c) = checks.get_mut(name) {
            c.group_id = group_id.map(str::to_string);
        }
    }

    async fn insert_subscriber(&self, subscriber: &Subscriber) {
        let mut subs = self.subscribers.lock().expect("subscribers lock poisoned");
        if subs.iter().any(|s| s.id == subscriber.id) {
            return;
        }
        subs.push(subscriber.clone());
    }

    async fn get_subscriber(&self, id: &str) -> Option<Subscriber> {
        self.subscribers
            .lock()
            .expect("subscribers lock poisoned")
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    async fn confirm_subscriber(&self, id: &str) {
        let mut subs = self.subscribers.lock().expect("subscribers lock poisoned");
        if let Some(s) = subs.iter_mut().find(|s| s.id == id) {
            s.confirmed = true;
        }
    }

    async fn delete_subscriber(&self, id: &str) {
        self.subscribers
            .lock()
            .expect("subscribers lock poisoned")
            .retain(|s| s.id != id);
    }

    async fn list_subscribers(&self) -> Vec<Subscriber> {
        let mut v: Vec<Subscriber> = self
            .subscribers
            .lock()
            .expect("subscribers lock poisoned")
            .clone();
        v.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        v
    }
}

// --------------------------------------------------------------------------------------
// PostgreSQL-backed `Store` (portable: standard SQL, runtime queries, no macros).
// --------------------------------------------------------------------------------------
//
// Selected at runtime by `BEACON_STORE=postgres`. The `Store` trait is async, so each method
// drives sqlx natively and the handlers + prober `.await` it on the serving runtime — there
// is NO `block_in_place` and NO sync-over-async bridge, so a DB round-trip never blocks a
// worker thread. Every write is a single idempotent `INSERT .. ON CONFLICT DO NOTHING`, so
// no in-process serializer is needed (the DB enforces uniqueness); reads run fully concurrently.

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

/// PostgreSQL-backed [`Store`]. Holds just a `PgPool`; the async trait methods drive sqlx
/// natively, so no worker thread is ever blocked on a DB round-trip.
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Open a pooled connection. Async; call from within a Tokio runtime.
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(database_url)
            .await?;
        Ok(Self::from_pool(pool))
    }

    /// Construct from an existing pool (used by tests that share a pool).
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Idempotent, portable migration. Standard SQL only — safe to run on every startup.
    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS checks (\
                 name TEXT PRIMARY KEY, \
                 kind TEXT NOT NULL, \
                 target TEXT NOT NULL, \
                 enabled BOOLEAN NOT NULL DEFAULT TRUE\
             )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS check_results (\
                 name TEXT NOT NULL, \
                 ok BOOLEAN NOT NULL, \
                 latency_ms BIGINT NOT NULL, \
                 ts BIGINT NOT NULL, \
                 PRIMARY KEY (name, ts)\
             )",
        )
        .execute(&self.pool)
        .await?;
        // NOTE: the PRIMARY KEY (name, ts) already backs both the rolling-uptime range
        // scans (WHERE name = ? AND ts >= ?) and the latest-result lookup (ORDER BY ts
        // DESC LIMIT 1), so no extra index is needed.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS incidents (\
                 id TEXT PRIMARY KEY, \
                 title TEXT NOT NULL, \
                 status TEXT NOT NULL, \
                 body TEXT NOT NULL, \
                 created_at BIGINT NOT NULL, \
                 updated_at BIGINT NOT NULL\
             )",
        )
        .execute(&self.pool)
        .await?;
        // Statuspage evolution: severity + affected components + resolved marker on the
        // incidents row. ADD COLUMN IF NOT EXISTS keeps the migration idempotent and safe
        // over an existing table; the defaults backfill old rows sensibly.
        for stmt in [
            "ALTER TABLE incidents ADD COLUMN IF NOT EXISTS severity TEXT NOT NULL DEFAULT 'minor'",
            "ALTER TABLE incidents ADD COLUMN IF NOT EXISTS affected TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE incidents ADD COLUMN IF NOT EXISTS resolved_at BIGINT NOT NULL DEFAULT 0",
        ] {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS incident_updates (\
                 id TEXT PRIMARY KEY, \
                 incident_id TEXT NOT NULL, \
                 status TEXT NOT NULL, \
                 body TEXT NOT NULL, \
                 created_at BIGINT NOT NULL\
             )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS maintenances (\
                 id TEXT PRIMARY KEY, \
                 title TEXT NOT NULL, \
                 body TEXT NOT NULL, \
                 starts_at BIGINT NOT NULL, \
                 ends_at BIGINT NOT NULL, \
                 affected TEXT NOT NULL DEFAULT ''\
             )",
        )
        .execute(&self.pool)
        .await?;
        // Component groups: public-page sections with a rolled-up status pill. The nullable
        // `checks.group_id` join column is added by an idempotent ALTER so existing rows and
        // the default seed read back as NULL (ungrouped) — byte-identical when unused.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS component_groups (\
                 id TEXT PRIMARY KEY, \
                 name TEXT NOT NULL, \
                 position BIGINT NOT NULL DEFAULT 0\
             )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("ALTER TABLE checks ADD COLUMN IF NOT EXISTS group_id TEXT")
            .execute(&self.pool)
            .await?;
        // Public status-update subscribers (webhook). `confirmed` gates the fan-out; `secret`
        // is the per-subscriber HMAC signing key; `id` is the confirm/unsubscribe token.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS subscribers (\
                 id TEXT PRIMARY KEY, \
                 kind TEXT NOT NULL, \
                 target TEXT NOT NULL, \
                 secret TEXT NOT NULL, \
                 confirmed BOOLEAN NOT NULL DEFAULT FALSE, \
                 created_at BIGINT NOT NULL\
             )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_check_async(&self, c: &Check) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO checks (name, kind, target, enabled, group_id) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (name) DO NOTHING",
        )
        .bind(&c.name)
        .bind(&c.kind)
        .bind(&c.target)
        .bind(c.enabled)
        .bind(&c.group_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_checks_async(&self) -> Result<Vec<Check>, sqlx::Error> {
        let rows =
            sqlx::query("SELECT name, kind, target, enabled, group_id FROM checks ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(Check {
                name: row.try_get("name")?,
                kind: row.try_get("kind")?,
                target: row.try_get("target")?,
                enabled: row.try_get("enabled")?,
                group_id: row.try_get("group_id")?,
            });
        }
        Ok(out)
    }

    async fn count_checks_async(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM checks")
            .fetch_one(&self.pool)
            .await?;
        row.try_get("n")
    }

    async fn insert_result_async(
        &self,
        name: &str,
        ok: bool,
        latency_ms: i64,
        ts: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO check_results (name, ok, latency_ms, ts) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (name, ts) DO NOTHING",
        )
        .bind(name)
        .bind(ok)
        .bind(latency_ms)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn latest_result_async(&self, name: &str) -> Result<Option<CheckResult>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT name, ok, latency_ms, ts FROM check_results \
             WHERE name = $1 ORDER BY ts DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(CheckResult {
                name: r.try_get("name")?,
                ok: r.try_get("ok")?,
                latency_ms: r.try_get("latency_ms")?,
                ts: r.try_get("ts")?,
            })),
            None => Ok(None),
        }
    }

    async fn uptime_counts_async(
        &self,
        name: &str,
        since_ts: i64,
    ) -> Result<(i64, i64), sqlx::Error> {
        // SUM(CASE WHEN ok THEN 1 ELSE 0 END) instead of COUNT(*) FILTER (...) keeps the
        // aggregate to the most portable SQL form (FusionDB-safe over pgwire).
        let row = sqlx::query(
            "SELECT COUNT(*) AS total, \
                    COALESCE(SUM(CASE WHEN ok THEN 1 ELSE 0 END), 0) AS up \
             FROM check_results WHERE name = $1 AND ts >= $2",
        )
        .bind(name)
        .bind(since_ts)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.try_get("total")?, row.try_get("up")?))
    }

    async fn insert_incident_async(&self, i: &Incident) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO incidents \
             (id, title, status, severity, affected, body, created_at, updated_at, resolved_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&i.id)
        .bind(&i.title)
        .bind(&i.status)
        .bind(&i.severity)
        .bind(&i.affected)
        .bind(&i.body)
        .bind(i.created_at)
        .bind(i.updated_at)
        .bind(i.resolved_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn incident_from_row(row: &sqlx::postgres::PgRow) -> Result<Incident, sqlx::Error> {
        Ok(Incident {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            status: row.try_get("status")?,
            severity: row.try_get("severity")?,
            affected: row.try_get("affected")?,
            body: row.try_get("body")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            resolved_at: row.try_get("resolved_at")?,
        })
    }

    async fn list_incidents_async(&self) -> Result<Vec<Incident>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, title, status, severity, affected, body, created_at, updated_at, \
             resolved_at FROM incidents ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(Self::incident_from_row(row)?);
        }
        Ok(out)
    }

    async fn get_incident_async(&self, id: &str) -> Result<Option<Incident>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, title, status, severity, affected, body, created_at, updated_at, \
             resolved_at FROM incidents WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(Self::incident_from_row).transpose()
    }

    async fn set_incident_status_async(
        &self,
        id: &str,
        status: &str,
        updated_at: i64,
        resolved_at: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE incidents SET status = $2, updated_at = $3, resolved_at = $4 WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(updated_at)
        .bind(resolved_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_incident_update_async(&self, u: &IncidentUpdate) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO incident_updates (id, incident_id, status, body, created_at) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&u.id)
        .bind(&u.incident_id)
        .bind(&u.status)
        .bind(&u.body)
        .bind(u.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_incident_updates_async(&self) -> Result<Vec<IncidentUpdate>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, incident_id, status, body, created_at FROM incident_updates \
             ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(IncidentUpdate {
                id: row.try_get("id")?,
                incident_id: row.try_get("incident_id")?,
                status: row.try_get("status")?,
                body: row.try_get("body")?,
                created_at: row.try_get("created_at")?,
            });
        }
        Ok(out)
    }

    async fn insert_maintenance_async(&self, m: &Maintenance) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO maintenances (id, title, body, starts_at, ends_at, affected) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&m.id)
        .bind(&m.title)
        .bind(&m.body)
        .bind(m.starts_at)
        .bind(m.ends_at)
        .bind(&m.affected)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_maintenances_async(&self) -> Result<Vec<Maintenance>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, title, body, starts_at, ends_at, affected FROM maintenances \
             ORDER BY starts_at",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(Maintenance {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                body: row.try_get("body")?,
                starts_at: row.try_get("starts_at")?,
                ends_at: row.try_get("ends_at")?,
                affected: row.try_get("affected")?,
            });
        }
        Ok(out)
    }

    async fn daily_uptime_async(&self, since_ts: i64) -> Result<Vec<DailyUptime>, sqlx::Error> {
        // ONE aggregate for the whole 90-day bar grid: integer division on the epoch
        // buckets each result into its day, portable across Postgres/FusionDB (no
        // date_trunc, no extensions). SUM(CASE ...) mirrors uptime_counts.
        let rows = sqlx::query(
            "SELECT name, ts / 86400 AS day, COUNT(*) AS total, \
                    COALESCE(SUM(CASE WHEN ok THEN 1 ELSE 0 END), 0) AS up \
             FROM check_results WHERE ts >= $1 \
             GROUP BY name, ts / 86400 ORDER BY name, day",
        )
        .bind(since_ts)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(DailyUptime {
                name: row.try_get("name")?,
                day: row.try_get("day")?,
                total: row.try_get("total")?,
                up: row.try_get("up")?,
            });
        }
        Ok(out)
    }

    async fn latency_series_async(
        &self,
        since_ts: i64,
        bucket_secs: i64,
    ) -> Result<Vec<LatencyPoint>, sqlx::Error> {
        // ONE aggregate for the whole sparkline grid: integer division buckets each result by
        // time; SUM(latency_ms) + COUNT(*) keep the average derivation in Rust (no AVG), the
        // most portable SQL form (FusionDB-safe over pgwire). bucket_secs is bound as a param.
        let width = bucket_secs.max(1);
        let rows = sqlx::query(
            "SELECT name, ts / $2 AS bucket, \
                    CAST(COALESCE(SUM(latency_ms), 0) AS BIGINT) AS sum_latency, COUNT(*) AS n \
             FROM check_results WHERE ts >= $1 \
             GROUP BY name, ts / $2 ORDER BY name, bucket",
        )
        .bind(since_ts)
        .bind(width)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(LatencyPoint {
                name: row.try_get("name")?,
                bucket: row.try_get("bucket")?,
                sum_latency_ms: row.try_get("sum_latency")?,
                count: row.try_get("n")?,
            });
        }
        Ok(out)
    }

    async fn insert_component_group_async(&self, g: &ComponentGroup) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO component_groups (id, name, position) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&g.id)
        .bind(&g.name)
        .bind(g.position)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_component_groups_async(&self) -> Result<Vec<ComponentGroup>, sqlx::Error> {
        let rows =
            sqlx::query("SELECT id, name, position FROM component_groups ORDER BY position, name")
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(ComponentGroup {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                position: row.try_get("position")?,
            });
        }
        Ok(out)
    }

    async fn set_check_group_async(
        &self,
        name: &str,
        group_id: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE checks SET group_id = $2 WHERE name = $1")
            .bind(name)
            .bind(group_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    fn subscriber_from_row(row: &sqlx::postgres::PgRow) -> Result<Subscriber, sqlx::Error> {
        Ok(Subscriber {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            target: row.try_get("target")?,
            secret: row.try_get("secret")?,
            confirmed: row.try_get("confirmed")?,
            created_at: row.try_get("created_at")?,
        })
    }

    async fn insert_subscriber_async(&self, s: &Subscriber) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO subscribers (id, kind, target, secret, confirmed, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&s.id)
        .bind(&s.kind)
        .bind(&s.target)
        .bind(&s.secret)
        .bind(s.confirmed)
        .bind(s.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_subscriber_async(&self, id: &str) -> Result<Option<Subscriber>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, kind, target, secret, confirmed, created_at FROM subscribers WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(Self::subscriber_from_row).transpose()
    }

    async fn confirm_subscriber_async(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE subscribers SET confirmed = TRUE WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_subscriber_async(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM subscribers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_subscribers_async(&self) -> Result<Vec<Subscriber>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, kind, target, secret, confirmed, created_at FROM subscribers \
             ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(Self::subscriber_from_row(row)?);
        }
        Ok(out)
    }
}

#[async_trait]
impl Store for PgStore {
    async fn insert_check(&self, check: &Check) {
        if let Err(e) = self.insert_check_async(check).await {
            tracing::error!(error = %e, "pg insert_check failed");
        }
    }

    async fn list_checks(&self) -> Vec<Check> {
        self.list_checks_async().await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg list_checks failed");
            Vec::new()
        })
    }

    async fn count_checks(&self) -> usize {
        self.count_checks_async()
            .await
            .map(|n| n.max(0) as usize)
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "pg count_checks failed");
                0
            })
    }

    async fn insert_result(&self, name: &str, ok: bool, latency_ms: i64, ts: i64) {
        if let Err(e) = self.insert_result_async(name, ok, latency_ms, ts).await {
            tracing::error!(error = %e, "pg insert_result failed");
        }
    }

    async fn latest_result(&self, name: &str) -> Option<CheckResult> {
        self.latest_result_async(name).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg latest_result failed");
            None
        })
    }

    async fn uptime_counts(&self, name: &str, since_ts: i64) -> (u64, u64) {
        self.uptime_counts_async(name, since_ts)
            .await
            .map(|(t, u)| (t.max(0) as u64, u.max(0) as u64))
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "pg uptime_counts failed");
                (0, 0)
            })
    }

    async fn insert_incident(&self, incident: &Incident) {
        if let Err(e) = self.insert_incident_async(incident).await {
            tracing::error!(error = %e, "pg insert_incident failed");
        }
    }

    async fn list_incidents(&self) -> Vec<Incident> {
        self.list_incidents_async().await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg list_incidents failed");
            Vec::new()
        })
    }

    async fn get_incident(&self, id: &str) -> Option<Incident> {
        self.get_incident_async(id).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg get_incident failed");
            None
        })
    }

    async fn set_incident_status(&self, id: &str, status: &str, updated_at: i64, resolved_at: i64) {
        if let Err(e) = self
            .set_incident_status_async(id, status, updated_at, resolved_at)
            .await
        {
            tracing::error!(error = %e, "pg set_incident_status failed");
        }
    }

    async fn insert_incident_update(&self, update: &IncidentUpdate) {
        if let Err(e) = self.insert_incident_update_async(update).await {
            tracing::error!(error = %e, "pg insert_incident_update failed");
        }
    }

    async fn list_incident_updates(&self) -> Vec<IncidentUpdate> {
        self.list_incident_updates_async()
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "pg list_incident_updates failed");
                Vec::new()
            })
    }

    async fn insert_maintenance(&self, maintenance: &Maintenance) {
        if let Err(e) = self.insert_maintenance_async(maintenance).await {
            tracing::error!(error = %e, "pg insert_maintenance failed");
        }
    }

    async fn list_maintenances(&self) -> Vec<Maintenance> {
        self.list_maintenances_async().await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg list_maintenances failed");
            Vec::new()
        })
    }

    async fn daily_uptime(&self, since_ts: i64) -> Vec<DailyUptime> {
        self.daily_uptime_async(since_ts).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg daily_uptime failed");
            Vec::new()
        })
    }

    async fn latency_series(&self, since_ts: i64, bucket_secs: i64) -> Vec<LatencyPoint> {
        self.latency_series_async(since_ts, bucket_secs)
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "pg latency_series failed");
                Vec::new()
            })
    }

    async fn insert_component_group(&self, group: &ComponentGroup) {
        if let Err(e) = self.insert_component_group_async(group).await {
            tracing::error!(error = %e, "pg insert_component_group failed");
        }
    }

    async fn list_component_groups(&self) -> Vec<ComponentGroup> {
        self.list_component_groups_async()
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "pg list_component_groups failed");
                Vec::new()
            })
    }

    async fn set_check_group(&self, name: &str, group_id: Option<&str>) {
        if let Err(e) = self.set_check_group_async(name, group_id).await {
            tracing::error!(error = %e, "pg set_check_group failed");
        }
    }

    async fn insert_subscriber(&self, subscriber: &Subscriber) {
        if let Err(e) = self.insert_subscriber_async(subscriber).await {
            tracing::error!(error = %e, "pg insert_subscriber failed");
        }
    }

    async fn get_subscriber(&self, id: &str) -> Option<Subscriber> {
        self.get_subscriber_async(id).await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg get_subscriber failed");
            None
        })
    }

    async fn confirm_subscriber(&self, id: &str) {
        if let Err(e) = self.confirm_subscriber_async(id).await {
            tracing::error!(error = %e, "pg confirm_subscriber failed");
        }
    }

    async fn delete_subscriber(&self, id: &str) {
        if let Err(e) = self.delete_subscriber_async(id).await {
            tracing::error!(error = %e, "pg delete_subscriber failed");
        }
    }

    async fn list_subscribers(&self) -> Vec<Subscriber> {
        self.list_subscribers_async().await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "pg list_subscribers failed");
            Vec::new()
        })
    }
}
