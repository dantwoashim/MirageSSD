use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use mirage_cache::{
    ArenaShard, CacheLayout, InsertHook, InsertOutcome, InsertStep, IntegrityClass, ResidentIndex,
    SlotMetadata, SlotState, VerifyOutcome, insert_page, reconcile, verify_page,
};
use mirage_db::{CacheShardSpec, CacheSlotRecord, CacheSlotState, Database, check_database};
use mirage_fault_inject::{
    cache_scenario::{CacheFault, fault_at},
    process_kill::kill_after_ready,
};
use mirage_types::{ByteCount, MirageError, PageHash};

const PAGE_SIZE: usize = 64 * 1024;
const LOGICAL_SIZE: usize = 60 * 1024;
const SLOT_COUNT: u32 = 16;
const CRASH_CHILD: &str = "MIRAGE_GATE_C_CRASH_CHILD";
const CRASH_DIRECTORY: &str = "MIRAGE_GATE_C_CRASH_DIRECTORY";
const CRASH_READY: &str = "MIRAGE_GATE_C_CRASH_READY";
const CRASH_SCENARIO: &str = "MIRAGE_GATE_C_CRASH_SCENARIO";

#[test]
fn deterministic_cache_endurance_smoke() {
    let metrics = run_endurance(2_000);
    assert_eq!(metrics.operations, 2_000);
}

#[test]
fn concurrent_cache_reads_and_inserts_are_exact() {
    run_concurrent_phase(2, 16, 256);
}

#[test]
fn cache_process_kill_recovery_matrix() {
    run_process_kill_matrix();
}

