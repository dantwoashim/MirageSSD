mod support;

use minicbor::Decoder;
use mirage_manifest::{DecodeLimits, decode_manifest_bounded, encode_manifest, manifest_hash};

use support::{complex_manifest, minimal_manifest};

const MINIMAL: &[u8] = include_bytes!("fixtures/manifest-v2-minimal.cbor");
const COMPLEX: &[u8] = include_bytes!("fixtures/manifest-v2-complex.cbor");
const MINIMAL_HASH: &str = "9b12bdb250ca44ba47002530b6318769d2101de5dee3a1ae9b243df52af9f6ba";
const COMPLEX_HASH: &str = "11db8730eec0179459187169002473b7f4642f403ecac8f20aa8188d2de88831";

#[test]
fn canonical_bytes_and_hashes_match_golden_fixtures() {
    let minimal = minimal_manifest();
    let complex = complex_manifest();
    assert_eq!(encode_manifest(&minimal).expect("encode minimal"), MINIMAL);
    assert_eq!(encode_manifest(&complex).expect("encode complex"), COMPLEX);
    assert_eq!(
        manifest_hash(&minimal).expect("hash minimal").to_string(),
        MINIMAL_HASH
    );
    assert_eq!(
        manifest_hash(&complex).expect("hash complex").to_string(),
        COMPLEX_HASH
    );
}

#[test]
fn decode_roundtrip_reencodes_to_one_canonical_representation() {
    for fixture in [MINIMAL, COMPLEX] {
        let decoded = decode_manifest_bounded(fixture, DecodeLimits::default())
            .expect("decode golden fixture");
        assert_eq!(
            encode_manifest(&decoded).expect("reencode fixture"),
            fixture
        );
    }

    let reordered = reordered_top_level(COMPLEX);
    let decoded = decode_manifest_bounded(&reordered, DecodeLimits::default())
        .expect("field order is not semantically significant");
    assert_eq!(encode_manifest(&decoded).expect("normalize order"), COMPLEX);
}

#[test]
fn truncation_duplicate_keys_and_trailing_bytes_are_rejected() {
    assert!(
        decode_manifest_bounded(&COMPLEX[..COMPLEX.len() - 1], DecodeLimits::default()).is_err()
    );
    assert!(
        decode_manifest_bounded(&duplicate_first_top_key(COMPLEX), DecodeLimits::default())
            .is_err()
    );
    let mut trailing = COMPLEX.to_vec();
    trailing.push(0);
    assert!(decode_manifest_bounded(&trailing, DecodeLimits::default()).is_err());
}

#[test]
fn hostile_declared_collection_count_is_rejected_before_allocation() {
    let hostile = replace_directory_count_with_u32_max(MINIMAL);
    let limits = DecodeLimits {
        max_directories: 8,
        ..DecodeLimits::default()
    };
    assert!(decode_manifest_bounded(&hostile, limits).is_err());
}

fn top_level_fields(bytes: &[u8]) -> (usize, Vec<(usize, usize)>) {
    let mut decoder = Decoder::new(bytes);
    assert_eq!(decoder.map().expect("top map"), Some(9));
    let header_end = decoder.position();
    let mut fields = Vec::new();
    for _ in 0..9 {
        let start = decoder.position();
        decoder.skip().expect("field key");
        decoder.skip().expect("field value");
        fields.push((start, decoder.position()));
    }
    assert_eq!(decoder.position(), bytes.len());
    (header_end, fields)
}

fn reordered_top_level(bytes: &[u8]) -> Vec<u8> {
    let (header_end, fields) = top_level_fields(bytes);
    let mut output = bytes[..header_end].to_vec();
    for &(start, end) in fields.iter().rev() {
        output.extend_from_slice(&bytes[start..end]);
    }
    output
}

fn duplicate_first_top_key(bytes: &[u8]) -> Vec<u8> {
    let (header_end, fields) = top_level_fields(bytes);
    let mut output = bytes[..header_end].to_vec();
    let (first, first_end) = fields[0];
    output.extend_from_slice(&bytes[first..first_end]);
    output.extend_from_slice(&bytes[first..first_end]);
    for &(start, end) in &fields[1..8] {
        output.extend_from_slice(&bytes[start..end]);
    }
    output
}

fn replace_directory_count_with_u32_max(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = Decoder::new(bytes);
    assert_eq!(decoder.map().expect("top map"), Some(9));
    loop {
        let key = decoder.u32().expect("integer key");
        if key == 5 {
            let array_header = decoder.position();
            assert_eq!(bytes[array_header], 0x81);
            let mut output = bytes[..array_header].to_vec();
            output.extend_from_slice(&[0x9A, 0xFF, 0xFF, 0xFF, 0xFF]);
            output.extend_from_slice(&bytes[array_header + 1..]);
            return output;
        }
        decoder.skip().expect("field value");
    }
}
