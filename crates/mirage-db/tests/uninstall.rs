use mirage_db::{NewRepository, check_uninstall_safety};
use mirage_types::{RepositoryId, RepositoryState};
use tempfile::tempdir;

#[test]
fn missing_database_is_safe_but_mounted_repository_blocks_uninstall() {
    let root = tempdir().expect("temporary root");
    let path = root.path().join("control.db");
    assert!(
        check_uninstall_safety(&path)
            .expect("missing database check")
            .is_safe()
    );

    let database = mirage_db::Database::open(&path).expect("database");
    database
        .create_repository(NewRepository {
            repository_id: RepositoryId::from_bytes([9; 16]),
            display_name: "uninstall fixture".into(),
            local_root: root.path().join("game"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyMounted,
            created_at_ns: 1,
        })
        .expect("repository");
    drop(database);

    let report = check_uninstall_safety(&path).expect("mounted check");
    assert!(!report.is_safe());
    assert_eq!(report.blocking_repositories, 1);
}

#[test]
fn ready_unmounted_repository_does_not_block_retained_data_uninstall() {
    let root = tempdir().expect("temporary root");
    let path = root.path().join("control.db");
    let database = mirage_db::Database::open(&path).expect("database");
    database
        .create_repository(NewRepository {
            repository_id: RepositoryId::from_bytes([7; 16]),
            display_name: "retained fixture".into(),
            local_root: root.path().join("game"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    drop(database);

    assert!(
        check_uninstall_safety(&path)
            .expect("unmounted check")
            .is_safe()
    );
}
