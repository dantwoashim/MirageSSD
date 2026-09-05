mod support;

use std::panic::{AssertUnwindSafe, catch_unwind};

use mirage_index::{Header, MountIndex, SectionKind, compile_to_bytes};

use support::complex_manifest;

#[test]
fn mapped_and_owned_readers_expose_exact_constant_time_views() {
    let bytes = compile_to_bytes(&complex_manifest()).expect("compile index");
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = temporary.path().join("mount.midx");
    std::fs::write(&path, &bytes).expect("index file");
    let mapped = MountIndex::open(&path).expect("mapped index");
    let owned = MountIndex::from_bytes(bytes).expect("owned index");
    for index in [&mapped, &owned] {
        assert_eq!(index.directory_count(), 2);
        assert_eq!(index.file_count(), 2);
        assert_eq!(index.extent_count(), 2);
        assert_eq!(index.page_count(), 2);
        assert_eq!(
            index
                .page_by_ordinal(1)
                .expect("tail page")
                .logical_length(),
            333
        );
        assert_eq!(
            index.file_by_index(0).expect("file").name().expect("name"),
            "settings.toml"
        );
    }
}

#[test]
fn truncation_and_authenticated_semantic_corruption_return_errors_not_panics() {
    let valid = compile_to_bytes(&complex_manifest()).expect("compile index");
    for length in (0..valid.len()).step_by(17) {
        let bytes = valid[..length].to_vec();
        let result = catch_unwind(AssertUnwindSafe(|| MountIndex::from_bytes(bytes)));
        assert!(result.is_ok(), "panic at length {length}");
        assert!(result.expect("no panic").is_err());
    }

    let header = Header::parse(&valid).expect("header");
    let mut foreign_parent = valid.clone();
    let directories = header.section(SectionKind::Directories);
    set_u32(
        &mut foreign_parent,
        directories.offset as usize + 56,
        u32::MAX,
    );
    rehash(&mut foreign_parent);

    let mut invalid_utf8 = valid.clone();
    let strings = header.section(SectionKind::Strings);
    invalid_utf8[strings.offset as usize] = 0xFF;
    rehash(&mut invalid_utf8);

    let mut remote_overflow = valid.clone();
    let locations = header.section(SectionKind::RemoteLocations);
    set_u64(
        &mut remote_overflow,
        locations.offset as usize + 88,
        u64::MAX,
    );
    rehash(&mut remote_overflow);

    for bytes in [foreign_parent, invalid_utf8, remote_overflow] {
        let result = catch_unwind(AssertUnwindSafe(|| MountIndex::from_bytes(bytes)));
        assert!(result.is_ok());
        assert!(result.expect("no panic").is_err());
    }
}

#[test]
fn any_unauthenticated_body_change_fails_whole_index_hash() {
    let mut bytes = compile_to_bytes(&complex_manifest()).expect("compile index");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(MountIndex::from_bytes(bytes).is_err());
}

fn rehash(bytes: &mut [u8]) {
    bytes[88..120].fill(0);
    let hash = Header::compute_index_hash(bytes).expect("compute hash");
    bytes[88..120].copy_from_slice(&hash);
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
