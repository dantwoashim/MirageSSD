use std::collections::{HashMap, HashSet};

use mirage_backend::ObjectKind;
use mirage_types::{ByteCount, CheckedRange, MirageError, PageHash};

use crate::model::{MANIFEST_FORMAT_VERSION, ManifestSummary, RepositoryManifest};
use crate::path::{MAX_LOGICAL_PATH_BYTES, validate_component, windows_case_key};

pub const MAX_DIRECTORIES: usize = 1_000_000;
pub const MAX_FILES: usize = 1_000_000;
pub const MAX_EXTENTS: usize = 16_000_000;
pub const MAX_PAGES: usize = 16_000_000;
pub const MAX_REMOTE_LOCATIONS: usize = 16_000_000;
pub const MAX_FILE_BYTES: u64 = i64::MAX as u64;

const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "bat", "cmd", "com", "cpl", "dll", "exe", "msi", "ps1", "scr", "sys",
];

pub fn validate_manifest(manifest: &RepositoryManifest) -> Result<(), MirageError> {
    if manifest.format_version != MANIFEST_FORMAT_VERSION {
        return Err(MirageError::unsupported_layout(
            "manifest format version is not supported",
        ));
    }
    let page_size = manifest.page_size.as_u64();
    if !(64 * 1024..=16 * 1024 * 1024).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(MirageError::manifest_invalid(
            "manifest page size is outside the supported power-of-two range",
        ));
    }
    check_count(manifest.directories.len(), MAX_DIRECTORIES, "directories")?;
    check_count(manifest.files.len(), MAX_FILES, "files")?;
    check_count(manifest.extents.len(), MAX_EXTENTS, "extents")?;
    check_count(manifest.pages.len(), MAX_PAGES, "pages")?;
    check_count(
        manifest.remote_locations.len(),
        MAX_REMOTE_LOCATIONS,
        "remote locations",
    )?;
    validate_directories(manifest)?;
    validate_pages_and_locations(manifest, page_size)?;
    validate_files(manifest, page_size)
}

fn check_count(actual: usize, maximum: usize, label: &str) -> Result<(), MirageError> {
    if actual > maximum {
        return Err(MirageError::manifest_invalid(format!(
            "manifest {label} count exceeds its declared bound"
        )));
    }
    Ok(())
}

fn validate_directories(manifest: &RepositoryManifest) -> Result<(), MirageError> {
    let roots: Vec<_> = manifest
        .directories
        .iter()
        .enumerate()
        .filter(|(_, directory)| directory.parent.is_none())
        .collect();
    if roots.len() != 1 || !roots[0].1.name.is_empty() {
        return Err(MirageError::manifest_invalid(
            "manifest must contain exactly one empty-name root directory",
        ));
    }
    let root_index = roots[0].0;
    for (index, directory) in manifest.directories.iter().enumerate() {
        if index != root_index {
            validate_component(&directory.name)?;
            let parent = usize::try_from(directory.parent.ok_or_else(|| {
                MirageError::manifest_invalid("non-root directory has no parent")
            })?)
            .map_err(|_| MirageError::manifest_invalid("directory parent index overflows"))?;
            if parent >= manifest.directories.len() || parent == index {
                return Err(MirageError::manifest_invalid(
                    "directory parent index is invalid",
                ));
            }
        }
        let mut cursor = index;
        let mut hops = 0usize;
        while cursor != root_index {
            hops = hops
                .checked_add(1)
                .ok_or_else(|| MirageError::manifest_invalid("directory ancestry overflows"))?;
            if hops > manifest.directories.len() {
                return Err(MirageError::manifest_invalid(
                    "directory parent graph contains a cycle",
                ));
            }
            cursor = usize::try_from(manifest.directories[cursor].parent.ok_or_else(|| {
                MirageError::manifest_invalid("directory ancestry does not reach root")
            })?)
            .map_err(|_| MirageError::manifest_invalid("directory parent index overflows"))?;
            if cursor >= manifest.directories.len() {
                return Err(MirageError::manifest_invalid(
                    "directory parent index is out of bounds",
                ));
            }
        }
        if logical_directory_length(manifest, index)? > MAX_LOGICAL_PATH_BYTES {
            return Err(MirageError::manifest_invalid(
                "logical directory path exceeds its declared bound",
            ));
        }
    }

    let mut siblings: HashSet<(u32, String)> = HashSet::new();
    for directory in &manifest.directories {
        if let Some(parent) = directory.parent
            && !siblings.insert((parent, windows_case_key(&directory.name)))
        {
            return Err(MirageError::manifest_invalid(
                "case-colliding sibling directories are forbidden",
            ));
        }
    }
    Ok(())
}

