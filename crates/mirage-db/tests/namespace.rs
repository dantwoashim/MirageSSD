use mirage_db::{Database, NamespaceNodeKind};
use mirage_types::{RepositoryId, root_inode};
use tempfile::tempdir;

fn open() -> (tempfile::TempDir, Database) {
    let dir = tempdir().expect("temp directory");
    let db = Database::open(&dir.path().join("control.db")).expect("open database");
    (dir, db)
}

fn vol(byte: u8) -> RepositoryId {
    RepositoryId::from_bytes([byte; 16])
}

#[test]
fn create_lookup_and_stat_round_trip() {
    let (_dir, db) = open();
    let volume = vol(1);
    let root = db
        .namespace_create_volume(volume, 1)
        .expect("create volume");
    assert_eq!(root, root_inode(volume));

    let entry = db
        .namespace_create(volume, root, "Game", NamespaceNodeKind::Directory, 2)
        .expect("create directory");
    let file = db
        .namespace_create(volume, entry.inode, "Save.bin", NamespaceNodeKind::File, 3)
        .expect("create file");
    db.namespace_set_file_roots(volume, file.inode, 4096, Some([7; 32]), None, 4)
        .expect("set file roots");

    let found = db
        .namespace_lookup(volume, root, "game")
        .expect("lookup")
        .expect("entry present");
    assert_eq!(found.display_name, "Game");
    assert_eq!(found.inode, entry.inode);

    let stat = db
        .namespace_stat(volume, file.inode)
        .expect("stat")
        .expect("inode present");
    assert_eq!(stat.kind, NamespaceNodeKind::File);
    assert_eq!(stat.size, 4096);
    assert_eq!(stat.version_root, Some([7; 32]));

    assert_eq!(
        db.namespace_resolve_path(volume, "game/save.bin")
            .expect("resolve"),
        Some(file.inode)
    );
}

#[test]
fn rename_preserves_inode_identity_across_directories() {
    let (_dir, db) = open();
    let volume = vol(2);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let a = db
        .namespace_create(volume, root, "A", NamespaceNodeKind::Directory, 2)
        .unwrap();
    let b = db
        .namespace_create(volume, root, "B", NamespaceNodeKind::Directory, 3)
        .unwrap();
    let file = db
        .namespace_create(volume, a.inode, "x.txt", NamespaceNodeKind::File, 4)
        .unwrap();

    db.namespace_rename(volume, a.inode, "x.txt", b.inode, "y.txt", 5)
        .expect("rename");

    assert_eq!(db.namespace_lookup(volume, a.inode, "x.txt").unwrap(), None);
    let moved = db
        .namespace_lookup(volume, b.inode, "y.txt")
        .unwrap()
        .expect("moved entry");
    assert_eq!(moved.inode, file.inode);
    assert_eq!(
        db.namespace_resolve_path(volume, "B/y.txt").unwrap(),
        Some(file.inode)
    );
}

#[test]
fn rename_rejects_directory_cycles() {
    let (_dir, db) = open();
    let volume = vol(3);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let a = db
        .namespace_create(volume, root, "outer", NamespaceNodeKind::Directory, 2)
        .unwrap();
    let b = db
        .namespace_create(volume, a.inode, "inner", NamespaceNodeKind::Directory, 3)
        .unwrap();

    assert!(
        db.namespace_rename(volume, root, "outer", b.inode, "moved", 4)
            .is_err()
    );
    assert_eq!(
        db.namespace_lookup(volume, root, "outer")
            .unwrap()
            .unwrap()
            .inode,
        a.inode
    );
}

#[test]
fn delete_requires_empty_directory() {
    let (_dir, db) = open();
    let volume = vol(4);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let dir = db
        .namespace_create(volume, root, "d", NamespaceNodeKind::Directory, 2)
        .unwrap();
    db.namespace_create(volume, dir.inode, "f", NamespaceNodeKind::File, 3)
        .unwrap();
    assert!(db.namespace_delete(volume, root, "d", 5).is_err());
    db.namespace_delete(volume, dir.inode, "f", 4).unwrap();
    db.namespace_delete(volume, root, "d", 6).unwrap();
    assert_eq!(db.namespace_lookup(volume, root, "d").unwrap(), None);
}

