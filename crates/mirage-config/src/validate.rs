//! Pure validation of [`ServiceConfig`] values.
//!
//! Validation performs no filesystem, network, or environment access. Path rules are purely
//! syntactic and are selected by an explicit [`PathPolicy`] rather than by `cfg!(windows)` at the
//! point of use, so the same fixture produces the same verdict on every host. That keeps the
//! Windows and Linux CI jobs in agreement about what a valid configuration is.

use std::path::Path;

use mirage_types::{ByteCount, MirageError};

use crate::model::{
    CONFIG_FORMAT_VERSION, CacheConfig, SchedulerConfig, ServiceConfig, TelemetryConfig,
};

/// Smallest accepted cache page size.
pub const PAGE_SIZE_MIN: ByteCount = ByteCount::from_u64(64 * 1024);
/// Largest accepted cache page size.
pub const PAGE_SIZE_MAX: ByteCount = ByteCount::from_u64(16 * 1024 * 1024);
/// Largest accepted concurrent backend request limit.
pub const MAX_CONCURRENT_REQUESTS_MAX: u32 = 1024;
/// Largest accepted log retention window, in days.
pub const LOG_RETENTION_DAYS_MAX: u32 = 365;
/// Largest accepted retained log ceiling.
pub const MAX_LOG_BYTES_MAX: ByteCount = ByteCount::from_u64(64 * 1024 * 1024 * 1024);

/// Characters that may not appear inside a Windows path component.
///
/// `:` is excluded from components because the only legal colon in an accepted path is the drive
/// separator; a component-level colon denotes an NTFS alternate data stream.
const WINDOWS_FORBIDDEN_COMPONENT_CHARS: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];

/// Which platform's path syntax a configuration is validated against.
///
/// The policy is an explicit parameter so a Windows-shaped configuration can be validated on a
/// Linux build machine and vice versa. Only [`PathPolicy::host`] consults the compilation target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathPolicy {
    /// Absolute drive-letter paths; rejects UNC shares and the device namespace.
    Windows,
    /// Absolute POSIX paths rooted at `/`.
    Unix,
}