fn logical_directory_length(
    manifest: &RepositoryManifest,
    mut index: usize,
) -> Result<usize, MirageError> {
    let mut length = 0usize;
    loop {
        let directory = &manifest.directories[index];
        if !directory.name.is_empty() {
            length = length
                .checked_add(directory.name.len() + usize::from(length != 0))
                .ok_or_else(|| MirageError::manifest_invalid("logical path length overflows"))?;
        }
        match directory.parent {
            Some(parent) => {
                index = usize::try_from(parent)
                    .map_err(|_| MirageError::manifest_invalid("directory index overflows"))?;
            }
            None => return Ok(length),
        }
    }
}

fn validate_pages_and_locations(
    manifest: &RepositoryManifest,
    page_size: u64,
) -> Result<(), MirageError> {
    for location in &manifest.remote_locations {
        location.object.validate()?;
        if location.object.kind != ObjectKind::Pack {
            return Err(MirageError::manifest_invalid(
                "page remote location must reference an immutable pack",
            ));
        }
        if location.encoded_length.is_zero() {
            return Err(MirageError::manifest_invalid(
                "encoded page range cannot be empty",
            ));
        }
        let range = CheckedRange::new(location.offset, location.encoded_length.as_u64())?;
        if range.end_exclusive() > location.object.byte_length.as_u64() {
            return Err(MirageError::manifest_invalid(
                "encoded page range exceeds immutable object length",
            ));
        }
    }
    for page in &manifest.pages {
        if page.logical_length == 0 || u64::from(page.logical_length) > page_size {
            return Err(MirageError::manifest_invalid(
                "page logical length is zero or exceeds page size",
            ));
        }
        let remote = usize::try_from(page.remote_location)
            .map_err(|_| MirageError::manifest_invalid("remote location index overflows"))?;
        if remote >= manifest.remote_locations.len() {
            return Err(MirageError::manifest_invalid(
                "page remote location index is out of bounds",
            ));
        }
    }
    Ok(())
}

fn validate_files(manifest: &RepositoryManifest, page_size: u64) -> Result<(), MirageError> {
    let mut sibling_names: HashSet<(u32, String)> = HashSet::new();
    let mut directory_names: HashSet<(u32, String)> = manifest
        .directories
        .iter()
        .filter_map(|directory| {
            directory
                .parent
                .map(|parent| (parent, windows_case_key(&directory.name)))
        })
        .collect();
    let mut stable_ids = HashSet::new();
    for file in &manifest.files {
        validate_component(&file.name)?;
        let parent = usize::try_from(file.parent_directory)
            .map_err(|_| MirageError::manifest_invalid("file parent index overflows"))?;
        if parent >= manifest.directories.len() {
            return Err(MirageError::manifest_invalid(
                "file parent directory is out of bounds",
            ));
        }
        let key = (file.parent_directory, windows_case_key(&file.name));
        if !sibling_names.insert(key.clone()) || directory_names.remove(&key) {
            return Err(MirageError::manifest_invalid(
                "case-colliding sibling paths are forbidden",
            ));
        }
        if !stable_ids.insert(file.stable_id) {
            return Err(MirageError::manifest_invalid(
                "duplicate stable file identifier",
            ));
        }
        if file.logical_size.as_u64() > MAX_FILE_BYTES {
            return Err(MirageError::manifest_invalid(
                "file logical length exceeds declared bound",
            ));
        }
        let path_length = logical_directory_length(manifest, parent)?
            .checked_add(file.name.len() + 1)
            .ok_or_else(|| MirageError::manifest_invalid("logical file path length overflows"))?;
        if path_length > MAX_LOGICAL_PATH_BYTES {
            return Err(MirageError::manifest_invalid(
                "logical file path exceeds declared bound",
            ));
        }
        if file.class.is_virtual() && has_executable_extension(&file.name) {
            return Err(MirageError::manifest_invalid(
                "executable-class extension cannot be virtualized by default",
            ));
        }
        validate_file_extents(manifest, file, page_size)?;
    }
    Ok(())
}

