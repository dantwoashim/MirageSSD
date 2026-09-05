use std::collections::BTreeSet;

use mirage_index::{MountIndex, compile_to_bytes};
use mirage_manifest::{DecodeLimits, RepositoryManifest, decode_manifest_bounded};
use mirage_predictor::{TouchKind, TraceEvent, normalize_trace};
use proptest::prelude::*;

const MANIFEST: &[u8] =
    include_bytes!("../../mirage-manifest/tests/fixtures/manifest-v2-complex.cbor");

fn mounted() -> (RepositoryManifest, MountIndex, u32) {
    let manifest = decode_manifest_bounded(MANIFEST, DecodeLimits::default()).expect("manifest");
    let index =
        MountIndex::from_bytes(compile_to_bytes(&manifest).expect("compile")).expect("index");
    let ordinal = (0..index.file_count() as u32)
        .find(|ordinal| index.file_by_index(*ordinal).expect("file").extent_count() > 0)
        .expect("virtual file");
    (manifest, index, ordinal)
}

proptest! {
    #[test]
    fn normalized_unique_pages_match_independent_oracle(
        reads in prop::collection::vec((0_u64..3_000_000, 1_u32..1_500_000), 1..200)
    ) {
        let (_manifest, index, ordinal) = mounted();
        let file = index.file_by_index(ordinal).expect("file");
        let events = reads.iter().enumerate().map(|(event, (offset, length))| TraceEvent {
            timestamp_ns: event as u64,
            stable_file_id: file.stable_id(),
            offset: *offset,
            length: *length,
            flags: 0,
        }).collect::<Vec<_>>();
        let normalized = normalize_trace(&index, &events, None).expect("normalize");
        let actual = normalized.touches.iter().map(|touch| {
            prop_assert_eq!(touch.kind, TouchKind::First);
            Ok(touch.page_ordinal.as_u32())
        }).collect::<Result<BTreeSet<_>, TestCaseError>>()?;
        let mut expected = BTreeSet::new();
        for (offset, length) in reads {
            if offset >= file.logical_size() { continue; }
            let end = offset.saturating_add(u64::from(length)).min(file.logical_size());
            for relative_extent in 0..file.extent_count() {
                let extent = file.extent(relative_extent).expect("extent");
                let extent_end = extent.logical_offset() + extent.logical_length();
                let begin = offset.max(extent.logical_offset());
                let finish = end.min(extent_end);
                if begin >= finish { continue; }
                let first = (begin - extent.logical_offset()) / file.page_size();
                let last = (finish - 1 - extent.logical_offset()) / file.page_size();
                for page in first..=last { expected.insert(extent.page_start() + page as u32); }
            }
        }
        prop_assert_eq!(actual, expected);
    }
}

#[test]
fn quality_and_repeat_sampling_are_explicit_and_original_is_preserved() {
    let (_, index, ordinal) = mounted();
    let file = index.file_by_index(ordinal).expect("file");
    let events = vec![
        TraceEvent {
            timestamp_ns: 10,
            stable_file_id: file.stable_id(),
            offset: 0,
            length: 1,
            flags: 0,
        },
        TraceEvent {
            timestamp_ns: 9,
            stable_file_id: file.stable_id(),
            offset: 0,
            length: 1,
            flags: 0,
        },
        TraceEvent {
            timestamp_ns: 11,
            stable_file_id: file.stable_id(),
            offset: 0,
            length: 1,
            flags: 0,
        },
        TraceEvent {
            timestamp_ns: 12,
            stable_file_id: mirage_types::StableFileId::from_u64(u64::MAX),
            offset: 0,
            length: 1,
            flags: 0,
        },
        TraceEvent {
            timestamp_ns: 13,
            stable_file_id: file.stable_id(),
            offset: file.logical_size(),
            length: 1,
            flags: 0,
        },
        TraceEvent {
            timestamp_ns: 14,
            stable_file_id: file.stable_id(),
            offset: 0,
            length: 0,
            flags: 0,
        },
    ];
    let result = normalize_trace(&index, &events, Some(2)).expect("normalize");
    assert_eq!(result.original_events, events);
    assert_eq!(
        result
            .touches
            .iter()
            .filter(|touch| touch.kind == TouchKind::First)
            .count(),
        1
    );
    assert_eq!(
        result
            .touches
            .iter()
            .filter(|touch| touch.kind == TouchKind::SampledRepeat)
            .count(),
        1
    );
    assert_eq!(result.quality.non_monotonic_timestamps, 1);
    assert_eq!(result.quality.unknown_files, 1);
    assert_eq!(result.quality.invalid_ranges, 1);
    assert_eq!(result.quality.zero_length_reads, 1);
}
