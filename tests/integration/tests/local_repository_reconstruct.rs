use std::time::Instant;

use mirage_backend_local::LocalObjectBackend;
use mirage_corpus_gen::{PatternKind, fill_at};
use mirage_engine::{extract_virtual_files, publish_base_generation, recover_repository};
use mirage_index::{MountIndex, NodeIndex, compile_to_path, resolve_range};
use mirage_manifest::{FileClass, InMemoryTestSigner};
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const LOGICAL_BYTES: usize = 8 * 1024 * 1024;
const READ_COUNT: usize = 100_000;
const SEED: u64 = 0x35_2026_0901;

#[test]
fn local_repository_reconstructs_and_answers_100k_deterministic_ranges() {
    let started = Instant::now();
    let root = tempfile::tempdir().expect("workspace");
    let source = root.path().join("source");
    let staging = root.path().join("staging");
    let backend_root = root.path().join("backend");
    let extracted = root.path().join("extracted");
    std::fs::create_dir(&source).expect("source directory");
    let mut original = vec![0_u8; LOGICAL_BYTES];
    fill_at(SEED, PatternKind::PageRecognizable, 0, &mut original);
    std::fs::write(source.join("oracle.bin"), &original).expect("source corpus");

    let repository = RepositoryId::from_bytes([0x35; 16]);
    let imported = import_local(&ImportPlan {
        repository_id: repository,
        generation_id: GenerationId::ZERO,
        source_root: source,
        files: vec![PlannedFile {
            relative_path: "oracle.bin".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 1024 * 1024,
        pack_target: 3 * 1024 * 1024,
        output_staging_directory: staging.clone(),
        encryption: None,
    })
    .expect("local import");
    let generated_pack_count = imported.packs.len();
    let backend = LocalObjectBackend::open(&backend_root, repository).expect("local backend");
    let signer = InMemoryTestSigner::new([0x35; 16], [0x53; 32]);
    futures_executor::block_on(publish_base_generation(
        &backend,
        &imported.packs,
        &imported.manifest,
        &signer,
    ))
    .expect("publish base generation");
    drop(imported);
    std::fs::remove_dir_all(staging).expect("discard import workspace");

    let recovered = futures_executor::block_on(recover_repository(
        &LocalObjectBackend::open(&backend_root, repository).expect("fresh backend"),
        repository,
        &signer,
    ))
    .expect("recover solely from committed objects");
    let index_path = root.path().join("repository.midx");
    compile_to_path(&recovered.manifest, &index_path).expect("compile mount index");
    let index = MountIndex::open(&index_path).expect("open mount index");
    let file = match index.lookup_path("oracle.bin").expect("lookup") {
        Some(NodeIndex::File(ordinal)) => index.file_by_index(ordinal).expect("file view"),
        _ => panic!("oracle file absent from mount index"),
    };
    assert_eq!(file.logical_size(), LOGICAL_BYTES as u64);
    for (offset, length) in [
        (0, 1),
        (1024 * 1024 - 1, 2),
        (1024 * 1024, 1),
        (LOGICAL_BYTES as u64 - 1, 8),
        (LOGICAL_BYTES as u64, 8),
    ] {
        resolve_range(file, offset, length).expect("boundary range resolves");
    }

    let report = futures_executor::block_on(extract_virtual_files(
        &backend,
        &recovered.manifest,
        &extracted,
    ))
    .expect("extract committed repository");
    let reconstructed = std::fs::read(extracted.join("oracle.bin")).expect("reconstructed file");
    assert_eq!(report.files_written, 1);
    assert_eq!(reconstructed, original);

    let mut state = SEED;
    for _ in 0..READ_COUNT {
        state = splitmix64(state);
        let offset = state % (LOGICAL_BYTES as u64 + 1);
        state = splitmix64(state);
        let requested = (state as usize % 8192) + 1;
        let end = offset
            .saturating_add(requested as u64)
            .min(LOGICAL_BYTES as u64) as usize;
        let start = offset as usize;
        let spans = resolve_range(file, offset, requested).expect("random range resolves");
        assert_eq!(
            spans.iter().map(|span| span.len as usize).sum::<usize>(),
            end - start
        );
        assert_eq!(&reconstructed[start..end], &original[start..end]);
    }

    eprintln!(
        "week7_e2e duration_ms={} packs={} committed_packs={} reads={} bounded_bytes={}",
        started.elapsed().as_millis(),
        generated_pack_count,
        recovered.verified_pack_count,
        READ_COUNT,
        original.len() + reconstructed.len()
    );
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
