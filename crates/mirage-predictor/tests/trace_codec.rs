use mirage_predictor::{TraceBlockDecoder, TraceBlockEncoder, TraceEvent, TraceHeader};
use mirage_types::{GenerationId, RepositoryId, StableFileId};

fn header() -> TraceHeader {
    TraceHeader {
        schema_version: 1,
        repository_id: RepositoryId::from_bytes([1; 16]),
        manifest_generation: GenerationId::from_u64(7),
        page_size: 1_048_576,
        machine_profile: "test".into(),
        dropped_event_count: 0,
    }
}
fn events() -> Vec<TraceEvent> {
    vec![
        TraceEvent {
            timestamp_ns: 10,
            stable_file_id: StableFileId::from_u64(2),
            offset: 0,
            length: 4096,
            flags: 1,
        },
        TraceEvent {
            timestamp_ns: 25,
            stable_file_id: StableFileId::from_u64(3),
            offset: 4096,
            length: 8192,
            flags: 0,
        },
    ]
}

#[test]
fn golden_stream_round_trip_and_corruption_rejection() {
    let mut encoder = TraceBlockEncoder::new(Vec::new(), &header()).expect("encoder");
    encoder.write_block(&events()).expect("block");
    let bytes = encoder.finish();
    assert_eq!(&bytes[..8], b"MIRTRC1\0");
    let mut decoder = TraceBlockDecoder::new(bytes.as_slice()).expect("decoder");
    assert_eq!(decoder.header, header());
    assert_eq!(decoder.read_block().expect("block"), Some(events()));
    assert_eq!(decoder.read_block().expect("eof"), None);
    let mut corrupt = bytes;
    *corrupt.last_mut().expect("byte") ^= 1;
    assert!(
        TraceBlockDecoder::new(corrupt.as_slice())
            .expect("header")
            .read_block()
            .is_err()
    );
}

#[test]
fn unknown_version_and_unreasonable_count_are_rejected() {
    let mut encoder = TraceBlockEncoder::new(Vec::new(), &header()).expect("encoder");
    encoder.write_block(&events()).expect("block");
    let mut bytes = encoder.finish();
    bytes[8] = 2;
    assert!(TraceBlockDecoder::new(bytes.as_slice()).is_err());
    let mut encoder = TraceBlockEncoder::new(Vec::new(), &header()).expect("encoder");
    encoder.write_block(&events()).expect("block");
    let mut bytes = encoder.finish();
    let start = 50 + header().machine_profile.len();
    bytes[start..start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        TraceBlockDecoder::new(bytes.as_slice())
            .expect("header")
            .read_block()
            .is_err()
    );
}
