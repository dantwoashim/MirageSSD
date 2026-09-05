use std::cmp::Ordering;

use mirage_types::MirageError;

use crate::name::{compare_ordinal_ignore_case, validate_component};
use crate::reader::MountIndex;
use crate::view::DirectoryView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeIndex {
    Directory(u32),
    File(u32),
}

impl MountIndex {
    pub fn lookup_child(
        &self,
        directory: DirectoryView<'_>,
        name: &str,
    ) -> Result<Option<NodeIndex>, MirageError> {
        validate_component(name)?;
        let directory_match = binary_search(
            directory.first_directory(),
            directory.directory_count(),
            name,
            |ordinal| self.directory_by_index(ordinal)?.name(),
        )?;
        let file_match = binary_search(
            directory.first_file(),
            directory.file_count(),
            name,
            |ordinal| self.file_by_index(ordinal)?.name(),
        )?;
        match (directory_match, file_match) {
            (Some(_), Some(_)) => Err(MirageError::repository_conflict(
                "mount index contains an ambiguous namespace key",
            )),
            (Some(index), None) => Ok(Some(NodeIndex::Directory(index))),
            (None, Some(index)) => Ok(Some(NodeIndex::File(index))),
            (None, None) => Ok(None),
        }
    }

    pub fn lookup_path(&self, path: &str) -> Result<Option<NodeIndex>, MirageError> {
        let trimmed = path.trim_matches(|character| character == '/' || character == '\\');
        if trimmed.is_empty() {
            return Ok(Some(NodeIndex::Directory(0)));
        }
        let mut current = self.directory_by_index(0)?;
        let mut components = trimmed.split(['/', '\\']).peekable();
        while let Some(component) = components.next() {
            let found = self.lookup_child(current, component)?;
            match (found, components.peek().is_some()) {
                (Some(NodeIndex::Directory(index)), true) => {
                    current = self.directory_by_index(index)?;
                }
                (Some(node), false) => return Ok(Some(node)),
                (Some(NodeIndex::File(_)), true) => return Ok(None),
                (None, _) => return Ok(None),
            }
        }
        Ok(Some(NodeIndex::Directory(current.ordinal())))
    }
}

fn binary_search<'a>(
    first: u32,
    count: u32,
    needle: &str,
    mut key_at: impl FnMut(u32) -> Result<&'a str, MirageError>,
) -> Result<Option<u32>, MirageError> {
    let mut low = 0_u32;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        let ordinal = first
            .checked_add(middle)
            .ok_or_else(|| MirageError::manifest_invalid("lookup child ordinal overflows"))?;
        match compare_ordinal_ignore_case(key_at(ordinal)?, needle) {
            Ordering::Less => low = middle + 1,
            Ordering::Equal => return Ok(Some(ordinal)),
            Ordering::Greater => high = middle,
        }
    }
    Ok(None)
}
