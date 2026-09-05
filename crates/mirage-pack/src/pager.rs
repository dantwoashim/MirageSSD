use std::fs::{File, Metadata};
use std::io::{Read, Seek};
use std::path::Path;
use std::time::SystemTime;

use bytes::Bytes;
use mirage_types::MirageError;
use same_file::Handle;

use crate::page::PlainPage;

const MIN_PAGE_SIZE: u32 = 64 * 1024;
const MAX_PAGE_SIZE: u32 = 16 * 1024 * 1024;

pub struct PageIter<R> {
    reader: R,
    remaining: u64,
    page_size: usize,
    finished: bool,
}

pub fn page_file<R: Read + Seek>(
    reader: R,
    size: u64,
    page_size: u32,
) -> Result<PageIter<R>, MirageError> {
    validate_page_size(page_size)?;
    Ok(PageIter {
        reader,
        remaining: size,
        page_size: usize::try_from(page_size)
            .map_err(|_| MirageError::invalid_argument("page size does not fit this platform"))?,
        finished: false,
    })
}

impl<R: Read + Seek> Iterator for PageIter<R> {
    type Item = Result<PlainPage, MirageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.remaining == 0 {
            return None;
        }
        let wanted = usize::try_from(self.remaining.min(self.page_size as u64))
            .expect("bounded page length fits usize");
        let mut bytes = vec![0_u8; wanted];
        if let Err(error) = self.reader.read_exact(&mut bytes) {
            self.finished = true;
            return Some(Err(MirageError::from(error)));
        }
        self.remaining -= wanted as u64;
        Some(Ok(PlainPage::from_bytes(Bytes::from(bytes))))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let pages = self.remaining.div_ceil(self.page_size as u64);
        let pages = usize::try_from(pages).unwrap_or(usize::MAX);
        (pages, Some(pages))
    }
}

pub fn page_path(path: &Path, page_size: u32) -> Result<Vec<PlainPage>, MirageError> {
    page_path_with_validation_hook(path, page_size, || Ok(()))
}

#[doc(hidden)]
pub fn page_path_with_validation_hook(
    path: &Path,
    page_size: u32,
    after_read: impl FnOnce() -> Result<(), MirageError>,
) -> Result<Vec<PlainPage>, MirageError> {
    let mut file = File::open(path).map_err(MirageError::from)?;
    let opened_identity = Handle::from_file(file.try_clone().map_err(MirageError::from)?)
        .map_err(MirageError::from)?;
    let before = file.metadata().map_err(MirageError::from)?;
    reject_non_regular(&before)?;
    let fingerprint = SourceFingerprint::from_metadata(&before);
    let pages: Result<Vec<_>, _> = page_file(&mut file, before.len(), page_size)?.collect();
    let pages = pages?;
    after_read()?;
    let after_handle = file.metadata().map_err(MirageError::from)?;
    let after_path = std::fs::metadata(path).map_err(MirageError::from)?;
    let current_identity = Handle::from_path(path).map_err(MirageError::from)?;
    if fingerprint != SourceFingerprint::from_metadata(&after_handle)
        || fingerprint != SourceFingerprint::from_metadata(&after_path)
        || opened_identity != current_identity
    {
        return Err(MirageError::integrity_mismatch(
            "source file changed while it was being paged",
        ));
    }
    Ok(pages)
}

fn validate_page_size(page_size: u32) -> Result<(), MirageError> {
    if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(MirageError::invalid_argument(
            "page size must be a supported power of two",
        ));
    }
    Ok(())
}

fn reject_non_regular(metadata: &Metadata) -> Result<(), MirageError> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MirageError::unsupported_layout(
            "paging requires a regular non-reparse source file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
        if metadata.file_attributes() & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_SPARSE_FILE)
            != 0
        {
            return Err(MirageError::unsupported_layout(
                "sparse or reparse source behavior is not represented by logical paging",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}

impl SourceFingerprint {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        }
    }
}
