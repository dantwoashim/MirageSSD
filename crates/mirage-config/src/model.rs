//! Versioned, secret-free configuration model for the MirageSSD service.
//!
//! Every struct in this module rejects unknown fields so a typo or a stale key from an older
//! release fails loudly instead of being silently ignored. No field in this model holds a token,
//! refresh token, OAuth client secret, password, or authorization URL: credentials live outside the
//! repository in Windows credential/DPAPI storage (architecture section 12.7).

use core::fmt;
use std::path::{Path, PathBuf};

use mirage_types::ByteCount;
use serde::{Deserialize, Serialize};

/// Configuration format version understood by this build.
///
/// A configuration file declaring any other version is rejected with
/// `MIRAGE_UNSUPPORTED_LAYOUT` rather than being interpreted on a best-effort basis.
pub const CONFIG_FORMAT_VERSION: u32 = 1;

/// Constructs a `ByteCount` from a whole number of mebibytes at compile time.
const fn mib(count: u64) -> ByteCount {
    ByteCount::from_u64(count * 1024 * 1024)
}

/// Constructs a `ByteCount` from a whole number of gibibytes at compile time.
const fn gib(count: u64) -> ByteCount {
    ByteCount::from_u64(count * 1024 * 1024 * 1024)
}

/// Top-level MirageSSD service configuration.
///
/// `format_version` and `program_data_root` are mandatory. Every remaining section falls back to
/// the safe defaults defined in this module, so a minimal configuration file is two lines long.
///
/// `Debug` is implemented manually so the local `program_data_root` (which normally contains a
/// machine or user specific path) is redacted from logs and diagnostic bundles.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// Declared configuration format version; must equal [`CONFIG_FORMAT_VERSION`].
    pub format_version: u32,
    /// Root directory owning the arena, metadata database, journals, and logs.
    pub program_data_root: PathBuf,
    /// Sparse fixed-slot SSD cache arena settings.
    #[serde(default)]
    pub cache: CacheConfig,
    /// Request coordinator and network scheduler settings.
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    /// Observability, log retention, and first-touch telemetry settings.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

impl ServiceConfig {
    /// Builds a configuration with the current format version, the supplied
    /// `program_data_root`, and safe defaults for every other section.
    #[must_use]
    pub fn with_root(program_data_root: PathBuf) -> Self {
        Self {
            format_version: CONFIG_FORMAT_VERSION,
            program_data_root,
            cache: CacheConfig::default(),
            scheduler: SchedulerConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }

    /// Returns the configured `program_data_root`.
    #[must_use]
    pub fn program_data_root(&self) -> &Path {
        &self.program_data_root
    }
}

impl fmt::Debug for ServiceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceConfig")
            .field("format_version", &self.format_version)
            .field("program_data_root", &RedactedPath(&self.program_data_root))
            .field("cache", &self.cache)
            .field("scheduler", &self.scheduler)
            .field("telemetry", &self.telemetry)
            .finish()
    }
}

/// Debug wrapper that reveals only the byte length of a path, never its contents.
struct RedactedPath<'a>(&'a Path);

impl fmt::Debug for RedactedPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted path: {} bytes>", self.0.as_os_str().len())
    }
}

/// Sparse fixed-slot SSD cache arena configuration (architecture sections 7.1 and 8).
///
/// Every byte counted here is inside the hard physical cache envelope: committed, reserved,
/// dirty, staged, journal, spill, and allocation-rounding bytes all draw from `budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    /// Integrity, residency, and eviction unit. Must be a power of two.
    #[serde(default = "default_page_size")]
    pub page_size: ByteCount,
    /// Hard physical cache envelope across all arena shards and metadata.
    #[serde(default = "default_budget")]
    pub budget: ByteCount,
    /// Portion of `budget` withheld for in-progress update staging and journals.
    #[serde(default = "default_update_reserve")]
    pub update_reserve: ByteCount,
    /// Portion of `budget` withheld for the metadata database and indexes.
    #[serde(default = "default_metadata_reserve")]
    pub metadata_reserve: ByteCount,
    /// Logical size of one sparse arena shard file. Must be a multiple of `page_size`.
    #[serde(default = "default_arena_shard_size")]
    pub arena_shard_size: ByteCount,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            page_size: default_page_size(),
            budget: default_budget(),
            update_reserve: default_update_reserve(),
            metadata_reserve: default_metadata_reserve(),
            arena_shard_size: default_arena_shard_size(),
        }
    }
}

