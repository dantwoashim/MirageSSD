use std::collections::BTreeMap;

use mirage_backend::{BackendId, ImmutableRevision, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_index::MountIndex;
use mirage_types::{ByteCount, CheckedRange, ContentHash, MirageError, PageHash};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLocation {
    pub object: RemoteObjectRef,
    pub encoded_range: CheckedRange,
    pub logical_length: u32,
}
#[derive(Debug, Default)]
pub struct PageLocationMap {
    entries: BTreeMap<PageHash, PageLocation>,
}
impl PageLocationMap {
    pub fn build(index: &MountIndex) -> Result<Self, MirageError> {
        let mut map = Self::default();
        for ordinal in 0..u32::try_from(index.page_count())
            .map_err(|_| MirageError::unsupported_layout("page count exceeds u32"))?
        {
            let page = index.page_by_ordinal(ordinal)?;
            let remote = page.remote_location()?;
            let location = PageLocation {
                object: RemoteObjectRef {
                    backend_id: BackendId::new(remote.backend_id()?)?,
                    provider_object_id: ProviderObjectId::new(remote.provider_object_id()?)?,
                    immutable_revision: remote
                        .immutable_revision()?
                        .map(ImmutableRevision::new)
                        .transpose()?,
                    byte_length: ByteCount::from_u64(remote.object_length()),
                    content_hash: ContentHash::from_bytes(remote.object_hash()),
                    kind: remote.object_kind(),
                },
                encoded_range: CheckedRange::new(remote.pack_offset(), remote.encoded_length())?,
                logical_length: page.logical_length(),
            };
            map.insert(page.plaintext_hash(), location)?;
        }
        Ok(map)
    }
    pub fn insert(&mut self, hash: PageHash, location: PageLocation) -> Result<(), MirageError> {
        if location.object.kind != ObjectKind::Pack
            || location.encoded_range.end_exclusive() > location.object.byte_length.as_u64()
        {
            return Err(MirageError::manifest_invalid(
                "page location is outside immutable pack",
            ));
        }
        if let Some(existing) = self.entries.get(&hash) {
            if existing != &location {
                return Err(MirageError::repository_conflict(
                    "duplicate page hash has conflicting remote locations",
                ));
            }
            return Ok(());
        }
        self.entries.insert(hash, location);
        Ok(())
    }
    pub fn get(&self, hash: PageHash) -> Result<&PageLocation, MirageError> {
        self.entries
            .get(&hash)
            .ok_or_else(|| MirageError::remote_object_missing("page has no remote location"))
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
