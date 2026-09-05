//! TOML parsing, file loading, and developer-only environment overrides.
//!
//! Loading is deliberately two-stage: parsing turns text into a [`ServiceConfig`], and validation
//! decides whether that configuration is usable. Every public entry point that returns a
//! configuration to a caller has run both stages, so a `ServiceConfig` obtained from this module is
//! always valid against the policy it was loaded with.
//!
//! Environment overrides exist for developer workflows only. They are governed by a closed
//! allowlist ([`DEV_ENV_ALLOWLIST`]); an unrecognized variable under the [`DEV_ENV_PREFIX`] prefix
//! is a hard error rather than a silent no-op. No override carries a token, credential, or
//! authorization URL, and no such field exists in the model to receive one.

use std::collections::BTreeMap;
use std::path::Path;

use mirage_types::MirageError;

use crate::model::ServiceConfig;
use crate::validate::{PathPolicy, validate};

/// Prefix reserved for developer-only environment overrides.
pub const DEV_ENV_PREFIX: &str = "MIRAGE_DEV_";

/// Overrides [`crate::model::TelemetryConfig::log_retention_days`].
pub const ENV_LOG_RETENTION_DAYS: &str = "MIRAGE_DEV_LOG_RETENTION_DAYS";
/// Overrides [`crate::model::SchedulerConfig::max_concurrent_requests`].
pub const ENV_MAX_CONCURRENT_REQUESTS: &str = "MIRAGE_DEV_MAX_CONCURRENT_REQUESTS";
/// Overrides [`crate::model::TelemetryConfig::first_touch_logging`].
pub const ENV_FIRST_TOUCH_LOGGING: &str = "MIRAGE_DEV_FIRST_TOUCH_LOGGING";

/// Closed allowlist of environment variables permitted to override configuration fields.
///
/// Adding an entry here is a deliberate act. Nothing on this list can carry a secret.
pub const DEV_ENV_ALLOWLIST: &[&str] = &[
    ENV_FIRST_TOUCH_LOGGING,
    ENV_LOG_RETENTION_DAYS,
    ENV_MAX_CONCURRENT_REQUESTS,
];

/// Read-only view of an environment.
///
/// Injecting the environment keeps tests away from `std::env::set_var`, which is process-global and
/// unsound to call while other test threads are running.
pub trait EnvSource {
    /// Returns the value bound to `key`, if any.
    fn get(&self, key: &str) -> Option<String>;

    /// Returns every bound variable name starting with `prefix`.
    fn keys_with_prefix(&self, prefix: &str) -> Vec<String>;
}

/// [`EnvSource`] backed by the real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn keys_with_prefix(&self, prefix: &str) -> Vec<String> {
        std::env::vars()
            .map(|(key, _)| key)
            .filter(|key| key.starts_with(prefix))
            .collect()
    }
}

/// In-memory [`EnvSource`] for tests and dry runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapEnv {
    vars: BTreeMap<String, String>,
}

impl MapEnv {
    /// Creates an empty environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `key` to `value` and returns the environment.
    #[must_use]
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_string(), value.to_string());
        self
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn keys_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.vars
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// Parses TOML text into a [`ServiceConfig`] without validating it.
///
/// # Errors
///
/// Returns `MIRAGE_INVALID_ARGUMENT` when the text is not well-formed TOML, is missing a mandatory
/// field, has a value of the wrong type, or contains a field the model does not declare.
pub fn parse_from_str(text: &str) -> Result<ServiceConfig, MirageError> {
    toml::from_str::<ServiceConfig>(text).map_err(|error| {
        MirageError::invalid_argument("invalid configuration TOML").with_source(error)
    })
}

/// Parses and validates TOML text against an explicit path policy.
///
/// # Errors
///
/// Returns the parse error from [`parse_from_str`] or the first validation failure.
pub fn load_from_str(text: &str, policy: PathPolicy) -> Result<ServiceConfig, MirageError> {
    let config = parse_from_str(text)?;
    validate(&config, policy)?;
    Ok(config)
}

