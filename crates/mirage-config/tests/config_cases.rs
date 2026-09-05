//! Roadmap Day 6 acceptance cases for `mirage-config`.
//!
//! Covers the golden valid configuration, unknown-field rejection, invalid sizes, arithmetic
//! overflow, path traversal, and redacted debug output, plus the developer environment allowlist.

use std::path::PathBuf;

use mirage_config::{
    CONFIG_FORMAT_VERSION, CacheConfig, ENV_FIRST_TOUCH_LOGGING, ENV_LOG_RETENTION_DAYS,
    ENV_MAX_CONCURRENT_REQUESTS, MapEnv, PathPolicy, SchedulerConfig, ServiceConfig,
    TelemetryConfig, apply_env_overrides, load_from_path, load_from_str, parse_from_str, validate,
    validate_cache,
};
use mirage_types::ByteCount;

/// Every field spelled out, used as the golden fixture.
const GOLDEN: &str = r#"
format_version = 1
program_data_root = 'C:\ProgramData\MirageSSD'

[cache]
page_size = 1048576
budget = 34359738368
update_reserve = 2147483648
metadata_reserve = 536870912
arena_shard_size = 8589934592

[scheduler]
max_concurrent_requests = 16
max_inflight_bytes = 67108864
readahead_window_min = 2097152
readahead_window_max = 16777216

[telemetry]
log_retention_days = 14
max_log_bytes = 268435456
first_touch_logging = true
"#;

/// Only the two mandatory fields; every section falls back to defaults.
const MINIMAL: &str = r#"
format_version = 1
program_data_root = 'C:\ProgramData\MirageSSD'
"#;

fn windows_config() -> ServiceConfig {
    load_from_str(MINIMAL, PathPolicy::Windows).expect("minimal configuration is valid")
}

// ---------------------------------------------------------------------------
// Golden valid configuration
// ---------------------------------------------------------------------------

#[test]
fn golden_config_parses_validates_and_matches_declared_values() {
    let config = load_from_str(GOLDEN, PathPolicy::Windows).expect("golden configuration is valid");

    assert_eq!(config.format_version, CONFIG_FORMAT_VERSION);
    assert_eq!(
        config.program_data_root,
        PathBuf::from(r"C:\ProgramData\MirageSSD")
    );
    assert_eq!(config.cache.page_size, ByteCount::from_u64(1024 * 1024));
    assert_eq!(
        config.cache.budget,
        ByteCount::from_u64(32 * 1024 * 1024 * 1024)
    );
    assert_eq!(config.scheduler.max_concurrent_requests, 16);
    assert_eq!(
        config.scheduler.readahead_window_max,
        ByteCount::from_u64(16 * 1024 * 1024)
    );
    assert_eq!(config.telemetry.log_retention_days, 14);
    assert!(config.telemetry.first_touch_logging);
}

#[test]
fn golden_config_round_trips_through_toml() {
    let config = load_from_str(GOLDEN, PathPolicy::Windows).expect("golden configuration is valid");
    let rendered = toml::to_string(&config).expect("serialize configuration");
    let reparsed = load_from_str(&rendered, PathPolicy::Windows).expect("re-parse rendered TOML");
    assert_eq!(reparsed, config);
}

#[test]
fn minimal_config_applies_safe_defaults() {
    let config = windows_config();
    assert_eq!(config.cache, CacheConfig::default());
    assert_eq!(config.scheduler, SchedulerConfig::default());
    assert_eq!(config.telemetry, TelemetryConfig::default());
}

#[test]
fn golden_and_minimal_agree_on_every_default() {
    let golden = load_from_str(GOLDEN, PathPolicy::Windows).expect("golden configuration is valid");
    assert_eq!(golden, windows_config());
}

#[test]
fn config_declares_no_secret_bearing_fields() {
    let rendered = toml::to_string(&windows_config()).expect("serialize configuration");
    let lowered = rendered.to_ascii_lowercase();
    for forbidden in [
        "token",
        "secret",
        "credential",
        "password",
        "api_key",
        "client_id",
        "refresh",
        "oauth",
        "http",
    ] {
        assert!(
            !lowered.contains(forbidden),
            "configuration surface must not expose a `{forbidden}` field"
        );
    }
}

// ---------------------------------------------------------------------------
// Unknown fields and versioning
// ---------------------------------------------------------------------------

