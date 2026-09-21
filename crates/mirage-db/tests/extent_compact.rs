use mirage_db::{
    ByteExtent, Database, ExtentKind, PhysicalExtentRecord, PhysicalExtentState,
    PhysicalFileRecord, PhysicalReservationRecord,
};
use mirage_types::{InodeId, PageHash, RepositoryId};

const JOURNAL_FILE: [u8; 16] = [0x4a; 16];

#[allow(clippy::too_many_arguments)]
fn dirty_extent(
    extent_id: [u8; 16],
    volume: RepositoryId,
    inode: InodeId,
    version: i64,
    start: u64,
    length: u64,
    payload_id: [u8; 16],
    payload_offset: u64,
) -> ByteExtent {
    ByteExtent {
        extent_id,
        volume_id: volume,
        inode,
        version,
        start,
        length,
        kind: ExtentKind::Dirty,
        page_hash: None,
        base_offset: None,
        payload_id: Some(payload_id),
        payload_offset: Some(payload_offset),
        created_ns: 1,
    }
}

fn live_payload(db: &Database, payload_id: [u8; 16], slot: i64) {
    db.writer()
        .physical_reserve_extent(
            PhysicalExtentRecord {
                extent_id: payload_id,
                file_id: JOURNAL_FILE,
                slot_index: slot,
                length_bytes: 4096,
                state: PhysicalExtentState::Reserved,
                page_hash: None,
                checksum: None,
                pin_count: 0,
                generation: 0,
                updated_ns: 1,
            },
            PhysicalReservationRecord {
                extent_id: payload_id,
                owner_epoch: [0xee; 16],
                expires_ns: i64::MAX,
            },
        )
        .unwrap();
    db.writer()
        .physical_commit_extent(payload_id, PageHash::from_bytes([0x99; 32]), [0xcc; 32], 2)
        .unwrap();
}

#[test]
fn compaction_drops_superseded_versions_and_marks_dead_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("control.db")).unwrap();
    let volume = RepositoryId::from_bytes([7; 16]);
    let inode = InodeId::from_bytes([3; 16]);
    let (a, b, c) = ([0xa1; 16], [0xb2; 16], [0xc3; 16]);
    db.writer()
        .physical_register_file(PhysicalFileRecord {
            file_id: JOURNAL_FILE,
            path: "journal".into(),
            zone: 1,
            extent_bytes: 0,
            extent_count: 0,
            created_ns: 1,
        })
        .unwrap();
    live_payload(&db, a, 0);
    live_payload(&db, b, 1);
    live_payload(&db, c, 2);

    // v1 references payload A; v2 re-references A at an offset and adds B;
    // v3 supersedes both with payload C only.
    db.writer()
        .extent_replace(
            volume,
            inode,
            1,
            vec![dirty_extent([1; 16], volume, inode, 1, 0, 4096, a, 0)],
            10,
        )
        .unwrap();
    db.writer()
        .extent_replace(
            volume,
            inode,
            2,
            vec![
                dirty_extent([2; 16], volume, inode, 2, 0, 1024, a, 4),
                dirty_extent([3; 16], volume, inode, 2, 1024, 1024, b, 0),
            ],
            20,
        )
        .unwrap();
    db.writer()
        .extent_replace(
            volume,
            inode,
            3,
            vec![dirty_extent([4; 16], volume, inode, 3, 0, 2048, c, 0)],
            30,
        )
        .unwrap();

    let dead = db
        .writer()
        .extent_compact_volume(volume, JOURNAL_FILE, 40)
        .unwrap();
    let mut dead_ids: Vec<[u8; 16]> = dead.iter().map(|(id, _)| *id).collect();
    dead_ids.sort();
    assert_eq!(dead_ids, vec![a, b]);
    assert!(dead.iter().all(|(_, length)| *length == 4096));

    // Only head-version rows remain; A and B are dead, C stays alive.
    assert!(
        db.extents_at(volume, inode, 1).unwrap().is_empty()
            && db.extents_at(volume, inode, 2).unwrap().is_empty()
    );
    assert_eq!(db.extents_at(volume, inode, 3).unwrap().len(), 1);
    let (_, extents, _) = db.load_physical_state().unwrap();
    let state_of = |id: [u8; 16]| {
        extents
            .iter()
            .find(|extent| extent.extent_id == id)
            .map(|extent| extent.state)
    };
    assert_eq!(state_of(a), Some(PhysicalExtentState::Dead));
    assert_eq!(state_of(b), Some(PhysicalExtentState::Dead));
    assert_eq!(state_of(c), Some(PhysicalExtentState::Alive));

    // Idempotent: a second compact transitions nothing.
    assert!(
        db.writer()
            .extent_compact_volume(volume, JOURNAL_FILE, 50)
            .unwrap()
            .is_empty()
    );
}
