use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mirage_engine::{
    AccessMask, AccessPattern, BufferCacheMode, BufferingHint, CacheTier, EngineResult,
    FileHandleContext, ProcessRole, ReadContext, ReadEngine, ReadOutcome, ReadPriority,
};
use mirage_types::{ByteCount, GenerationId, MirageError, PageOrdinal};
use tokio_util::sync::CancellationToken;

const PAGE_SIZE: usize = 1024 * 1024;

struct MemoryEngine {
    generation: GenerationId,
    files: BTreeMap<u32, Vec<u8>>,
}

#[async_trait]
impl ReadEngine for MemoryEngine {
    fn open_file(&self, file_index: u32, access: AccessMask) -> EngineResult<FileHandleContext> {
        if !access.contains(AccessMask::READ_DATA) {
            return Err(MirageError::invalid_argument("read access is required"));
        }
        let bytes = self
            .files
            .get(&file_index)
            .ok_or_else(|| MirageError::invalid_argument("unknown file index"))?;
        Ok(FileHandleContext::new(
            file_index,
            self.generation,
            ByteCount::from_u64(bytes.len() as u64),
            access,
        ))
    }

    async fn read_into(
        &self,
        handle: &FileHandleContext,
        offset: u64,
        destination: &mut [u8],
        context: ReadContext,
    ) -> EngineResult<ReadOutcome> {
        if context.is_cancelled() {
            return Err(MirageError::cancelled("read cancelled before dispatch"));
        }
        if context.is_expired() {
            return Err(MirageError::deadline_exceeded("read deadline expired"));
        }
        if handle.generation_id() != self.generation {
            return Err(MirageError::repository_conflict(
                "stale file-handle generation",
            ));
        }
        let file = self
            .files
            .get(&handle.file_index())
            .ok_or_else(|| MirageError::invalid_argument("unknown file index"))?;
        let start = usize::try_from(offset)
            .map_err(|_| MirageError::invalid_argument("read offset is not addressable"))?;
        if start >= file.len() || destination.is_empty() {
            return Ok(ReadOutcome::empty(CacheTier::Memory));
        }
        let count = destination.len().min(file.len() - start);
        destination[..count].copy_from_slice(&file[start..start + count]);
        let first_page = start / PAGE_SIZE;
        let last_page = (start + count - 1) / PAGE_SIZE;
        let pages_touched = (first_page..=last_page)
            .map(|page| PageOrdinal::from_u32(u32::try_from(page).expect("test page fits u32")))
            .collect();
        Ok(ReadOutcome {
            bytes_transferred: ByteCount::from_u64(count as u64),
            cache_tier: CacheTier::Memory,
            pages_touched,
            seal_violation: None,
        })
    }
}

fn context() -> ReadContext {
    ReadContext {
        priority: ReadPriority::Blocking,
        deadline: Instant::now() + Duration::from_secs(1),
        cancellation: CancellationToken::new(),
        process_role: ProcessRole::Game,
        buffering: BufferingHint {
            cache_mode: BufferCacheMode::Buffered,
            access_pattern: AccessPattern::Random,
        },
    }
}

fn assert_object_safe(_: &dyn ReadEngine) {}

#[test]
fn fake_engine_reads_exact_bytes_across_pages_without_windows() {
    let mut bytes = vec![0_u8; PAGE_SIZE + 4];
    bytes[PAGE_SIZE - 2..].copy_from_slice(b"ABCDEF");
    let engine = MemoryEngine {
        generation: GenerationId::from_u64(7),
        files: BTreeMap::from([(9, bytes)]),
    };
    assert_object_safe(&engine);

    let handle = engine
        .open_file(9, AccessMask::READ_ONLY)
        .expect("open memory file");
    let mut destination = [0_u8; 6];
    let outcome = futures_executor::block_on(engine.read_into(
        &handle,
        (PAGE_SIZE - 2) as u64,
        &mut destination,
        context(),
    ))
    .expect("read memory file");

    assert_eq!(&destination, b"ABCDEF");
    assert_eq!(outcome.bytes_transferred.as_u64(), 6);
    assert_eq!(
        outcome.pages_touched,
        vec![PageOrdinal::from_u32(0), PageOrdinal::from_u32(1)]
    );
    assert_eq!(outcome.cache_tier, CacheTier::Memory);
    assert!(outcome.seal_violation.is_none());
}

#[test]
fn cancellation_and_deadline_are_explicit_failures() {
    let engine = MemoryEngine {
        generation: GenerationId::from_u64(1),
        files: BTreeMap::from([(1, b"data".to_vec())]),
    };
    let handle = engine.open_file(1, AccessMask::READ_DATA).expect("open");
    let mut destination = [0_u8; 4];

    let cancelled = context();
    cancelled.cancellation.cancel();
    let error =
        futures_executor::block_on(engine.read_into(&handle, 0, &mut destination, cancelled))
            .expect_err("cancelled read");
    assert_eq!(error.code, "MIRAGE_CANCELLED");

    let mut expired = context();
    expired.deadline = Instant::now() - Duration::from_millis(1);
    let error = futures_executor::block_on(engine.read_into(&handle, 0, &mut destination, expired))
        .expect_err("expired read");
    assert_eq!(error.code, "MIRAGE_DEADLINE_EXCEEDED");
}
