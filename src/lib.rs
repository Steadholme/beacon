//! Beacon — uptime monitoring + PUBLIC status page + SSO admin for the Steadholme stack.
//!
//! Library root: defines [`AppState`], wires the routes via [`app`], and provides
//! [`build_dev_state`] / [`state_with`] (in-memory store, seeded checks) and
//! [`build_state_from_env`] (env-selected store). Integration tests consume [`app`] and the
//! [`monitor`] sweep directly, exactly like keystone/keyward.
//!
//! Endpoints:
//! - `GET  /healthz`                  liveness (container HEALTHCHECK)
//! - `GET  /status`                   PUBLIC server-rendered status page (no auth)
//! - `GET  /api/status`               PUBLIC machine-readable status JSON (no auth)
//! - `GET  /assets/beacon-20260907.css` PUBLIC immutable shared stylesheet
//! - `GET  /feed.xml`                 PUBLIC RSS 2.0 incident feed (no auth)
//! - `POST /subscriptions`            PUBLIC webhook subscribe (double opt-in, no auth)
//! - `GET  /subscriptions/confirm`    PUBLIC confirm a subscription (capability token)
//! - `GET  /subscriptions/unsubscribe`PUBLIC unsubscribe (capability token)
//! - `GET  /admin`                    operator dashboard (behind gateway `auth=sso`)
//! - `POST /admin/incidents`          open an incident (SSO identity + CSRF)
//! - `POST /admin/incidents/update`   append a timeline update / move status (SSO + CSRF)
//! - `POST /admin/incidents/resolve`  resolve an incident (SSO + CSRF)
//! - `POST /admin/maintenances`       schedule a maintenance window (SSO + CSRF)
//! - `POST /admin/groups`             create a component group (SSO + CSRF)
//! - `POST /admin/groups/assign`      assign a component to a group (SSO + CSRF)

pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod i18n;
pub mod model;
pub mod monitor;
pub mod notify;
pub mod probe;
pub mod store;
pub mod vitals;

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
    pub vitals: Option<Arc<vitals::VitalsHandle>>,
    pub public_status: Arc<model::PublicStatusCache>,
}

/// Build the router wiring all endpoints onto `state`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        // --- public status surface ---
        // The subdomain-per-service map fronts Beacon at status.w33d.xyz (auth=public) and
        // forwards the path UNMODIFIED, so the host ROOT must render the PUBLIC status page
        // (the admin dashboard stays the fallback for stray paths; its writes still require a
        // gateway-injected identity that Sluice strips on public routes).
        .route("/", get(handlers::status::status_page))
        .route("/status", get(handlers::status::status_page))
        .route("/api/status", get(handlers::status::api_status))
        .route(handlers::APP_CSS_PATH, get(handlers::app_css_asset))
        .route("/feed.xml", get(handlers::feed::feed_xml))
        // --- public status-update subscriptions (webhook; no auth, double opt-in) ---
        .route("/subscriptions", post(handlers::subscriptions::subscribe))
        .route(
            "/subscriptions/confirm",
            get(handlers::subscriptions::confirm),
        )
        .route(
            "/subscriptions/unsubscribe",
            get(handlers::subscriptions::unsubscribe),
        )
        // --- admin (gateway auth=sso; reads injected X-Auth-* identity + CSRF on writes) ---
        .route("/admin", get(handlers::admin::admin_page))
        .route("/admin/incidents", post(handlers::admin::create_incident))
        .route(
            "/admin/incidents/update",
            post(handlers::admin::post_incident_update),
        )
        .route(
            "/admin/incidents/resolve",
            post(handlers::admin::resolve_incident),
        )
        .route(
            "/admin/maintenances",
            post(handlers::admin::create_maintenance),
        )
        .route("/admin/groups", post(handlers::admin::create_group))
        .route(
            "/admin/groups/assign",
            post(handlers::admin::assign_component),
        )
        // Sluice forwards the gateway prefix UNMODIFIED (no strip): the admin dashboard is
        // mounted at the `/beacon` route, so a request arrives here as `GET /beacon`.
        // Register the admin page as the fallback (mirrors watchtower) so it renders behind
        // the gateway prefix. The public /status route stays an explicit match above.
        .fallback(get(handlers::admin::admin_page))
        .with_state(state)
}

/// Trusted service-to-service router for the separate internal listener. Network isolation is
/// the authentication boundary: production exposes this port only on the Docker `holdfast`
/// network and Sluice never routes to it. Keeping this a different Router makes it impossible
/// for the public listener to accidentally match the raw status route.
pub fn internal_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/api/status", get(handlers::status::api_internal_status))
        .with_state(state)
}

/// Build dev state from an explicit [`Config`]: an empty [`InMemoryStore`] seeded with the
/// config's checks. Used by `main`'s memory mode and the integration tests, so they need no
/// database.
pub async fn state_with(config: Config) -> AppState {
    let store = Arc::new(InMemoryStore::new());
    seed_if_empty(store.as_ref(), &config.seed).await;
    let vitals = config
        .vitals_url
        .as_ref()
        .map(|u| Arc::new(vitals::VitalsHandle::new(u.clone())));
    let public_status = Arc::new(model::PublicStatusCache::new(config.status_cache_ttl));
    AppState {
        config: Arc::new(config),
        store,
        vitals,
        public_status,
    }
}

/// Convenience: dev state with the default [`Config`] (default component seed).
pub async fn build_dev_state() -> AppState {
    state_with(Config::dev()).await
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
        other => {
            return Err(format!(
                "unknown BEACON_STORE={other} (use memory|postgres)"
            ))
        }
    };

    seed_if_empty(store.as_ref(), &config.seed).await;

    let vitals = config
        .vitals_url
        .as_ref()
        .map(|u| Arc::new(vitals::VitalsHandle::new(u.clone())));
    let public_status = Arc::new(model::PublicStatusCache::new(config.status_cache_ttl));

    let state = AppState {
        config: Arc::new(config),
        store,
        vitals,
        public_status,
    };
    state
        .public_status
        .warm(
            Arc::clone(&state.store),
            &state.config.public_catalog,
            now_secs(),
        )
        .await;
    Ok(state)
}

/// Seed the checks table from `seed` only when it is currently empty.
pub async fn seed_if_empty(store: &dyn Store, seed: &[Check]) {
    if store.count_checks().await > 0 {
        return;
    }
    for check in seed {
        store.insert_check(check).await;
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
