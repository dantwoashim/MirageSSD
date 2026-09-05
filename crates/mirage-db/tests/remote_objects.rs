use mirage_backend::{BackendId, ImmutableRevision, ObjectKind, ProviderObjectId};
use mirage_db::{
    BackendAccount, Database, RemoteObjectRecord, UploadSession, UpsertRemoteObjectOutcome,
};
use mirage_types::{ByteCount, ContentHash, MirageErrorKind, UpdateId};
use tempfile::tempdir;

fn test_content_hash(byte: u8) -> ContentHash {
    ContentHash::from_bytes([byte; 32])
}

fn test_update_id(byte: u8) -> UpdateId {
    UpdateId::from_bytes([byte; 16])
}

#[test]
fn backend_account_registration_and_immutability() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let backend_id = BackendId::new("gdrive-primary").expect("backend id");
    let subject_hash = test_content_hash(0x10);

    let account = BackendAccount {
        backend_id: backend_id.clone(),
        provider: "google-drive".to_string(),
        account_subject_hash: subject_hash,
        state: "active".to_string(),
        updated_at_ns: 1_000_000,
    };

    db.register_backend_account(account)
        .expect("register account");

    // Idempotent state update with matching immutable identity
    let account_updated = BackendAccount {
        backend_id: backend_id.clone(),
        provider: "google-drive".to_string(),
        account_subject_hash: subject_hash,
        state: "rate_limited".to_string(),
        updated_at_ns: 2_000_000,
    };
    db.register_backend_account(account_updated)
        .expect("update account state");

    // Conflicting update with changed subject hash is rejected
    let conflicting = BackendAccount {
        backend_id: backend_id.clone(),
        provider: "google-drive".to_string(),
        account_subject_hash: test_content_hash(0x99),
        state: "active".to_string(),
        updated_at_ns: 3_000_000,
    };
    let result = db.register_backend_account(conflicting);
    assert!(result.is_err());
    assert_eq!(
        result.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );
}

#[test]
fn remote_object_upsert_and_conflict_rejection() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let backend_id = BackendId::new("gdrive-primary").expect("backend id");
    db.register_backend_account(BackendAccount {
        backend_id: backend_id.clone(),
        provider: "google-drive".to_string(),
        account_subject_hash: test_content_hash(0x01),
        state: "active".to_string(),
        updated_at_ns: 1_000_000,
    })
    .expect("register account");

    let object_key = test_content_hash(0xAA);
    let record = RemoteObjectRecord {
        backend_id: backend_id.clone(),
        object_key,
        provider_object_id: ProviderObjectId::new("1a2b3c4d5e").expect("provider id"),
        immutable_revision: Some(ImmutableRevision::new("rev-1").expect("rev")),
        object_kind: ObjectKind::Pack,
        byte_length: ByteCount::from_u64(64 * 1024 * 1024),
        content_hash: test_content_hash(0xBB),
        state: "available".to_string(),
        created_at_ns: 1_000_100,
    };

    let outcome1 = db
        .upsert_remote_object(record.clone())
        .expect("upsert remote object");
    assert_eq!(outcome1, UpsertRemoteObjectOutcome::Inserted);

    // Idempotent re-insert
    let outcome2 = db
        .upsert_remote_object(record.clone())
        .expect("re-upsert same object");
    assert_eq!(outcome2, UpsertRemoteObjectOutcome::AlreadyPresent);

    // Query loaded object
    let loaded = db
        .load_remote_object(&backend_id, object_key)
        .expect("load remote object");
    assert_eq!(loaded, Some(record));

    // Conflict with different content hash
    let conflicting = RemoteObjectRecord {
        backend_id: backend_id.clone(),
        object_key,
        provider_object_id: ProviderObjectId::new("1a2b3c4d5e").expect("provider id"),
        immutable_revision: Some(ImmutableRevision::new("rev-1").expect("rev")),
        object_kind: ObjectKind::Pack,
        byte_length: ByteCount::from_u64(64 * 1024 * 1024),
        content_hash: test_content_hash(0xCC), // Different hash!
        state: "available".to_string(),
        created_at_ns: 1_000_200,
    };
    let result = db.upsert_remote_object(conflicting);
    assert!(result.is_err());
    assert_eq!(
        result.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );
}