#[test]
fn folded_name_collisions_and_reserved_names() {
    let (_dir, db) = open();
    let volume = vol(5);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    db.namespace_create(volume, root, "Report.TXT", NamespaceNodeKind::File, 2)
        .unwrap();
    // Case-folded collision is a conflict, not a second entry.
    assert!(
        db.namespace_create(volume, root, "report.txt", NamespaceNodeKind::File, 3)
            .is_err()
    );
    for name in ["CON", "con", "LPT3", "foo.txt ", "foo.", "a/b", "a\\b"] {
        assert!(
            db.namespace_create(volume, root, name, NamespaceNodeKind::File, 4)
                .is_err(),
            "accepted invalid name {name:?}"
        );
    }
    // Unicode case folding: "Ä.txt" collides with "ä.txt".
    db.namespace_create(volume, root, "Ä.txt", NamespaceNodeKind::File, 5)
        .unwrap();
    assert!(
        db.namespace_create(volume, root, "ä.txt", NamespaceNodeKind::File, 6)
            .is_err()
    );
}

#[test]
fn paged_listing_survives_mutation() {
    let (_dir, db) = open();
    let volume = vol(6);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    for index in 0..50u32 {
        db.namespace_create(
            volume,
            root,
            &format!("file-{index:03}"),
            NamespaceNodeKind::File,
            2,
        )
        .unwrap();
    }
    let page1 = db.namespace_list_children(volume, root, None, 20).unwrap();
    assert_eq!(page1.len(), 20);
    // Mutate mid-enumeration: delete an entry already returned.
    db.namespace_delete(volume, root, "file-000", 3).unwrap();
    let marker = page1.last().unwrap().folded_name.clone();
    let page2 = db
        .namespace_list_children(volume, root, Some(&marker), 40)
        .unwrap();
    let names: Vec<_> = page1
        .iter()
        .chain(&page2)
        .map(|entry| entry.display_name.clone())
        .collect();
    assert_eq!(names.len(), 50);
    assert!(
        names.iter().all(
            |name| name != "file-000" || names.iter().filter(|seen| *seen == name).count() == 1
        )
    );
    // Inode ids stay attached to their entries through the enumeration.
    assert!(
        page2
            .iter()
            .all(|entry| entry.inode != mirage_types::InodeId::from_bytes([0; 16]))
    );
}

#[test]
fn legacy_translation_is_explicit() {
    let (_dir, db) = open();
    let volume = vol(7);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let file = db
        .namespace_create(volume, root, "old.dat", NamespaceNodeKind::File, 2)
        .unwrap();
    assert_eq!(
        db.namespace_resolve_legacy(volume, "old.dat").unwrap(),
        None
    );
    db.namespace_record_legacy(volume, "old.dat", file.inode)
        .unwrap();
    assert_eq!(
        db.namespace_resolve_legacy(volume, "old.dat").unwrap(),
        Some(file.inode)
    );
}

#[test]
fn foreign_volume_and_missing_parent_are_rejected() {
    let (_dir, db) = open();
    let volume = vol(8);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    // Creating under a missing parent fails closed.
    let ghost = mirage_types::InodeId::from_bytes([0xaa; 16]);
    assert!(
        db.namespace_create(volume, ghost, "x", NamespaceNodeKind::File, 2)
            .is_err()
    );
    // The same folded name in a second volume is independent.
    let other = vol(9);
    db.namespace_create_volume(other, 1).unwrap();
    let root2 = root_inode(other);
    db.namespace_create(volume, root, "same", NamespaceNodeKind::File, 2)
        .unwrap();
    db.namespace_create(other, root2, "same", NamespaceNodeKind::File, 2)
        .unwrap();
}

