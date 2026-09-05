//! Versioned, secret-free service configuration for MirageSSD.
//!
//! This crate owns the typed configuration contract: the model ([`model`]), the pure validation
//! rules ([`validate`]), and TOML/environment ingestion ([`load`]). It holds no credentials, opens
//! no network connection, and touches the filesystem only in [`load::load_from_path`].
//!
//! ```
//! use mirage_config::{PathPolicy, load_from_str};
//!
//! let config = load_from_str(
//!     "format_version = 1\nprogram_data_root = '/var/lib/miragessd'\n",
//!     PathPolicy::Unix,
//! )
//! .expect("valid configuration");
//! assert_eq!(config.cache.page_size.as_u64(), 1024 * 1024);
//! ```

#![forbid(unsafe_code)]

pub mod load;
pub mod model;
pub mod validate;

pub use load::{
    DEV_ENV_ALLOWLIST, DEV_ENV_PREFIX, ENV_FIRST_TOUCH_LOGGING, ENV_LOG_RETENTION_DAYS,
    ENV_MAX_CONCURRENT_REQUESTS, EnvSource, MapEnv, ProcessEnv, apply_env_overrides,
    load_from_path, load_from_str, parse_from_str,
};
pub use model::{
    CONFIG_FORMAT_VERSION, CacheConfig, SchedulerConfig, ServiceConfig, TelemetryConfig,
};
pub use validate::{
    LOG_RETENTION_DAYS_MAX, MAX_CONCURRENT_REQUESTS_MAX, MAX_LOG_BYTES_MAX, PAGE_SIZE_MAX,
    PAGE_SIZE_MIN, PathPolicy, validate, validate_cache, validate_program_data_root,
    validate_scheduler, validate_telemetry,
};
