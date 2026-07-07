//! Server configuration, env-driven with working dev defaults.
//!
//! Every value keeps its dev default when the corresponding env var is unset/empty, so the
//! in-memory dev path boots with NO configuration and NO database — exactly like
//! keystone/keyward. Production overrides each via the environment.

use std::time::Duration;

use crate::store::Check;

/// Default listen address (all interfaces, port 8400).
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8400";
/// Default seconds between probe sweeps (`CHECK_INTERVAL`).
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 30;
/// Default per-probe timeout in seconds (`PROBE_TIMEOUT`).
pub const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 5;

/// Runtime configuration. Cheap to clone; shared read-only behind `Arc`.
#[derive(Clone, Debug)]
pub struct Config {
    /// Listen address (`BIND_ADDR`).
    pub bind_addr: String,
    /// Interval between probe sweeps (`CHECK_INTERVAL`, seconds).
    pub check_interval: Duration,
    /// Per-probe connect/response timeout (`PROBE_TIMEOUT`, seconds).
    pub probe_timeout: Duration,
    /// Internal vitals service base URL (`VITALS_URL`), disabled when unset.
    pub vitals_url: Option<String>,
    /// Checks seeded into an EMPTY checks table on first boot (`BEACON_SEED` JSON, else the
    /// built-in HOLDFAST default seed).
    pub seed: Vec<Check>,
}

impl Config {
    /// Default development configuration (in-memory friendly, no database, default seed).
    pub fn dev() -> Self {
        Config {
            bind_addr: DEFAULT_BIND_ADDR.to_string(),
            check_interval: Duration::from_secs(DEFAULT_CHECK_INTERVAL_SECS),
            probe_timeout: Duration::from_secs(DEFAULT_PROBE_TIMEOUT_SECS),
            vitals_url: None,
            seed: default_seed(),
        }
    }

    /// Configuration with the dev defaults overridden by environment variables.
    pub fn from_env() -> Self {
        let mut config = Config::dev();
        if let Some(v) = env_nonempty("BIND_ADDR") {
            config.bind_addr = v;
        }
        if let Some(v) = env_nonempty("CHECK_INTERVAL").and_then(|v| v.parse::<u64>().ok()) {
            config.check_interval = Duration::from_secs(v.max(1));
        }
        if let Some(v) = env_nonempty("PROBE_TIMEOUT").and_then(|v| v.parse::<u64>().ok()) {
            config.probe_timeout = Duration::from_secs(v.max(1));
        }
        config.vitals_url = env_nonempty("VITALS_URL");
        if let Some(raw) = env_nonempty("BEACON_SEED") {
            match parse_seed(&raw) {
                Ok(checks) if !checks.is_empty() => config.seed = checks,
                Ok(_) => tracing::warn!("BEACON_SEED parsed to an empty list — using default seed"),
                Err(e) => {
                    tracing::warn!(error = %e, "BEACON_SEED is not valid JSON — using default seed")
                }
            }
        }
        config
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::dev()
    }
}

/// The built-in HOLDFAST component seed: the gateway (public TLS surface), the identity
/// provider, and the internal CA. Targets resolve on the `holdfast` Docker network; an
/// operator overrides the whole list via `BEACON_SEED`.
pub fn default_seed() -> Vec<Check> {
    vec![
        Check {
            name: "Gateway".to_string(),
            kind: "http".to_string(),
            target: "https://sso.w33d.xyz/healthz".to_string(),
            enabled: true,
            group_id: None,
        },
        Check {
            name: "Identity".to_string(),
            kind: "tcp".to_string(),
            target: "keystone:8443".to_string(),
            enabled: true,
            group_id: None,
        },
        Check {
            name: "CA".to_string(),
            kind: "http".to_string(),
            target: "http://keyward:8200/healthz".to_string(),
            enabled: true,
            group_id: None,
        },
    ]
}

/// One entry of the `BEACON_SEED` JSON array. `enabled` defaults to true when omitted.
#[derive(serde::Deserialize)]
struct SeedEntry {
    name: String,
    kind: String,
    target: String,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

/// Parse a `BEACON_SEED` JSON array (`[{"name","kind","target","enabled"?}, ...]`).
fn parse_seed(raw: &str) -> Result<Vec<Check>, serde_json::Error> {
    let entries: Vec<SeedEntry> = serde_json::from_str(raw)?;
    Ok(entries
        .into_iter()
        .map(|e| Check {
            name: e.name,
            kind: e.kind,
            target: e.target,
            enabled: e.enabled,
            group_id: None,
        })
        .collect())
}

/// Read an env var, returning `None` when unset OR empty (empty never clobbers a default).
fn env_nonempty(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seed_reads_entries_and_defaults_enabled() {
        let raw = r#"[
            {"name":"A","kind":"http","target":"http://a/health"},
            {"name":"B","kind":"tcp","target":"b:9000","enabled":false}
        ]"#;
        let checks = parse_seed(raw).unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "A");
        assert!(checks[0].enabled, "enabled defaults to true");
        assert!(!checks[1].enabled);
    }

    #[test]
    fn default_seed_has_three_components() {
        let seed = default_seed();
        assert_eq!(seed.len(), 3);
        assert!(seed.iter().any(|c| c.name == "Gateway" && c.kind == "http"));
        assert!(seed.iter().any(|c| c.name == "Identity" && c.kind == "tcp"));
    }
}
