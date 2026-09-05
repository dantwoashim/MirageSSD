mod support;

use mirage_manifest::{FileClass, validate_manifest};
use mirage_types::{ByteCount, StableFileId};

use support::{PAGE_SIZE, complex_manifest};

#[test]
fn valid_manifest_has_exact_summary() {
    let manifest = complex_manifest();
    validate_manifest(&manifest).expect("valid complex manifest");
    let summary = manifest.summary().expect("valid summary");
    assert_eq!(summary.total_logical_bytes.as_u64(), PAGE_SIZE + 333 + 42);
    assert_eq!(summary.native_logical_bytes.as_u64(), 42);
    assert_eq!(summary.virtual_logical_bytes.as_u64(), PAGE_SIZE + 333);
    assert_eq!(summary.unique_page_count, 2);
}

#[test]
fn directory_graph_requires_one_root_and_acyclic_parents() {
    let mut no_root = complex_manifest();
    no_root.directories[0].parent = Some(1);
    assert!(validate_manifest(&no_root).is_err());

    let mut cycle = complex_manifest();
    cycle.directories.push(mirage_manifest::DirectoryRecord {
        parent: Some(1),
        name: "Nested".to_string(),
    });
    cycle.directories[1].parent = Some(2);
    assert!(validate_manifest(&cycle).is_err());
}

#[test]
fn windows_case_collisions_and_unsafe_components_are_rejected() {
    let mut directory_collision = complex_manifest();
    directory_collision
        .directories
        .push(mirage_manifest::DirectoryRecord {
            parent: Some(0),
            name: "assets".to_string(),
        });
    assert!(validate_manifest(&directory_collision).is_err());

    let mut file_collision = complex_manifest();
    let mut duplicate = file_collision.files[0].clone();
    duplicate.name = "SETTINGS.TOML".to_string();
    duplicate.stable_id = StableFileId::from_u64(100);
    file_collision.files.push(duplicate);
    assert!(validate_manifest(&file_collision).is_err());

    let mut traversal = complex_manifest();
    traversal.files[0].name = "..".to_string();
    assert!(validate_manifest(&traversal).is_err());
}

#[test]
fn virtual_extents_must_cover_bytes_without_gaps_or_overlap() {
    let mut gap = complex_manifest();
    gap.extents[1].logical_offset += 1;
    assert!(validate_manifest(&gap).is_err());

    let mut overlap = complex_manifest();
    overlap.extents[1].logical_offset -= 1;
    assert!(validate_manifest(&overlap).is_err());

    let mut incomplete = complex_manifest();
    incomplete.files[1].logical_size = ByteCount::from_u64(PAGE_SIZE + 334);
    assert!(validate_manifest(&incomplete).is_err());
}

#[test]
fn page_tail_and_remote_ranges_are_checked() {
    let mut oversized_page = complex_manifest();
    oversized_page.pages[1].logical_length = u32::try_from(PAGE_SIZE + 1).expect("fits u32");
    assert!(validate_manifest(&oversized_page).is_err());

    let mut remote_overflow = complex_manifest();
    remote_overflow.remote_locations[1].offset = PAGE_SIZE + 1;
    assert!(validate_manifest(&remote_overflow).is_err());
}

#[test]
fn executable_extensions_cannot_be_virtual_and_native_files_have_no_extents() {
    let mut executable = complex_manifest();
    executable.files[1].name = "engine.EXE".to_string();
    assert!(validate_manifest(&executable).is_err());

    let mut native_extent = complex_manifest();
    native_extent.files[1].class = FileClass::NativeMutable;
    assert!(validate_manifest(&native_extent).is_err());
}

#[test]
fn duplicate_stable_file_ids_are_rejected() {
    let mut manifest = complex_manifest();
    manifest.files[1].stable_id = manifest.files[0].stable_id;
    assert!(validate_manifest(&manifest).is_err());
}
