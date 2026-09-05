use std::io::Cursor;

use mirage_pack::pager::{page_file, page_path_with_validation_hook};
use proptest::prelude::*;

const PAGE_SIZE: u32 = 64 * 1024;

#[test]
fn empty_exact_and_one_byte_tail_are_deterministic() {
    for bytes in [
        vec![],
        vec![7; PAGE_SIZE as usize],
        vec![9; PAGE_SIZE as usize + 1],
    ] {
        let first: Vec<_> = page_file(Cursor::new(&bytes), bytes.len() as u64, PAGE_SIZE)
            .expect("pager")
            .collect::<Result<_, _>>()
            .expect("pages");
        let second: Vec<_> = page_file(Cursor::new(&bytes), bytes.len() as u64, PAGE_SIZE)
            .expect("pager")
            .collect::<Result<_, _>>()
            .expect("pages");
        assert_eq!(first, second);
        assert_eq!(
            first
                .iter()
                .map(|page| page.logical_len as usize)
                .sum::<usize>(),
            bytes.len()
        );
    }
}

#[test]
fn twenty_gib_logical_fixture_is_bounded_without_materialization() {
    let logical_size = 20_u64 * 1024 * 1024 * 1024;
    let pager = page_file(Cursor::new(Vec::<u8>::new()), logical_size, PAGE_SIZE).expect("pager");
    assert_eq!(pager.size_hint(), (327_680, Some(327_680)));
    assert!(std::mem::size_of_val(&pager) < 128);
}

#[test]
fn post_read_source_mutation_is_rejected() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("asset.pak");
    std::fs::write(&path, vec![1_u8; PAGE_SIZE as usize]).expect("source");
    let result = page_path_with_validation_hook(&path, PAGE_SIZE, || {
        std::fs::write(&path, vec![2_u8; PAGE_SIZE as usize + 1]).map_err(Into::into)
    });
    assert!(result.is_err());
}

proptest! {
    #[test]
    fn random_sizes_reconstruct_exact_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..300_000)) {
        let pages: Vec<_> = page_file(Cursor::new(&bytes), bytes.len() as u64, PAGE_SIZE)
            .expect("pager").collect::<Result<_, _>>().expect("pages");
        let reconstructed: Vec<_> = pages.iter().flat_map(|page| page.bytes.iter().copied()).collect();
        prop_assert_eq!(reconstructed, bytes);
    }
}
