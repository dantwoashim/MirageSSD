use mirage_db::{
    ByteExtent, Database, ExtentKind, ExtentMutation, JournalEntry, NamespaceNodeKind,
    OperationKind, OperationPayloadRecord, OperationRecord, OperationStatus, PhysicalCommit,
    PhysicalExtentRecord, PhysicalExtentState, PhysicalFileRecord, PhysicalReservationRecord,
    SequencedMutation,
};
use mirage_types::{PageHash, RepositoryId};
use tempfile::tempdir;

fn open() -> (tempfile::TempDir, Database) {
    let dir = tempdir().expect("temp directory");
    let db = Database::open(&dir.path().join("control.db")).expect("open database");
    (dir, db)
}

fn vol(byte: u8) -> RepositoryId {
    RepositoryId::from_bytes([byte; 16])
}

fn journal(id: u8, kind: OperationKind, payload: Vec<u8>) -> JournalEntry {
    JournalEntry {
        operation_id: [id; 16],
        kind,
        payload,
        created_ns: 5,
    }
}

fn operation(volume: RepositoryId, id: u8, kind: OperationKind) -> OperationRecord {
    OperationRecord {
        operation_id: [id; 16],
        device_seq: 0, // assigned inside the transaction
        volume_id: volume,
        base_commit: None,
        kind,
        payload: Vec::new(),
        status: OperationStatus::Pending,
        flush_group: None,
        depends_on: None,
        created_ns: 9,
    }
}

fn arena(file_id: u8) -> PhysicalFileRecord {
    PhysicalFileRecord {
        file_id: [file_id; 16],
        path: "C:\\arena.bin".to_string(),
        zone: 0,
        extent_bytes: 4096,
        extent_count: 8,
        created_ns: 1,
    }
}

fn reserved_extent(id: u8, file_id: u8, slot: i64) -> PhysicalExtentRecord {
    PhysicalExtentRecord {
        extent_id: [id; 16],
        file_id: [file_id; 16],
        slot_index: slot,
        length_bytes: 4096,
        state: PhysicalExtentState::Reserved,
        page_hash: None,
        checksum: None,
        pin_count: 0,
        generation: 0,
        updated_ns: 3,
    }
}

fn reservation(id: u8) -> PhysicalReservationRecord {
    PhysicalReservationRecord {
        extent_id: [id; 16],
        owner_epoch: [0xAB; 16],
        expires_ns: 99,
    }
}

#[test]
fn create_journaled_commits_node_and_one_operation() {
    let (_dir, db) = open();
    let volume = vol(1);
    let root = db.namespace_create_volume(volume, 1).unwrap();

    let first = db
        .namespace_create_journaled(
            volume,
            root,
            "a.txt",
            NamespaceNodeKind::File,
            10,
            journal(0x01, OperationKind::Create, b"p/a.txt".to_vec()),
        )
        .expect("first create");
    let second = db
        .namespace_create_journaled(
            volume,
            root,
            "b.txt",
            NamespaceNodeKind::File,
            11,
            journal(0x02, OperationKind::Create, b"p/b.txt".to_vec()),
        )
        .expect("second create");

    assert!(
        db.namespace_lookup(volume, root, "a.txt")
            .unwrap()
            .is_some()
    );
    let ops = db.replayable_operations(volume).unwrap();
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].operation_id, [0x01; 16]);
    assert_eq!(ops[0].kind, OperationKind::Create);
    assert_eq!(ops[0].payload, b"p/a.txt".to_vec());
    assert_eq!(ops[0].status, OperationStatus::Committed);
    // Second operation got the next sequence after the first.
    assert_eq!(ops[1].operation_id, [0x02; 16]);
    assert_eq!(ops[1].device_seq, ops[0].device_seq + 1);
    let _ = first;
    let _ = second;
}

