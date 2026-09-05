use std::sync::Arc;

use bytes::Bytes;
use futures_executor::block_on;
use mirage_backend::{ObjectBackend, ObjectKind, UploadSource};
use mirage_backend_local::LocalObjectBackend;
use mirage_cache::{ArenaShard, CacheLayout, ResidentIndex};
use mirage_db::{CacheShardSpec, Database};
use mirage_pack::{PlainPage, encode_plain_frame};
use mirage_scheduler::{FetchPriority, FetchWindow, FrameMapping, fetch_window};
use mirage_types::{ByteCount, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

#[test]
fn local_backend_window_commits_valid_neighbors_independently() {
    block_on(async {
        let directory = tempfile::tempdir().expect("directory");
        let backend = LocalObjectBackend::open(
            &directory.path().join("remote"),
            RepositoryId::from_bytes([1; 16]),
        )
        .expect("backend");
        let page1 = PlainPage::from_bytes(Bytes::from(vec![1; 4096]));
        let page2 = PlainPage::from_bytes(Bytes::from(vec![2; 4096]));
        let frame1 = encode_plain_frame(&page1).expect("frame");
        let mut frame2 = encode_plain_frame(&page2).expect("frame");
        frame2[100] ^= 1;
        let mut wire = frame1.clone();
        wire.extend_from_slice(&frame2);
        let content = ContentHash::from_bytes(*blake3::hash(&wire).as_bytes());
        let object = backend
            .put_immutable(
                ObjectKind::Pack,
                UploadSource::from_bytes(Bytes::from(wire.clone())),
                content,
                CancellationToken::new(),
            )
            .await
            .expect("put");
        let layout = CacheLayout {
            page_size: ByteCount::from_u64(64 * 1024),
            slot_count: 4,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        };
        let shard = Arc::new(
            ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("arena"),
        );
        let db = Database::open(&directory.path().join("control.db")).expect("db");
        db.register_cache_shard(CacheShardSpec {
            shard_id: 0,
            relative_path: "arena.bin".into(),
            page_size: layout.page_size,
            slot_count: layout.slot_count,
        })
        .expect("register");
        let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
        let window = FetchWindow {
            object,
            range: CheckedRange::new(0, wire.len() as u64).expect("range"),
            priority: FetchPriority::P0Blocking,
            frames: vec![
                FrameMapping {
                    page_hash: page1.hash,
                    window_offset: 0,
                    encoded_length: frame1.len() as u64,
                },
                FrameMapping {
                    page_hash: page2.hash,
                    window_offset: frame1.len() as u64,
                    encoded_length: frame2.len() as u64,
                },
            ],
            gap_bytes: 0,
        };
        let results = fetch_window(
            &backend,
            window,
            &db,
            shard,
            &index,
            CancellationToken::new(),
            1024 * 1024,
        )
        .await
        .expect("fetch");
        assert!(results[0].result.is_ok());
        assert!(results[1].result.is_err());
        assert!(index.acquire(page1.hash).expect("lookup").is_some());
        assert!(index.acquire(page2.hash).expect("lookup").is_none());
    });
}
