use bytes::Bytes;
use mirage_pack::{PackReader, PackWriter, PackWriterOptions, PlainPage};

fn options(target_size: u64) -> PackWriterOptions {
    PackWriterOptions {
        page_size: 64 * 1024,
        target_size,
        align_frames_4k: true,
    }
}

#[test]
fn duplicate_page_is_stored_once_and_pack_is_self_verifying() {
    let directory = tempfile::tempdir().expect("temp directory");
    let page = PlainPage::from_bytes(Bytes::from(vec![7_u8; 64 * 1024]));
    let mut writer = PackWriter::create(directory.path(), options(1024 * 1024)).expect("writer");
    let first = writer.append_page(&page).expect("append");
    let duplicate = writer.append_page(&page).expect("deduplicate");
    assert_eq!(first, duplicate);
    let complete = writer.finish().expect("finish");
    assert_eq!(complete.entries.len(), 1);
    assert!(
        complete
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pack-")
    );
    let reader = PackReader::open_verified(&complete.path).expect("verified reader");
    assert_eq!(reader.content_hash(), complete.content_hash);
}

#[test]
fn target_size_signals_rollover_without_splitting_a_frame() {
    let directory = tempfile::tempdir().expect("temp directory");
    let first = PlainPage::from_bytes(Bytes::from(vec![1_u8; 64 * 1024]));
    let second = PlainPage::from_bytes(Bytes::from(vec![2_u8; 64 * 1024]));
    let mut writer = PackWriter::create(directory.path(), options(80 * 1024)).expect("writer");
    writer
        .append_page(&first)
        .expect("first frame may exceed target intact");
    assert!(writer.would_rollover(&second).expect("projection"));
    assert!(writer.append_page(&second).is_err());
    assert_eq!(writer.finish().expect("finish").entries.len(), 1);
}

#[test]
fn unfinished_temporary_pack_is_never_accepted() {
    let directory = tempfile::tempdir().expect("temp directory");
    let page = PlainPage::from_bytes(Bytes::from(vec![4_u8; 1024]));
    let mut writer = PackWriter::create(directory.path(), options(1024 * 1024)).expect("writer");
    writer.append_page(&page).expect("append");
    drop(writer);
    let temporary = std::fs::read_dir(directory.path())
        .expect("read staging")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tmp"))
        .expect("unfinished pack");
    assert!(PackReader::open_verified(&temporary).is_err());
}

#[test]
fn deterministic_input_produces_identical_pack_identity() {
    fn build(root: &std::path::Path) -> mirage_types::ContentHash {
        let mut writer = PackWriter::create(root, options(1024 * 1024)).expect("writer");
        for value in 0_u8..5 {
            writer
                .append_page(&PlainPage::from_bytes(Bytes::from(vec![value; 4096])))
                .expect("append");
        }
        writer.finish().expect("finish").content_hash
    }
    let first = tempfile::tempdir().expect("first");
    let second = tempfile::tempdir().expect("second");
    assert_eq!(build(first.path()), build(second.path()));
}
