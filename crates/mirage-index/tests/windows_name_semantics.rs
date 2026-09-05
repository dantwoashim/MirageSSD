mod support;

use mirage_index::{DirectoryEntry, MountIndex, NodeIndex, compile_to_bytes};

use support::namespace_manifest;

#[test]
fn platform_ordinal_wrapper_is_case_insensitive() {
    assert_eq!(
        mirage_index::name::compare_ordinal_ignore_case("MiXeD", "mixed"),
        std::cmp::Ordering::Equal
    );
}

#[test]
fn mixed_case_unicode_and_separator_variants_resolve_without_external_state() {
    let index = MountIndex::from_bytes(
        compile_to_bytes(&namespace_manifest()).expect("compile namespace index"),
    )
    .expect("mount namespace index");
    assert!(matches!(
        index.lookup_path("assets/世界.PAK").expect("lookup"),
        Some(NodeIndex::File(_))
    ));
    assert!(matches!(
        index.lookup_path(r"ASSETS\deep\mixed.BIN").expect("lookup"),
        Some(NodeIndex::File(_))
    ));
    assert_eq!(index.lookup_path("missing").expect("lookup"), None);
    assert_eq!(index.lookup_path("Alpha.txt/child").expect("lookup"), None);
    assert!(index.lookup_path("../Alpha.txt").is_err());
}

#[test]
fn enumeration_is_complete_stable_and_marker_paginated() {
    let index = MountIndex::from_bytes(
        compile_to_bytes(&namespace_manifest()).expect("compile namespace index"),
    )
    .expect("mount namespace index");
    let root = index.directory_by_index(0).expect("root");
    let all = index
        .children_after_marker(root, None, 100)
        .expect("enumerate root");
    let names = all.iter().map(|entry| entry.name()).collect::<Vec<_>>();
    assert_eq!(
        names,
        ["Alpha.txt", "Assets", "Empty", "settings.toml", "zulu.txt"]
    );
    assert!(matches!(all[0], DirectoryEntry::File { .. }));
    assert!(matches!(all[1], DirectoryEntry::Directory { .. }));

    let first = index
        .children_after_marker(root, None, 2)
        .expect("first page");
    let marker = first.last().expect("marker").name();
    let second = index
        .children_after_marker(root, Some(marker), 100)
        .expect("second page");
    let combined = first
        .iter()
        .chain(&second)
        .map(|entry| entry.name())
        .collect::<Vec<_>>();
    assert_eq!(combined, names);
}

#[test]
fn empty_directories_and_long_paths_are_supported() {
    let index = MountIndex::from_bytes(
        compile_to_bytes(&namespace_manifest()).expect("compile namespace index"),
    )
    .expect("mount namespace index");
    let empty = match index.lookup_path("empty").expect("lookup empty") {
        Some(NodeIndex::Directory(index)) => index,
        other => panic!("expected empty directory, got {other:?}"),
    };
    assert!(
        index
            .children_after_marker(
                index.directory_by_index(empty).expect("empty directory"),
                None,
                1,
            )
            .expect("enumerate empty")
            .is_empty()
    );

    let long = format!("assets/deep/{}", "LongDirectoryName".repeat(8));
    assert!(matches!(
        index.lookup_path(&long).expect("long lookup"),
        Some(NodeIndex::Directory(_))
    ));
}
