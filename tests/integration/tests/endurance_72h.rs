use mirage_cache::{ArenaShard, CacheLayout, InsertOutcome, ResidentIndex, insert_page, reconcile};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, PageHash};
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::Arc,
    time::{Duration, Instant},
};

const PAGE_SIZE: usize = 64 * 1024;
const LOGICAL_SIZE: usize = 60 * 1024;
const SLOT_COUNT: u32 = 32;

#[test]
fn mixed_recovery_endurance_smoke() {
    run(Duration::from_secs(30), Some(5_000));
}

#[test]
#[ignore = "72-hour release gate; set MIRAGE_ENDURANCE_SECONDS to override"]
fn recovery_endurance_72h() {
    let seconds = std::env::var("MIRAGE_ENDURANCE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(72 * 60 * 60);
    run(Duration::from_secs(seconds), None);
}

fn run(duration: Duration, operation_limit: Option<u64>) {
    let directory = tempfile::tempdir().expect("directory");
    let arena_path = directory.path().join("arena.bin");
    let database_path = directory.path().join("control.db");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(PAGE_SIZE as u64),
        slot_count: SLOT_COUNT,
        db_journal_allowance: ByteCount::from_u64(1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(1024 * 1024),
    };
    let shard = Arc::new(ArenaShard::create(&arena_path, layout).expect("arena"));
    let mut database = Database::open(&database_path).expect("database");
    database
        .register_cache_shard(CacheShardSpec {
            shard_id: 0,
            relative_path: "arena.bin".into(),
            page_size: layout.page_size,
            slot_count: layout.slot_count,
        })
        .expect("register shard");
    let mut index = ResidentIndex::rebuild(&database, Arc::clone(&shard)).expect("index");
    let mut resident = VecDeque::new();
    let started = Instant::now();
    let mut next_progress = Duration::from_secs(60);
    let mut operation = 0_u64;
    let mut maximum_allocated = 0_u64;

    while started.elapsed() < duration && operation_limit.is_none_or(|limit| operation < limit) {
        if resident.len() == SLOT_COUNT as usize {
            let oldest = resident.pop_front().expect("resident page");
            assert!(index.evict(&database, oldest).expect("evict"));
        }
        let bytes = deterministic_page(operation);
        let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        let record =
            match insert_page(&database, Arc::clone(&shard), hash, &bytes, &()).expect("insert") {
                InsertOutcome::Inserted(value) | InsertOutcome::Existing(value) => value,
            };
        index.install(record, Arc::clone(&shard)).expect("install");
        resident.push_back(hash);

        let guard = index.acquire(hash).expect("acquire").expect("resident");
        let mut actual = vec![0_u8; guard.logical_length() as usize];
        guard
            .read_exact(0, &mut actual)
            .expect("read resident page");
        assert_eq!(actual, bytes, "byte mismatch at operation {operation}");

        if operation != 0 && operation.is_multiple_of(127) {
            assert!(
                reconcile(&database, &shard, true)
                    .expect("pre-restart check")
                    .actions
                    .is_empty()
            );
            drop(index);
            drop(database);
            database = Database::open(&database_path).expect("restart database");
            index = ResidentIndex::rebuild(&database, Arc::clone(&shard)).expect("restart index");
        }
        if operation.is_multiple_of(31) {
            let diagnostics = shard.diagnostics().expect("diagnostics");
            maximum_allocated = maximum_allocated.max(diagnostics.allocated_bytes);
            assert!(
                diagnostics.allocated_bytes <= layout.declared_physical_budget().expect("budget")
            );
        }
        operation += 1;
        let elapsed = started.elapsed();
        if elapsed >= next_progress {
            println!(
                "MIRAGE_ENDURANCE_PROGRESS elapsed_seconds={} operations={} maximum_allocated_bytes={}",
                elapsed.as_secs(),
                operation,
                maximum_allocated
            );
            io::stdout().flush().expect("flush endurance progress");
            next_progress = elapsed + Duration::from_secs(60);
        }
    }

    let elapsed = started.elapsed();
    assert!(operation > 0);
    assert!(maximum_allocated <= layout.declared_physical_budget().expect("budget"));
    assert!(
        reconcile(&database, &shard, true)
            .expect("final repair check")
            .actions
            .is_empty()
    );
    println!(
        "MIRAGE_ENDURANCE_SUMMARY elapsed_seconds={} operations={} maximum_allocated_bytes={}",
        elapsed.as_secs(),
        operation,
        maximum_allocated
    );
    io::stdout().flush().expect("flush endurance summary");
}

fn deterministic_page(operation: u64) -> Vec<u8> {
    let mut bytes = vec![0_u8; LOGICAL_SIZE];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = operation.wrapping_mul(131).wrapping_add(index as u64 * 17) as u8;
    }
    bytes[..8].copy_from_slice(&operation.to_le_bytes());
    bytes
}
