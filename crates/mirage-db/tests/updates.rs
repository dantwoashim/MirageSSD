use std::path::PathBuf;

use mirage_db::{
    Database, NativeSnapshot, NewRepository, NewUpdateJournal, OverlayPage, VerifiedGeneration,
    check_database,
};
use mirage_types::{
    ByteCount, CommitHash, ContentHash, GenerationId, ManifestHash, MirageErrorKind, PageHash,
    PageOrdinal, PageState, RepositoryId, RepositoryState, SlotIndex, StableFileId, UpdateEvent,
    UpdateId, UpdateState,
};
use tempfile::tempdir;

fn test_repo_id(byte: u8) -> RepositoryId {
    RepositoryId::from_bytes([byte; 16])
}

fn test_update_id(byte: u8) -> UpdateId {
    UpdateId::from_bytes([byte; 16])
}

fn test_hash(byte: u8) -> ContentHash {
    ContentHash::from_bytes([byte; 32])
}

fn setup_repo_with_generation(db: &Database, repo_id: RepositoryId, gen_id: GenerationId) {
    db.create_repository(NewRepository {
        repository_id: repo_id,
        display_name: "Update Test Repo".to_string(),
        local_root: PathBuf::from(r"C:\Games\UpdateTest"),
        owner_sid: "S-1-5-18".into(),
        content_encrypted: false,
        initial_state: RepositoryState::ReadyUnmounted,
        created_at_ns: 1_000_000,
    })
    .expect("create repo");

    db.insert_verified_generation(VerifiedGeneration {
        repository_id: repo_id,
        generation_id: gen_id,
        commit_hash: CommitHash::from_bytes([0x11; 32]),
        manifest_hash: ManifestHash::from_bytes([0x22; 32]),
        manifest_local_path: PathBuf::from(r"C:\Games\UpdateTest\manifest.bin"),
        mount_index_path: None,
        created_at_ns: 1_000_100,
    })
    .expect("insert verified gen");
}

