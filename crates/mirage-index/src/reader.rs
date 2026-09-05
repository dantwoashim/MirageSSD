use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use mirage_backend::ObjectKind;
use mirage_manifest::validate_component;
use mirage_types::MirageError;

use crate::checked_slice::bytes_at;
use crate::format::{HEADER_SIZE, SectionKind};
use crate::header::Header;
use crate::mapped_file::ReadOnlyMapping;
use crate::name::{compare_names, compare_ordinal_ignore_case, ordinal_key};
use crate::record::{
    DirectoryRecord, ExtentRecord, FileRecord, NO_PARENT, PageRecord, RemoteLocationRecord,
    StringRef,
};
use crate::view::{DirectoryView, ExtentView, FileView, PageView, RemoteLocationView};

#[derive(Debug)]
enum Backing {
    Mapped(ReadOnlyMapping),
    Owned(Box<[u8]>),
}

impl Backing {
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Mapped(mapping) => mapping.as_bytes(),
            Self::Owned(bytes) => bytes,
        }
    }
}

#[derive(Debug)]
pub struct MountIndex {
    backing: Arc<Backing>,
    header: Header,
}

impl MountIndex {
    pub fn open(path: &Path) -> Result<Self, MirageError> {
        Self::from_backing(Backing::Mapped(ReadOnlyMapping::open(path)?))
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, MirageError> {
        Self::from_backing(Backing::Owned(bytes.into_boxed_slice()))
    }

    fn from_backing(backing: Backing) -> Result<Self, MirageError> {
        let backing = Arc::new(backing);
        let bytes = backing.as_bytes();
        let header = Header::parse(bytes)?;
        if Header::compute_index_hash(bytes)? != header.index_hash {
            return Err(MirageError::integrity_mismatch(
                "mount index whole-file hash does not match",
            ));
        }
        let index = Self { backing, header };
        index.validate_records()?;
        Ok(index)
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub fn directory_count(&self) -> u64 {
        self.header.section(SectionKind::Directories).count
    }

    #[must_use]
    pub fn file_count(&self) -> u64 {
        self.header.section(SectionKind::Files).count
    }

    #[must_use]
    pub fn extent_count(&self) -> u64 {
        self.header.section(SectionKind::Extents).count
    }

    #[must_use]
    pub fn page_count(&self) -> u64 {
        self.header.section(SectionKind::Pages).count
    }

    #[must_use]
    pub fn remote_location_count(&self) -> u64 {
        self.header.section(SectionKind::RemoteLocations).count
    }

    pub fn directory_by_index(&self, index: u32) -> Result<DirectoryView<'_>, MirageError> {
        Ok(DirectoryView::new(
            self,
            index,
            DirectoryRecord::decode(self.record_bytes(SectionKind::Directories, index)?)?,
        ))
    }

    pub fn file_by_index(&self, index: u32) -> Result<FileView<'_>, MirageError> {
        Ok(FileView::new(
            self,
            index,
            FileRecord::decode(self.record_bytes(SectionKind::Files, index)?)?,
        ))
    }

