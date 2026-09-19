use std::sync::Arc;

use mirage_cache::{
    ArenaShard, CacheLayout, InsertHook, InsertOutcome, InsertStep, ResidentIndex, insert_page,
    insert_reserved_pages,
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

#[test]
fn reservation_batch_preserves_requested_locality_and_hash_sorted_results() {
    let (_directory, db, _shard, _, _) = setup();
    let requested = [3, 1, 2].map(|value| PageHash::from_bytes([value; 32]));
    let outcomes = db
        .reserve_cache_slots_batch(requested.iter().map(|hash| (*hash, 4096)).collect())
        .unwrap();
    let records: Vec<_> = outcomes
        .into_iter()
        .map(|outcome| match outcome {
            mirage_db::ReserveCacheSlotOutcome::Reserved(record) => record,
            _ => panic!("unexpected resident"),
        })
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|record| record.page_hash.unwrap())
            .collect::<Vec<_>>(),
        [requested[1], requested[2], requested[0]]
    );
    for (slot, hash) in requested.iter().enumerate() {
        assert_eq!(
            records
                .iter()
                .find(|record| record.page_hash == Some(*hash))
                .unwrap()
                .slot_index,
            slot as u32
        );
    }
    let before = db.load_cache_slots().unwrap();
    assert!(
        db.reserve_cache_slots_batch(vec![
            (requested[0], 4096),
            (requested[1], 4096),
            (requested[0], 8192)
        ])
        .is_err()
    );
    assert_eq!(db.load_cache_slots().unwrap(), before);
}

fn reserve_batch(db: &Database, payloads: &[Vec<u8>]) -> Vec<mirage_db::CacheSlotRecord> {
    let requests = payloads
        .iter()
        .map(|bytes| {
            (
                PageHash::from_bytes(*blake3::hash(bytes).as_bytes()),
                bytes.len() as u32,
            )
        })
        .collect();
    let reservations = db.reserve_cache_slots_batch(requests).unwrap();
    payloads
        .iter()
        .map(|bytes| {
            let hash = PageHash::from_bytes(*blake3::hash(bytes).as_bytes());
            reservations
                .iter()
                .find_map(|outcome| match outcome {
                    mirage_db::ReserveCacheSlotOutcome::Reserved(record)
                        if record.page_hash == Some(hash) =>
                    {
                        Some(*record)
                    }
                    _ => None,
                })
                .unwrap()
        })
        .collect()
}

#[test]
fn batch_insertion_is_all_or_nothing_at_every_crash_boundary() {
    for step in [
        InsertStep::Reserved,
        InsertStep::Written,
        InsertStep::Flushed,
        InsertStep::Rehashed,
        InsertStep::MetadataCommitted,
    ] {
        let (_directory, db, shard, bytes, _) = setup();
        let payloads = vec![bytes, vec![0x67; 131]];
        let records = reserve_batch(&db, &payloads);
        let batch: Vec<_> = records
            .iter()
            .copied()
            .zip(payloads.iter().map(Vec::as_slice))
            .collect();
        assert!(insert_reserved_pages(&db, Arc::clone(&shard), &batch, &FailAfter(step)).is_err());
        let residents = db.load_resident_cache_slots().unwrap();
        assert_eq!(
            residents.len(),
            if step == InsertStep::MetadataCommitted {
                2
            } else {
                0
            },
            "{step:?}"
        );
        let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).unwrap();
        for (record, expected) in records.iter().zip(&payloads) {
            if let Some(guard) = index.acquire(record.page_hash.unwrap()).unwrap() {
                let mut actual = vec![0; expected.len()];
                guard.read_exact(0, &mut actual).unwrap();
                assert_eq!(actual, *expected);
            }
        }
        let report = mirage_cache::reconcile(&db, &shard, true).unwrap();
        assert!(report.actions.is_empty(), "{step:?}: {:?}", report.actions);
    }
}

#[test]
fn batch_rehash_failure_never_publishes_a_good_prefix() {
    let (_directory, db, shard, bytes, _) = setup();
    let mut payloads = vec![bytes, vec![0x63; 8193]];
    let records = reserve_batch(&db, &payloads);
    payloads[1][0] ^= 1;
    let batch: Vec<_> = records
        .iter()
        .copied()
        .zip(payloads.iter().map(Vec::as_slice))
        .collect();
    let error = insert_reserved_pages(&db, Arc::clone(&shard), &batch, &()).unwrap_err();
    assert_eq!(error.code, "MIRAGE_INTEGRITY_MISMATCH");
    assert!(db.load_resident_cache_slots().unwrap().is_empty());
    assert!(
        db.load_cache_slots()
            .unwrap()
            .iter()
            .all(|slot| slot.state == mirage_db::CacheSlotState::Free)
    );
}

