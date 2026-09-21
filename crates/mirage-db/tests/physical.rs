use mirage_db::physical;
use mirage_db::{
    Database, PhysicalExtentRecord, PhysicalExtentState, PhysicalFileRecord,
    PhysicalReservationRecord,
};
use mirage_types::PageHash;
use rusqlite::Connection;
use tempfile::tempdir;

fn arena(byte: u8) -> PhysicalFileRecord {
    PhysicalFileRecord {
        file_id: [byte; 16],
        path: format!("C:\\arena-{byte}.bin"),
        zone: 0,
        extent_bytes: 4096,
        extent_count: 8,
        created_ns: 1,
    }
}

fn extent(byte: u8, file: u8, slot: i64) -> PhysicalExtentRecord {
    PhysicalExtentRecord {
        extent_id: [byte; 16],
        file_id: [file; 16],
        slot_index: slot,
        length_bytes: 4096,
        state: PhysicalExtentState::Reserved,
        page_hash: None,
        checksum: None,
        pin_count: 0,
        generation: 0,
        updated_ns: 10,
    }
}

fn reservation(extent_id: [u8; 16], expires_ns: i64) -> PhysicalReservationRecord {
    PhysicalReservationRecord {
        extent_id,
        owner_epoch: [0xEE; 16],
        expires_ns,
    }
}

#[test]
fn reservation_is_durable_before_commit_and_restart_replays() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("control.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn.execute_batch(
        &[
            "CREATE TABLE physical_files (file_id BLOB PRIMARY KEY CHECK(length(file_id)=16), path TEXT NOT NULL UNIQUE, zone INTEGER NOT NULL, extent_bytes INTEGER NOT NULL CHECK(extent_bytes>0), extent_count INTEGER NOT NULL CHECK(extent_count>=0), created_ns INTEGER NOT NULL) STRICT;",
            "CREATE TABLE physical_extents (extent_id BLOB PRIMARY KEY CHECK(length(extent_id)=16), file_id BLOB NOT NULL REFERENCES physical_files(file_id) ON DELETE RESTRICT, slot_index INTEGER NOT NULL CHECK(slot_index>=0), length_bytes INTEGER NOT NULL CHECK(length_bytes>0), state TEXT NOT NULL CHECK(state IN ('reserved','alive','dead','evicting')), page_hash BLOB CHECK(page_hash IS NULL OR length(page_hash)=32), checksum BLOB CHECK(checksum IS NULL OR length(checksum)=32), pin_count INTEGER NOT NULL DEFAULT 0 CHECK(pin_count>=0), generation INTEGER NOT NULL CHECK(generation>=0), updated_ns INTEGER NOT NULL, UNIQUE(file_id, slot_index)) STRICT;",
            "CREATE TABLE physical_reservations (extent_id BLOB PRIMARY KEY REFERENCES physical_extents(extent_id) ON DELETE RESTRICT, owner_epoch BLOB NOT NULL CHECK(length(owner_epoch)=16), created_ns INTEGER NOT NULL, expires_ns INTEGER NOT NULL CHECK(expires_ns>created_ns)) STRICT;",
        ]
        .join("\n"),
    )
    .unwrap();
    physical::register_file(&mut conn, &arena(1)).unwrap();
    physical::reserve_extent(&mut conn, &extent(7, 1, 0), &reservation([7; 16], 9999)).unwrap();
    // The durable reservation is visible to a second connection — bytes may
    // now be written to the slot.
    let reader = Connection::open(&path).unwrap();
    let (_, extents, reservations) = physical::load_physical_state(&reader).unwrap();
    assert_eq!(extents.len(), 1);
    assert_eq!(reservations.len(), 1);
    physical::commit_extent(
        &mut conn,
        &[7; 16],
        PageHash::from_bytes([5; 32]),
        [9; 32],
        11,
    )
    .unwrap();
    let (_, extents, reservations) = physical::load_physical_state(&reader).unwrap();
    assert_eq!(extents[0].state, PhysicalExtentState::Alive);
    assert!(reservations.is_empty());
}

