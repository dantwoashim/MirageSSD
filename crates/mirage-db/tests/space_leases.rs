use std::path::PathBuf;

use mirage_db::{Database, NewRepository, NewSpaceLease, SpaceLeaseEvent, SpaceLeaseState};
use mirage_types::{MirageErrorKind, RepositoryId, RepositoryState, SpaceLeaseId};
use tempfile::tempdir;

fn repository_id() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

fn lease_id(value: u8) -> SpaceLeaseId {
    SpaceLeaseId::from_bytes([value; 16])
}

fn setup() -> (tempfile::TempDir, Database) {
    let directory = tempdir().expect("temp directory");
    let database = Database::open(&directory.path().join("control-plane.db")).expect("database");
    database
        .create_repository(NewRepository {
            repository_id: repository_id(),
            display_name: "Capacity Test".to_owned(),
            local_root: PathBuf::from(r"C:\CapacityTest"),
            owner_sid: "S-1-5-18".to_owned(),
            content_encrypted: true,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    (directory, database)
}

#[test]
fn lease_lifecycle_is_durable_and_active_bytes_are_exact() {
    let (_directory, database) = setup();
    let id = lease_id(2);
    database
        .create_space_lease(NewSpaceLease {
            lease_id: id,
            repository_id: repository_id(),
            target_volume_id: "volume-a".to_owned(),
            requested_bytes: 70,
            planned_reclaim_bytes: 50,
            created_at_ns: 10,
            expires_at_ns: 100,
        })
        .expect("create lease");

    assert_eq!(
        database.active_space_lease_bytes(repository_id()).unwrap(),
        70
    );
    assert_eq!(
        database.load_space_lease(id).unwrap().unwrap().state,
        SpaceLeaseState::Preparing
    );

    assert_eq!(
        database
            .transition_space_lease(
                id,
                SpaceLeaseState::Preparing,
                SpaceLeaseEvent::ReclaimFinished,
                20,
            )
            .unwrap(),
        SpaceLeaseState::Ready
    );
    assert_eq!(
        database
            .transition_space_lease(id, SpaceLeaseState::Ready, SpaceLeaseEvent::Consume, 30,)
            .unwrap(),
        SpaceLeaseState::Consumed
    );
    assert_eq!(
        database
            .transition_space_lease(id, SpaceLeaseState::Consumed, SpaceLeaseEvent::Release, 40,)
            .unwrap(),
        SpaceLeaseState::Released
    );
    assert_eq!(
        database.active_space_lease_bytes(repository_id()).unwrap(),
        0
    );
}

#[test]
fn stale_invalid_and_expired_transitions_fail_closed() {
    let (_directory, database) = setup();
    let id = lease_id(3);
    database
        .create_space_lease(NewSpaceLease {
            lease_id: id,
            repository_id: repository_id(),
            target_volume_id: "volume-a".to_owned(),
            requested_bytes: 1,
            planned_reclaim_bytes: 0,
            created_at_ns: 10,
            expires_at_ns: 20,
        })
        .expect("create lease");

    let stale =
        database.transition_space_lease(id, SpaceLeaseState::Ready, SpaceLeaseEvent::Consume, 11);
    assert_eq!(stale.unwrap_err().kind, MirageErrorKind::RepositoryConflict);

    let expired = database.transition_space_lease(
        id,
        SpaceLeaseState::Preparing,
        SpaceLeaseEvent::ReclaimFinished,
        20,
    );
    assert_eq!(
        expired.unwrap_err().kind,
        MirageErrorKind::RepositoryConflict
    );

    assert_eq!(
        database
            .transition_space_lease(id, SpaceLeaseState::Preparing, SpaceLeaseEvent::Fail, 20,)
            .unwrap(),
        SpaceLeaseState::Failed
    );
}

#[test]
fn duplicate_and_invalid_lease_creation_are_rejected() {
    let (_directory, database) = setup();
    let lease = NewSpaceLease {
        lease_id: lease_id(4),
        repository_id: repository_id(),
        target_volume_id: "volume-a".to_owned(),
        requested_bytes: 10,
        planned_reclaim_bytes: 0,
        created_at_ns: 10,
        expires_at_ns: 20,
    };
    database
        .create_space_lease(lease.clone())
        .expect("create lease");
    assert!(database.create_space_lease(lease.clone()).is_err());

    let invalid = database.create_space_lease(NewSpaceLease {
        lease_id: lease_id(5),
        requested_bytes: 0,
        ..lease
    });
    assert_eq!(invalid.unwrap_err().kind, MirageErrorKind::InvalidArgument);
}

#[test]
fn physical_promises_are_isolated_by_volume_and_expire_from_accounting() {
    let (_directory, database) = setup();
    for (value, volume, bytes) in [(6, "volume-a", 20), (7, "volume-b", 90)] {
        database
            .create_space_lease(NewSpaceLease {
                lease_id: lease_id(value),
                repository_id: repository_id(),
                target_volume_id: volume.to_owned(),
                requested_bytes: bytes,
                planned_reclaim_bytes: 0,
                created_at_ns: 10,
                expires_at_ns: 100,
            })
            .expect("create volume lease");
    }

    assert_eq!(
        database
            .active_space_lease_bytes_for_volume("volume-a", 50)
            .unwrap(),
        20
    );
    assert_eq!(
        database
            .active_space_lease_bytes_for_volume("volume-b", 50)
            .unwrap(),
        90
    );
    assert_eq!(
        database
            .active_space_lease_bytes_for_volume("volume-a", 100)
            .unwrap(),
        0
    );
}
