mod support;

use mirage_index::{MountIndex, NodeIndex, compile_to_bytes, resolve_range};
use mirage_manifest::{ExtentRecord, PageRecord, validate_manifest};
use mirage_types::{ByteCount, PageHash};
use proptest::prelude::*;

use support::{complex_manifest, namespace_manifest};

#[test]
fn exact_boundaries_tail_eof_and_zero_length_reads() {
    let index = mounted_complex();
    let file = virtual_file(&index);
    assert!(resolve_range(file, 0, 0).expect("zero read").is_empty());
    assert!(
        resolve_range(file, file.logical_size(), 10)
            .expect("EOF read")
            .is_empty()
    );
    let tail = resolve_range(file, 1024 * 1024 - 2, 400).expect("tail-spanning read");
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].page_ordinal.as_u32(), 0);
    assert_eq!(tail[0].page_offset, 1024 * 1024 - 2);
    assert_eq!(tail[0].len, 2);
    assert_eq!(tail[1].page_ordinal.as_u32(), 1);
    assert_eq!(tail[1].page_offset, 0);
    assert_eq!(tail[1].len, 333);
    assert_eq!(tail[1].dst_offset, 2);
}

#[test]
fn reads_spanning_thousands_of_pages_remain_exact_and_bounded() {
    let mut manifest = complex_manifest();
    let page_size = manifest.page_size.as_u64();
    let page_count = 3_000_u32;
    manifest.files[1].logical_size = ByteCount::from_u64(page_size * u64::from(page_count));
    manifest.files[1].extent_count = 1;
    manifest.extents = vec![ExtentRecord {
        logical_offset: 0,
        logical_length: manifest.files[1].logical_size,
        page_start: 0,
        page_count,
    }];
    manifest.pages = (0..page_count)
        .map(|ordinal| PageRecord {
            plaintext_hash: PageHash::from_bytes([ordinal as u8; 32]),
            logical_length: u32::try_from(page_size).expect("page size fits u32"),
            remote_location: 0,
        })
        .collect();
    manifest.remote_locations.truncate(1);
    validate_manifest(&manifest).expect("large valid manifest");
    let index = MountIndex::from_bytes(compile_to_bytes(&manifest).expect("compile large index"))
        .expect("mount large index");
    let file = index.file_by_index(1).expect("virtual file");
    let requested = usize::try_from(page_size * u64::from(page_count))
        .expect("test address space supports requested length");
    let spans = resolve_range(file, 0, requested).expect("resolve large read");
    assert_eq!(spans.len(), page_count as usize);
    assert_eq!(
        spans.last().expect("last span").page_ordinal.as_u32(),
        page_count - 1
    );
}

#[test]
fn overflowing_read_range_is_rejected() {
    let index = mounted_complex();
    let file = virtual_file(&index);
    assert!(resolve_range(file, u64::MAX, 2).is_err());
}

proptest! {
    #[test]
    fn resolution_matches_contiguous_byte_oracle(offset in 0_u64..1_051_000, length in 0_usize..2_100_000) {
        let index = mounted_complex();
        let file = virtual_file(&index);
        let spans = resolve_range(file, offset, length).expect("resolve property range");
        let expected = if offset >= file.logical_size() {
            0
        } else {
            u64::try_from(length)
                .expect("length fits")
                .min(file.logical_size() - offset)
        };
        let mut cursor = 0_u64;
        for span in &spans {
            prop_assert_eq!(u64::from(span.dst_offset), cursor);
            prop_assert!(span.len > 0);
            cursor += u64::from(span.len);
        }
        prop_assert_eq!(cursor, expected);
    }
}

fn mounted_complex() -> MountIndex {
    MountIndex::from_bytes(compile_to_bytes(&namespace_manifest()).expect("compile index"))
        .expect("mount index")
}

fn virtual_file(index: &MountIndex) -> mirage_index::FileView<'_> {
    let ordinal = match index
        .lookup_path("assets/世界.pak")
        .expect("lookup virtual file")
    {
        Some(NodeIndex::File(ordinal)) => ordinal,
        other => panic!("expected virtual file, got {other:?}"),
    };
    index.file_by_index(ordinal).expect("virtual file view")
}