#[test]
fn create_journaled_duplicate_operation_id_rolls_back_node() {
    let (_dir, db) = open();
    let volume = vol(2);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    db.namespace_create_journaled(
        volume,
        root,
        "a.txt",
        NamespaceNodeKind::File,
        10,
        journal(0x01, OperationKind::Create, Vec::new()),
    )
    .expect("first create");

    let error = db.namespace_create_journaled(
        volume,
        root,
        "b.txt",
        NamespaceNodeKind::File,
        11,
        journal(0x01, OperationKind::Create, Vec::new()),
    );
    assert!(error.is_err(), "duplicate operation id must fail");
    assert!(
        db.namespace_lookup(volume, root, "b.txt")
            .unwrap()
            .is_none(),
        "rolled-back node must not exist"
    );
}

#[test]
fn delete_journaled_duplicate_operation_id_keeps_node() {
    let (_dir, db) = open();
    let volume = vol(3);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    db.namespace_create_journaled(
        volume,
        root,
        "keep.txt",
        NamespaceNodeKind::File,
        10,
        journal(0x07, OperationKind::Create, Vec::new()),
    )
    .expect("create");

    let error = db.namespace_delete_journaled(
        volume,
        root,
        "keep.txt",
        11,
        journal(0x07, OperationKind::Delete, Vec::new()),
    );
    assert!(error.is_err(), "duplicate operation id must fail");
    assert!(
        db.namespace_lookup(volume, root, "keep.txt")
            .unwrap()
            .is_some(),
        "rolled-back delete must leave the node"
    );
}

#[test]
fn rename_journaled_moves_entry_and_journals_rename() {
    let (_dir, db) = open();
    let volume = vol(4);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let entry = db
        .namespace_create_journaled(
            volume,
            root,
            "old.txt",
            NamespaceNodeKind::File,
            10,
            journal(0x01, OperationKind::Create, Vec::new()),
        )
        .expect("create");

    db.namespace_rename_journaled(
        volume,
        root,
        "old.txt",
        root,
        "new.txt",
        11,
        journal(0x02, OperationKind::Rename, b"old\nnew".to_vec()),
    )
    .expect("rename");

    assert!(
        db.namespace_lookup(volume, root, "old.txt")
            .unwrap()
            .is_none()
    );
    let moved = db
        .namespace_lookup(volume, root, "new.txt")
        .unwrap()
        .expect("renamed entry");
    assert_eq!(moved.inode, entry.inode);

    let ops = db.replayable_operations(volume).unwrap();
    let rename = ops
        .iter()
        .find(|op| op.kind == OperationKind::Rename)
        .expect("committed rename operation");
    assert_eq!(rename.operation_id, [0x02; 16]);
    assert_eq!(rename.payload, b"old\nnew".to_vec());
}

#[test]
fn sequenced_mutation_commits_everything_in_one_transaction() {
    let (_dir, db) = open();
    let volume = vol(5);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let file = db
        .namespace_create_journaled(
            volume,
            root,
            "f.bin",
            NamespaceNodeKind::File,
            10,
            journal(0x01, OperationKind::Create, Vec::new()),
        )
        .expect("create");
    db.writer()
        .physical_register_file(arena(0xF0))
        .expect("arena");
    // Burn a sequence so the committed operation proves its seq was
    // assigned inside the transaction, not taken from the record.
    let burned = db.next_operation_seq(volume).unwrap();

    let mutation = SequencedMutation {
        reservation: Some((reserved_extent(0xE1, 0xF0, 0), reservation(0xE1))),
        extents: Some(ExtentMutation {
            volume_id: volume,
            inode: file.inode,
            version: 1,
            eof: 4096,
            extents: vec![ByteExtent {
                extent_id: [0xB1; 16],
                volume_id: volume,
                inode: file.inode,
                version: 1,
                start: 0,
                length: 4096,
                kind: ExtentKind::Dirty,
                page_hash: None,
                base_offset: None,
                payload_id: Some([0xE1; 16]),
                payload_offset: Some(0),
                created_ns: 100,
            }],
        }),
        operation: operation(volume, 0xAA, OperationKind::Write),
        payloads: vec![OperationPayloadRecord {
            payload_id: [0xE1; 16],
            operation_id: [0xAA; 16],
            path: "payload.bin".to_string(),
            bytes: 4096,
            checksum: Some([0xCC; 32]),
            flushed_ns: None,
        }],
        physical: Some(PhysicalCommit {
            extent_id: [0xE1; 16],
            page_hash: PageHash::from_bytes([0xCC; 32]),
            checksum: [0xCC; 32],
        }),
        modified_ns: Some((file.inode, 424242)),
        now_ns: 100,
    };
    db.sequenced_mutation_commit(mutation)
        .expect("sequenced commit");

    let ops = db.replayable_operations(volume).unwrap();
    let write = ops
        .iter()
        .find(|op| op.operation_id == [0xAA; 16])
        .expect("committed write operation");
    assert_eq!(
        write.device_seq,
        burned + 1,
        "seq assigned inside the transaction"
    );
    assert_eq!(write.status, OperationStatus::Committed);

    let (head_version, eof) = db.extent_head(volume, file.inode).unwrap().unwrap();
    assert_eq!((head_version, eof), (1, 4096));

    let (_, extents, reservations) = db.load_physical_state().unwrap();
    let extent = extents
        .iter()
        .find(|extent| extent.extent_id == [0xE1; 16])
        .expect("physical extent");
    assert_eq!(extent.state, PhysicalExtentState::Alive);
    assert!(reservations.is_empty(), "reservation consumed by commit");

    let stat = db.namespace_stat(volume, file.inode).unwrap().unwrap();
    assert_eq!(stat.modified_ns, 424242);
}

