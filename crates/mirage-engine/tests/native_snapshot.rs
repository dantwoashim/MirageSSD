use mirage_engine::update::{create_native_snapshots, restore_native_snapshots};
use std::path::PathBuf;
#[test]
fn modified_and_deleted_native_files_restore_exactly() {
    let game = tempfile::tempdir().unwrap();
    let rollback = tempfile::tempdir().unwrap();
    std::fs::create_dir(game.path().join("bin")).unwrap();
    let path = game.path().join("bin/game.exe");
    std::fs::write(&path, b"original native bytes").unwrap();
    let records = create_native_snapshots(
        game.path(),
        rollback.path(),
        &[PathBuf::from("bin/game.exe")],
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    restore_native_snapshots(game.path(), &records).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"original native bytes");
}
#[test]
fn escape_and_corrupt_snapshot_fail_closed() {
    let game = tempfile::tempdir().unwrap();
    let rollback = tempfile::tempdir().unwrap();
    assert!(
        create_native_snapshots(game.path(), rollback.path(), &[PathBuf::from("../escape")])
            .is_err()
    );
    std::fs::write(game.path().join("config.ini"), b"safe").unwrap();
    let records =
        create_native_snapshots(game.path(), rollback.path(), &[PathBuf::from("config.ini")])
            .unwrap();
    std::fs::write(&records[0].snapshot_path, b"tampered").unwrap();
    assert!(restore_native_snapshots(game.path(), &records).is_err());
}