#[test]
fn device_identity_is_stable_and_nonzero() {
    let (_dir, db) = open();
    let first = db.device_id(1).expect("device id");
    let second = db.device_id(2).expect("device id again");
    assert_eq!(first, second);
    assert_ne!(first, mirage_types::DeviceId::from_bytes([0; 16]));
}

#[test]
fn mutations_record_deltas_and_checkpoints() {
    let (_dir, db) = open();
    let volume = vol(10);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let entry = db
        .namespace_create(volume, root, "a.txt", mirage_db::NamespaceNodeKind::File, 2)
        .unwrap();
    db.namespace_rename(volume, root, "a.txt", root, "b.txt", 3)
        .unwrap();
    assert_eq!(db.namespace_delta_backlog(volume).unwrap(), 2);

    let (seq, hash) = db.namespace_checkpoint(volume, 4).expect("checkpoint");
    assert_eq!(seq, 1);
    assert_ne!(hash, [0; 32]);
    // New deltas open a segment on top of the new checkpoint.
    db.namespace_delete(volume, root, "b.txt", 5).unwrap();
    assert_eq!(db.namespace_delta_backlog(volume).unwrap(), 1);
    assert_eq!(db.namespace_stat(volume, entry.inode).unwrap(), None);
}

#[test]
fn checkpoint_document_decodes_with_live_entries() {
    let (_dir, db) = open();
    let volume = vol(11);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let dir = db
        .namespace_create(
            volume,
            root,
            "d",
            mirage_db::NamespaceNodeKind::Directory,
            2,
        )
        .unwrap();
    let file = db
        .namespace_create(
            volume,
            dir.inode,
            "f",
            mirage_db::NamespaceNodeKind::File,
            3,
        )
        .unwrap();
    let (_seq, hash) = db.namespace_checkpoint(volume, 4).unwrap();
    // Read the stored document back and decode it with the versioned codec.
    let document = db
        .namespace_latest_checkpoint_document(volume)
        .unwrap()
        .expect("checkpoint document");
    let decoded = mirage_manifest::namespace::decode_checkpoint(&document).unwrap();
    assert_eq!(mirage_manifest::namespace::checkpoint_hash(&document), hash);
    assert_eq!(decoded.nodes.len(), 3);
    let file_node = decoded
        .nodes
        .iter()
        .find(|node| node.inode == file.inode)
        .expect("file node");
    assert_eq!(file_node.parent, Some(dir.inode));
    assert_eq!(file_node.display_name, "f");
    // The checkpoint closes the open segment; new deltas start at 0.
    assert_eq!(db.namespace_delta_backlog(volume).unwrap(), 0);
}

#[test]
fn pin_held_covers_descendants_and_unpin_releases() {
    let (_dir, db) = open();
    let volume = vol(9);
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let dir = db
        .namespace_create(volume, root, "keep", NamespaceNodeKind::Directory, 2)
        .unwrap();
    let file = db
        .namespace_create(volume, dir.inode, "a.bin", NamespaceNodeKind::File, 3)
        .unwrap();
    let other = db
        .namespace_create(volume, root, "free.bin", NamespaceNodeKind::File, 4)
        .unwrap();

    db.namespace_pin(volume, dir.inode, 5).unwrap();
    assert!(db.namespace_pin_held(volume, dir.inode).unwrap());
    // A file under the pinned directory is protected without its own row.
    assert!(db.namespace_pin_held(volume, file.inode).unwrap());
    assert!(!db.namespace_pin_held(volume, other.inode).unwrap());
    assert!(!db.namespace_pin_held(volume, root).unwrap());
    assert_eq!(db.namespace_pins(volume).unwrap(), vec![dir.inode]);

    assert!(db.namespace_unpin(volume, dir.inode).unwrap());
    assert!(!db.namespace_pin_held(volume, file.inode).unwrap());
    // Unpinning something never pinned reports false, not an error.
    assert!(!db.namespace_unpin(volume, other.inode).unwrap());
}
