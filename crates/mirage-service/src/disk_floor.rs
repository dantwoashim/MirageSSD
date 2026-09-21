//! Per-disk free-space floor decisions and volume-root normalization.

use mirage_types::MirageError;

pub const GIB: u64 = 1 << 30;

/// Default hysteresis: the larger of 1 GiB and 5% of the floor.
pub fn default_hysteresis(floor_bytes: u64) -> u64 {
    GIB.max(floor_bytes / 20)
}

/// Bytes to reclaim so `free` reaches `floor + hysteresis`; `None` when the
/// floor is not breached.
pub fn reclaim_target(free: u64, floor: u64, hysteresis: u64) -> Option<u64> {
    if free >= floor {
        return None;
    }
    Some(floor.saturating_add(hysteresis) - free)
}

/// Normalizes `D:`, `D:\`, or `d:\` to `D:\`. Drive letters only.
pub fn normalize_volume_root(input: &str) -> Result<String, MirageError> {
    let trimmed = input.trim().trim_end_matches(['\\', '/']);
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:\\", letter.to_ascii_uppercase()))
        }
        _ => Err(MirageError::invalid_argument(
            "disk floor applies to a drive root such as D:",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_target_boundary() {
        assert_eq!(reclaim_target(100, 100, 10), None);
        assert_eq!(reclaim_target(99, 100, 10), Some(11));
        assert_eq!(reclaim_target(0, 100, 10), Some(110));
    }

    #[test]
    fn reclaim_target_overflow_safe() {
        assert_eq!(reclaim_target(0, u64::MAX, 1), Some(u64::MAX));
        // floor+hysteresis saturates at u64::MAX rather than wrapping.
        assert_eq!(reclaim_target(u64::MAX - 1, u64::MAX, 5), Some(1));
        assert_eq!(reclaim_target(0, u64::MAX - 4, 4), Some(u64::MAX));
    }

    #[test]
    fn default_hysteresis_bounds() {
        assert_eq!(default_hysteresis(1), GIB);
        assert_eq!(default_hysteresis(40 * GIB), 2 * GIB);
    }

    #[test]
    fn normalize_roots() {
        assert_eq!(normalize_volume_root("D:").unwrap(), "D:\\");
        assert_eq!(normalize_volume_root("d:\\").unwrap(), "D:\\");
        assert_eq!(normalize_volume_root("E:/").unwrap(), "E:\\");
        assert!(normalize_volume_root("D:\\data").is_err());
        assert!(normalize_volume_root("").is_err());
        assert!(normalize_volume_root("1:").is_err());
    }
}
