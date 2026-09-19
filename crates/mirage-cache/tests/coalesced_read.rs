use std::sync::Arc;

use mirage_cache::{
    ArenaShard, CacheLayout, ResidentIndex, insert_page, read_contiguous, slots_are_contiguous,
};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, PageHash};

const PAGE: usize = 64 * 1024;

struct Fixture {
    _directory: tempfile::TempDir,
    shard: Arc<ArenaShard>,
    index: ResidentIndex,
    pages: Vec<(PageHash, Vec<u8>)>,
}

fn fixture_with(unbuffered: bool, lengths: &[usize]) -> Fixture {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(PAGE as u64),
        slot_count: 8,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let arena = directory.path().join("arena.bin");
    let shard = Arc::new(ArenaShard::create(&arena, layout).expect("arena"));
    let db = Database::open(&directory.path().join("control.db")).expect("db");
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register shard");
    let pages: Vec<(PageHash, Vec<u8>)> = lengths
        .iter()
        .enumerate()
        .map(|(ordinal, length)| {
            let bytes = vec![ordinal as u8 + 1; *length];
            (
                PageHash::from_bytes(*blake3::hash(&bytes).as_bytes()),
                bytes,
            )
        })
        .collect();
    for (hash, bytes) in &pages {
        insert_page(&db, Arc::clone(&shard), *hash, bytes, &()).expect("insert");
    }
    drop(shard);
    let shard = Arc::new(if unbuffered {
        ArenaShard::open_read_unbuffered(&arena, layout).expect("unbuffered arena")
    } else {
        ArenaShard::open(&arena, layout).expect("reopened arena")
    });
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    Fixture {
        _directory: directory,
        shard,
        index,
        pages,
    }
}

fn fixture(unbuffered: bool) -> Fixture {
    fixture_with(unbuffered, &[PAGE; 4])
}

#[test]
fn guards_from_another_arena_cannot_authorize_a_read() {
    let first = fixture(false);
    let second = fixture(false);
    let guard = first.index.acquire(first.pages[0].0).unwrap().unwrap();
    let mut output = vec![0xa5; PAGE];
    assert!(read_contiguous(&second.shard, &[guard], 0, &mut output).is_err());
    assert!(output.iter().all(|byte| *byte == 0xa5));
}

#[test]
fn adjacent_slots_read_as_one_contiguous_span() {
    let fixture = fixture(false);
    let guards: Vec<_> = fixture
        .pages
        .iter()
        .take(3)
        .map(|(hash, _)| fixture.index.acquire(*hash).unwrap().expect("resident"))
        .collect();
    assert!(slots_are_contiguous(&guards));
    let mut output = vec![0_u8; PAGE * 2 + PAGE / 2];
    read_contiguous(&fixture.shard, &guards, 0, &mut output).expect("coalesced read");
    let expected: Vec<u8> = fixture
        .pages
        .iter()
        .take(3)
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .take(output.len())
        .collect();
    assert_eq!(output, expected);
}

#[test]
fn non_adjacent_guards_are_rejected() {
    let fixture = fixture(false);
    let first = fixture.index.acquire(fixture.pages[0].0).unwrap().unwrap();
    let third = fixture.index.acquire(fixture.pages[2].0).unwrap().unwrap();
    let mut output = vec![0_u8; PAGE];
    assert!(read_contiguous(&fixture.shard, &[first, third], 0, &mut output).is_err());
    assert!(read_contiguous(&fixture.shard, &[], 0, &mut output).is_err());
}

#[test]
fn a_short_middle_page_is_rejected() {
    let fixture = fixture_with(false, &[PAGE, PAGE / 2, PAGE]);
    let guards: Vec<_> = fixture
        .pages
        .iter()
        .map(|(hash, _)| fixture.index.acquire(*hash).unwrap().expect("resident"))
        .collect();
    assert!(slots_are_contiguous(&guards));
    let mut output = vec![0_u8; PAGE / 2];
    assert!(read_contiguous(&fixture.shard, &guards, 0, &mut output).is_err());
}

#[test]
fn a_read_past_the_last_page_length_is_rejected() {
    let fixture = fixture(false);
    let guards: Vec<_> = fixture
        .pages
        .iter()
        .take(2)
        .map(|(hash, _)| fixture.index.acquire(*hash).unwrap().expect("resident"))
        .collect();
    let mut output = vec![0_u8; PAGE + 2];
    // Offset PAGE-1: PAGE+2 bytes would pass the second page's logical end.
    assert!(read_contiguous(&fixture.shard, &guards, PAGE as u32 - 1, &mut output).is_err());
    let mut fits = vec![0_u8; PAGE];
    read_contiguous(&fixture.shard, &guards, PAGE as u32 - 1, &mut fits).expect("boundary fits");
    assert_eq!(
        &fits[..2],
        &[fixture.pages[0].1[PAGE - 1], fixture.pages[1].1[0]]
    );
}

#[test]
fn unaligned_unbuffered_coalesced_read_matches_buffered_bytes() {
    let fixture = fixture(true);
    let guards: Vec<_> = fixture
        .pages
        .iter()
        .take(3)
        .map(|(hash, _)| fixture.index.acquire(*hash).unwrap().expect("resident"))
        .collect();
    let mut output = vec![0_u8; PAGE + 3000];
    read_contiguous(&fixture.shard, &guards, 1234, &mut output).expect("unbuffered read");
    let expected: Vec<u8> = fixture
        .pages
        .iter()
        .take(3)
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .skip(1234)
        .take(output.len())
        .collect();
    assert_eq!(output, expected);
}
