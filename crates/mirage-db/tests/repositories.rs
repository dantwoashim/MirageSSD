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
