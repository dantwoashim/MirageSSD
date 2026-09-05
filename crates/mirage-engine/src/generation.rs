use crate::PageLocationMap;
use mirage_index::{FileView, MountIndex};
use mirage_types::{GenerationId, MirageError, RepositoryId};
use std::sync::Arc;

#[derive(Debug)]
pub struct MountGeneration {
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub index: Arc<MountIndex>,
    pub page_locations: Arc<PageLocationMap>,
}
impl MountGeneration {
    pub fn new(
        repository_id: RepositoryId,
        generation_id: GenerationId,
        index: Arc<MountIndex>,
        cache_page_size: u64,
    ) -> Result<Self, MirageError> {
        if index.header().page_size != cache_page_size {
            return Err(MirageError::unsupported_layout(
                "mount index and cache page sizes differ",
            ));
        }
        let page_locations = Arc::new(PageLocationMap::build(&index)?);
        Ok(Self {
            repository_id,
            generation_id,
            index,
            page_locations,
        })
    }
    pub fn file(&self, index: u32) -> Result<FileView<'_>, MirageError> {
        self.index.file_by_index(index)
    }
}