#[test]
fn resumable_upload_session_advance_and_recovery() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");
    let db = Database::open(&db_path).expect("open database");

    let backend_id = BackendId::new("gdrive-primary").expect("backend id");
    db.register_backend_account(BackendAccount {
        backend_id: backend_id.clone(),
        provider: "google-drive".to_string(),
        account_subject_hash: test_content_hash(0x01),
        state: "active".to_string(),
        updated_at_ns: 1_000_000,
    })
    .expect("register account");

    let upload_id = test_update_id(0x01);
    let object_key = test_content_hash(0x55);
    let total_length = ByteCount::from_u64(10 * 1024 * 1024);

    let session = UploadSession {
        backend_id: backend_id.clone(),
        upload_id,
        object_key,
        provider_session_id: "upload-session-xyz".to_string(),
        committed_offset: ByteCount::ZERO,
        total_length,
        state: "in_progress".to_string(),
        updated_at_ns: 1_000_000,
    };

    db.begin_upload_session(session.clone())
        .expect("begin upload session");

    // Idempotent begin
    db.begin_upload_session(session)
        .expect("idempotent begin upload session");

    // Advance upload offset: 0 -> 4MB
    let offset_4m = ByteCount::from_u64(4 * 1024 * 1024);
    let advanced = db
        .advance_upload_session(
            backend_id.clone(),
            upload_id,
            ByteCount::ZERO,
            offset_4m,
            "in_progress".to_string(),
            1_000_100,
        )
        .expect("advance upload to 4MB");
    assert_eq!(advanced.committed_offset, offset_4m);

    // Stale expected offset returns conflict
    let stale_advance = db.advance_upload_session(
        backend_id.clone(),
        upload_id,
        ByteCount::ZERO, // Stale: currently 4MB
        ByteCount::from_u64(8 * 1024 * 1024),
        "in_progress".to_string(),
        1_000_200,
    );
    assert!(stale_advance.is_err());
    assert_eq!(
        stale_advance.expect_err("error").kind,
        MirageErrorKind::RepositoryConflict
    );

    // Backward offset returns invalid argument
    let backward_advance = db.advance_upload_session(
        backend_id.clone(),
        upload_id,
        offset_4m,
        ByteCount::from_u64(2 * 1024 * 1024), // Backward!
        "in_progress".to_string(),
        1_000_300,
    );
    assert!(backward_advance.is_err());
    assert_eq!(
        backward_advance.expect_err("error").kind,
        MirageErrorKind::InvalidArgument
    );

    // Advance beyond total length returns invalid argument
    let overflow_advance = db.advance_upload_session(
        backend_id.clone(),
        upload_id,
        offset_4m,
        ByteCount::from_u64(20 * 1024 * 1024), // Exceeds 10MB!
        "in_progress".to_string(),
        1_000_400,
    );
    assert!(overflow_advance.is_err());
    assert_eq!(
        overflow_advance.expect_err("error").kind,
        MirageErrorKind::InvalidArgument
    );

    // Advance to completion 10MB
    let completed = db
        .advance_upload_session(
            backend_id.clone(),
            upload_id,
            offset_4m,
            total_length,
            "completed".to_string(),
            1_000_500,
        )
        .expect("advance upload to completion");
    assert_eq!(completed.committed_offset, total_length);
    assert_eq!(completed.state, "completed");

    // Restart database and verify recovery without provider listing
    drop(db);
    let db2 = Database::open(&db_path).expect("reopen database");
    let recovered = db2
        .load_upload_session(&backend_id, upload_id)
        .expect("load upload session after restart");
    assert_eq!(recovered, Some(completed));
}