    pub fn extent_by_index(&self, index: u32) -> Result<ExtentView<'_>, MirageError> {
        Ok(ExtentView::new(
            self,
            index,
            ExtentRecord::decode(self.record_bytes(SectionKind::Extents, index)?)?,
        ))
    }

    pub fn page_by_ordinal(&self, ordinal: u32) -> Result<PageView<'_>, MirageError> {
        Ok(PageView::new(
            self,
            ordinal,
            PageRecord::decode(self.record_bytes(SectionKind::Pages, ordinal)?)?,
        ))
    }

    pub fn remote_location_by_index(
        &self,
        index: u32,
    ) -> Result<RemoteLocationView<'_>, MirageError> {
        Ok(RemoteLocationView::new(
            self,
            index,
            RemoteLocationRecord::decode(self.record_bytes(SectionKind::RemoteLocations, index)?)?,
        ))
    }

    pub(crate) fn string(&self, reference: StringRef) -> Result<&str, MirageError> {
        let section = self.header.section(SectionKind::Strings);
        let relative_end = reference
            .offset
            .checked_add(u64::from(reference.length))
            .ok_or_else(|| MirageError::manifest_invalid("string reference overflows"))?;
        if relative_end > section.length {
            return Err(MirageError::manifest_invalid(
                "string reference exceeds string table",
            ));
        }
        let absolute = section
            .offset
            .checked_add(reference.offset)
            .ok_or_else(|| MirageError::manifest_invalid("string offset overflows"))?;
        std::str::from_utf8(bytes_at(
            self.backing.as_bytes(),
            absolute,
            u64::from(reference.length),
        )?)
        .map_err(|_| MirageError::manifest_invalid("index string is not valid UTF-8"))
    }

    fn record_bytes(&self, kind: SectionKind, index: u32) -> Result<&[u8], MirageError> {
        let section = self.header.section(kind);
        if u64::from(index) >= section.count {
            return Err(MirageError::invalid_argument(
                "mount index record ordinal is out of bounds",
            ));
        }
        let width = u64::from(kind.record_size());
        let relative = u64::from(index)
            .checked_mul(width)
            .ok_or_else(|| MirageError::manifest_invalid("record offset overflows"))?;
        let absolute = section
            .offset
            .checked_add(relative)
            .ok_or_else(|| MirageError::manifest_invalid("record address overflows"))?;
        bytes_at(self.backing.as_bytes(), absolute, width)
    }

    fn validate_records(&self) -> Result<(), MirageError> {
        if self.backing.as_bytes().len() < HEADER_SIZE || self.directory_count() == 0 {
            return Err(MirageError::manifest_invalid(
                "mount index has no root directory",
            ));
        }
        let mut expected_directory_start = 1_u64;
        let mut expected_file_start = 0_u64;
        for index in 0..self.directory_count() {
            let index = u32::try_from(index)
                .map_err(|_| MirageError::manifest_invalid("directory count exceeds u32"))?;
            let directory = self.directory_by_index(index)?;
            if directory.canonical_index() != index {
                return Err(MirageError::manifest_invalid(
                    "directory canonical index is contradictory",
                ));
            }
            if (index == 0) != (directory.parent_index() == NO_PARENT) {
                return Err(MirageError::manifest_invalid(
                    "mount index root or parent identity is invalid",
                ));
            }
            if index != 0 && directory.parent_index() >= index {
                return Err(MirageError::manifest_invalid(
                    "directory parent must precede its child",
                ));
            }
            if index == 0 {
                if !directory.name()?.is_empty() {
                    return Err(MirageError::manifest_invalid(
                        "mount index root name is not empty",
                    ));
                }
            } else {
                validate_component(directory.name()?)?;
            }
            validate_range(
                directory.first_directory(),
                directory.directory_count(),
                self.directory_count(),
                "directory child range",
            )?;
            validate_range(
                directory.first_file(),
                directory.file_count(),
                self.file_count(),
                "file child range",
            )?;
            if u64::from(directory.first_directory()) != expected_directory_start
                || u64::from(directory.first_file()) != expected_file_start
            {
                return Err(MirageError::manifest_invalid(
                    "namespace child ranges are not canonical partitions",
                ));
            }
            expected_directory_start = expected_directory_start
                .checked_add(u64::from(directory.directory_count()))
                .ok_or_else(|| MirageError::manifest_invalid("directory partition overflows"))?;
            expected_file_start = expected_file_start
                .checked_add(u64::from(directory.file_count()))
                .ok_or_else(|| MirageError::manifest_invalid("file partition overflows"))?;
            if ordinal_key(directory.name()?) != directory.key()? {
                return Err(MirageError::manifest_invalid(
                    "directory lookup key does not match its name",
                ));
            }
            for child in directory.first_directory()
                ..directory
                    .first_directory()
                    .checked_add(directory.directory_count())
                    .ok_or_else(|| MirageError::manifest_invalid("child range overflows"))?
            {
                if self.directory_by_index(child)?.parent_index() != index {
                    return Err(MirageError::manifest_invalid(
                        "directory child range has a foreign parent",
                    ));
                }
            }
            for child in directory.first_file()
                ..directory
                    .first_file()
                    .checked_add(directory.file_count())
                    .ok_or_else(|| MirageError::manifest_invalid("file range overflows"))?
            {
                if self.file_by_index(child)?.parent_index() != index {
                    return Err(MirageError::manifest_invalid(
                        "file child range has a foreign parent",
                    ));
                }
            }
            self.validate_child_order(directory)?;
        }
        if expected_directory_start != self.directory_count()
            || expected_file_start != self.file_count()
        {
            return Err(MirageError::manifest_invalid(
                "namespace child ranges do not cover their tables",
            ));
        }

        let mut expected_extent_start = 0_u64;
        let mut stable_ids = HashSet::new();
        for index in 0..self.file_count() {
            let ordinal = u32::try_from(index)
                .map_err(|_| MirageError::manifest_invalid("file count exceeds u32"))?;
            let file = self.file_by_index(ordinal)?;
            if file.canonical_index() != ordinal {
                return Err(MirageError::manifest_invalid(
                    "file canonical index is contradictory",
                ));
            }
            if u64::from(file.parent_index()) >= self.directory_count() {
                return Err(MirageError::manifest_invalid(
                    "file parent is out of bounds",
                ));
            }
            validate_component(file.name()?)?;
            if ordinal_key(file.name()?) != file.key()? {
                return Err(MirageError::manifest_invalid(
                    "file lookup key does not match its name",
                ));
            }
            validate_range(
                file.extent_start(),
                file.extent_count(),
                self.extent_count(),
                "file extent range",
            )?;
            if u64::from(file.extent_start()) != expected_extent_start {
                return Err(MirageError::manifest_invalid(
                    "file extent ranges are not a canonical partition",
                ));
            }
            expected_extent_start = expected_extent_start
                .checked_add(u64::from(file.extent_count()))
                .ok_or_else(|| MirageError::manifest_invalid("extent partition overflows"))?;
            if !stable_ids.insert(file.stable_id()) {
                return Err(MirageError::manifest_invalid(
                    "mount index contains duplicate stable file IDs",
                ));
            }
            self.validate_file_layout(file)?;
        }
        if expected_extent_start != self.extent_count() {
            return Err(MirageError::manifest_invalid(
                "file extent ranges do not cover the extent table",
            ));
        }

        let mut expected_page_start = 0_u64;
        for index in 0..self.extent_count() {
            let extent = self.extent_by_index(
                u32::try_from(index)
                    .map_err(|_| MirageError::manifest_invalid("extent count exceeds u32"))?,
            )?;
            validate_range(
                extent.page_start(),
                extent.page_count(),
                self.page_count(),
                "extent page range",
            )?;
            if extent.logical_length() == 0 || extent.page_count() == 0 {
                return Err(MirageError::manifest_invalid("mount index extent is empty"));
            }
            if u64::from(extent.page_start()) != expected_page_start {
                return Err(MirageError::manifest_invalid(
                    "extent page ranges are not a canonical partition",
                ));
            }
            expected_page_start = expected_page_start
                .checked_add(u64::from(extent.page_count()))
                .ok_or_else(|| MirageError::manifest_invalid("page partition overflows"))?;
            let last_page = extent.page(extent.page_count() - 1)?;
            if extent.tail_length() != last_page.logical_length() {
                return Err(MirageError::manifest_invalid(
                    "extent tail length contradicts its final page",
                ));
            }
        }
        if expected_page_start != self.page_count() {
            return Err(MirageError::manifest_invalid(
                "extent page ranges do not cover the page table",
            ));
        }
        if self.remote_location_count() != self.page_count() {
            return Err(MirageError::manifest_invalid(
                "canonical mount index requires one remote record per page",
            ));
        }
        for index in 0..self.page_count() {
            let page = self.page_by_ordinal(
                u32::try_from(index)
                    .map_err(|_| MirageError::manifest_invalid("page count exceeds u32"))?,
            )?;
            if page.logical_length() == 0
                || u64::from(page.logical_length()) > self.header.page_size
                || u64::from(page.remote_location_index()) != index
            {
                return Err(MirageError::manifest_invalid(
                    "page record has invalid length or remote location",
                ));
            }
        }
        for index in 0..self.remote_location_count() {
            let location =
                self.remote_location_by_index(u32::try_from(index).map_err(|_| {
                    MirageError::manifest_invalid("remote location count exceeds u32")
                })?)?;
            location.backend_id()?;
            location.provider_object_id()?;
            location.immutable_revision()?;
            if location.object_kind() != ObjectKind::Pack || location.encoded_length() == 0 {
                return Err(MirageError::manifest_invalid(
                    "remote location is not a non-empty immutable pack range",
                ));
            }
            let end = location
                .pack_offset()
                .checked_add(location.encoded_length())
                .ok_or_else(|| MirageError::manifest_invalid("remote pack range overflows"))?;
            if end > location.object_length() {
                return Err(MirageError::manifest_invalid(
                    "remote pack range exceeds object length",
                ));
            }
        }
        Ok(())
    }

    fn validate_file_layout(&self, file: FileView<'_>) -> Result<(), MirageError> {
        if !file.class().is_virtual() {
            if file.extent_count() != 0 {
                return Err(MirageError::manifest_invalid(
                    "native file contains virtual extents",
                ));
            }
            return Ok(());
        }
        if file.logical_size() == 0 {
            if file.extent_count() != 0 {
                return Err(MirageError::manifest_invalid(
                    "empty virtual file contains extents",
                ));
            }
            return Ok(());
        }
        if file.extent_count() == 0 {
            return Err(MirageError::manifest_invalid(
                "non-empty virtual file has no extents",
            ));
        }
        let mut expected_offset = 0_u64;
        for extent_index in 0..file.extent_count() {
            let extent = file.extent(extent_index)?;
            if extent.logical_offset() != expected_offset || extent.logical_length() == 0 {
                return Err(MirageError::manifest_invalid(
                    "file extents are gapped, overlapping, or empty",
                ));
            }
            let mut page_bytes = 0_u64;
            for page_index in 0..extent.page_count() {
                let page = extent.page(page_index)?;
                if page_index + 1 != extent.page_count()
                    && u64::from(page.logical_length()) != self.header.page_size
                {
                    return Err(MirageError::manifest_invalid(
                        "non-final extent page is shorter than page size",
                    ));
                }
                page_bytes = page_bytes
                    .checked_add(u64::from(page.logical_length()))
                    .ok_or_else(|| MirageError::manifest_invalid("extent page bytes overflow"))?;
            }
            if page_bytes != extent.logical_length() {
                return Err(MirageError::manifest_invalid(
                    "extent length does not equal its page bytes",
                ));
            }
            expected_offset = expected_offset
                .checked_add(extent.logical_length())
                .ok_or_else(|| MirageError::manifest_invalid("file length overflows"))?;
        }
        if expected_offset != file.logical_size() {
            return Err(MirageError::manifest_invalid(
                "file extents do not cover its exact logical size",
            ));
        }
        Ok(())
    }

    fn validate_child_order(&self, directory: DirectoryView<'_>) -> Result<(), MirageError> {
        let mut previous: Option<&str> = None;
        for child in directory.first_directory()
            ..directory
                .first_directory()
                .checked_add(directory.directory_count())
                .ok_or_else(|| MirageError::manifest_invalid("directory child range overflows"))?
        {
            let name = self.directory_by_index(child)?.name()?;
            if previous.is_some_and(|value| !compare_names(value, name).is_lt()) {
                return Err(MirageError::manifest_invalid(
                    "directory children are not in strict lookup-key order",
                ));
            }
            previous = Some(name);
        }
        previous = None;
        for child in directory.first_file()
            ..directory
                .first_file()
                .checked_add(directory.file_count())
                .ok_or_else(|| MirageError::manifest_invalid("file child range overflows"))?
        {
            let name = self.file_by_index(child)?.name()?;
            if previous.is_some_and(|value| !compare_names(value, name).is_lt()) {
                return Err(MirageError::manifest_invalid(
                    "file children are not in strict lookup-key order",
                ));
            }
            previous = Some(name);
        }
        let mut directory_cursor = directory.first_directory();
        let directory_end = directory_cursor
            .checked_add(directory.directory_count())
            .ok_or_else(|| MirageError::manifest_invalid("directory child range overflows"))?;
        let mut file_cursor = directory.first_file();
        let file_end = file_cursor
            .checked_add(directory.file_count())
            .ok_or_else(|| MirageError::manifest_invalid("file child range overflows"))?;
        while directory_cursor < directory_end && file_cursor < file_end {
            let directory_name = self.directory_by_index(directory_cursor)?.name()?;
            let file_name = self.file_by_index(file_cursor)?.name()?;
            match compare_ordinal_ignore_case(directory_name, file_name) {
                std::cmp::Ordering::Less => directory_cursor += 1,
                std::cmp::Ordering::Greater => file_cursor += 1,
                std::cmp::Ordering::Equal => {
                    return Err(MirageError::manifest_invalid(
                        "directory and file names collide under lookup semantics",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_range(start: u32, count: u32, maximum: u64, label: &str) -> Result<(), MirageError> {
    let end = u64::from(start)
        .checked_add(u64::from(count))
        .ok_or_else(|| MirageError::manifest_invalid(format!("{label} overflows")))?;
    if end > maximum {
        return Err(MirageError::manifest_invalid(format!(
            "{label} exceeds its section"
        )));
    }
    Ok(())
}
