use std::fs;
use std::path::Path;

use mirage_db::{APPLICATION_ID, Database, check_database};
use mirage_types::MirageErrorKind;
use rusqlite::Connection;
use tempfile::tempdir;

fn open_raw_connection(path: &Path) -> Connection {
    Connection::open(path).expect("open raw sqlite connection")
}

#[test]
fn fresh_database_initialization_and_idempotent_restart() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("control-plane.db");

    // Fresh startup initializes application ID, WAL, and every embedded migration.
    let db = Database::open(&db_path).expect("open fresh database");
    assert_eq!(db.reads().database_path(), db_path.as_path());

    let report = check_database(&db_path).expect("check database");
    assert_eq!(report.report_version, 1);
    assert!(report.quick_check_ok);
    assert!(report.integrity_check_ok);
    assert_eq!(report.foreign_key_violation_count, 0);

    // Verify raw SQLite state
    let raw = open_raw_connection(&db_path);
    let app_id: i32 = raw
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .expect("read application_id");
    assert_eq!(app_id, APPLICATION_ID);

    let journal_mode: String = raw
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("read journal_mode");
    assert_eq!(journal_mode.to_lowercase(), "wal");

    let migration_count: i64 = raw
        .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count schema_migrations");
    assert_eq!(migration_count, 10);
    drop(raw);
    drop(db);

    // Repeated startup on same file: must succeed idempotently
    let db2 = Database::open(&db_path).expect("reopen database");
    let raw2 = open_raw_connection(&db_path);
    let migration_count2: i64 = raw2
        .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count schema_migrations after restart");
    assert_eq!(migration_count2, 10);
    drop(raw2);
    drop(db2);
}

#[test]
fn wrong_application_id_is_rejected() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("foreign.db");

    let raw = open_raw_connection(&db_path);
    raw.pragma_update(None, "application_id", 0x1234_5678)
        .expect("set foreign application_id");
    drop(raw);

    let result = Database::open(&db_path);
    assert!(result.is_err());
    let error = result.expect_err("error");
    assert_eq!(error.kind, MirageErrorKind::UnsupportedLayout);
}

#[test]
fn modified_migration_history_checksum_is_rejected() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("tampered.db");

    let db = Database::open(&db_path).expect("open fresh database");
    drop(db);

    let raw = open_raw_connection(&db_path);
    let bad_checksum = [0xFF_u8; 32];
    raw.execute(
        "UPDATE schema_migrations SET checksum = ?1 WHERE version = 1",
        [bad_checksum.as_slice()],
    )
    .expect("tamper checksum");
    drop(raw);

    let result = Database::open(&db_path);
    assert!(result.is_err());
    let error = result.expect_err("error");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
}

#[test]
fn non_contiguous_migration_history_is_rejected() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("gapped.db");

    let db = Database::open(&db_path).expect("open fresh database");
    drop(db);

    let raw = open_raw_connection(&db_path);
    raw.execute("DELETE FROM schema_migrations WHERE version = 2", [])
        .expect("delete version 2");
    drop(raw);

    let result = Database::open(&db_path);
    assert!(result.is_err());
    let error = result.expect_err("error");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
}

#[test]
fn future_unsupported_migration_version_is_rejected() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("future.db");

    let db = Database::open(&db_path).expect("open fresh database");
    drop(db);

    let raw = open_raw_connection(&db_path);
    raw.execute(
        "INSERT INTO schema_migrations(version, name, checksum, applied_at_ns)
         VALUES (11, '0011_future.sql', ?1, 1000)",
        [[0xAA_u8; 32].as_slice()],
    )
    .expect("insert future migration");
    drop(raw);

    let result = Database::open(&db_path);
    assert!(result.is_err());
    let error = result.expect_err("error");
    assert_eq!(error.kind, MirageErrorKind::UnsupportedLayout);
}

#[test]
fn corrupted_database_fails_startup_quick_check() {
    let dir = tempdir().expect("temp directory");
    let db_path = dir.path().join("corrupt.db");

    let db = Database::open(&db_path).expect("open fresh database");
    drop(db);

    // Corrupt database bytes
    let mut bytes = fs::read(&db_path).expect("read db bytes");
    if bytes.len() > 200 {
        for b in &mut bytes[100..200] {
            *b = 0xAA;
        }
    }
    fs::write(&db_path, bytes).expect("write corrupt bytes");

    let result = Database::open(&db_path);
    assert!(result.is_err());
}
