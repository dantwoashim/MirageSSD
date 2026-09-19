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
    let root = db.namespace_create_volume(volume, 1).expect("create volume");
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

    assert_eq!(
        db.namespace_lookup(volume, a.inode, "x.txt").unwrap(),
        None
    );
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
        db.namespace_lookup(volume, root, "outer").unwrap().unwrap().inode,
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
    assert!(db.namespace_delete(volume, root, "d").is_err());
    db.namespace_delete(volume, dir.inode, "f").unwrap();
    db.namespace_delete(volume, root, "d").unwrap();
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
    let page1 = db
        .namespace_list_children(volume, root, None, 20)
        .unwrap();
    assert_eq!(page1.len(), 20);
    // Mutate mid-enumeration: delete an entry already returned.
    db.namespace_delete(volume, root, "file-000").unwrap();
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
    assert!(names.iter().all(|name| name != "file-000" || names
        .iter()
        .filter(|seen| *seen == name)
        .count()
        == 1));
    // Inode ids stay attached to their entries through the enumeration.
    assert!(page2.iter().all(|entry| entry.inode != mirage_types::InodeId::from_bytes([0; 16])));
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
    db.namespace_record_legacy(volume, "old.dat", file.inode).unwrap();
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