fn validate_file_extents(
    manifest: &RepositoryManifest,
    file: &crate::model::FileRecord,
    page_size: u64,
) -> Result<(), MirageError> {
    let start = usize::try_from(file.extent_start)
        .map_err(|_| MirageError::manifest_invalid("file extent start overflows"))?;
    let count = usize::try_from(file.extent_count)
        .map_err(|_| MirageError::manifest_invalid("file extent count overflows"))?;
    let end = start
        .checked_add(count)
        .ok_or_else(|| MirageError::manifest_invalid("file extent slice overflows"))?;
    if end > manifest.extents.len() {
        return Err(MirageError::manifest_invalid(
            "file extent slice is out of bounds",
        ));
    }
    if !file.class.is_virtual() {
        if count != 0 {
            return Err(MirageError::manifest_invalid(
                "native files cannot reference virtual page extents",
            ));
        }
        return Ok(());
    }
    if file.logical_size.is_zero() {
        if count != 0 {
            return Err(MirageError::manifest_invalid(
                "empty virtual file must not contain extents",
            ));
        }
        return Ok(());
    }
    if count == 0 {
        return Err(MirageError::manifest_invalid(
            "non-empty virtual file has no extents",
        ));
    }
    let mut expected_offset = 0u64;
    for extent in &manifest.extents[start..end] {
        if extent.logical_offset != expected_offset || extent.logical_length.is_zero() {
            return Err(MirageError::manifest_invalid(
                "file extents are gapped, overlapping, unsorted, or empty",
            ));
        }
        let page_start = usize::try_from(extent.page_start)
            .map_err(|_| MirageError::manifest_invalid("extent page start overflows"))?;
        let page_count = usize::try_from(extent.page_count)
            .map_err(|_| MirageError::manifest_invalid("extent page count overflows"))?;
        let page_end = page_start
            .checked_add(page_count)
            .ok_or_else(|| MirageError::manifest_invalid("extent page slice overflows"))?;
        if page_count == 0 || page_end > manifest.pages.len() {
            return Err(MirageError::manifest_invalid(
                "extent page slice is empty or out of bounds",
            ));
        }
        let mut page_bytes = 0u64;
        for (position, page) in manifest.pages[page_start..page_end].iter().enumerate() {
            if position + 1 != page_count && u64::from(page.logical_length) != page_size {
                return Err(MirageError::manifest_invalid(
                    "only the final page in an extent may be short",
                ));
            }
            page_bytes = page_bytes
                .checked_add(u64::from(page.logical_length))
                .ok_or_else(|| MirageError::manifest_invalid("extent page length overflows"))?;
        }
        if page_bytes != extent.logical_length.as_u64() {
            return Err(MirageError::manifest_invalid(
                "extent logical length does not equal its page lengths",
            ));
        }
        expected_offset = expected_offset
            .checked_add(extent.logical_length.as_u64())
            .ok_or_else(|| MirageError::manifest_invalid("file extent length overflows"))?;
    }
    if expected_offset != file.logical_size.as_u64() {
        return Err(MirageError::manifest_invalid(
            "file extents do not cover the exact logical file length",
        ));
    }
    Ok(())
}

fn has_executable_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        EXECUTABLE_EXTENSIONS
            .iter()
            .any(|item| extension.eq_ignore_ascii_case(item))
    })
}

pub(crate) fn manifest_summary(
    manifest: &RepositoryManifest,
) -> Result<ManifestSummary, MirageError> {
    let mut total = ByteCount::ZERO;
    let mut native = ByteCount::ZERO;
    let mut virtual_bytes = ByteCount::ZERO;
    for file in &manifest.files {
        total = total.checked_add(file.logical_size).ok_or_else(|| {
            MirageError::manifest_invalid("manifest total logical bytes overflow")
        })?;
        if file.class.is_virtual() {
            virtual_bytes = virtual_bytes
                .checked_add(file.logical_size)
                .ok_or_else(|| {
                    MirageError::manifest_invalid("manifest virtual byte total overflows")
                })?;
        } else {
            native = native.checked_add(file.logical_size).ok_or_else(|| {
                MirageError::manifest_invalid("manifest native byte total overflows")
            })?;
        }
    }
    let unique: HashMap<PageHash, ()> = manifest
        .pages
        .iter()
        .map(|page| (page.plaintext_hash, ()))
        .collect();
    Ok(ManifestSummary {
        total_logical_bytes: total,
        unique_page_count: unique.len() as u64,
        native_logical_bytes: native,
        virtual_logical_bytes: virtual_bytes,
    })
}