impl PathPolicy {
    /// Returns the policy matching the platform this build targets.
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Constructs an `InvalidArgument` error.
fn invalid(message: impl Into<String>) -> MirageError {
    MirageError::invalid_argument(message)
}

/// Validates an entire configuration against the supplied path policy.
///
/// # Errors
///
/// Returns `MIRAGE_UNSUPPORTED_LAYOUT` when `format_version` is not [`CONFIG_FORMAT_VERSION`],
/// and `MIRAGE_INVALID_ARGUMENT` for every other rule violation.
pub fn validate(config: &ServiceConfig, policy: PathPolicy) -> Result<(), MirageError> {
    if config.format_version != CONFIG_FORMAT_VERSION {
        return Err(MirageError::unsupported_layout(format!(
            "unsupported configuration format_version {}: this build supports exactly version {}",
            config.format_version, CONFIG_FORMAT_VERSION
        )));
    }

    validate_program_data_root(&config.program_data_root, policy)?;
    validate_cache(&config.cache)?;
    validate_scheduler(&config.scheduler, config.cache.page_size)?;
    validate_telemetry(&config.telemetry)
}

/// Validates the cache arena section in isolation.
///
/// # Errors
///
/// Returns `MIRAGE_INVALID_ARGUMENT` when the page size is not a power of two or is out of range,
/// when the arena shard size or budget is not a whole number of pages, when the derived slot count
/// exceeds the 32-bit slot index space, or when the reserves do not leave at least one usable page.
pub fn validate_cache(cache: &CacheConfig) -> Result<(), MirageError> {
    let page = cache.page_size.as_u64();
    if !page.is_power_of_two() {
        return Err(invalid(format!(
            "cache.page_size ({page} bytes) must be a power of two"
        )));
    }
    if cache.page_size < PAGE_SIZE_MIN || cache.page_size > PAGE_SIZE_MAX {
        return Err(invalid(format!(
            "cache.page_size ({page} bytes) must be between {} and {} bytes",
            PAGE_SIZE_MIN.as_u64(),
            PAGE_SIZE_MAX.as_u64()
        )));
    }

    // Every arena slot holds exactly one cache page (architecture section 8.2), so a shard must
    // divide into whole slots with no unaddressable tail.
    let shard = cache.arena_shard_size.as_u64();
    if shard == 0 || !shard.is_multiple_of(page) {
        return Err(invalid(format!(
            "cache.arena_shard_size ({shard} bytes) must be a non-zero multiple of \
             cache.page_size ({page} bytes)"
        )));
    }

    let budget = cache.budget.as_u64();
    if budget == 0 || !budget.is_multiple_of(page) {
        return Err(invalid(format!(
            "cache.budget ({budget} bytes) must be a non-zero multiple of cache.page_size \
             ({page} bytes) so no allocation-rounding bytes escape the hard envelope"
        )));
    }

    let slots = budget / page;
    if slots > u64::from(u32::MAX) {
        return Err(invalid(format!(
            "cache.budget ({budget} bytes) yields {slots} slots, which exceeds the {} addressable \
             slot indices",
            u32::MAX
        )));
    }

    let reserved = cache
        .update_reserve
        .checked_add(cache.metadata_reserve)
        .ok_or_else(|| {
            invalid(format!(
                "cache.update_reserve ({} bytes) + cache.metadata_reserve ({} bytes) overflows a \
                 64-bit byte count",
                cache.update_reserve.as_u64(),
                cache.metadata_reserve.as_u64()
            ))
        })?;

    let usable = cache.budget.checked_sub(reserved).ok_or_else(|| {
        invalid(format!(
            "cache.budget ({budget} bytes) is smaller than cache.update_reserve + \
             cache.metadata_reserve ({} bytes)",
            reserved.as_u64()
        ))
    })?;

    if usable < cache.page_size {
        return Err(invalid(format!(
            "cache.budget ({budget} bytes) minus reserves ({} bytes) leaves {} bytes, which is \
             less than one {page}-byte page",
            reserved.as_u64(),
            usable.as_u64()
        )));
    }

    Ok(())
}

/// Validates the scheduler section against the configured page size.
///
/// # Errors
///
/// Returns `MIRAGE_INVALID_ARGUMENT` when the concurrency limit is out of range or when a fetch
/// window is not a whole number of pages, is inverted, or exceeds the in-flight byte ceiling.
pub fn validate_scheduler(
    scheduler: &SchedulerConfig,
    page_size: ByteCount,
) -> Result<(), MirageError> {
    let page = page_size.as_u64();
    if page == 0 {
        return Err(invalid("cache.page_size must be non-zero"));
    }

    if scheduler.max_concurrent_requests == 0
        || scheduler.max_concurrent_requests > MAX_CONCURRENT_REQUESTS_MAX
    {
        return Err(invalid(format!(
            "scheduler.max_concurrent_requests ({}) must be between 1 and \
             {MAX_CONCURRENT_REQUESTS_MAX}",
            scheduler.max_concurrent_requests
        )));
    }

    if scheduler.max_inflight_bytes < page_size {
        return Err(invalid(format!(
            "scheduler.max_inflight_bytes ({} bytes) must be at least one {page}-byte page",
            scheduler.max_inflight_bytes.as_u64()
        )));
    }

    for (name, window) in [
        ("readahead_window_min", scheduler.readahead_window_min),
        ("readahead_window_max", scheduler.readahead_window_max),
    ] {
        let bytes = window.as_u64();
        if bytes == 0 || !bytes.is_multiple_of(page) {
            return Err(invalid(format!(
                "scheduler.{name} ({bytes} bytes) must be a non-zero multiple of cache.page_size \
                 ({page} bytes)"
            )));
        }
    }

    if scheduler.readahead_window_min > scheduler.readahead_window_max {
        return Err(invalid(format!(
            "scheduler.readahead_window_min ({} bytes) must not exceed \
             scheduler.readahead_window_max ({} bytes)",
            scheduler.readahead_window_min.as_u64(),
            scheduler.readahead_window_max.as_u64()
        )));
    }

    if scheduler.readahead_window_max > scheduler.max_inflight_bytes {
        return Err(invalid(format!(
            "scheduler.readahead_window_max ({} bytes) must not exceed \
             scheduler.max_inflight_bytes ({} bytes)",
            scheduler.readahead_window_max.as_u64(),
            scheduler.max_inflight_bytes.as_u64()
        )));
    }

    Ok(())
}

/// Validates the telemetry section in isolation.
///
/// # Errors
///
/// Returns `MIRAGE_INVALID_ARGUMENT` when retention or the log ceiling is zero or out of range.
pub fn validate_telemetry(telemetry: &TelemetryConfig) -> Result<(), MirageError> {
    if telemetry.log_retention_days == 0 || telemetry.log_retention_days > LOG_RETENTION_DAYS_MAX {
        return Err(invalid(format!(
            "telemetry.log_retention_days ({}) must be between 1 and {LOG_RETENTION_DAYS_MAX}",
            telemetry.log_retention_days
        )));
    }

    if telemetry.max_log_bytes.is_zero() || telemetry.max_log_bytes > MAX_LOG_BYTES_MAX {
        return Err(invalid(format!(
            "telemetry.max_log_bytes ({} bytes) must be between 1 and {} bytes",
            telemetry.max_log_bytes.as_u64(),
            MAX_LOG_BYTES_MAX.as_u64()
        )));
    }

    Ok(())
}

/// Validates `program_data_root` syntactically against the supplied policy.
///
/// Error messages never echo the path itself, so a validation failure cannot copy a user profile
/// directory into logs, diagnostic bundles, or public error envelopes.
///
/// # Errors
///
/// Returns `MIRAGE_INVALID_ARGUMENT` when the path is empty, not valid UTF-8, not absolute,
/// contains a relative component, or uses syntax the policy forbids.
pub fn validate_program_data_root(root: &Path, policy: PathPolicy) -> Result<(), MirageError> {
    let raw = root
        .to_str()
        .ok_or_else(|| invalid("program_data_root must be valid UTF-8"))?;

    if raw.is_empty() {
        return Err(invalid("program_data_root must not be empty"));
    }
    if raw.contains('\0') {
        return Err(invalid("program_data_root must not contain a NUL byte"));
    }

    match policy {
        PathPolicy::Windows => validate_windows_root(raw),
        PathPolicy::Unix => validate_unix_root(raw),
    }
}

/// Returns `true` for the two characters Windows accepts as path separators.
const fn is_windows_separator(c: char) -> bool {
    c == '\\' || c == '/'
}

/// Applies the Windows path rules to an already non-empty, NUL-free path string.
fn validate_windows_root(raw: &str) -> Result<(), MirageError> {
    let bytes = raw.as_bytes();

    // `\\server\share`, `\\?\C:\...`, and `\\.\PhysicalDrive0` all start with two separators.
    // None of them is an acceptable service root: verbatim and device paths bypass the very
    // normalization rules this function exists to enforce.
    if bytes.len() >= 2
        && is_windows_separator(bytes[0] as char)
        && is_windows_separator(bytes[1] as char)
    {
        return Err(invalid(
            "program_data_root must not be a UNC share or a device-namespace path",
        ));
    }

    let has_drive_root = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && is_windows_separator(bytes[2] as char);
    if !has_drive_root {
        return Err(invalid(
            "program_data_root must be an absolute drive-letter path, for example \
             C:\\ProgramData\\MirageSSD",
        ));
    }

    // The first three bytes are ASCII, so slicing at index 3 is on a character boundary.
    for component in raw[3..].split(is_windows_separator) {
        if component.is_empty() {
            continue;
        }
        reject_relative_component(component)?;
        if component.ends_with(' ') || component.ends_with('.') {
            return Err(invalid(
                "program_data_root components must not end with a space or a period",
            ));
        }
        if component
            .chars()
            .any(|c| WINDOWS_FORBIDDEN_COMPONENT_CHARS.contains(&c) || c.is_control())
        {
            return Err(invalid(
                "program_data_root contains a character that is not permitted in a Windows path \
                 component (< > : \" | ? * or a control character)",
            ));
        }
    }

    Ok(())
}

/// Applies the POSIX path rules to an already non-empty, NUL-free path string.
fn validate_unix_root(raw: &str) -> Result<(), MirageError> {
    if !raw.starts_with('/') {
        return Err(invalid(
            "program_data_root must be an absolute path beginning with '/'",
        ));
    }

    for component in raw.split('/') {
        if component.is_empty() {
            continue;
        }
        reject_relative_component(component)?;
    }

    Ok(())
}

/// Rejects `.` and `..` components, which would let a configured root escape its declared location.
fn reject_relative_component(component: &str) -> Result<(), MirageError> {
    if component == "." || component == ".." {
        return Err(invalid(
            "program_data_root must not contain a relative '.' or '..' component",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_default_sections_validate() {
        let cache = CacheConfig::default();
        assert!(validate_cache(&cache).is_ok());
        assert!(validate_scheduler(&SchedulerConfig::default(), cache.page_size).is_ok());
        assert!(validate_telemetry(&TelemetryConfig::default()).is_ok());
    }

    #[test]
    fn test_windows_and_unix_policies_are_host_independent() {
        let windows_root = PathBuf::from(r"C:\ProgramData\MirageSSD");
        let unix_root = PathBuf::from("/var/lib/miragessd");

        assert!(validate_program_data_root(&windows_root, PathPolicy::Windows).is_ok());
        assert!(validate_program_data_root(&windows_root, PathPolicy::Unix).is_err());
        assert!(validate_program_data_root(&unix_root, PathPolicy::Unix).is_ok());
        assert!(validate_program_data_root(&unix_root, PathPolicy::Windows).is_err());
    }

    #[test]
    fn test_root_error_messages_never_echo_the_path() {
        let root = PathBuf::from(r"C:\Users\alice\..\secret");
        let err = validate_program_data_root(&root, PathPolicy::Windows)
            .expect_err("relative component must be rejected");
        assert!(!err.message.contains("alice"));
        assert!(!err.message.contains("secret"));
    }
}