#[test]
#[ignore = "multi-hour Gate C; run with MIRAGE_GATE_C_OPERATIONS=1000000"]
fn gate_c_multi_hour() {
    let operations = std::env::var("MIRAGE_GATE_C_OPERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000_000);
    let metrics = run_endurance(operations);
    run_concurrent_phase(4, 64, 4_096);
    run_process_kill_matrix();
    eprintln!(
        "gate-c metrics operations={} elapsed_ms={} operations_per_second={} peak_allocated_bytes={} physical_budget_bytes={} index_restarts={} reconciliations={} corruptions_quarantined={} crash_scenarios=10",
        metrics.operations,
        metrics.elapsed.as_millis(),
        metrics.operations_per_second(),
        metrics.peak_allocated_bytes,
        metrics.physical_budget_bytes,
        metrics.index_restarts,
        metrics.reconciliations,
        metrics.corruptions_quarantined,
    );
}

#[derive(Debug)]
struct EnduranceMetrics {
    operations: u64,
    elapsed: Duration,
    peak_allocated_bytes: u64,
    physical_budget_bytes: u64,
    index_restarts: u64,
    reconciliations: u64,
    corruptions_quarantined: u64,
}

impl EnduranceMetrics {
    fn operations_per_second(&self) -> u64 {
        let nanos = self.elapsed.as_nanos();
        if nanos == 0 {
            return 0;
        }
        ((u128::from(self.operations) * 1_000_000_000) / nanos) as u64
    }
}

fn run_endurance(operations: u64) -> EnduranceMetrics {
    let directory = tempfile::tempdir().expect("directory");
    let layout = cache_layout(SLOT_COUNT);
    let arena_path = directory.path().join("arena.bin");
    let db_path = directory.path().join("control.db");
    let shard = Arc::new(ArenaShard::create(&arena_path, layout).expect("shard"));
    let mut db = Database::open(&db_path).expect("database");
    register_shard(&db, layout);
    let mut index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    let mut resident = VecDeque::new();
    let mut peak_allocated_bytes = 0;
    let mut index_restarts = 0;
    let mut reconciliations = 0;
    let mut corruptions_quarantined = 0;
    let physical_budget_bytes = layout.declared_physical_budget().expect("budget");
    let started = Instant::now();

    for operation in 0..operations {
        if resident.len() == layout.slot_count as usize {
            let (hash, _) = resident.pop_front().expect("oldest resident");
            assert!(index.evict(&db, hash).expect("evict"));
        }
        let bytes = deterministic_page(operation);
        let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        let record = match insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert")
        {
            InsertOutcome::Inserted(value) | InsertOutcome::Existing(value) => value,
        };
        index.install(record, Arc::clone(&shard)).expect("install");
        resident.push_back((hash, operation));
        assert_page(&index, hash, &bytes, operation);

        match fault_at(operation) {
            CacheFault::Reopen => {
                drop(index);
                drop(db);
                db = Database::open(&db_path).expect("reopen database");
                index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("rebuild index");
                let &(check_hash, check_operation) = resident.back().expect("restart resident");
                assert_page(
                    &index,
                    check_hash,
                    &deterministic_page(check_operation),
                    operation,
                );
                index_restarts += 1;
            }
            CacheFault::Reconcile => {
                let report = reconcile(&db, &shard, false).expect("reconcile");
                assert!(report.blocked.is_empty(), "reconciliation was blocked");
                reconciliations += 1;
            }
            CacheFault::CorruptClean => {
                let (corrupt_hash, _) = resident.pop_back().expect("corruption target");
                let mut corrupt = bytes.clone();
                corrupt[LOGICAL_SIZE / 2] ^= 0xA5;
                shard
                    .write_slot(record.slot_index, &corrupt)
                    .expect("write intentional corruption");
                shard.flush().expect("flush intentional corruption");
                assert_eq!(
                    verify_page(&index, &db, corrupt_hash, IntegrityClass::Clean)
                        .expect("verify intentional corruption"),
                    VerifyOutcome::Quarantined
                );
                assert!(
                    index
                        .acquire(corrupt_hash)
                        .expect("lookup quarantined page")
                        .is_none(),
                    "a quarantined clean page remained readable"
                );
                corruptions_quarantined += 1;
            }
            CacheFault::None => {}
        }
        let allocated_bytes = shard.diagnostics().expect("usage").allocated_bytes;
        peak_allocated_bytes = peak_allocated_bytes.max(allocated_bytes);
        assert!(
            allocated_bytes <= physical_budget_bytes,
            "physical cache allocation exceeded the declared hard budget at operation {operation}"
        );
    }

    let final_report = reconcile(&db, &shard, true).expect("final reconcile");
    assert!(final_report.actions.is_empty());
    assert!(final_report.blocked.is_empty());
    assert_no_leaked_reservations(&db);
    let database_report = check_database(&db_path).expect("database integrity report");
    assert!(database_report.quick_check_ok);
    assert_eq!(database_report.foreign_key_violation_count, 0);

    EnduranceMetrics {
        operations,
        elapsed: started.elapsed(),
        peak_allocated_bytes,
        physical_budget_bytes,
        index_restarts,
        reconciliations,
        corruptions_quarantined,
    }
}

fn run_concurrent_phase(writer_count: usize, pages_per_writer: usize, reader_iterations: usize) {
    let directory = tempfile::tempdir().expect("concurrency directory");
    let required_slots = 8 + writer_count * pages_per_writer;
    let slot_count = u32::try_from(required_slots).expect("slot count");
    let layout = cache_layout(slot_count);
    let arena_path = directory.path().join("arena.bin");
    let db_path = directory.path().join("control.db");
    let shard = Arc::new(ArenaShard::create(&arena_path, layout).expect("concurrency shard"));
    let db = Database::open(&db_path).expect("concurrency database");
    register_shard(&db, layout);
    let index = Arc::new(ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index"));
    let anchors: Arc<Vec<(PageHash, Vec<u8>)>> = Arc::new(
        (0..8_u64)
            .map(|operation| {
                let bytes = deterministic_page(operation + 10_000_000);
                let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
                let record = inserted_record(
                    insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert anchor"),
                );
                index
                    .install(record, Arc::clone(&shard))
                    .expect("install anchor");
                (hash, bytes)
            })
            .collect(),
    );
    let reader_count = 2;
    let barrier = Arc::new(Barrier::new(writer_count + reader_count));

    thread::scope(|scope| {
        for reader in 0..reader_count {
            let barrier = Arc::clone(&barrier);
            let index = Arc::clone(&index);
            let anchors = Arc::clone(&anchors);
            scope.spawn(move || {
                barrier.wait();
                for iteration in 0..reader_iterations {
                    let (hash, expected) = &anchors[(iteration + reader) % anchors.len()];
                    assert_page(&index, *hash, expected, iteration as u64);
                }
            });
        }
        for writer in 0..writer_count {
            let barrier = Arc::clone(&barrier);
            let index = Arc::clone(&index);
            let shard = Arc::clone(&shard);
            let db = db.clone();
            scope.spawn(move || {
                barrier.wait();
                for page in 0..pages_per_writer {
                    let operation = 20_000_000 + (writer * pages_per_writer + page) as u64;
                    let bytes = deterministic_page(operation);
                    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
                    let record = inserted_record(
                        insert_page(&db, Arc::clone(&shard), hash, &bytes, &())
                            .expect("concurrent insert"),
                    );
                    index
                        .install(record, Arc::clone(&shard))
                        .expect("concurrent install");
                    assert_page(&index, hash, &bytes, operation);
                }
            });
        }
    });

    let final_report = reconcile(&db, &shard, true).expect("concurrent final reconcile");
    assert!(final_report.actions.is_empty());
    assert!(final_report.blocked.is_empty());
    assert_no_leaked_reservations(&db);
    assert!(
        shard
            .diagnostics()
            .expect("concurrent usage")
            .allocated_bytes
            <= layout.declared_physical_budget().expect("budget")
    );
}

fn run_process_kill_matrix() {
    for scenario in [
        "insert-reserved",
        "insert-written",
        "insert-flushed",
        "insert-rehashed",
        "insert-committed",
        "evict-db-marked",
        "evict-metadata-flushed",
        "evict-payload-deallocated",
        "evict-free-metadata-flushed",
        "evict-db-finished",
    ] {
        let directory = tempfile::tempdir().expect("crash directory");
        initialize_crash_cache(directory.path(), scenario.starts_with("evict-"));
        let ready_path = directory.path().join("ready");
        let mut child = Command::new(std::env::current_exe().expect("test executable"));
        child
            .arg("--exact")
            .arg("gate_c_crash_worker")
            .arg("--ignored")
            .arg("--nocapture")
            .env(CRASH_CHILD, "1")
            .env(CRASH_DIRECTORY, directory.path())
            .env(CRASH_READY, &ready_path)
            .env(CRASH_SCENARIO, scenario);
        let killed = kill_after_ready(&mut child, &ready_path, Duration::from_secs(15))
            .unwrap_or_else(|error| panic!("crash scenario {scenario} failed: {error}"));
        assert!(!killed.status.success());
        assert_crash_cache_recovers(directory.path(), scenario);
    }
}

#[test]
#[ignore = "subprocess entry point for Gate C crash recovery"]
fn gate_c_crash_worker() {
    if std::env::var_os(CRASH_CHILD).is_none() {
        return;
    }
    let directory = required_path(CRASH_DIRECTORY);
    let ready_path = required_path(CRASH_READY);
    let scenario = std::env::var(CRASH_SCENARIO).expect("crash scenario");
    let layout = cache_layout(SLOT_COUNT);
    let shard =
        Arc::new(ArenaShard::open(&directory.join("arena.bin"), layout).expect("open crash shard"));
    let db = Database::open(&directory.join("control.db")).expect("open crash database");

    if let Some(step) = scenario.strip_prefix("insert-") {
        let bytes = deterministic_page(30_000_000);
        let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        let hook = CrashInsertHook {
            target: step.to_owned(),
            ready_path,
        };
        insert_page(&db, shard, hash, &bytes, &hook).expect("crash insertion");
        panic!("insertion crash point was not reached");
    }

    let target = scenario
        .strip_prefix("evict-")
        .expect("eviction crash scenario");
    let record = db
        .load_resident_cache_slots()
        .expect("load eviction resident")
        .into_iter()
        .next()
        .expect("eviction resident");
    let evicting = db.begin_cache_eviction(record).expect("begin eviction");
    crash_if(target, "db-marked", &ready_path);
    shard
        .write_metadata(
            record.slot_index,
            metadata_for(evicting, SlotState::Evicting),
        )
        .expect("write evicting metadata");
    shard.flush_metadata().expect("flush evicting metadata");
    crash_if(target, "metadata-flushed", &ready_path);
    shard
        .deallocate_slot(record.slot_index)
        .expect("deallocate eviction payload");
    crash_if(target, "payload-deallocated", &ready_path);
    let free = CacheSlotRecord {
        state: CacheSlotState::Free,
        page_hash: None,
        logical_length: 0,
        ..evicting
    };
    shard
        .write_metadata(record.slot_index, metadata_for(free, SlotState::Free))
        .expect("write free metadata");
    shard.flush_metadata().expect("flush free metadata");
    crash_if(target, "free-metadata-flushed", &ready_path);
    db.finish_cache_deallocation(evicting)
        .expect("finish eviction");
    crash_if(target, "db-finished", &ready_path);
    panic!("eviction crash point was not reached");
}

struct CrashInsertHook {
    target: String,
    ready_path: PathBuf,
}

impl InsertHook for CrashInsertHook {
    fn after(&self, step: InsertStep) -> Result<(), MirageError> {
        let current = match step {
            InsertStep::Reserved => "reserved",
            InsertStep::Written => "written",
            InsertStep::Flushed => "flushed",
            InsertStep::Rehashed => "rehashed",
            InsertStep::MetadataCommitted => "committed",
        };
        crash_if(&self.target, current, &self.ready_path);
        Ok(())
    }
}

fn initialize_crash_cache(directory: &Path, with_resident: bool) {
    let layout = cache_layout(SLOT_COUNT);
    let shard = Arc::new(
        ArenaShard::create(&directory.join("arena.bin"), layout).expect("create crash shard"),
    );
    let db = Database::open(&directory.join("control.db")).expect("create crash database");
    register_shard(&db, layout);
    if with_resident {
        let bytes = deterministic_page(40_000_000);
        let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        insert_page(&db, shard, hash, &bytes, &()).expect("seed eviction resident");
    }
}

fn assert_crash_cache_recovers(directory: &Path, scenario: &str) {
    let layout = cache_layout(SLOT_COUNT);
    let shard = Arc::new(
        ArenaShard::open(&directory.join("arena.bin"), layout).expect("reopen crash shard"),
    );
    let db_path = directory.join("control.db");
    let db = Database::open(&db_path)
        .unwrap_or_else(|error| panic!("database recovery failed after {scenario}: {error}"));
    let applied = reconcile(&db, &shard, false)
        .unwrap_or_else(|error| panic!("cache recovery failed after {scenario}: {error}"));
    assert!(
        applied.blocked.is_empty(),
        "recovery blocked after {scenario}"
    );
    let final_report = reconcile(&db, &shard, true).expect("post-recovery check");
    assert!(
        final_report.actions.is_empty(),
        "recovery left pending actions after {scenario}: {:?}",
        final_report.actions
    );
    assert!(final_report.blocked.is_empty());
    assert_no_leaked_reservations(&db);
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("post-crash index");
    for record in db
        .load_resident_cache_slots()
        .expect("post-crash residents")
    {
        let hash = record.page_hash.expect("resident hash");
        assert_eq!(
            verify_page(&index, &db, hash, IntegrityClass::Clean).expect("verify recovered page"),
            VerifyOutcome::Verified,
            "a recovered resident contained wrong bytes after {scenario}"
        );
    }
    let database_report = check_database(&db_path).expect("post-crash database integrity");
    assert!(database_report.quick_check_ok);
    assert_eq!(database_report.foreign_key_violation_count, 0);
}

fn assert_page(index: &ResidentIndex, hash: PageHash, expected: &[u8], operation: u64) {
    let guard = index
        .acquire(hash)
        .expect("acquire resident page")
        .unwrap_or_else(|| panic!("resident page missing at operation {operation}"));
    assert_eq!(guard.logical_length() as usize, expected.len());
    let mut actual = vec![0_u8; expected.len()];
    guard
        .read_exact(0, &mut actual)
        .expect("read resident page");
    assert_eq!(actual, expected, "wrong bytes at operation {operation}");
}

fn assert_no_leaked_reservations(db: &Database) {
    assert!(
        db.load_cache_slots()
            .expect("cache slots")
            .into_iter()
            .all(|slot| matches!(slot.state, CacheSlotState::Free | CacheSlotState::Resident)),
        "cache restart left a reserved or evicting slot"
    );
}

fn inserted_record(outcome: InsertOutcome) -> CacheSlotRecord {
    match outcome {
        InsertOutcome::Inserted(record) | InsertOutcome::Existing(record) => record,
    }
}

fn register_shard(db: &Database, layout: CacheLayout) {
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register shard");
}

fn cache_layout(slot_count: u32) -> CacheLayout {
    CacheLayout {
        page_size: ByteCount::from_u64(PAGE_SIZE as u64),
        slot_count,
        db_journal_allowance: ByteCount::from_u64(1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(1024 * 1024),
    }
}

fn deterministic_page(operation: u64) -> Vec<u8> {
    let mut bytes = vec![0_u8; LOGICAL_SIZE];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = operation.wrapping_mul(131).wrapping_add(index as u64 * 17) as u8;
    }
    bytes[..8].copy_from_slice(&operation.to_le_bytes());
    bytes
}

fn metadata_for(record: CacheSlotRecord, state: SlotState) -> SlotMetadata {
    SlotMetadata {
        generation: record.generation,
        state,
        page_hash: record
            .page_hash
            .unwrap_or_else(|| PageHash::from_bytes([0; 32])),
        logical_length: record.logical_length,
    }
}

fn crash_if(target: &str, current: &str, ready_path: &Path) {
    if target != current {
        return;
    }
    let mut ready = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(ready_path)
        .expect("create crash readiness marker");
    ready.write_all(current.as_bytes()).expect("write marker");
    ready.sync_all().expect("flush marker");
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")))
}
