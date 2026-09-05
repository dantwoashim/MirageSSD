mod support;

use std::panic::{AssertUnwindSafe, catch_unwind};

use mirage_index::{Header, MountIndex, compile_to_bytes};

use support::complex_manifest;

#[test]
fn valid_header_roundtrips_exactly() {
    let bytes = compile_to_bytes(&complex_manifest()).expect("compile index");
    let header = Header::parse(&bytes).expect("parse header");
    assert_eq!(header.encode().expect("encode header"), bytes[..320]);
    assert_eq!(header.file_length, bytes.len() as u64);
    assert_eq!(
        Header::compute_index_hash(&bytes).expect("hash"),
        header.index_hash
    );
}

#[test]
fn malicious_header_fields_are_rejected() {
    let valid = compile_to_bytes(&complex_manifest()).expect("compile index");
    let mut cases = Vec::new();

    let mut unknown_kind = valid.clone();
    set_u32(&mut unknown_kind, 128, 99);
    cases.push(unknown_kind);

    let mut duplicate_kind = valid.clone();
    set_u32(&mut duplicate_kind, 160, 1);
    set_u32(&mut duplicate_kind, 164, 1);
    cases.push(duplicate_kind);

    let mut unknown_width = valid.clone();
    set_u32(&mut unknown_width, 132, 8);
    cases.push(unknown_width);

    let mut misaligned = valid.clone();
    set_u64(&mut misaligned, 136, 321);
    cases.push(misaligned);

    let first_offset = u64::from_le_bytes(valid[136..144].try_into().expect("offset"));
    let mut overlap = valid.clone();
    set_u64(&mut overlap, 168, first_offset);
    cases.push(overlap);

    let mut overflowing = valid.clone();
    set_u64(&mut overflowing, 136, u64::MAX - 63);
    cases.push(overflowing);

    let mut contradictory_length = valid.clone();
    set_u64(&mut contradictory_length, 120, u64::MAX);
    cases.push(contradictory_length);

    for bytes in cases {
        assert!(Header::parse(&bytes).is_err());
    }
}

#[test]
fn truncation_and_header_bit_flips_never_panic() {
    let valid = compile_to_bytes(&complex_manifest()).expect("compile index");
    for length in 0..320 {
        let result = catch_unwind(AssertUnwindSafe(|| Header::parse(&valid[..length])));
        assert!(result.is_ok(), "panic at truncation length {length}");
        assert!(result.expect("no panic").is_err());
    }
    for byte_index in 0..320 {
        let mut corrupted = valid.clone();
        corrupted[byte_index] ^= 0x80;
        let result = catch_unwind(AssertUnwindSafe(|| MountIndex::from_bytes(corrupted)));
        assert!(
            result.is_ok(),
            "panic after flipping header byte {byte_index}"
        );
    }
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