#[test]
fn database_batch_rejects_stale_or_duplicate_slots_without_partial_commit() {
    let (_directory, db, _shard, bytes, _) = setup();
    let records = reserve_batch(&db, &[bytes, vec![0x68; 7]]);
    let mut stale = records.clone();
    stale[1].generation += 1;
    assert!(db.commit_cache_slots_batch(stale).is_err());
    assert!(
        db.commit_cache_slots_batch(vec![records[0], records[0]])
            .is_err()
    );
    assert!(db.load_resident_cache_slots().unwrap().is_empty());
    assert!(
        db.load_cache_slots()
            .unwrap()
            .iter()
            .filter(|slot| slot.page_hash.is_some())
            .all(|slot| slot.state == mirage_db::CacheSlotState::Reserved)
    );
    let committed = db.commit_cache_slots_batch(records).unwrap();
    assert_eq!(committed.len(), 2);
    assert!(
        committed
            .iter()
            .all(|record| record.state == mirage_db::CacheSlotState::Resident)
    );
}

#[test]
fn batch_writes_consecutive_full_slots_as_one_run_with_exact_bytes() {
    let (_directory, db, shard, _, _) = setup();
    let page = 64 * 1024;
    let payloads: Vec<Vec<u8>> = (0_u8..5)
        .map(|index| {
            let len = if index == 4 { 12_345 } else { page };
            (0..len).map(|i| (i as u8) ^ (index * 37)).collect()
        })
        .collect();
    let records = reserve_batch(&db, &payloads);
    assert!(
        records
            .windows(2)
            .all(|pair| pair[1].slot_index == pair[0].slot_index + 1),
        "fresh reservations occupy consecutive slots"
    );
    let batch: Vec<_> = records
        .iter()
        .copied()
        .zip(payloads.iter().map(Vec::as_slice))
        .collect();
    insert_reserved_pages(&db, Arc::clone(&shard), &batch, &()).unwrap();
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).unwrap();
    for (record, expected) in &batch {
        let guard = index.acquire(record.page_hash.unwrap()).unwrap().unwrap();
        assert_eq!(guard.logical_length() as usize, expected.len());
        let mut actual = vec![0; expected.len()];
        guard.read_exact(0, &mut actual).unwrap();
        assert_eq!(&actual[..], *expected);
    }
    let guards: Vec<_> = batch
        .iter()
        .map(|(record, _)| index.acquire(record.page_hash.unwrap()).unwrap().unwrap())
        .collect();
    let total: usize = payloads.iter().map(Vec::len).sum();
    let mut joined = vec![0; total];
    mirage_cache::read_contiguous(&shard, &guards, 0, &mut joined).unwrap();
    let expected: Vec<u8> = payloads.iter().flatten().copied().collect();
    assert_eq!(joined, expected);
    #[cfg(windows)]
    {
        let extents = shard.physical_extent_count().unwrap();
        assert!(
            extents <= 2,
            "five consecutive slots written as one run should allocate at most a header extent plus one payload extent, got {extents}"
        );
    }
}

#[test]
fn batch_with_a_short_middle_page_splits_the_run_and_keeps_every_slot_exact() {
    let (_directory, db, shard, _, _) = setup();
    let page = 64 * 1024;
    let payloads: Vec<Vec<u8>> = [page, 100, page, page]
        .iter()
        .enumerate()
        .map(|(index, len)| {
            (0..*len)
                .map(|i| (i as u8).wrapping_add(index as u8))
                .collect()
        })
        .collect();
    let records = reserve_batch(&db, &payloads);
    let batch: Vec<_> = records
        .iter()
        .copied()
        .zip(payloads.iter().map(Vec::as_slice))
        .collect();
    insert_reserved_pages(&db, Arc::clone(&shard), &batch, &()).unwrap();
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).unwrap();
    for (record, expected) in &batch {
        let guard = index.acquire(record.page_hash.unwrap()).unwrap().unwrap();
        let mut actual = vec![0; expected.len()];
        guard.read_exact(0, &mut actual).unwrap();
        assert_eq!(&actual[..], *expected);
    }
}

#[test]
fn contiguous_write_rejects_payloads_that_do_not_fill_the_middle_slots() {
    let (_directory, _db, shard, _, _) = setup();
    let page = 64 * 1024;
    assert!(shard.write_slots_contiguous(0, 0, &[1]).is_err());
    assert!(shard.write_slots_contiguous(0, 2, &vec![1; page]).is_err());
    assert!(
        shard
            .write_slots_contiguous(0, 2, &vec![1; 2 * page + 1])
            .is_err()
    );
    assert!(
        shard
            .write_slots_contiguous(7, 2, &vec![1; page + 1])
            .is_err()
    );
    shard
        .write_slots_contiguous(0, 2, &vec![1; page + 1])
        .unwrap();
}