#[test]
fn update_journal_lifecycle_overlays_and_state_transitions() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let repo_id = test_repo_id(1);
    let gen1 = GenerationId::from_u64(1);
    let gen2 = GenerationId::from_u64(2);
    setup_repo_with_generation(&db, repo_id, gen1);

    let update_id = test_update_id(1);
    let journal = NewUpdateJournal {
        update_id,
        repository_id: repo_id,
        base_generation: gen1,
        target_generation: gen2,
        journal_path: PathBuf::from(r"C:\Games\UpdateTest\update-1.journal"),
        created_at_ns: 1_000_000_000,
    };

    db.create_update_journal(journal)
        .expect("create update journal");

    assert_eq!(
        db.load_update_state(update_id).expect("load update state"),
        Some(UpdateState::Created)
    );

    // Second active update on same repository is rejected
    let second_update = NewUpdateJournal {
        update_id: test_update_id(2),
        repository_id: repo_id,
        base_generation: gen1,
        target_generation: gen2,
        journal_path: PathBuf::from(r"C:\Games\UpdateTest\update-2.journal"),
        created_at_ns: 1_000_000_100,
    };
    let second_result = db.create_update_journal(second_update);
    assert!(second_result.is_err());
    assert_eq!(
        second_result.expect_err("error").kind,
        MirageErrorKind::UpdateActive
    );

    // Upsert overlay page
    let overlay = OverlayPage {
        update_id,
        file_id: StableFileId::from_u64(10),
        page_index: PageOrdinal::from_u32(0),
        state: PageState::DirtyLocal,
        arena_slot: Some((0, SlotIndex::from_u32(42))),
        page_hash: Some(PageHash::from_bytes([0xAA; 32])),
        staging_object_key: Some(test_hash(0xBB)),
        staging_offset: Some(0),
        encoded_length: Some(ByteCount::from_u64(65536)),
    };
    db.upsert_overlay_page(overlay)
        .expect("upsert overlay page");

    // Upsert native snapshot
    let snapshot = NativeSnapshot {
        update_id,
        relative_path: "Game.exe".to_string(),
        snapshot_path: PathBuf::from(r"C:\Games\UpdateTest\snapshots\Game.exe"),
        byte_length: ByteCount::from_u64(1024 * 1024),
        content_hash: test_hash(0xCC),
    };
    db.upsert_native_snapshot(snapshot)
        .expect("upsert native snapshot");

    // Transition update: Created -> NativeSnapshotInProgress
    let next1 = db
        .transition_update_state(
            update_id,
            UpdateState::Created,
            UpdateEvent::NativeSnapshotStarted,
            "native snapshotting started".to_string(),
            1_000_000_200,
        )
        .expect("transition to native snapshot in progress");
    assert_eq!(next1, UpdateState::NativeSnapshotInProgress);

    // Stale expected state returns conflict
    let stale_result = db.transition_update_state(
        update_id,
        UpdateState::Created, // Stale!
        UpdateEvent::ApplyStarted,
        "apply started".to_string(),
        1_000_000_300,
    );
    assert!(stale_result.is_err());
    assert_eq!(
        stale_result.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );

    // Transition: NativeSnapshotInProgress -> Applying
    let next2 = db
        .transition_update_state(
            update_id,
            UpdateState::NativeSnapshotInProgress,
            UpdateEvent::ApplyStarted,
            "patch application started".to_string(),
            1_000_000_400,
        )
        .expect("transition to applying");
    assert_eq!(next2, UpdateState::Applying);

    // Transition: Applying -> DirtyPagesPresent
    let next3 = db
        .transition_update_state(
            update_id,
            UpdateState::Applying,
            UpdateEvent::DirtyPagesDetected,
            "dirty overlay pages generated".to_string(),
            1_000_000_500,
        )
        .expect("transition to dirty pages present");
    assert_eq!(next3, UpdateState::DirtyPagesPresent);

    // Transition: DirtyPagesPresent -> Staging
    let next4 = db
        .transition_update_state(
            update_id,
            UpdateState::DirtyPagesPresent,
            UpdateEvent::StagingStarted,
            "staging overlay pages to remote".to_string(),
            1_000_000_600,
        )
        .expect("transition to staging");
    assert_eq!(next4, UpdateState::Staging);

    // Transition: Staging -> AllContentStaged
    let next5 = db
        .transition_update_state(
            update_id,
            UpdateState::Staging,
            UpdateEvent::ContentStaged,
            "all overlay content staged".to_string(),
            1_000_000_700,
        )
        .expect("transition to all content staged");
    assert_eq!(next5, UpdateState::AllContentStaged);

    // Transition: AllContentStaged -> ManifestUploaded
    let next6 = db
        .transition_update_state(
            update_id,
            UpdateState::AllContentStaged,
            UpdateEvent::ManifestUploaded,
            "target manifest uploaded".to_string(),
            1_000_000_800,
        )
        .expect("transition to manifest uploaded");
    assert_eq!(next6, UpdateState::ManifestUploaded);

    // Transition: ManifestUploaded -> CommitUploaded
    let next7 = db
        .transition_update_state(
            update_id,
            UpdateState::ManifestUploaded,
            UpdateEvent::CommitUploaded,
            "target commit uploaded".to_string(),
            1_000_000_900,
        )
        .expect("transition to commit uploaded");
    assert_eq!(next7, UpdateState::CommitUploaded);

    // Transition: CommitUploaded -> CommitVerified
    let next8 = db
        .transition_update_state(
            update_id,
            UpdateState::CommitUploaded,
            UpdateEvent::CommitVerified,
            "remote commit verified".to_string(),
            1_000_001_000,
        )
        .expect("transition to commit verified");
    assert_eq!(next8, UpdateState::CommitVerified);

    // Transition: CommitVerified -> LocalActivationPending
    let next9 = db
        .transition_update_state(
            update_id,
            UpdateState::CommitVerified,
            UpdateEvent::LocalActivationRequested,
            "requesting local activation".to_string(),
            1_000_001_100,
        )
        .expect("transition to local activation pending");
    assert_eq!(next9, UpdateState::LocalActivationPending);

    // Transition: LocalActivationPending -> Committed
    let next10 = db
        .transition_update_state(
            update_id,
            UpdateState::LocalActivationPending,
            UpdateEvent::ActivationCommitted,
            "update committed successfully".to_string(),
            1_000_001_200,
        )
        .expect("transition to committed");
    assert_eq!(next10, UpdateState::Committed);

    // Verify database check passes
    let report = check_database(&db_path).expect("check database");
    assert!(report.quick_check_ok);
    assert!(report.integrity_check_ok);
    assert_eq!(report.foreign_key_violation_count, 0);
}
