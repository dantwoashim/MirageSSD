use bytes::Bytes;
use mirage_pack::{PlainPage, decode_plain_frame, encode_plain_frame};

#[test]
fn golden_frame_has_stable_header_and_round_trips_tail_page() {
    let page = PlainPage::from_bytes(Bytes::from_static(b"Mirage frame v1"));
    let encoded = encode_plain_frame(&page).expect("encode");
    assert_eq!(&encoded[..4], b"MPF1");
    assert_eq!(&encoded[4..6], &1_u16.to_le_bytes());
    assert_eq!(&encoded[8..40], page.hash.as_bytes());
    assert_eq!(&encoded[40..44], &15_u32.to_le_bytes());
    assert_eq!(encoded.len(), 79);
    assert_eq!(decode_plain_frame(&encoded).expect("decode").page, page);
}

#[test]
fn header_payload_lengths_and_unknown_codec_are_rejected() {
    let page = PlainPage::from_bytes(Bytes::from(vec![3_u8; 1024]));
    let encoded = encode_plain_frame(&page).expect("encode");
    for mutate in [8_usize, 44, 48, 52] {
        let mut corrupt = encoded.clone();
        corrupt[mutate] ^= 0x5a;
        assert!(decode_plain_frame(&corrupt).is_err(), "offset {mutate}");
    }
    assert!(decode_plain_frame(&encoded[..encoded.len() - 1]).is_err());
}

#[test]
fn generated_binary_corpus_matches_the_canonical_encoder() {
    let expected = encode_plain_frame(&PlainPage::from_bytes(Bytes::from_static(
        b"Mirage pack v1 tail",
    )))
    .expect("encode");
    assert_eq!(
        include_bytes!("../../../tests/fixtures/pack-v1/frame-tail.bin"),
        expected.as_slice()
    );
    assert!(
        decode_plain_frame(include_bytes!(
            "../../../tests/fixtures/pack-v1/frame-corrupt-header.bin"
        ))
        .is_err()
    );
    assert!(
        decode_plain_frame(include_bytes!(
            "../../../tests/fixtures/pack-v1/frame-truncated.bin"
        ))
        .is_err()
    );
}
