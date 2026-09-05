use mirage_backend::ObjectKind;
use mirage_manifest::{Codec, FileClass};
use mirage_types::{MirageError, PageHash, StableFileId};

use crate::reader::MountIndex;
use crate::record::{DirectoryRecord, ExtentRecord, FileRecord, PageRecord, RemoteLocationRecord};

#[derive(Debug, Clone, Copy)]
pub struct DirectoryView<'a> {
    index: &'a MountIndex,
    ordinal: u32,
    record: DirectoryRecord,
}

impl<'a> DirectoryView<'a> {
    pub(crate) const fn new(index: &'a MountIndex, ordinal: u32, record: DirectoryRecord) -> Self {
        Self {
            index,
            ordinal,
            record,
        }
    }

    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
    #[must_use]
    pub const fn parent_index(self) -> u32 {
        self.record.parent
    }
    #[must_use]
    pub const fn first_directory(self) -> u32 {
        self.record.first_directory
    }
    #[must_use]
    pub const fn directory_count(self) -> u32 {
        self.record.directory_count
    }
    #[must_use]
    pub const fn first_file(self) -> u32 {
        self.record.first_file
    }
    #[must_use]
    pub const fn file_count(self) -> u32 {
        self.record.file_count
    }
    #[must_use]
    pub const fn canonical_index(self) -> u32 {
        self.record.canonical_index
    }
    pub fn name(self) -> Result<&'a str, MirageError> {
        self.index.string(self.record.name)
    }
    pub fn key(self) -> Result<&'a str, MirageError> {
        self.index.string(self.record.key)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FileView<'a> {
    index: &'a MountIndex,
    ordinal: u32,
    record: FileRecord,
}

impl<'a> FileView<'a> {
    pub(crate) const fn new(index: &'a MountIndex, ordinal: u32, record: FileRecord) -> Self {
        Self {
            index,
            ordinal,
            record,
        }
    }

    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
    #[must_use]
    pub const fn parent_index(self) -> u32 {
        self.record.parent_directory
    }
    #[must_use]
    pub const fn class(self) -> FileClass {
        self.record.class
    }
    #[must_use]
    pub const fn logical_size(self) -> u64 {
        self.record.logical_size
    }
    #[must_use]
    pub const fn stable_id(self) -> StableFileId {
        self.record.stable_id
    }
    #[must_use]
    pub const fn extent_start(self) -> u32 {
        self.record.extent_start
    }
    #[must_use]
    pub const fn extent_count(self) -> u32 {
        self.record.extent_count
    }
    #[must_use]
    pub const fn canonical_index(self) -> u32 {
        self.record.canonical_index
    }
    #[must_use]
    pub const fn page_size(self) -> u64 {
        self.index.header().page_size
    }
    pub fn name(self) -> Result<&'a str, MirageError> {
        self.index.string(self.record.name)
    }
    pub fn key(self) -> Result<&'a str, MirageError> {
        self.index.string(self.record.key)
    }
    pub fn extent(self, relative: u32) -> Result<ExtentView<'a>, MirageError> {
        if relative >= self.extent_count() {
            return Err(MirageError::invalid_argument(
                "file extent ordinal is out of bounds",
            ));
        }
        self.index.extent_by_index(
            self.extent_start()
                .checked_add(relative)
                .ok_or_else(|| MirageError::manifest_invalid("file extent ordinal overflows"))?,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExtentView<'a> {
    index: &'a MountIndex,
    ordinal: u32,
    record: ExtentRecord,
}

impl<'a> ExtentView<'a> {
    pub(crate) const fn new(index: &'a MountIndex, ordinal: u32, record: ExtentRecord) -> Self {
        Self {
            index,
            ordinal,
            record,
        }
    }
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
    #[must_use]
    pub const fn logical_offset(self) -> u64 {
        self.record.logical_offset
    }
    #[must_use]
    pub const fn logical_length(self) -> u64 {
        self.record.logical_length
    }
    #[must_use]
    pub const fn page_start(self) -> u32 {
        self.record.page_start
    }
    #[must_use]
    pub const fn page_count(self) -> u32 {
        self.record.page_count
    }
    #[must_use]
    pub const fn tail_length(self) -> u32 {
        self.record.tail_length
    }
    pub fn page(self, relative: u32) -> Result<PageView<'a>, MirageError> {
        if relative >= self.page_count() {
            return Err(MirageError::invalid_argument(
                "extent page ordinal is out of bounds",
            ));
        }
        self.index.page_by_ordinal(
            self.page_start()
                .checked_add(relative)
                .ok_or_else(|| MirageError::manifest_invalid("extent page ordinal overflows"))?,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PageView<'a> {
    index: &'a MountIndex,
    ordinal: u32,
    record: PageRecord,
}

impl<'a> PageView<'a> {
    pub(crate) const fn new(index: &'a MountIndex, ordinal: u32, record: PageRecord) -> Self {
        Self {
            index,
            ordinal,
            record,
        }
    }
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
    #[must_use]
    pub const fn plaintext_hash(self) -> PageHash {
        self.record.plaintext_hash
    }
    #[must_use]
    pub const fn logical_length(self) -> u32 {
        self.record.logical_length
    }
    #[must_use]
    pub const fn remote_location_index(self) -> u32 {
        self.record.remote_location
    }
    pub fn remote_location(self) -> Result<RemoteLocationView<'a>, MirageError> {
        self.index
            .remote_location_by_index(self.remote_location_index())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RemoteLocationView<'a> {
    index: &'a MountIndex,
    ordinal: u32,
    record: RemoteLocationRecord,
}

impl<'a> RemoteLocationView<'a> {
    pub(crate) const fn new(
        index: &'a MountIndex,
        ordinal: u32,
        record: RemoteLocationRecord,
    ) -> Self {
        Self {
            index,
            ordinal,
            record,
        }
    }
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
    pub fn backend_id(self) -> Result<&'a str, MirageError> {
        self.index.string(self.record.backend_id)
    }
    pub fn provider_object_id(self) -> Result<&'a str, MirageError> {
        self.index.string(self.record.provider_object_id)
    }
    pub fn immutable_revision(self) -> Result<Option<&'a str>, MirageError> {
        self.record
            .immutable_revision
            .map(|value| self.index.string(value))
            .transpose()
    }
    #[must_use]
    pub const fn object_length(self) -> u64 {
        self.record.object_length
    }
    #[must_use]
    pub const fn object_hash(self) -> [u8; 32] {
        self.record.object_hash
    }
    #[must_use]
    pub const fn object_kind(self) -> ObjectKind {
        self.record.object_kind
    }
    #[must_use]
    pub const fn codec(self) -> Codec {
        self.record.codec
    }
    #[must_use]
    pub const fn pack_offset(self) -> u64 {
        self.record.pack_offset
    }
    #[must_use]
    pub const fn encoded_length(self) -> u64 {
        self.record.encoded_length
    }
}