/// Reads, parses, and validates a configuration file using the host path policy.
///
/// The path is not echoed into the returned error, so a missing or unreadable configuration cannot
/// leak a user profile directory into logs or public error envelopes.
///
/// # Errors
///
/// Returns `MIRAGE_IO_ERROR` when the file cannot be read, or the parse/validation error otherwise.
pub fn load_from_path(path: &Path) -> Result<ServiceConfig, MirageError> {
    let text = std::fs::read_to_string(path).map_err(MirageError::from)?;
    load_from_str(&text, PathPolicy::host())
}

/// Applies developer-only environment overrides, then revalidates.
///
/// The overrides are applied to a candidate copy which replaces `config` only after the result
/// validates, so a rejected override leaves `config` untouched rather than half-modified.
///
/// # Errors
///
/// Returns `MIRAGE_INVALID_ARGUMENT` when a variable under [`DEV_ENV_PREFIX`] is not on
/// [`DEV_ENV_ALLOWLIST`], when an allowlisted value does not parse, or when the overridden
/// configuration fails validation.
pub fn apply_env_overrides(
    config: &mut ServiceConfig,
    env: &dyn EnvSource,
    policy: PathPolicy,
) -> Result<(), MirageError> {
    for key in env.keys_with_prefix(DEV_ENV_PREFIX) {
        if !DEV_ENV_ALLOWLIST.contains(&key.as_str()) {
            return Err(MirageError::invalid_argument(format!(
                "environment variable {key} uses the reserved {DEV_ENV_PREFIX} prefix but is not \
                 on the developer override allowlist"
            )));
        }
    }

    let mut candidate = config.clone();

    if let Some(raw) = env.get(ENV_LOG_RETENTION_DAYS) {
        candidate.telemetry.log_retention_days = parse_u32(ENV_LOG_RETENTION_DAYS, &raw)?;
    }
    if let Some(raw) = env.get(ENV_MAX_CONCURRENT_REQUESTS) {
        candidate.scheduler.max_concurrent_requests = parse_u32(ENV_MAX_CONCURRENT_REQUESTS, &raw)?;
    }
    if let Some(raw) = env.get(ENV_FIRST_TOUCH_LOGGING) {
        candidate.telemetry.first_touch_logging = parse_bool(ENV_FIRST_TOUCH_LOGGING, &raw)?;
    }

    validate(&candidate, policy)?;
    *config = candidate;
    Ok(())
}

/// Parses a canonical decimal `u32` override value.
fn parse_u32(key: &str, raw: &str) -> Result<u32, MirageError> {
    raw.parse::<u32>().map_err(|err| {
        MirageError::invalid_argument(format!("environment variable {key} is not a u32: {err}"))
    })
}

/// Parses a canonical `true`/`false` override value.
fn parse_bool(key: &str, raw: &str) -> Result<bool, MirageError> {
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(MirageError::invalid_argument(format!(
            "environment variable {key} must be exactly \"true\" or \"false\""
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
format_version = 1
program_data_root = 'C:\ProgramData\MirageSSD'
"#;

    #[test]
    fn test_allowlist_is_sorted_and_prefixed() {
        let mut sorted = DEV_ENV_ALLOWLIST.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), DEV_ENV_ALLOWLIST);
        assert!(
            DEV_ENV_ALLOWLIST
                .iter()
                .all(|k| k.starts_with(DEV_ENV_PREFIX))
        );
    }

    #[test]
    fn test_overrides_are_atomic() {
        let mut config = load_from_str(MINIMAL, PathPolicy::Windows).expect("minimal config");
        let before = config.clone();

        let env = MapEnv::new().with(ENV_MAX_CONCURRENT_REQUESTS, "0");
        let err = apply_env_overrides(&mut config, &env, PathPolicy::Windows)
            .expect_err("zero concurrency must fail validation");
        assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
        assert_eq!(config, before);
    }
}
