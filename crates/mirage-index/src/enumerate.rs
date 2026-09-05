use std::cmp::Ordering;

use mirage_types::MirageError;

use crate::name::compare_names;
use crate::reader::MountIndex;
use crate::view::DirectoryView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryEntry<'a> {
    Directory { index: u32, name: &'a str },
    File { index: u32, name: &'a str },
}

impl DirectoryEntry<'_> {
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Directory { index, .. } | Self::File { index, .. } => index,
        }
    }
}

impl<'a> DirectoryEntry<'a> {
    #[must_use]
    pub const fn name(self) -> &'a str {
        match self {
            Self::Directory { name, .. } | Self::File { name, .. } => name,
        }
    }

    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory { .. })
    }
}

impl MountIndex {
    pub fn children_after_marker<'a>(
        &'a self,
        directory: DirectoryView<'a>,
        marker: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DirectoryEntry<'a>>, MirageError> {
        if limit > 4096 {
            return Err(MirageError::invalid_argument(
                "directory enumeration page exceeds 4096 entries",
            ));
        }
        let directory_end = directory
            .first_directory()
            .checked_add(directory.directory_count())
            .ok_or_else(|| MirageError::manifest_invalid("directory child range overflows"))?;
        let file_end = directory
            .first_file()
            .checked_add(directory.file_count())
            .ok_or_else(|| MirageError::manifest_invalid("file child range overflows"))?;
        let mut directory_cursor = directory.first_directory();
        let mut file_cursor = directory.first_file();
        if let Some(marker) = marker {
            directory_cursor = upper_bound(
                directory.first_directory(),
                directory.directory_count(),
                marker,
                |ordinal| self.directory_by_index(ordinal)?.name(),
            )?;
            file_cursor = upper_bound(
                directory.first_file(),
                directory.file_count(),
                marker,
                |ordinal| self.file_by_index(ordinal)?.name(),
            )?;
        }
        let total_children = u64::from(directory.directory_count())
            .checked_add(u64::from(directory.file_count()))
            .ok_or_else(|| MirageError::manifest_invalid("directory child count overflows"))?;
        let mut output =
            Vec::with_capacity(limit.min(usize::try_from(total_children).unwrap_or(usize::MAX)));
        while output.len() < limit && (directory_cursor < directory_end || file_cursor < file_end) {
            let take_directory = match (directory_cursor < directory_end, file_cursor < file_end) {
                (true, false) => true,
                (false, true) => false,
                (true, true) => {
                    let directory_name = self.directory_by_index(directory_cursor)?.name()?;
                    let file_name = self.file_by_index(file_cursor)?.name()?;
                    compare_names(directory_name, file_name) != Ordering::Greater
                }
                (false, false) => break,
            };
            let entry = if take_directory {
                let view = self.directory_by_index(directory_cursor)?;
                directory_cursor += 1;
                DirectoryEntry::Directory {
                    index: view.ordinal(),
                    name: view.name()?,
                }
            } else {
                let view = self.file_by_index(file_cursor)?;
                file_cursor += 1;
                DirectoryEntry::File {
                    index: view.ordinal(),
                    name: view.name()?,
                }
            };
            output.push(entry);
        }
        Ok(output)
    }
}

fn upper_bound<'a>(
    first: u32,
    count: u32,
    marker: &str,
    mut name_at: impl FnMut(u32) -> Result<&'a str, MirageError>,
) -> Result<u32, MirageError> {
    let mut low = 0_u32;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        let ordinal = first
            .checked_add(middle)
            .ok_or_else(|| MirageError::manifest_invalid("marker search ordinal overflows"))?;
        if compare_names(name_at(ordinal)?, marker).is_gt() {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    first
        .checked_add(low)
        .ok_or_else(|| MirageError::manifest_invalid("marker result ordinal overflows"))
}
