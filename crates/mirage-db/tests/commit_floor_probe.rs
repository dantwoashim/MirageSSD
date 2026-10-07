//! Commit-cost probe: how much of a namespace mutation is the durable
//! commit itself versus the statements inside it. Ignored by default; run
//! `cargo test --release -p mirage-db --test commit_floor_probe -- --ignored --nocapture`.

use std::time::Instant;

use mirage_db::{Database, JournalEntry, NamespaceNodeKind, OperationKind};
use mirage_types::RepositoryId;
use rusqlite::{Connection, params};

const OPS: usize = 300;

fn mean_us(started: Instant, count: usize) -> f64 {
    started.elapsed().as_secs_f64() * 1e6 / count as f64
}

fn raw_commits(synchronous: &str) -> f64 {
    let dir = tempfile::tempdir().expect("temp");
    let connection = Connection::open(dir.path().join("raw.db")).expect("open");
    connection
        .execute_batch(&format!(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = {synchronous};
             CREATE TABLE t (id INTEGER PRIMARY KEY, value BLOB NOT NULL);"
        ))
        .expect("pragmas");
    let started = Instant::now();
    for i in 0..OPS {
        connection.execute_batch("BEGIN").expect("begin");
        connection
            .execute(
                "INSERT INTO t (value) VALUES (?1)",
                params![vec![i as u8; 64]],
            )
            .expect("insert");
        connection.execute_batch("COMMIT").expect("commit");
    }
    mean_us(started, OPS)
}

fn journal(i: usize, kind: OperationKind, tag: u8) -> JournalEntry {
    let mut operation_id = [tag; 16];
    operation_id[..8].copy_from_slice(&(i as u64).to_le_bytes());
    JournalEntry {
        operation_id,
        kind,
        payload: format!("/f{i:05}").into_bytes(),
        created_ns: 10,
    }
}

/// Journaled create and delete cost under each durability mode, plus the
/// statement work alone (Group mode minus the raw NORMAL commit floor).
#[test]
#[ignore = "timing probe; run explicitly with --release --ignored --nocapture"]
fn durability_mode_probe() {
    for (label, durability) in [
        ("Strict", mirage_db::Durability::Strict),
        ("Group", mirage_db::Durability::Group),
    ] {
        let dir = tempfile::tempdir().expect("temp");
        let db = Database::open_with_durability(&dir.path().join("control.db"), durability)
            .expect("open database");
        let volume = RepositoryId::from_bytes([7; 16]);
        let root = db.namespace_create_volume(volume, 1).expect("volume");
        let started = Instant::now();
        for i in 0..OPS {
            db.namespace_create_journaled(
                volume,
                root,
                &format!("f{i:05}"),
                NamespaceNodeKind::File,
                10,
                journal(i, OperationKind::Create, 1),
            )
            .expect("journaled create");
        }
        let create = mean_us(started, OPS);
        let started = Instant::now();
        for i in 0..OPS {
            db.namespace_delete_journaled(
                volume,
                root,
                &format!("f{i:05}"),
                11,
                journal(i, OperationKind::Delete, 2),
            )
            .expect("journaled delete");
        }
        let delete = mean_us(started, OPS);
        println!(
            "{label:<6} journaled create {create:>8.1} us   journaled delete {delete:>8.1} us"
        );
    }
}

#[test]
#[ignore = "timing probe; run explicitly with --release --ignored --nocapture"]
fn commit_floor_probe() {
    let full = raw_commits("FULL");
    let normal = raw_commits("NORMAL");

    let dir = tempfile::tempdir().expect("temp");
    let db = Database::open(&dir.path().join("control.db")).expect("open database");
    let volume = RepositoryId::from_bytes([7; 16]);
    let root = db.namespace_create_volume(volume, 1).expect("volume");

    let started = Instant::now();
    for i in 0..OPS {
        db.namespace_create(
            volume,
            root,
            &format!("plain{i:05}"),
            NamespaceNodeKind::File,
            10,
        )
        .expect("plain create");
    }
    let plain = mean_us(started, OPS);

    let started = Instant::now();
    for i in 0..OPS {
        let mut operation_id = [0u8; 16];
        operation_id[..8].copy_from_slice(&(i as u64).to_le_bytes());
        db.namespace_create_journaled(
            volume,
            root,
            &format!("journaled{i:05}"),
            NamespaceNodeKind::File,
            10,
            JournalEntry {
                operation_id,
                kind: OperationKind::Create,
                payload: format!("/journaled{i:05}").into_bytes(),
                created_ns: 10,
            },
        )
        .expect("journaled create");
    }
    let journaled = mean_us(started, OPS);

    let started = Instant::now();
    for i in 0..OPS {
        db.namespace_lookup(volume, root, &format!("journaled{i:05}"))
            .expect("lookup")
            .expect("present");
    }
    let lookup = mean_us(started, OPS);

    // Same lookup statement on a plain connection: fresh prepare per call
    // (what mirage-db does today) versus the connection's statement cache.
    const LOOKUP: &str = "SELECT d.child_inode, d.display_name, d.folded_name, i.kind, i.size,
                    i.created_ns, i.modified_ns
             FROM dirents d JOIN inodes i
               ON i.volume_id = d.volume_id AND i.inode = d.child_inode
             WHERE d.volume_id = ?1 AND d.parent_inode = ?2 AND d.folded_name = ?3";
    let raw = Connection::open(dir.path().join("control.db")).expect("raw open");
    let names: Vec<String> = (0..OPS)
        .map(|i| {
            db.namespace_lookup(volume, root, &format!("journaled{i:05}"))
                .expect("lookup")
                .expect("present")
                .folded_name
        })
        .collect();
    let parent = root.as_bytes().to_vec();
    let started = Instant::now();
    for name in &names {
        let found: Vec<u8> = raw
            .query_row(
                LOOKUP,
                params![volume.as_bytes().as_slice(), parent, name],
                |row| row.get(0),
            )
            .expect("fresh lookup");
        assert_eq!(found.len(), 16);
    }
    let fresh_prepare = mean_us(started, OPS);
    let started = Instant::now();
    for name in &names {
        let found: Vec<u8> = raw
            .prepare_cached(LOOKUP)
            .expect("cached prepare")
            .query_row(params![volume.as_bytes().as_slice(), parent, name], |row| {
                row.get(0)
            })
            .expect("cached lookup");
        assert_eq!(found.len(), 16);
    }
    let cached_prepare = mean_us(started, OPS);

    println!("=== commit floor probe ({OPS} ops, temp dir on this disk) ===");
    println!("raw lookup, fresh prepare per call        {fresh_prepare:>9.1} us");
    println!("raw lookup, prepare_cached                {cached_prepare:>9.1} us");
    println!("raw insert+commit, WAL synchronous=FULL   {full:>9.1} us");
    println!("raw insert+commit, WAL synchronous=NORMAL {normal:>9.1} us (reference only)");
    println!("Database::namespace_create (1 txn)        {plain:>9.1} us");
    println!("Database::namespace_create_journaled      {journaled:>9.1} us");
    println!("Database::namespace_lookup (read)         {lookup:>9.1} us");
}
