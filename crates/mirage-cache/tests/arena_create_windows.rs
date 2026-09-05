use mirage_cache::{ArenaShard, CacheLayout};
use mirage_types::ByteCount;
use std::io::{Read, Seek, SeekFrom, Write};

fn layout() -> CacheLayout {
    CacheLayout {
        page_size: ByteCount::from_u64(1024 * 1024),
        slot_count: 4096,
        db_journal_allowance: ByteCount::from_u64(64 * 1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(1024 * 1024 * 1024),
    }
}

#[test]
fn create_reopen_and_validate_sparse_logical_layout() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("arena-0.bin");
    let shard = ArenaShard::create(&path, layout()).expect("create");
    let diagnostics = shard.diagnostics().expect("diagnostics");
    assert_eq!(
        diagnostics.logical_bytes,
        layout().logical_shard_bytes().expect("length")
    );
    #[cfg(windows)]
    assert!(
        diagnostics.allocated_bytes < 16 * 1024 * 1024,
        "new 4 GiB sparse shard allocated {} bytes",
        diagnostics.allocated_bytes
    );
    assert_eq!(
        shard.slot_offset(4095).expect("last"),
        4096 + 4095 * 1024 * 1024
    );
    assert!(shard.slot_offset(4096).is_err());
    drop(shard);
    ArenaShard::open(&path, layout()).expect("reopen");
    let wrong = CacheLayout {
        page_size: ByteCount::from_u64(512 * 1024),
        ..layout()
    };
    assert!(ArenaShard::open(&path, wrong).is_err());
}

#[test]
fn wrong_header_and_acl_or_share_failure_are_closed() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("arena.bin");
    let shard = ArenaShard::create(&path, layout()).expect("create");
    // Mandatory share-mode exclusion is provided by the Windows file handle.
    #[cfg(windows)]
    assert!(
        ArenaShard::open(&path, layout()).is_err(),
        "restrictive sharing must reject a second writer"
    );
    drop(shard);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open");
    file.seek(SeekFrom::Start(20)).expect("seek");
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).expect("read byte");
    byte[0] ^= 1;
    file.seek(SeekFrom::Start(20)).expect("seek");
    file.write_all(&byte).expect("corrupt byte");
    file.sync_all().expect("sync");
    drop(file);
    assert!(ArenaShard::open(&path, layout()).is_err());
}
