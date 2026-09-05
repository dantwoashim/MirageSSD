use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use bytes::Bytes;
use mirage_crypto::aead::RepositoryKey;
use mirage_pack::{
    PackEncryption, PackReadEncryption, PackReader, PackWriter, PackWriterOptions, PlainPage,
    plan_ranges,
};
use mirage_types::RepositoryId;

fn build_pack() -> (
    tempfile::TempDir,
    mirage_pack::CompletedPack,
    Vec<PlainPage>,
) {
    let directory = tempfile::tempdir().expect("temp directory");
    let pages: Vec<_> = [9_u8, 2, 7, 1]
        .into_iter()
        .map(|value| PlainPage::from_bytes(Bytes::from(vec![value; 64 * 1024])))
        .collect();
    let mut writer = PackWriter::create(
        directory.path(),
        PackWriterOptions {
            page_size: 64 * 1024,
            target_size: 4 * 1024 * 1024,
            align_frames_4k: true,
        },
    )
    .expect("writer");
    for page in &pages {
        writer.append_page(page).expect("append");
    }
    let complete = writer.finish().expect("finish");
    (directory, complete, pages)
}

#[test]
fn random_physical_order_is_hash_lookupable_and_exact() {
    let (_directory, complete, pages) = build_pack();
    let mut reader = PackReader::open_verified(&complete.path).expect("reader");
    for page in pages {
        assert_eq!(reader.read_page(page.hash).expect("read").page, page);
    }
}

#[test]
fn corrupted_index_and_truncation_are_rejected() {
    let (_directory, complete, _) = build_pack();
    let mut bytes = std::fs::read(&complete.path).expect("pack");
    let index_offset = complete
        .entries
        .iter()
        .map(|entry| entry.frame_offset + entry.frame_length)
        .max()
        .unwrap();
    bytes[index_offset as usize] ^= 1;
    let corrupt = complete.path.with_file_name("corrupt.bin");
    std::fs::write(&corrupt, &bytes).expect("corrupt pack");
    assert!(PackReader::open_verified(&corrupt).is_err());
    bytes.truncate(bytes.len() - 17);
    let truncated = complete.path.with_file_name("truncated.bin");
    std::fs::write(&truncated, bytes).expect("truncated pack");
    assert!(PackReader::open_verified(&truncated).is_err());
}

#[test]
fn range_plan_coalesces_bounded_frames_and_decodes_each_once() {
    let (_directory, complete, pages) = build_pack();
    let plans =
        plan_ranges(&complete.entries, complete.byte_length, 4096, 512 * 1024).expect("plan");
    let mut file = std::fs::File::open(&complete.path).expect("pack");
    let mut observed = Vec::new();
    for plan in plans {
        let mut bytes = vec![0_u8; plan.range.len() as usize];
        file.seek(SeekFrom::Start(plan.range.start()))
            .expect("seek");
        file.read_exact(&mut bytes).expect("range");
        for hash in plan.pages {
            let entry = complete
                .entries
                .iter()
                .find(|entry| entry.page_hash == hash)
                .copied()
                .unwrap();
            observed.push(
                PackReader::decode_page_from_range(plan.range.start(), &bytes, entry)
                    .expect("decode")
                    .page
                    .hash,
            );
        }
    }
    observed.sort();
    let mut expected: Vec<_> = pages.into_iter().map(|page| page.hash).collect();
    expected.sort();
    assert_eq!(observed, expected);
}

#[test]
fn overlapping_or_out_of_bounds_ranges_are_rejected() {
    let (_directory, complete, _) = build_pack();
    let mut entries = complete.entries.clone();
    entries.sort_by_key(|entry| entry.frame_offset);
    entries[1].frame_offset = entries[0].frame_offset + 1;
    assert!(plan_ranges(&entries, complete.byte_length, 0, 1024 * 1024).is_err());
    let mut out = complete.entries.clone();
    out[0].frame_offset = complete.byte_length;
    assert!(plan_ranges(&out, complete.byte_length, 0, 1024 * 1024).is_err());
}

#[test]
fn encrypted_pack_round_trips_and_rejects_missing_or_wrong_context() {
    let directory = tempfile::tempdir().expect("temp directory");
    let repository_id = RepositoryId::from_bytes([3; 16]);
    let key = Arc::new(RepositoryKey::from_bytes([0x5a; 32]));
    let page = PlainPage::from_bytes(Bytes::from(vec![0x91; 64 * 1024]));
    let mut writer = PackWriter::create_encrypted(
        directory.path(),
        PackWriterOptions {
            page_size: 64 * 1024,
            target_size: 1024 * 1024,
            align_frames_4k: true,
        },
        PackEncryption {
            repository_id,
            key: Arc::clone(&key),
        },
    )
    .expect("encrypted writer");
    writer.append_page(&page).expect("append");
    let complete = writer.finish().expect("finish");

    let mut structural = PackReader::open_verified(&complete.path).expect("structural reader");
    assert!(structural.is_encrypted());
    assert!(structural.read_page(page.hash).is_err());

    let mut reader = PackReader::open_verified_encrypted(
        &complete.path,
        PackReadEncryption {
            repository_id,
            key: Arc::clone(&key),
        },
    )
    .expect("reader");
    assert_eq!(reader.read_page(page.hash).expect("decrypt").page, page);

    let mut wrong_repository = PackReader::open_verified_encrypted(
        &complete.path,
        PackReadEncryption {
            repository_id: RepositoryId::from_bytes([4; 16]),
            key: Arc::clone(&key),
        },
    )
    .expect("structural open");
    assert!(wrong_repository.read_page(page.hash).is_err());

    let mut wrong_key = PackReader::open_verified_encrypted(
        &complete.path,
        PackReadEncryption {
            repository_id,
            key: Arc::new(RepositoryKey::from_bytes([0x6b; 32])),
        },
    )
    .expect("structural open");
    assert!(wrong_key.read_page(page.hash).is_err());
}