#[test]
fn expired_reservations_are_reaped_and_slots_recycled() {
    let dir = tempdir().unwrap();
    let db = Database::open(&dir.path().join("control.db")).unwrap();
    let mut conn = rusqlite::Connection::open(db.reads().database_path()).unwrap();
    physical::register_file(&mut conn, &arena(1)).unwrap();
    physical::reserve_extent(&mut conn, &extent(3, 1, 0), &reservation([3; 16], 50)).unwrap();
    let reaped = physical::reap_expired_reservations(&mut conn, 100).unwrap();
    assert_eq!(reaped, 1);
    let (_, extents, _) = physical::load_physical_state(&conn).unwrap();
    assert_eq!(extents[0].state, PhysicalExtentState::Dead);
    // A dead slot can be re-reserved by a new extent identity.
    physical::reserve_extent(&mut conn, &extent(4, 1, 0), &reservation([4; 16], 9999)).unwrap();
}

#[test]
fn pinned_extents_resist_eviction_and_slot_conflicts_fail() {
    let dir = tempdir().unwrap();
    let mut conn = Connection::open(dir.path().join("control.db")).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn.execute_batch(
        "CREATE TABLE physical_files (file_id BLOB PRIMARY KEY CHECK(length(file_id)=16), path TEXT NOT NULL UNIQUE, zone INTEGER NOT NULL, extent_bytes INTEGER NOT NULL CHECK(extent_bytes>0), extent_count INTEGER NOT NULL CHECK(extent_count>=0), created_ns INTEGER NOT NULL) STRICT;
         CREATE TABLE physical_extents (extent_id BLOB PRIMARY KEY CHECK(length(extent_id)=16), file_id BLOB NOT NULL REFERENCES physical_files(file_id) ON DELETE RESTRICT, slot_index INTEGER NOT NULL CHECK(slot_index>=0), length_bytes INTEGER NOT NULL CHECK(length_bytes>0), state TEXT NOT NULL CHECK(state IN ('reserved','alive','dead','evicting')), page_hash BLOB CHECK(page_hash IS NULL OR length(page_hash)=32), checksum BLOB CHECK(checksum IS NULL OR length(checksum)=32), pin_count INTEGER NOT NULL DEFAULT 0 CHECK(pin_count>=0), generation INTEGER NOT NULL CHECK(generation>=0), updated_ns INTEGER NOT NULL, UNIQUE(file_id, slot_index)) STRICT;
         CREATE TABLE physical_reservations (extent_id BLOB PRIMARY KEY REFERENCES physical_extents(extent_id) ON DELETE RESTRICT, owner_epoch BLOB NOT NULL CHECK(length(owner_epoch)=16), created_ns INTEGER NOT NULL, expires_ns INTEGER NOT NULL CHECK(expires_ns>created_ns)) STRICT;",
    )
    .unwrap();
    physical::register_file(&mut conn, &arena(1)).unwrap();
    physical::reserve_extent(&mut conn, &extent(5, 1, 0), &reservation([5; 16], 9999)).unwrap();
    physical::commit_extent(
        &mut conn,
        &[5; 16],
        PageHash::from_bytes([1; 32]),
        [2; 32],
        11,
    )
    .unwrap();
    physical::adjust_extent_pin(&mut conn, &[5; 16], 1).unwrap();
    assert!(physical::mark_extent_dead(&mut conn, &[5; 16], 12).is_err());
    physical::adjust_extent_pin(&mut conn, &[5; 16], -1).unwrap();
    physical::mark_extent_dead(&mut conn, &[5; 16], 13).unwrap();
    // A reserved/alive slot cannot be double-reserved.
    physical::reserve_extent(&mut conn, &extent(6, 1, 1), &reservation([6; 16], 9999)).unwrap();
    assert!(
        physical::reserve_extent(&mut conn, &extent(8, 1, 1), &reservation([8; 16], 9999)).is_err()
    );
}
