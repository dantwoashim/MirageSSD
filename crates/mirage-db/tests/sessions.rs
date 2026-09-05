use std::path::PathBuf;

use mirage_db::{
    Database, LeaseSpec, NewRepository, NewSealedSession, SessionProcess, VerifiedGeneration,
};
use mirage_types::{
    CommitHash, GenerationId, ManifestHash, MirageErrorKind, PageHash, RepositoryId,
    RepositoryState, SessionEvent, SessionId, SessionState,
};
use tempfile::tempdir;

fn test_repo_id(byte: u8) -> RepositoryId {
    RepositoryId::from_bytes([byte; 16])
}

fn test_session_id(byte: u8) -> SessionId {
    SessionId::from_bytes([byte; 16])
}

fn test_page_hash(byte: u8) -> PageHash {
    PageHash::from_bytes([byte; 32])
}

fn setup_repo_with_generation(db: &Database, repo_id: RepositoryId, gen_id: GenerationId) {
    db.create_repository(NewRepository {
        repository_id: repo_id,
        display_name: "Session Test Repo".to_string(),
        local_root: PathBuf::from(r"C:\Games\SessionTest"),
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
        manifest_local_path: PathBuf::from(r"C:\Games\SessionTest\manifest.bin"),
        mount_index_path: None,
        created_at_ns: 1_000_100,
    })
    .expect("insert verified gen");
}

#[test]
fn sealed_session_creation_with_leases_and_process_lifecycle() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let repo_id = test_repo_id(1);
    let gen_id = GenerationId::from_u64(1);
    setup_repo_with_generation(&db, repo_id, gen_id);

    let session_id = test_session_id(1);
    let leases = vec![
        LeaseSpec {
            page_hash: test_page_hash(0x01),
            reason: "hard_set".to_string(),
        },
        LeaseSpec {
            page_hash: test_page_hash(0x02),
            reason: "capsule".to_string(),
        },
    ];

    let new_session = NewSealedSession {
        session_id,
        repository_id: repo_id,
        generation_id: gen_id,
        capsule_id: None,
        expected_lease_count: 2,
        leases,
        started_at_ns: 1_000_000_000,
    };

    db.create_session_with_leases(new_session)
        .expect("create session with leases");

    let state = db
        .load_session_state(session_id)
        .expect("load session state");
    assert_eq!(state, Some(SessionState::Verifying));

    let lease_count = db
        .session_lease_count(session_id)
        .expect("session lease count");
    assert_eq!(lease_count, 2);

    // Record processes
    let proc1 = SessionProcess {
        session_id,
        process_id: 1001,
        started_at_ns: 1_000_000_100,
    };
    let proc2 = SessionProcess {
        session_id,
        process_id: 1002,
        started_at_ns: 1_000_000_200,
    };
    db.record_session_process(proc1).expect("record proc 1001");
    db.record_session_process(proc2).expect("record proc 1002");

    // Duplicate proc record is idempotent
    db.record_session_process(proc1).expect("idempotent proc1");

    let sealed = db
        .transition_session_state(
            session_id,
            SessionState::Verifying,
            SessionEvent::VerificationPassed,
            1_000_000_250,
        )
        .expect("transition to sealed ready");
    assert_eq!(sealed, SessionState::SealedReady);

    // Transition state: SealedReady -> Launching on LaunchRequested
    let next_state = db
        .transition_session_state(
            session_id,
            SessionState::SealedReady,
            SessionEvent::LaunchRequested,
            1_000_000_300,
        )
        .expect("transition to launching");
    assert_eq!(next_state, SessionState::Launching);

    // Transition state: Launching -> Active on LaunchObserved
    let active_state = db
        .transition_session_state(
            session_id,
            SessionState::Launching,
            SessionEvent::LaunchObserved,
            1_000_000_350,
        )
        .expect("transition to active");
    assert_eq!(active_state, SessionState::Active);

    // Attempt finish with incomplete process set fails
    let incomplete_finish = db.finish_session(session_id, vec![1001], 1_000_000_400);
    assert!(incomplete_finish.is_err());
    assert_eq!(
        incomplete_finish.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );

    // Leases must still be intact
    assert_eq!(db.session_lease_count(session_id).expect("lease count"), 2);

    // Finish session with exact set: [1001, 1002]
    db.finish_session(session_id, vec![1001, 1002], 1_000_000_500)
        .expect("finish session");

    // State is Completed, leases released (0 count)
    assert_eq!(
        db.load_session_state(session_id).expect("session state"),
        Some(SessionState::Completed)
    );
    assert_eq!(db.session_lease_count(session_id).expect("lease count"), 0);

    // Idempotent finish of completed session
    db.finish_session(session_id, vec![], 1_000_000_600)
        .expect("idempotent finish");
}

#[test]
fn session_lease_validation_and_seal_violations() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let repo_id = test_repo_id(2);
    let gen_id = GenerationId::from_u64(1);
    setup_repo_with_generation(&db, repo_id, gen_id);

    // Mismatched expected count is rejected
    let session_id = test_session_id(2);
    let mismatched = NewSealedSession {
        session_id,
        repository_id: repo_id,
        generation_id: gen_id,
        capsule_id: None,
        expected_lease_count: 5, // Mismatch!
        leases: vec![LeaseSpec {
            page_hash: test_page_hash(0x01),
            reason: "hard_set".to_string(),
        }],
        started_at_ns: 1_000_000_000,
    };
    let result = db.create_session_with_leases(mismatched);
    assert!(result.is_err());
    assert_eq!(
        result.expect_err("error").kind,
        MirageErrorKind::InvalidArgument
    );

    // Valid session creation
    let valid_session = NewSealedSession {
        session_id,
        repository_id: repo_id,
        generation_id: gen_id,
        capsule_id: None,
        expected_lease_count: 1,
        leases: vec![LeaseSpec {
            page_hash: test_page_hash(0x01),
            reason: "hard_set".to_string(),
        }],
        started_at_ns: 1_000_000_000,
    };
    db.create_session_with_leases(valid_session)
        .expect("create valid session");
    db.transition_session_state(
        session_id,
        SessionState::Verifying,
        SessionEvent::VerificationPassed,
        1_000_000_100,
    )
    .expect("seal valid session");

    // Mark seal violation 1
    let count1 = db
        .mark_seal_violation(session_id, "first seal violation summary".to_string())
        .expect("mark first violation");
    assert_eq!(count1, 1);
    assert_eq!(
        db.load_session_state(session_id).expect("session state"),
        Some(SessionState::Violated)
    );

    // Mark seal violation 2: count increments to 2
    let count2 = db
        .mark_seal_violation(session_id, "second seal violation summary".to_string())
        .expect("mark second violation");
    assert_eq!(count2, 2);
    assert_eq!(
        db.load_session_state(session_id).expect("session state"),
        Some(SessionState::Violated)
    );
}
