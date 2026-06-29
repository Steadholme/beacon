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
//! - `incidents(id TEXT PK, title TEXT, status TEXT, body TEXT, created_at BIGINT, updated_at BIGINT)`

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
}

/// One recorded probe outcome (maps 1:1 to a `check_results` row).
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub latency_ms: i64,
    pub ts: i64,
}

/// A manually posted incident (maps 1:1 to an `incidents` row).
#[derive(Clone, Debug, Serialize)]
pub struct Incident {
    pub id: String,
    pub title: String,
    pub status: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
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
}

// --------------------------------------------------------------------------------------
// In-memory `Store` (the default; keeps the whole service database-free for dev + tests).
// --------------------------------------------------------------------------------------

#[derive(Default)]
pub struct InMemoryStore {
    checks: Mutex<HashMap<String, Check>>,
    results: Mutex<Vec<CheckResult>>,
    incidents: Mutex<Vec<Incident>>,
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
        Ok(())
    }

    async fn insert_check_async(&self, c: &Check) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO checks (name, kind, target, enabled) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(&c.name)
        .bind(&c.kind)
        .bind(&c.target)
        .bind(c.enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_checks_async(&self) -> Result<Vec<Check>, sqlx::Error> {
        let rows = sqlx::query("SELECT name, kind, target, enabled FROM checks ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(Check {
                name: row.try_get("name")?,
                kind: row.try_get("kind")?,
                target: row.try_get("target")?,
                enabled: row.try_get("enabled")?,
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
            "INSERT INTO incidents (id, title, status, body, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&i.id)
        .bind(&i.title)
        .bind(&i.status)
        .bind(&i.body)
        .bind(i.created_at)
        .bind(i.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_incidents_async(&self) -> Result<Vec<Incident>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, title, status, body, created_at, updated_at FROM incidents \
             ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(Incident {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                status: row.try_get("status")?,
                body: row.try_get("body")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            });
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
}
