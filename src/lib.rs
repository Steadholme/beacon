//! Beacon — uptime monitoring + PUBLIC status page + SSO admin for the HOLDFAST stack.
//!
//! Library root: defines [`AppState`], wires the routes via [`app`], and provides
//! [`build_dev_state`] / [`state_with`] (in-memory store, seeded checks) and
//! [`build_state_from_env`] (env-selected store). Integration tests consume [`app`] and the
//! [`monitor`] sweep directly, exactly like keystone/keyward.
//!
//! Endpoints:
//! - `GET  /healthz`          liveness (container HEALTHCHECK)
//! - `GET  /status`           PUBLIC server-rendered status page (no auth)
//! - `GET  /api/status`       PUBLIC machine-readable status JSON (no auth)
//! - `GET  /admin`            operator dashboard (behind gateway `auth=sso`)
//! - `POST /admin/incidents`  post a manual incident (behind gateway `auth=sso`)

pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod model;
pub mod monitor;
pub mod probe;
pub mod store;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::{get, post};
use axum::Router;

use crate::config::Config;
use crate::store::{Check, InMemoryStore, PgStore, Store};

/// Shared application state. Cheap to clone (everything behind `Arc`).
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<dyn Store>,
}

/// Build the router wiring all endpoints onto `state`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        // --- public status surface ---
        .route("/status", get(handlers::status::status_page))
        .route("/api/status", get(handlers::status::api_status))
        // --- admin (gateway auth=sso; reads injected X-Auth-* identity) ---
        .route("/admin", get(handlers::admin::admin_page))
        .route("/admin/incidents", post(handlers::admin::create_incident))
        // Sluice forwards the gateway prefix UNMODIFIED (no strip): the admin dashboard is
        // mounted at the `/beacon` route, so a request arrives here as `GET /beacon`.
        // Register the admin page as the fallback (mirrors watchtower) so it renders behind
        // the gateway prefix. The public /status route stays an explicit match above.
        .fallback(get(handlers::admin::admin_page))
        .with_state(state)
}

/// Build dev state from an explicit [`Config`]: an empty [`InMemoryStore`] seeded with the
/// config's checks. Used by `main`'s memory mode and the integration tests, so they need no
/// database.
pub fn state_with(config: Config) -> AppState {
    let store = Arc::new(InMemoryStore::new());
    seed_if_empty(store.as_ref(), &config.seed);
    AppState {
        config: Arc::new(config),
        store,
    }
}

/// Convenience: dev state with the default [`Config`] (default component seed).
pub fn build_dev_state() -> AppState {
    state_with(Config::dev())
}

/// Build runtime state from the environment.
///
/// [`Config`] comes from [`Config::from_env`]. The store is selected by `BEACON_STORE`:
/// - `memory` (default): empty [`InMemoryStore`] — no database required.
/// - `postgres`: connect `DATABASE_URL`, run the idempotent migration, wire [`PgStore`].
///
/// In both cases the checks table is seeded from the config seed ONLY when it is empty, so
/// operator edits in Postgres survive restarts. Returns an error string on misconfiguration.
pub async fn build_state_from_env() -> Result<AppState, String> {
    let config = Config::from_env();

    let store_kind = std::env::var("BEACON_STORE").unwrap_or_else(|_| "memory".to_string());
    let store: Arc<dyn Store> = match store_kind.as_str() {
        "postgres" => {
            let database_url = std::env::var("DATABASE_URL")
                .map_err(|_| "BEACON_STORE=postgres requires DATABASE_URL".to_string())?;
            tracing::info!("BEACON_STORE=postgres — connecting to database");
            let pg = PgStore::connect(&database_url)
                .await
                .map_err(|e| format!("connect postgres: {e}"))?;
            pg.migrate()
                .await
                .map_err(|e| format!("run migration: {e}"))?;
            tracing::info!("postgres store ready (migrated)");
            Arc::new(pg)
        }
        "memory" => Arc::new(InMemoryStore::new()),
        other => return Err(format!("unknown BEACON_STORE={other} (use memory|postgres)")),
    };

    seed_if_empty(store.as_ref(), &config.seed);

    Ok(AppState {
        config: Arc::new(config),
        store,
    })
}

/// Seed the checks table from `seed` only when it is currently empty.
pub fn seed_if_empty(store: &dyn Store, seed: &[Check]) {
    if store.count_checks() > 0 {
        return;
    }
    for check in seed {
        store.insert_check(check);
    }
    if !seed.is_empty() {
        tracing::info!(count = seed.len(), "seeded checks from config");
    }
}

/// Current wall-clock time in epoch seconds.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs() as i64
}

/// Monotonic-ish nanosecond counter for incident ids (high-resolution, collision-resistant).
pub fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_nanos()
}
