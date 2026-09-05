use std::sync::Arc;

use mirage_cache::{
    ArenaShard, CacheLayout, InsertHook, InsertOutcome, InsertStep, ResidentIndex, insert_page,
};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, MirageError, PageHash};

struct FailAfter(InsertStep);
impl InsertHook for FailAfter {
    fn after(&self, step: InsertStep) -> Result<(), MirageError> {
        if step == self.0 {
            Err(MirageError::cancelled("injected crash point"))
        } else {
            Ok(())
        }
    }
}

fn setup() -> (
    tempfile::TempDir,
    Database,
    Arc<ArenaShard>,
    Vec<u8>,
    PageHash,
) {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 8,
        db_journal_allowance: ByteCount::from_u64(1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(1024 * 1024),
    };
    let shard =
        Arc::new(ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("shard"));
    let db = Database::open(&directory.path().join("control.db")).expect("database");
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register shard");
    let bytes = (0_u8..=254).cycle().take(60 * 1024).collect::<Vec<_>>();
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    (directory, db, shard, bytes, hash)
}

#[test]
fn every_insertion_crash_point_is_invisible_until_metadata_commit() {
    for step in [
        InsertStep::Reserved,
        InsertStep::Written,
        InsertStep::Flushed,
        InsertStep::Rehashed,
        InsertStep::MetadataCommitted,
    ] {
        let (_directory, db, shard, bytes, hash) = setup();
        assert!(insert_page(&db, Arc::clone(&shard), hash, &bytes, &FailAfter(step)).is_err());
        let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("rebuild");
        assert_eq!(
            index.acquire(hash).expect("lookup").is_some(),
            step == InsertStep::MetadataCommitted
        );
    }
}

#[test]
fn verified_commit_is_exact_and_duplicate_is_idempotent() {
    let (_directory, db, shard, bytes, hash) = setup();
    let inserted = insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert");
    assert!(matches!(inserted, InsertOutcome::Inserted(_)));
    assert!(matches!(
        insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("duplicate"),
        InsertOutcome::Existing(_)
    ));
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    let guard = index.acquire(hash).expect("lookup").expect("resident");
    let mut read = vec![0_u8; bytes.len()];
    guard.read_exact(0, &mut read).expect("read");
    assert_eq!(read, bytes);
    let wrong = PageHash::from_bytes([9; 32]);
    assert!(insert_page(&db, shard, wrong, &bytes, &()).is_err());
}
