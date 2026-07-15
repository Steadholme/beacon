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

/// One stable component on the public status surface.
///
/// `name` is the public API key (and therefore must stay aligned with Manifest's
/// `statusComponent`). `checks` is an operator-only list of one or more raw probe names. The
/// raw names and targets never leave the public projection.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct PublicComponent {
    pub name: String,
    #[serde(default = "default_public_group")]
    pub group: String,
    pub checks: Vec<String>,
}

fn default_public_group() -> String {
    "Services".to_string()
}

/// Runtime configuration. Cheap to clone; shared read-only behind `Arc`.
#[derive(Clone, Debug)]
pub struct Config {
    /// Listen address (`BIND_ADDR`).
    pub bind_addr: String,
    /// Optional second listener for trusted service-to-service reads
    /// (`BEACON_INTERNAL_BIND_ADDR`, e.g. `0.0.0.0:8401`). It is never routed by Sluice.
    pub internal_bind_addr: Option<String>,
    /// Interval between probe sweeps (`CHECK_INTERVAL`, seconds).
    pub check_interval: Duration,
    /// Per-probe connect/response timeout (`PROBE_TIMEOUT`, seconds).
    pub probe_timeout: Duration,
    /// Internal vitals service base URL (`VITALS_URL`), disabled when unset.
    pub vitals_url: Option<String>,
    /// Checks seeded into an EMPTY checks table on first boot (`BEACON_SEED` JSON, else the
    /// built-in Steadholme default seed).
    pub seed: Vec<Check>,
    /// Explicit public read-model projection (`BEACON_PUBLIC_CATALOG` JSON).
    ///
    /// This is deliberately separate from `checks`: adding an internal probe must never make
    /// it public by accident. A component may aggregate several raw checks.
    pub public_catalog: Vec<PublicComponent>,
    /// Whether anonymous webhook registration and delivery are enabled
    /// (`BEACON_PUBLIC_WEBHOOKS_ENABLED`). Defaults off; RSS and JSON remain available.
    pub public_webhooks_enabled: bool,
}

impl Config {
    /// Default development configuration (in-memory friendly, no database, default seed).
    pub fn dev() -> Self {
        Config {
            bind_addr: DEFAULT_BIND_ADDR.to_string(),
            internal_bind_addr: None,
            check_interval: Duration::from_secs(DEFAULT_CHECK_INTERVAL_SECS),
            probe_timeout: Duration::from_secs(DEFAULT_PROBE_TIMEOUT_SECS),
            vitals_url: None,
            seed: default_seed(),
            public_catalog: default_public_catalog(),
            public_webhooks_enabled: false,
        }
    }

    /// Configuration with the dev defaults overridden by environment variables.
    pub fn from_env() -> Self {
        let mut config = Config::dev();
        if let Some(v) = env_nonempty("BIND_ADDR") {
            config.bind_addr = v;
        }
        config.internal_bind_addr = env_nonempty("BEACON_INTERNAL_BIND_ADDR");
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
        // Unlike the seed, an explicitly supplied but malformed catalog FAILS CLOSED. Falling
        // back to every enabled check would turn an operator typo into an information leak.
        match std::env::var("BEACON_PUBLIC_CATALOG") {
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                tracing::warn!("BEACON_PUBLIC_CATALOG is not UTF-8 — public catalog disabled");
                config.public_catalog.clear();
            }
            Ok(raw) => match parse_public_catalog(&raw) {
                Ok(catalog) => config.public_catalog = catalog,
                Err(e) => {
                    tracing::warn!(error = %e, "BEACON_PUBLIC_CATALOG is invalid — public catalog disabled");
                    config.public_catalog.clear();
                }
            },
        }
        config.public_webhooks_enabled = env_nonempty("BEACON_PUBLIC_WEBHOOKS_ENABLED")
            .is_some_and(|v| {
                matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
            });
        config
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::dev()
    }
}

/// The built-in Steadholme component seed: the gateway (public TLS surface), the identity
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

/// Minimal fail-safe public catalog. CA and every other operator probe remain internal until
/// production explicitly lists them in `BEACON_PUBLIC_CATALOG`.
pub fn default_public_catalog() -> Vec<PublicComponent> {
    vec![
        PublicComponent {
            name: "Gateway".to_string(),
            group: "Core".to_string(),
            checks: vec!["Gateway".to_string()],
        },
        PublicComponent {
            name: "Identity".to_string(),
            group: "Core".to_string(),
            checks: vec!["Identity".to_string()],
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

/// Parse and validate the explicit public catalog. Empty arrays are valid and mean “publish no
/// components”. Invalid entries reject the whole document so partial parsing cannot create an
/// accidental, misleading projection.
pub fn parse_public_catalog(raw: &str) -> Result<Vec<PublicComponent>, String> {
    let mut entries: Vec<PublicComponent> =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let mut names = std::collections::HashSet::new();
    for entry in &mut entries {
        entry.name = entry.name.trim().to_string();
        entry.group = entry.group.trim().to_string();
        let mut seen_checks = std::collections::HashSet::new();
        entry.checks = entry
            .checks
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .filter(|name| seen_checks.insert((*name).to_string()))
            .map(str::to_string)
            .collect();
        if entry.name.is_empty() || entry.name.len() > 120 {
            return Err("component name must contain 1..=120 bytes".to_string());
        }
        if entry.group.is_empty() || entry.group.len() > 120 {
            return Err("component group must contain 1..=120 bytes".to_string());
        }
        if entry.checks.is_empty() {
            return Err(format!("component {} has no raw checks", entry.name));
        }
        if entry.checks.iter().any(|name| name.len() > 200) {
            return Err(format!(
                "component {} has an overlong raw check name",
                entry.name
            ));
        }
        if !names.insert(entry.name.clone()) {
            return Err(format!("duplicate public component name: {}", entry.name));
        }
    }
    Ok(entries)
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

    #[test]
    fn public_catalog_parses_aliases_and_aggregation() {
        let catalog = parse_public_catalog(
            r#"[{"name":"AI Gateway","group":"AI","checks":["primary","fallback"]}]"#,
        )
        .unwrap();
        assert_eq!(catalog[0].name, "AI Gateway");
        assert_eq!(catalog[0].checks, ["primary", "fallback"]);
    }

    #[test]
    fn public_catalog_empty_is_valid_but_invalid_entries_fail_closed() {
        assert!(parse_public_catalog("[]").unwrap().is_empty());
        assert!(parse_public_catalog(r#"[{"name":"x","checks":[]}]"#).is_err());
        assert!(parse_public_catalog(
            r#"[{"name":"x","checks":["a"]},{"name":"x","checks":["b"]}]"#
        )
        .is_err());
    }

    #[test]
    fn default_public_catalog_excludes_internal_ca() {
        let catalog = default_public_catalog();
        assert_eq!(catalog.len(), 2);
        assert!(catalog.iter().any(|c| c.name == "Gateway"));
        assert!(catalog.iter().any(|c| c.name == "Identity"));
        assert!(!catalog.iter().any(|c| c.name == "CA"));
    }
}