#[test]
fn sequenced_mutation_failure_leaves_no_partial_state() {
    let (_dir, db) = open();
    let volume = vol(6);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let file = db
        .namespace_create_journaled(
            volume,
            root,
            "f.bin",
            NamespaceNodeKind::File,
            10,
            journal(0x01, OperationKind::Create, Vec::new()),
        )
        .expect("create");
    db.writer()
        .physical_register_file(arena(0xF1))
        .expect("arena");
    db.writer()
        .physical_reserve_extent(reserved_extent(0xE2, 0xF1, 0), reservation(0xE2))
        .expect("occupy slot 0");

    // Reservation conflicts with the occupied slot: the whole sequenced
    // mutation must roll back — no extent version, no operation, no rows.
    let before_ops = db.replayable_operations(volume).unwrap().len();
    let mutation = SequencedMutation {
        reservation: Some((reserved_extent(0xE3, 0xF1, 0), reservation(0xE3))),
        extents: Some(ExtentMutation {
            volume_id: volume,
            inode: file.inode,
            version: 1,
            eof: 4096,
            extents: vec![ByteExtent {
                extent_id: [0xB2; 16],
                volume_id: volume,
                inode: file.inode,
                version: 1,
                start: 0,
                length: 4096,
                kind: ExtentKind::Dirty,
                page_hash: None,
                base_offset: None,
                payload_id: Some([0xE3; 16]),
                payload_offset: Some(0),
                created_ns: 100,
            }],
        }),
        operation: operation(volume, 0xBB, OperationKind::Write),
        payloads: Vec::new(),
        physical: None,
        modified_ns: Some((file.inode, 777)),
        now_ns: 100,
    };
    assert!(db.sequenced_mutation_commit(mutation).is_err());

    assert!(db.extent_head(volume, file.inode).unwrap().is_none());
    assert_eq!(
        db.replayable_operations(volume).unwrap().len(),
        before_ops,
        "no extra operation may be journaled"
    );
    let (_, extents, reservations) = db.load_physical_state().unwrap();
    assert!(
        extents.iter().all(|extent| extent.extent_id != [0xE3; 16]),
        "conflicting reservation rolled back"
    );
    assert_eq!(
        reservations
            .iter()
            .filter(|reservation| reservation.extent_id == [0xE3; 16])
            .count(),
        0
    );
    // The pre-existing reservation on slot 0 is untouched.
    assert!(reservations.iter().any(|r| r.extent_id == [0xE2; 16]));
    let stat = db.namespace_stat(volume, file.inode).unwrap().unwrap();
    assert_ne!(stat.modified_ns, 777);
}
