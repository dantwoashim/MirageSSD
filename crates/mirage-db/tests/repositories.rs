use std::path::PathBuf;

use mirage_db::{ActiveGeneration, Database, NewRepository, VerifiedGeneration};
use mirage_types::{
    CommitHash, GenerationId, ManifestHash, MirageErrorKind, RepositoryEvent, RepositoryId,
    RepositoryState,
};
use tempfile::tempdir;

fn test_repo_id(byte: u8) -> RepositoryId {
    RepositoryId::from_bytes([byte; 16])
}

fn test_commit_hash(byte: u8) -> CommitHash {
    CommitHash::from_bytes([byte; 32])
}

fn test_manifest_hash(byte: u8) -> ManifestHash {
    ManifestHash::from_bytes([byte; 32])
}

#[test]
fn unregister_is_atomic_with_seeded_names_sessions_updates_and_journal_allocations() {
    let root = tempdir().unwrap();
    let path = root.path().join("control.db");
    let db = Database::open(&path).unwrap();
    let volumes = [test_repo_id(41), test_repo_id(42)];
    let journal = mirage_db::payload_remote::MANAGED_JOURNAL_FILE_ID;
    db.writer()
        .physical_register_file(mirage_db::PhysicalFileRecord {
            file_id: journal,
            path: "journal".into(),
            zone: 1,
            extent_bytes: 0,
            extent_count: 0,
            created_ns: 1,
        })
        .unwrap();
    for volume in volumes {
        db.create_repository(NewRepository {
            repository_id: volume,
            display_name: "fixture".into(),
            local_root: root.path().join(volume.to_string()),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .unwrap();
        let namespace = db.namespace_create_volume(volume, 1).unwrap();
        let file = db
            .namespace_create(
                volume,
                namespace,
                "seed.dat",
                mirage_db::NamespaceNodeKind::File,
                2,
            )
            .unwrap();
        db.namespace_record_legacy(volume, "seed.dat", file.inode)
            .unwrap();
        let payload = *volume.as_bytes();
        db.writer()
            .physical_reserve_extent(
                mirage_db::PhysicalExtentRecord {
                    extent_id: payload,
                    file_id: journal,
                    slot_index: i64::from(payload[0]),
                    length_bytes: 4096,
                    state: mirage_db::PhysicalExtentState::Reserved,
                    page_hash: None,
                    checksum: None,
                    pin_count: 0,
                    generation: 0,
                    updated_ns: 1,
                },
                mirage_db::PhysicalReservationRecord {
                    extent_id: payload,
                    owner_epoch: [1; 16],
                    expires_ns: i64::MAX,
                },
            )
            .unwrap();
        db.writer()
            .physical_commit_extent(
                payload,
                mirage_types::PageHash::from_bytes([7; 32]),
                [7; 32],
                2,
            )
            .unwrap();
        db.writer()
            .extent_replace(
                volume,
                file.inode,
                1,
                vec![mirage_db::ByteExtent {
                    extent_id: payload,
                    volume_id: volume,
                    inode: file.inode,
                    version: 1,
                    start: 0,
                    length: 4096,
                    kind: mirage_db::ExtentKind::Dirty,
                    page_hash: None,
                    base_offset: None,
                    payload_id: Some(payload),
                    payload_offset: Some(0),
                    created_ns: 2,
                }],
                2,
            )
            .unwrap();
        db.insert_verified_generation(VerifiedGeneration {
            repository_id: volume,
            generation_id: GenerationId::ZERO,
            commit_hash: test_commit_hash(1),
            manifest_hash: test_manifest_hash(2),
            manifest_local_path: root.path().join("manifest"),
            mount_index_path: None,
            created_at_ns: 1,
        })
        .unwrap();
        db.activate_generation(volume, GenerationId::ZERO, test_commit_hash(1), None, 2)
            .unwrap();
    }
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute("INSERT INTO sessions(session_id,repository_id,generation,mode,state,expected_lease_count,started_at_ns) VALUES (?1,?1,0,'balanced','active',0,1)", [volumes[0].as_bytes().as_slice()]).unwrap();
    sql.execute("INSERT INTO update_journals(update_id,repository_id,base_generation,target_generation,state,journal_path,created_at_ns,updated_at_ns) VALUES (?1,?1,0,1,'committed','fixture',1,1)", [volumes[0].as_bytes().as_slice()]).unwrap();
    for volume in volumes {
        sql.execute(
            "INSERT INTO remote_heads VALUES (?1,zeroblob(32),0,'cursor',1)",
            [volume.as_bytes().as_slice()],
        )
        .unwrap();
        sql.execute(
            "INSERT INTO flush_groups(group_id,volume_id,opened_ns) VALUES (?1,?2,1)",
            rusqlite::params![
                i64::from(volume.as_bytes()[0]),
                volume.as_bytes().as_slice()
            ],
        )
        .unwrap();
    }
    sql.execute_batch("CREATE TRIGGER reject_unregister BEFORE DELETE ON repositories BEGIN SELECT RAISE(ABORT, 'injected unregister failure'); END;").unwrap();
    assert!(db.unregister_repository(volumes[0]).is_err());
    assert!(
        db.namespace_resolve_path(volumes[0], "seed.dat")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    sql.execute_batch("DROP TRIGGER reject_unregister").unwrap();
    assert!(db.unregister_repository(volumes[0]).unwrap());
    assert!(db.load_repository_state(volumes[0]).unwrap().is_none());
    assert!(
        db.namespace_resolve_path(volumes[1], "seed.dat")
            .unwrap()
            .is_some()
    );
    let (_, extents, reservations) = db.load_physical_state().unwrap();
    assert!(
        extents
            .iter()
            .all(|row| row.extent_id != *volumes[0].as_bytes())
    );
    assert_eq!(
        extents
            .iter()
            .find(|row| row.extent_id == *volumes[1].as_bytes())
            .unwrap()
            .state,
        mirage_db::PhysicalExtentState::Alive
    );
    assert!(
        reservations
            .iter()
            .all(|row| row.extent_id != *volumes[0].as_bytes())
    );
    for table in [
        "sessions",
        "update_journals",
        "remote_heads",
        "flush_groups",
    ] {
        let column = if ["sessions", "update_journals"].contains(&table) {
            "repository_id"
        } else {
            "volume_id"
        };
        assert_eq!(
            sql.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column}=?1"),
                [volumes[0].as_bytes().as_slice()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn repository_lifecycle_creation_and_state_transitions() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let repo_id = test_repo_id(1);
    let repo = NewRepository {
        repository_id: repo_id,
        display_name: "Game Repository Alpha".to_string(),
        local_root: PathBuf::from(r"C:\Games\Alpha"),
        owner_sid: "S-1-5-18".into(),
        content_encrypted: false,
        initial_state: RepositoryState::ReadyUnmounted,
        created_at_ns: 1_000_000_000,
    };

    db.create_repository(repo).expect("create repository");

    assert_eq!(
        db.load_repository_content_encrypted(repo_id)
            .expect("load encryption policy"),
        Some(false)
    );

    let state = db
        .load_repository_state(repo_id)
        .expect("load repository state");
    assert_eq!(state, Some(RepositoryState::ReadyUnmounted));

    let active = db
        .load_active_generation(repo_id)
        .expect("load active generation");
    assert_eq!(active, None);

    // Transition state from ReadyUnmounted -> Mounting on MountRequested
    let next = db
        .set_repository_state(
            repo_id,
            RepositoryState::ReadyUnmounted,
            RepositoryEvent::MountRequested,
            1_000_001_000,
        )
        .expect("transition state");
    assert_eq!(next, RepositoryState::Mounting);

    let current = db
        .load_repository_state(repo_id)
        .expect("load repository state");
    assert_eq!(current, Some(RepositoryState::Mounting));

    // Stale expected state returns conflict
    let stale_result = db.set_repository_state(
        repo_id,
        RepositoryState::ReadyUnmounted,
        RepositoryEvent::MountSucceeded,
        1_000_002_000,
    );
    assert!(stale_result.is_err());
    assert_eq!(
        stale_result.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );
}

#[test]
fn generation_insertion_and_atomic_activation() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let repo_id = test_repo_id(2);
    db.create_repository(NewRepository {
        repository_id: repo_id,
        display_name: "Game Repository Beta".to_string(),
        local_root: PathBuf::from(r"C:\Games\Beta"),
        owner_sid: "S-1-5-18".into(),
        content_encrypted: false,
        initial_state: RepositoryState::ReadyUnmounted,
        created_at_ns: 1_000_000_000,
    })
    .expect("create repository");

    let gen1 = GenerationId::from_u64(1);
    let commit1 = test_commit_hash(0x11);
    let manifest1 = test_manifest_hash(0x22);
    let manifest_path1 = PathBuf::from(r"C:\Games\Beta\manifest-1.bin");
    let index_path1 = PathBuf::from(r"C:\Games\Beta\index-1.midx");

    let verified_gen = VerifiedGeneration {
        repository_id: repo_id,
        generation_id: gen1,
        commit_hash: commit1,
        manifest_hash: manifest1,
        manifest_local_path: manifest_path1.clone(),
        mount_index_path: Some(index_path1.clone()),
        created_at_ns: 1_000_000_100,
    };

    db.insert_verified_generation(verified_gen.clone())
        .expect("insert verified generation 1");

    // Idempotent re-insertion succeeds
    db.insert_verified_generation(verified_gen)
        .expect("idempotent insert generation 1");

    // Conflicting re-insertion with different commit hash fails
    let conflicting = VerifiedGeneration {
        repository_id: repo_id,
        generation_id: gen1,
        commit_hash: test_commit_hash(0x99),
        manifest_hash: manifest1,
        manifest_local_path: manifest_path1.clone(),
        mount_index_path: Some(index_path1.clone()),
        created_at_ns: 1_000_000_100,
    };
    let conflict_result = db.insert_verified_generation(conflicting);
    assert!(conflict_result.is_err());
    assert_eq!(
        conflict_result.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );

    // Initial activation: expected_current is None
    db.activate_generation(repo_id, gen1, commit1, None, 1_000_000_200)
        .expect("activate generation 1");

    let active = db
        .load_active_generation(repo_id)
        .expect("load active generation");
    assert_eq!(
        active,
        Some(ActiveGeneration {
            repository_id: repo_id,
            generation_id: gen1,
            commit_hash: commit1,
            manifest_hash: manifest1,
            manifest_local_path: manifest_path1,
            mount_index_path: Some(index_path1),
        })
    );

    // Insert generation 2
    let gen2 = GenerationId::from_u64(2);
    let commit2 = test_commit_hash(0x33);
    let manifest2 = test_manifest_hash(0x44);
    let manifest_path2 = PathBuf::from(r"C:\Games\Beta\manifest-2.bin");

    db.insert_verified_generation(VerifiedGeneration {
        repository_id: repo_id,
        generation_id: gen2,
        commit_hash: commit2,
        manifest_hash: manifest2,
        manifest_local_path: manifest_path2.clone(),
        mount_index_path: None,
        created_at_ns: 1_000_000_300,
    })
    .expect("insert verified generation 2");

    // Activation with stale expected_current (None instead of Some((gen1, commit1))) fails
    let stale_activate = db.activate_generation(repo_id, gen2, commit2, None, 1_000_000_400);
    assert!(stale_activate.is_err());
    assert_eq!(
        stale_activate.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );

    // Activation with correct expected_current succeeds
    db.activate_generation(repo_id, gen2, commit2, Some((gen1, commit1)), 1_000_000_400)
        .expect("activate generation 2");

    let active2 = db
        .load_active_generation(repo_id)
        .expect("load active generation 2");
    assert_eq!(
        active2,
        Some(ActiveGeneration {
            repository_id: repo_id,
            generation_id: gen2,
            commit_hash: commit2,
            manifest_hash: manifest2,
            manifest_local_path: manifest_path2,
            mount_index_path: None,
        })
    );
}

#[test]
fn activation_fails_on_unverified_or_missing_generation() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let repo_id = test_repo_id(3);
    db.create_repository(NewRepository {
        repository_id: repo_id,
        display_name: "Game Repository Gamma".to_string(),
        local_root: PathBuf::from(r"C:\Games\Gamma"),
        owner_sid: "S-1-5-18".into(),
        content_encrypted: false,
        initial_state: RepositoryState::ReadyUnmounted,
        created_at_ns: 1_000_000_000,
    })
    .expect("create repository");

    let gen99 = GenerationId::from_u64(99);
    let commit99 = test_commit_hash(0x99);

    let result = db.activate_generation(repo_id, gen99, commit99, None, 1_000_000_100);
    assert!(result.is_err());
}