/// Request coordinator and network scheduler configuration (architecture sections 7.2 and 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Upper bound on simultaneously outstanding backend requests.
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: u32,
    /// Upper bound on bytes outstanding across all in-flight backend requests.
    #[serde(default = "default_max_inflight_bytes")]
    pub max_inflight_bytes: ByteCount,
    /// Smallest sequential read-ahead fetch window.
    #[serde(default = "default_readahead_window_min")]
    pub readahead_window_min: ByteCount,
    /// Largest sequential read-ahead fetch window.
    #[serde(default = "default_readahead_window_max")]
    pub readahead_window_max: ByteCount,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_requests: default_max_concurrent_requests(),
            max_inflight_bytes: default_max_inflight_bytes(),
            readahead_window_min: default_readahead_window_min(),
            readahead_window_max: default_readahead_window_max(),
        }
    }
}

/// Observability and log retention configuration (architecture section 23).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Number of days local diagnostic logs are retained before rotation deletes them.
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    /// Upper bound on total retained log bytes.
    #[serde(default = "default_max_log_bytes")]
    pub max_log_bytes: ByteCount,
    /// Whether the filesystem first-touch log is captured during play.
    #[serde(default = "default_first_touch_logging")]
    pub first_touch_logging: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_retention_days: default_log_retention_days(),
            max_log_bytes: default_max_log_bytes(),
            first_touch_logging: default_first_touch_logging(),
        }
    }
}

/// Default page size: 1 MiB (architecture section 7.1).
#[must_use]
pub const fn default_page_size() -> ByteCount {
    mib(1)
}

/// Default hard cache envelope: 32 GiB (architecture section 8.2).
#[must_use]
pub const fn default_budget() -> ByteCount {
    gib(32)
}

/// Default update staging reserve: 2 GiB.
#[must_use]
pub const fn default_update_reserve() -> ByteCount {
    gib(2)
}

/// Default metadata reserve: 512 MiB.
#[must_use]
pub const fn default_metadata_reserve() -> ByteCount {
    mib(512)
}

/// Default arena shard size: 8 GiB (architecture section 8.2).
#[must_use]
pub const fn default_arena_shard_size() -> ByteCount {
    gib(8)
}

/// Default concurrent backend request limit.
#[must_use]
pub const fn default_max_concurrent_requests() -> u32 {
    16
}

/// Default in-flight byte ceiling: 64 MiB.
#[must_use]
pub const fn default_max_inflight_bytes() -> ByteCount {
    mib(64)
}

/// Default minimum sequential read-ahead window: 2 MiB (architecture section 7.2).
#[must_use]
pub const fn default_readahead_window_min() -> ByteCount {
    mib(2)
}

/// Default maximum sequential read-ahead window: 16 MiB (architecture section 7.2).
#[must_use]
pub const fn default_readahead_window_max() -> ByteCount {
    mib(16)
}

/// Default log retention: 14 days.
#[must_use]
pub const fn default_log_retention_days() -> u32 {
    14
}

/// Default retained log ceiling: 256 MiB.
#[must_use]
pub const fn default_max_log_bytes() -> ByteCount {
    mib(256)
}

/// Default first-touch logging state: enabled.
#[must_use]
pub const fn default_first_touch_logging() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_match_architecture_units() {
        let cache = CacheConfig::default();
        assert_eq!(cache.page_size.as_u64(), 1024 * 1024);
        assert_eq!(cache.arena_shard_size.as_u64(), 8 * 1024 * 1024 * 1024);
        assert_eq!(cache.budget.as_u64() / cache.page_size.as_u64(), 32_768);

        let scheduler = SchedulerConfig::default();
        assert_eq!(scheduler.readahead_window_min.as_u64(), 2 * 1024 * 1024);
        assert_eq!(scheduler.readahead_window_max.as_u64(), 16 * 1024 * 1024);
    }

    #[test]
    fn test_debug_redacts_program_data_root() {
        let config = ServiceConfig::with_root(PathBuf::from(r"C:\Users\alice\MirageSSD"));
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("alice"));
        assert!(!rendered.contains("MirageSSD"));
        assert!(rendered.contains("<redacted path:"));
    }
}
