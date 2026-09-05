//! Property-based tests for range arithmetic, overflow boundaries, page splitting, and canonical hex parsing.

use proptest::prelude::*;

use mirage_types::hash::{CommitHash, ManifestHash, PageHash};
use mirage_types::id::{
    CapsuleId, DeviceId, PackId, RepositoryId, SessionId, StableFileId, UpdateId,
};
use mirage_types::range::CheckedRange;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn prop_range_overflow_boundary(start in 0u64..=u64::MAX, len in 0u64..=u64::MAX) {
        let would_overflow = start.checked_add(len).is_none();
        let result = CheckedRange::new(start, len);
        if would_overflow {
            prop_assert!(result.is_err());
            prop_assert!(CheckedRange::try_new(start, len).is_none());
        } else {
            prop_assert!(result.is_ok());
            let r = result.unwrap();
            prop_assert_eq!(r.start(), start);
            prop_assert_eq!(r.len(), len);
            prop_assert_eq!(r.end_exclusive(), start + len);
        }
    }

    #[test]
    fn prop_empty_range_properties(start in 0u64..=u64::MAX, test_offset in 0u64..=u64::MAX) {
        let empty_range = CheckedRange::empty(start);
        prop_assert!(empty_range.is_empty());
        prop_assert_eq!(empty_range.len(), 0);
        prop_assert_eq!(empty_range.end_exclusive(), start);
        prop_assert!(!empty_range.contains_offset(test_offset));
    }

    #[test]
    fn prop_eof_clipping(
        start in 0u64..100_000_000,
        len in 0u64..100_000_000,
        file_size in 0u64..200_000_000
    ) {
        let range = CheckedRange::new(start, len).unwrap();
        let clipped = range.clip_to_eof(file_size);

        prop_assert!(clipped.end_exclusive() <= file_size);
        prop_assert!(clipped.start() <= file_size);

        if start >= file_size {
            prop_assert!(clipped.is_empty());
            prop_assert_eq!(clipped.start(), file_size);
        } else if range.end_exclusive() <= file_size {
            prop_assert_eq!(clipped, range);
        } else {
            prop_assert_eq!(clipped.start(), start);
            prop_assert_eq!(clipped.end_exclusive(), file_size);
            prop_assert_eq!(clipped.len(), file_size - start);
        }
    }

    #[test]
    fn prop_page_splitting(
        start in 0u64..50_000_000,
        len in 1u64..10_000_000,
        page_size in 1024u64..=4_194_304
    ) {
        let range = CheckedRange::new(start, len).unwrap();
        let slices = range.split_to_pages(page_size).unwrap();

        prop_assert!(!slices.is_empty());

        let total_bytes: u64 = slices.iter().map(|s| s.length as u64).sum();
        prop_assert_eq!(total_bytes, len);

        prop_assert_eq!(slices.first().unwrap().file_offset.as_u64(), start);

        for (i, slice) in slices.iter().enumerate() {
            prop_assert!(slice.length > 0);
            prop_assert!((slice.offset_in_page as u64) + (slice.length as u64) <= page_size);

            if i > 0 {
                let prev = &slices[i - 1];
                prop_assert_eq!(slice.page.as_u32(), prev.page.as_u32() + 1);
                prop_assert_eq!(
                    slice.file_offset.as_u64(),
                    prev.file_offset.as_u64() + (prev.length as u64)
                );
                prop_assert_eq!(slice.offset_in_page, 0);
            }
        }
    }

    #[test]
    fn prop_hash32_hex_round_trip(bytes in proptest::array::uniform32(0u8..=255)) {
        let c_hash = CommitHash::from_bytes(bytes);
        let c_hex = c_hash.to_string();
        prop_assert_eq!(c_hex.len(), 64);
        prop_assert!(c_hex.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        let c_parsed = CommitHash::from_canonical_hex(&c_hex).unwrap();
        prop_assert_eq!(c_parsed, c_hash);

        let m_hash = ManifestHash::from_bytes(bytes);
        let m_hex = m_hash.to_string();
        let m_parsed = ManifestHash::from_canonical_hex(&m_hex).unwrap();
        prop_assert_eq!(m_parsed, m_hash);

        let p_hash = PageHash::from_bytes(bytes);
        let p_hex = p_hash.to_string();
        let p_parsed = PageHash::from_canonical_hex(&p_hex).unwrap();
        prop_assert_eq!(p_parsed, p_hash);
    }

    #[test]
    fn prop_id128_hex_round_trip(bytes in proptest::array::uniform16(0u8..=255)) {
        let repo_id = RepositoryId::from_bytes(bytes);
        let r_hex = repo_id.to_string();
        prop_assert_eq!(r_hex.len(), 32);
        prop_assert!(r_hex.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        let r_parsed = RepositoryId::from_canonical_hex(&r_hex).unwrap();
        prop_assert_eq!(r_parsed, repo_id);

        let pack_id = PackId::from_bytes(bytes);
        let p_parsed = PackId::from_canonical_hex(&pack_id.to_string()).unwrap();
        prop_assert_eq!(p_parsed, pack_id);

        let capsule_id = CapsuleId::from_bytes(bytes);
        let cap_parsed = CapsuleId::from_canonical_hex(&capsule_id.to_string()).unwrap();
        prop_assert_eq!(cap_parsed, capsule_id);

        let session_id = SessionId::from_bytes(bytes);
        let s_parsed = SessionId::from_canonical_hex(&session_id.to_string()).unwrap();
        prop_assert_eq!(s_parsed, session_id);

        let update_id = UpdateId::from_bytes(bytes);
        let u_parsed = UpdateId::from_canonical_hex(&update_id.to_string()).unwrap();
        prop_assert_eq!(u_parsed, update_id);

        let device_id = DeviceId::from_bytes(bytes);
        let d_parsed = DeviceId::from_canonical_hex(&device_id.to_string()).unwrap();
        prop_assert_eq!(d_parsed, device_id);
    }

    #[test]
    fn prop_stable_file_id_hex_round_trip(val in 0u64..=u64::MAX) {
        let id = StableFileId::from_u64(val);
        let hex = id.to_canonical_hex();
        prop_assert_eq!(hex.len(), 16);
        let parsed = StableFileId::from_canonical_hex(&hex).unwrap();
        prop_assert_eq!(parsed, id);
    }
}
