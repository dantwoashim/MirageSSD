use mirage_etw::correlate::TraceEvent;
use mirage_etw::writer::SegmentWriter;

#[test]
fn segments_publish_atomically_and_respect_bounds() {
    let root = tempfile::tempdir().unwrap();
    let mut writer = SegmentWriter::new(root.path(), 2).unwrap();
    let event = TraceEvent {
        timestamp_100ns: 1,
        process_id: 2,
        path: Some("asset.bin".into()),
        offset: 3,
        size: 4,
        write: false,
    };
    let path = writer.write(vec![event]).unwrap();
    assert!(path.exists());
    assert!(!root.path().join("segment-00000000.tmp").exists());
    assert!(writer.write(Vec::new()).is_err());
}
