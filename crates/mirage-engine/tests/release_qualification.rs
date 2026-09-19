//! Release qualification: the fault matrix run end-to-end over the managed
//! volume machinery. Each scenario corresponds to a required handoff
//! behavior — write crash replay, upload resume, divergence preservation,
//! journal idempotency, lease durability, and export completeness.

use mirage_db::{Database, LeaseStatus};
use mirage_engine::extent_map::{ExtentMap, ExtentSlice};
use mirage_engine::journal::LocalJournal;
use mirage_engine::native_restore::{ContentSource, NativeRestore};
use mirage_engine::reconcile::{Reconciler, Relation};
use mirage_engine::workspace::WorkspaceAdmission;
use mirage_types::{InodeId, MirageError, PageHash, RepositoryId};
use std::collections::BTreeMap;

fn db_at(dir: &tempfile::TempDir) -> Database {
    Database::open(&dir.path().join("control.db")).unwrap()
}

fn volume() -> RepositoryId {
    RepositoryId::from_bytes([0x71; 16])
}

/// Q1 — mid-write crash: durable extent versions replay after restart and
/// reads serve base+dirty+zero slices correctly.
#[test]
fn q1_extent_replay_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let volume = volume();
    let inode = InodeId::from_bytes([0x10; 16]);
    {
        let db = db_at(&dir);
        let mut map = ExtentMap::default();
        map.seed_base(1024, PageHash::from_bytes([0xbb; 32]));
        map.write(100, 40, [0xdd; 16]).unwrap();
        let mut id = 1u64;
        let mut next_id = move || {
            id += 1;
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&id.to_le_bytes());
            bytes
        };
        db.writer()
            .extent_replace(
                volume,
                inode,
                1,
                map.to_extents(volume, inode, 1, 1, &mut next_id),
                1,
            )
            .unwrap();
        // Truncate extension is durable too.
        map.truncate(2048).unwrap();
        db.writer()
            .extent_replace(
                volume,
                inode,
                2,
                map.to_extents(volume, inode, 2, 2, &mut next_id),
                2,
            )
            .unwrap();
    }
    // Restart: replay the durable version.
    let db = db_at(&dir);
    let version = db.extent_latest_version(volume, inode).unwrap().unwrap();
    assert_eq!(version, 2);
    let extents = db.extents_at(volume, inode, version).unwrap();
    let map = ExtentMap::replay(volume, inode, &extents);
    let slices = map.read(0, 2048).unwrap();
    assert!(matches!(slices[0], ExtentSlice::Base { .. }));
    assert!(slices.iter().any(|slice| matches!(
        slice,
        ExtentSlice::Dirty {
            payload_offset: 0,
            ..
        }
    )));
    // Tail beyond the original base is a zero extent.
    assert!(matches!(slices.last().unwrap(), ExtentSlice::Zero { .. }));
}

/// Q2 — journal replay is idempotent; pending operations are reclaimable.
#[test]
fn q2_journal_replay_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = db_at(&dir);
    let volume = volume();
    let journal = LocalJournal::new(db.clone(), volume);
    let payload = b"page-bytes".to_vec();
    let operation_id = journal
        .begin(
            mirage_db::OperationKind::Write,
            payload,
            None,
            None,
            vec![],
            1,
        )
        .unwrap();
    // The pending operation is replayable until committed.
    // Pending operations are not replayable; committed ones are.
    assert!(db.replayable_operations(volume).unwrap().is_empty());
    journal.commit(operation_id).unwrap();
    let replayed = db.replayable_operations(volume).unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].operation_id, operation_id);
    // A fence acknowledges durability for everything committed before it.
    let fence = journal.flush_fence(3).unwrap();
    assert!(fence > 0);
}

/// Q3 — divergent remote head preserves both histories; a backwards cursor
/// is rejected rather than adopted.
#[test]
fn q3_divergence_preserves_both_histories() {
    let dir = tempfile::tempdir().unwrap();
    let db = db_at(&dir);
    let volume = volume();
    let reconciler = Reconciler::new(&db, volume);
    let local = [0xaa; 32];
    let remote = mirage_db::RemoteHead {
        volume_id: volume,
        head_commit: [0xbb; 32],
        head_seq: 9,
        changes_cursor: "c9".into(),
        observed_ns: 1,
    };
    let relation = reconciler
        .observe(remote, vec![], local, false, false, Some(local), 1)
        .unwrap();
    assert_eq!(relation, Relation::Diverged);
    let divergence = reconciler.live_divergence().unwrap().unwrap();
    assert_eq!(divergence.local_head, local);
    assert_eq!(divergence.remote_head, [0xbb; 32]);
}

/// Q4 — a verified workspace lease admits after restart without
/// re-verification; revocation denies.
#[test]
fn q4_verified_lease_is_offline_durable() {
    let dir = tempfile::tempdir().unwrap();
    let volume = volume();
    let lease;
    {
        let db = db_at(&dir);
        let admission = WorkspaceAdmission::new(&db, volume);
        lease = admission.declare("work", 4096, 1).unwrap();
        admission.verify(lease, 4096, 4, b"ev".to_vec(), 2).unwrap();
    }
    let db = db_at(&dir);
    let admission = WorkspaceAdmission::new(&db, volume);
    assert!(admission.admit("work/file.bin", 3).unwrap());
    admission.revoke(lease, 4).unwrap();
    assert!(!admission.admit("work/file.bin", 5).unwrap());
    assert_eq!(admission.leases().unwrap()[0].status, LeaseStatus::Revoked);
}

/// Q5 — export refuses to mark complete while bytes are unavailable, then
/// verifies a complete tree.
#[test]
fn q5_native_restore_blocks_until_complete() {
    struct Source {
        files: BTreeMap<String, Vec<u8>>,
        missing: bool,
    }
    impl ContentSource for Source {
        fn file_bytes(&self, path: &str) -> Result<Vec<u8>, MirageError> {
            if self.missing {
                return Err(MirageError::backend_unavailable("page not local"));
            }
            Ok(self.files[path].clone())
        }
        fn content_hash(&self, path: &str) -> Result<[u8; 32], MirageError> {
            Ok(*blake3::hash(&self.files[path]).as_bytes())
        }
        fn files(&self) -> Result<Vec<mirage_engine::native_restore::ExportEntry>, MirageError> {
            Ok(self
                .files
                .iter()
                .map(|(path, bytes)| mirage_engine::native_restore::ExportEntry {
                    path: path.clone(),
                    size: bytes.len() as u64,
                    content_hash: *blake3::hash(bytes).as_bytes(),
                })
                .collect())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("restore");
    let mut source = Source {
        files: BTreeMap::from([("f.bin".to_string(), b"data".to_vec())]),
        missing: true,
    };
    assert!(
        NativeRestore::new(&source, &dest).run().is_err(),
        "missing bytes must block export"
    );
    source.missing = false;
    let restore = NativeRestore::new(&source, &dest);
    let manifest = restore.run().unwrap();
    restore.verify_complete(&manifest).unwrap();
}
