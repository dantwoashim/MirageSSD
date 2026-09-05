use std::fs;
use std::io;
use std::path::Path;

use mirage_manifest::{
    ClassificationRuleSet, Inventory, InventoryEntry, InventoryEntryKind, InventoryScanner,
    ReparsePolicy,
};

#[test]
fn scan_is_stable_read_only_and_counts_observed_hard_links() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("Source");
    fs::create_dir(&root).expect("source directory");
    let primary = root.join("payload.pak");
    let linked = root.join("payload-copy.pak");
    fs::write(&primary, b"immutable payload").expect("payload");
    fs::hard_link(&primary, &linked).expect("hard link");
    let before = metadata_fingerprint(&primary);

    let first = InventoryScanner::default().scan(&root).expect("first scan");
    let second = InventoryScanner::default()
        .scan(&root)
        .expect("second scan");
    assert_eq!(first, second);
    assert_eq!(metadata_fingerprint(&primary), before);
    assert_eq!(first.total_regular_file_bytes, 34);
    for path in ["payload.pak", "payload-copy.pak"] {
        let entry = first
            .entries
            .iter()
            .find(|entry| entry.relative_path == path)
            .expect("hard-link entry");
        assert_eq!(entry.hard_link_count, Some(2));
    }
}

#[test]
fn unicode_long_and_read_only_names_are_inventory_safe() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("Source");
    let nested = root.join("長い名前").join("x".repeat(120));
    fs::create_dir_all(&nested).expect("nested source");
    let path = nested.join("κόσμος.assets");
    fs::write(&path, vec![7_u8; 1024]).expect("unicode file");
    let original_permissions = fs::metadata(&path).expect("metadata").permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).expect("read-only");

    let inventory = InventoryScanner::default().scan(&root).expect("scan");
    let entry = inventory
        .entries
        .iter()
        .find(|entry| entry.relative_path.ends_with("κόσμος.assets"))
        .expect("unicode entry");
    assert!(entry.read_only);
    assert_eq!(entry.size, 1024);

    fs::set_permissions(path, original_permissions).expect("restore permissions");
}

#[test]
fn reparse_points_are_recorded_without_following_or_rejected_by_policy() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("Source");
    fs::create_dir(&root).expect("source directory");
    let target = root.join("target.bin");
    let link = root.join("link.bin");
    fs::write(&target, b"target").expect("target");
    if create_file_symlink(&target, &link).is_err() {
        return;
    }

    let inventory = InventoryScanner::default()
        .scan(&root)
        .expect("record reparse point");
    let linked = inventory
        .entries
        .iter()
        .find(|entry| entry.relative_path == "link.bin")
        .expect("link entry");
    assert_eq!(linked.kind, InventoryEntryKind::ReparsePoint);
    assert!(
        InventoryScanner::new(ReparsePolicy::Reject)
            .scan(&root)
            .is_err()
    );
}

#[test]
fn permission_failure_is_reported_without_partial_inventory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("Source");
    fs::create_dir(&root).expect("source directory");
    fs::write(root.join("denied.bin"), b"secret").expect("file");
    let result = InventoryScanner::default().scan_with_preflight(&root, |path| {
        if path.file_name().is_some_and(|name| name == "denied.bin") {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
}

#[test]
fn synthetic_windows_case_collision_is_rejected() {
    let entries = ["Data.pak", "data.PAK"]
        .into_iter()
        .map(|path| InventoryEntry {
            relative_path: path.to_string(),
            kind: InventoryEntryKind::File,
            size: 1,
            attributes: 0,
            read_only: false,
            created_utc_ns: None,
            modified_utc_ns: None,
            extension: Some("pak".to_string()),
            reparse_tag: None,
            hard_link_count: Some(1),
        })
        .collect();
    let inventory = Inventory {
        format_version: 1,
        entries,
        total_regular_file_bytes: 2,
    };
    assert!(inventory.validate().is_err());
}

#[test]
fn classification_is_conservative_and_size_bounded() {
    let rules = ClassificationRuleSet::default();
    let executable = entry("game.exe", "exe", 50_000_000);
    let configuration = entry("settings.toml", "toml", 50_000_000);
    let small_asset = entry("tiny.pak", "pak", 1024);
    let large_asset = entry("world.pak", "pak", 2 * 1024 * 1024);
    let unknown = entry("save.dat", "dat", 50_000_000);
    assert!(!rules.classify(&executable).eligible_for_virtualization);
    assert!(!rules.classify(&configuration).eligible_for_virtualization);
    assert!(!rules.classify(&small_asset).eligible_for_virtualization);
    assert!(rules.classify(&large_asset).eligible_for_virtualization);
    assert!(!rules.classify(&unknown).eligible_for_virtualization);
}

fn entry(path: &str, extension: &str, size: u64) -> InventoryEntry {
    InventoryEntry {
        relative_path: path.to_string(),
        kind: InventoryEntryKind::File,
        size,
        attributes: 0,
        read_only: false,
        created_utc_ns: None,
        modified_utc_ns: None,
        extension: Some(extension.to_string()),
        reparse_tag: None,
        hard_link_count: Some(1),
    }
}

fn metadata_fingerprint(path: &Path) -> (u64, bool, Option<std::time::SystemTime>) {
    let metadata = fs::metadata(path).expect("metadata");
    (
        metadata.len(),
        metadata.permissions().readonly(),
        metadata.modified().ok(),
    )
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(any(windows, unix)))]
fn create_file_symlink(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symlinks unsupported",
    ))
}