#[test]
fn unknown_top_level_field_is_rejected() {
    let text = format!("{MINIMAL}\nunexpected_key = 1\n");
    let err = parse_from_str(&text).expect_err("unknown top-level field must be rejected");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn unknown_nested_field_is_rejected() {
    let text = format!("{MINIMAL}\n[cache]\npage_sizes = 1048576\n");
    let err = parse_from_str(&text).expect_err("unknown nested field must be rejected");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn unknown_section_is_rejected() {
    let text = format!("{MINIMAL}\n[backend]\nprovider = \"drive\"\n");
    let err = parse_from_str(&text).expect_err("unknown section must be rejected");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn missing_format_version_is_rejected() {
    let err = parse_from_str("program_data_root = '/var/lib/miragessd'\n")
        .expect_err("format_version is mandatory");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn unsupported_format_version_is_an_unsupported_layout() {
    let text = "format_version = 2\nprogram_data_root = '/var/lib/miragessd'\n";
    let err =
        load_from_str(text, PathPolicy::Unix).expect_err("future format versions are not guessed");
    assert_eq!(err.code, "MIRAGE_UNSUPPORTED_LAYOUT");
}

// ---------------------------------------------------------------------------
// Invalid sizes
// ---------------------------------------------------------------------------

/// Builds a configuration whose cache section has been mutated by `edit`.
fn cache_config(edit: impl FnOnce(&mut CacheConfig)) -> ServiceConfig {
    let mut config = windows_config();
    edit(&mut config.cache);
    config
}

#[test]
fn page_size_that_is_not_a_power_of_two_is_rejected() {
    let config = cache_config(|cache| cache.page_size = ByteCount::from_u64(3 * 1024 * 1024));
    let err = validate(&config, PathPolicy::Windows).expect_err("page size must be a power of two");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn zero_page_size_is_rejected_without_dividing_by_zero() {
    let config = cache_config(|cache| cache.page_size = ByteCount::ZERO);
    assert!(validate(&config, PathPolicy::Windows).is_err());
}

#[test]
fn out_of_range_page_sizes_are_rejected() {
    let too_small = cache_config(|cache| cache.page_size = ByteCount::from_u64(32 * 1024));
    assert!(validate(&too_small, PathPolicy::Windows).is_err());

    let too_large = cache_config(|cache| {
        cache.page_size = ByteCount::from_u64(32 * 1024 * 1024);
        cache.arena_shard_size = ByteCount::from_u64(32 * 1024 * 1024);
    });
    assert!(validate(&too_large, PathPolicy::Windows).is_err());
}

#[test]
fn arena_shard_size_that_is_not_a_whole_number_of_pages_is_rejected() {
    let config = cache_config(|cache| {
        cache.arena_shard_size = ByteCount::from_u64(8 * 1024 * 1024 * 1024 + 1);
    });
    let err = validate(&config, PathPolicy::Windows)
        .expect_err("a shard must divide into whole page slots");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn budget_smaller_than_the_reserves_is_rejected() {
    let config = cache_config(|cache| {
        cache.budget = ByteCount::from_u64(1024 * 1024 * 1024);
        cache.update_reserve = ByteCount::from_u64(2 * 1024 * 1024 * 1024);
        cache.metadata_reserve = ByteCount::from_u64(512 * 1024 * 1024);
    });
    let err = validate(&config, PathPolicy::Windows)
        .expect_err("budget must cover the metadata and update reserves");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn reserves_leaving_less_than_one_page_are_rejected() {
    let page = 1024 * 1024;
    let config = cache_config(|cache| {
        cache.budget = ByteCount::from_u64(4 * page);
        cache.update_reserve = ByteCount::from_u64(3 * page + 1);
        cache.metadata_reserve = ByteCount::ZERO;
    });
    assert!(validate(&config, PathPolicy::Windows).is_err());
}

#[test]
fn reserve_sum_overflow_is_reported_not_wrapped() {
    let config = cache_config(|cache| {
        cache.update_reserve = ByteCount::from_u64(u64::MAX);
        cache.metadata_reserve = ByteCount::from_u64(1);
    });
    let err = validate_cache(&config.cache).expect_err("reserve overflow must be reported");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
    assert!(err.message.contains("overflow"));
}

#[test]
fn budget_exceeding_the_slot_index_space_is_rejected() {
    // 64 KiB pages over a 256 TiB budget needs 2^32 slots: one more than `SlotIndex` can address.
    let page: u64 = 64 * 1024;
    let config = cache_config(|cache| {
        cache.page_size = ByteCount::from_u64(page);
        cache.arena_shard_size = ByteCount::from_u64(page);
        cache.budget = ByteCount::from_u64((u64::from(u32::MAX) + 1) * page);
    });
    let err = validate(&config, PathPolicy::Windows)
        .expect_err("slot count must fit the 32-bit slot index");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn negative_byte_counts_are_rejected_at_parse_time() {
    let text = format!("{MINIMAL}\n[cache]\nbudget = -1\n");
    let err = parse_from_str(&text).expect_err("a byte count cannot be negative");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn inverted_and_oversized_readahead_windows_are_rejected() {
    let mut inverted = windows_config();
    inverted.scheduler.readahead_window_min = ByteCount::from_u64(16 * 1024 * 1024);
    inverted.scheduler.readahead_window_max = ByteCount::from_u64(2 * 1024 * 1024);
    assert!(validate(&inverted, PathPolicy::Windows).is_err());

    let mut oversized = windows_config();
    oversized.scheduler.readahead_window_max = ByteCount::from_u64(128 * 1024 * 1024);
    assert!(validate(&oversized, PathPolicy::Windows).is_err());
}

#[test]
fn out_of_range_scheduler_and_telemetry_scalars_are_rejected() {
    let mut zero_concurrency = windows_config();
    zero_concurrency.scheduler.max_concurrent_requests = 0;
    assert!(validate(&zero_concurrency, PathPolicy::Windows).is_err());

    let mut excessive_concurrency = windows_config();
    excessive_concurrency.scheduler.max_concurrent_requests = 4096;
    assert!(validate(&excessive_concurrency, PathPolicy::Windows).is_err());

    let mut zero_retention = windows_config();
    zero_retention.telemetry.log_retention_days = 0;
    assert!(validate(&zero_retention, PathPolicy::Windows).is_err());

    let mut zero_log_bytes = windows_config();
    zero_log_bytes.telemetry.max_log_bytes = ByteCount::ZERO;
    assert!(validate(&zero_log_bytes, PathPolicy::Windows).is_err());
}

// ---------------------------------------------------------------------------
// Path policy and traversal
// ---------------------------------------------------------------------------

/// Validates `root` under `policy` via the full configuration path.
fn check_root(root: &str, policy: PathPolicy) -> Result<(), mirage_types::MirageError> {
    let mut config = windows_config();
    config.program_data_root = PathBuf::from(root);
    validate(&config, policy)
}

#[test]
fn windows_policy_accepts_absolute_drive_paths() {
    assert!(check_root(r"C:\ProgramData\MirageSSD", PathPolicy::Windows).is_ok());
    assert!(check_root(r"D:\", PathPolicy::Windows).is_ok());
    assert!(check_root("E:/Mirage/data", PathPolicy::Windows).is_ok());
}

#[test]
fn windows_policy_rejects_traversal_relative_unc_and_device_paths() {
    for rejected in [
        r"C:\ProgramData\..\..\Windows\System32",
        r"C:\ProgramData\.\MirageSSD",
        r"ProgramData\MirageSSD",
        r"\ProgramData\MirageSSD",
        r"\\server\share\MirageSSD",
        r"\\?\C:\ProgramData\MirageSSD",
        r"\\.\PhysicalDrive0",
        r"C:\ProgramData\Mirage:stream",
        r"C:\ProgramData\Mirage*",
        r"C:\ProgramData\Mirage ",
        r"C:\ProgramData\Mirage.",
        "",
    ] {
        assert!(
            check_root(rejected, PathPolicy::Windows).is_err(),
            "windows policy must reject {rejected:?}"
        );
    }
}

#[test]
fn unix_policy_accepts_absolute_posix_paths_and_rejects_traversal() {
    assert!(check_root("/var/lib/miragessd", PathPolicy::Unix).is_ok());
    assert!(check_root("/", PathPolicy::Unix).is_ok());

    for rejected in [
        "/var/lib/../../etc/shadow",
        "/var/./lib",
        "var/lib/miragessd",
        "",
    ] {
        assert!(
            check_root(rejected, PathPolicy::Unix).is_err(),
            "unix policy must reject {rejected:?}"
        );
    }
}

#[test]
fn path_policy_verdicts_do_not_depend_on_the_host_platform() {
    // The same inputs are evaluated under both policies on whichever platform runs the suite, so
    // the Windows and Linux CI jobs assert identical behaviour.
    assert!(check_root(r"C:\ProgramData\MirageSSD", PathPolicy::Windows).is_ok());
    assert!(check_root(r"C:\ProgramData\MirageSSD", PathPolicy::Unix).is_err());
    assert!(check_root("/var/lib/miragessd", PathPolicy::Unix).is_ok());
    assert!(check_root("/var/lib/miragessd", PathPolicy::Windows).is_err());
}

#[test]
fn path_rejection_messages_never_echo_the_path() {
    let err = check_root(r"C:\Users\alice\..\private", PathPolicy::Windows)
        .expect_err("traversal must be rejected");
    assert!(!err.message.contains("alice"));
    assert!(!err.message.contains("private"));
    assert!(!err.to_public_envelope().message.contains("alice"));
}

// ---------------------------------------------------------------------------
// Redacted debug output
// ---------------------------------------------------------------------------

#[test]
fn debug_output_redacts_the_program_data_root() {
    let mut config = windows_config();
    config.program_data_root = PathBuf::from(r"C:\Users\alice\AppData\Local\MirageSSD");

    let rendered = format!("{config:?}");
    assert!(!rendered.contains("alice"));
    assert!(!rendered.contains("AppData"));
    assert!(!rendered.contains(r"C:\"));
    assert!(rendered.contains("<redacted path:"));

    // The non-path sections stay legible so a diagnostic bundle is still useful.
    assert!(rendered.contains("page_size"));
    assert!(rendered.contains("log_retention_days"));
}

// ---------------------------------------------------------------------------
// Developer environment overrides
// ---------------------------------------------------------------------------

#[test]
fn allowlisted_environment_overrides_apply() {
    let mut config = windows_config();
    let env = MapEnv::new()
        .with(ENV_LOG_RETENTION_DAYS, "30")
        .with(ENV_MAX_CONCURRENT_REQUESTS, "64")
        .with(ENV_FIRST_TOUCH_LOGGING, "false");

    apply_env_overrides(&mut config, &env, PathPolicy::Windows).expect("allowlisted overrides");

    assert_eq!(config.telemetry.log_retention_days, 30);
    assert_eq!(config.scheduler.max_concurrent_requests, 64);
    assert!(!config.telemetry.first_touch_logging);
}

#[test]
fn unrecognized_developer_variable_is_rejected() {
    let mut config = windows_config();
    let env = MapEnv::new().with("MIRAGE_DEV_DRIVE_REFRESH_TOKEN", "unused");

    let err = apply_env_overrides(&mut config, &env, PathPolicy::Windows)
        .expect_err("only allowlisted developer overrides are accepted");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
}

#[test]
fn unparseable_override_values_are_rejected() {
    let mut config = windows_config();

    let bad_number = MapEnv::new().with(ENV_LOG_RETENTION_DAYS, "thirty");
    assert!(apply_env_overrides(&mut config, &bad_number, PathPolicy::Windows).is_err());

    let bad_bool = MapEnv::new().with(ENV_FIRST_TOUCH_LOGGING, "TRUE");
    assert!(apply_env_overrides(&mut config, &bad_bool, PathPolicy::Windows).is_err());

    assert_eq!(config, windows_config());
}

#[test]
fn overrides_are_revalidated_before_being_committed() {
    let mut config = windows_config();
    let env = MapEnv::new().with(ENV_LOG_RETENTION_DAYS, "4000");

    let err = apply_env_overrides(&mut config, &env, PathPolicy::Windows)
        .expect_err("overrides must satisfy the same rules as file values");
    assert_eq!(err.code, "MIRAGE_INVALID_ARGUMENT");
    assert_eq!(config.telemetry.log_retention_days, 14);
}

#[test]
fn an_empty_environment_changes_nothing() {
    let mut config = windows_config();
    apply_env_overrides(&mut config, &MapEnv::new(), PathPolicy::Windows).expect("no overrides");
    assert_eq!(config, windows_config());
}

// ---------------------------------------------------------------------------
// File loading
// ---------------------------------------------------------------------------

#[test]
fn load_from_path_reads_parses_and_validates() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("mirage.toml");
    let root = if cfg!(windows) {
        r"C:\ProgramData\MirageSSD"
    } else {
        "/var/lib/miragessd"
    };
    std::fs::write(
        &path,
        format!("format_version = 1\nprogram_data_root = '{root}'\n"),
    )
    .expect("write configuration");

    let config = load_from_path(&path).expect("load configuration from disk");
    assert_eq!(config.program_data_root, PathBuf::from(root));
    assert_eq!(config.cache, CacheConfig::default());
}

#[test]
fn load_from_path_reports_a_missing_file_as_io_without_leaking_the_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("does-not-exist.toml");

    let err = load_from_path(&path).expect_err("missing configuration file");
    assert_eq!(err.code, "MIRAGE_IO_ERROR");
    assert!(!err.message.contains("does-not-exist"));
}
