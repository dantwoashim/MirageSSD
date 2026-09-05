use mirage_cache::{ArenaHeader, CacheLayout, SlotMetadata, SlotState};
use mirage_types::{ByteCount, PageHash};

#[test]
fn layout_header_and_slot_metadata_are_checked_and_canonical() {
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(1024 * 1024),
        slot_count: 4096,
        db_journal_allowance: ByteCount::from_u64(128 * 1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(1024 * 1024 * 1024),
    };
    layout.validate().expect("layout");
    assert_eq!(layout.slot_offset(0).expect("first"), 4096);
    assert_eq!(layout.slot_offset(1).expect("second"), 4096 + 1024 * 1024);
    assert!(layout.slot_offset(4096).is_err());
    let header = ArenaHeader {
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    };
    let encoded = header.encode().expect("encode");
    assert_eq!(ArenaHeader::decode(&encoded).expect("decode"), header);
    let mut corrupt = encoded;
    corrupt[20] ^= 1;
    assert!(ArenaHeader::decode(&corrupt).is_err());
    let metadata = SlotMetadata {
        generation: 7,
        state: SlotState::Resident,
        page_hash: PageHash::from_bytes([9; 32]),
        logical_length: 777,
    };
    let encoded = metadata.encode();
    assert_eq!(SlotMetadata::decode(&encoded).expect("decode"), metadata);
    let mut corrupt = encoded;
    corrupt[44] ^= 1;
    assert!(SlotMetadata::decode(&corrupt).is_err());
}

#[test]
fn layout_rejects_invalid_sizes_counts_and_overflow() {
    for layout in [
        CacheLayout {
            page_size: ByteCount::ZERO,
            slot_count: 1,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        },
        CacheLayout {
            page_size: ByteCount::from_u64(100_000),
            slot_count: 1,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        },
        CacheLayout {
            page_size: ByteCount::from_u64(1024 * 1024),
            slot_count: 0,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        },
        CacheLayout {
            page_size: ByteCount::from_u64(16 * 1024 * 1024),
            slot_count: u32::MAX,
            db_journal_allowance: ByteCount::from_u64(u64::MAX),
            filesystem_reserve: ByteCount::ZERO,
        },
    ] {
        assert!(layout.validate().is_err());
    }
}
