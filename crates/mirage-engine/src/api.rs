use async_trait::async_trait;
use mirage_types::MirageError;

use crate::{AccessMask, FileHandleContext, ReadContext, ReadOutcome};

pub type EngineResult<T> = Result<T, MirageError>;

/// Core read API. `open_file` is synchronous by design and therefore cannot hide cloud discovery.
#[async_trait]
pub trait ReadEngine: Send + Sync {
    fn open_file(&self, file_index: u32, access: AccessMask) -> EngineResult<FileHandleContext>;

    async fn read_into(
        &self,
        handle: &FileHandleContext,
        offset: u64,
        destination: &mut [u8],
        context: ReadContext,
    ) -> EngineResult<ReadOutcome>;
}
