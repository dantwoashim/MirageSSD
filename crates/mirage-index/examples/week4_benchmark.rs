use std::error::Error;
use std::time::Instant;

use mirage_index::{MountIndex, NodeIndex, compile_to_bytes};
use mirage_manifest::{
    DirectoryRecord, FileClass, FileRecord, MANIFEST_FORMAT_VERSION, RepositoryManifest,
};
use mirage_types::{ByteCount, GenerationId, RepositoryId, StableFileId};

const FILE_COUNT: usize = 200_000;
const LOGICAL_BYTES: u64 = 150 * 1024 * 1024 * 1024;
const LOOKUP_COUNT: usize = 1_000_000;

fn main() -> Result<(), Box<dyn Error>> {
    let construction_started = Instant::now();
    let manifest = synthetic_manifest()?;
    let construction_elapsed = construction_started.elapsed();

    let compile_started = Instant::now();
    let bytes = compile_to_bytes(&manifest)?;
    let compile_elapsed = compile_started.elapsed();

    let mount_started = Instant::now();
    let index = MountIndex::from_bytes(bytes)?;
    let mount_elapsed = mount_started.elapsed();
    let resident_after_mount = memory_stats::memory_stats().map(|stats| stats.physical_mem);

    let mut state = 0xD1B5_4A32_D192_ED03_u64;
    let mut latencies = Vec::with_capacity(LOOKUP_COUNT);
    let lookup_started = Instant::now();
    for _ in 0..LOOKUP_COUNT {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let ordinal = usize::try_from(state % FILE_COUNT as u64)?;
        let path = format!("asset-{ordinal:06}.bin");
        let started = Instant::now();
        if !matches!(index.lookup_path(&path)?, Some(NodeIndex::File(_))) {
            return Err("benchmark lookup missed an existing file".into());
        }
        latencies.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let lookup_elapsed = lookup_started.elapsed();
    latencies.sort_unstable();
    println!(
        "files={FILE_COUNT} logical_gib=150 index_bytes={} construct_ms={} compile_ms={} mount_ms={} resident_after_mount_bytes={} lookups={LOOKUP_COUNT} lookup_total_ms={} p50_ns={} p99_ns={}",
        index.header().file_length,
        construction_elapsed.as_millis(),
        compile_elapsed.as_millis(),
        mount_elapsed.as_millis(),
        resident_after_mount.map_or(0, |bytes| bytes),
        lookup_elapsed.as_millis(),
        latencies[LOOKUP_COUNT / 2],
        latencies[LOOKUP_COUNT * 99 / 100],
    );
    Ok(())
}

fn synthetic_manifest() -> Result<RepositoryManifest, Box<dyn Error>> {
    let base_size = LOGICAL_BYTES / FILE_COUNT as u64;
    let remainder = usize::try_from(LOGICAL_BYTES % FILE_COUNT as u64)?;
    let files = (0..FILE_COUNT)
        .map(|index| FileRecord {
            parent_directory: 0,
            name: format!("asset-{index:06}.bin"),
            logical_size: ByteCount::from_u64(base_size + u64::from(index < remainder)),
            stable_id: StableFileId::from_u64(u64::try_from(index).unwrap_or(u64::MAX) + 1),
            class: FileClass::NativeMutable,
            extent_start: 0,
            extent_count: 0,
        })
        .collect();
    Ok(RepositoryManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        repository_id: RepositoryId::from_bytes([0xB4; 16]),
        generation_id: GenerationId::from_u64(4),
        page_size: ByteCount::from_u64(1024 * 1024),
        directories: vec![DirectoryRecord {
            parent: None,
            name: String::new(),
        }],
        files,
        extents: Vec::new(),
        pages: Vec::new(),
        remote_locations: Vec::new(),
    })
}
