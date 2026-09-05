use std::collections::HashMap;
use std::path::{Component, Path};

use mirage_backend::RemoteObjectRef;
use mirage_types::{ByteCount, GenerationId, MirageError, PageHash, RepositoryId, StableFileId};

use crate::{
    Codec, DirectoryRecord, ExtentRecord, FileClass, FileRecord, MANIFEST_FORMAT_VERSION,
    PageRecord, RemoteLocation, RepositoryManifest, validate_manifest,
};

#[derive(Debug, Clone)]
pub struct ManifestPage {
    pub hash: PageHash,
    pub logical_length: u32,
    pub object: RemoteObjectRef,
    pub offset: u64,
    pub encoded_length: ByteCount,
    pub codec: Codec,
}

#[derive(Debug, Clone)]
pub struct ManifestFile {
    pub relative_path: String,
    pub logical_size: ByteCount,
    pub class: FileClass,
    pub pages: Vec<ManifestPage>,
}

#[derive(Debug, Clone, Copy)]
pub struct ManifestBuilder {
    repository_id: RepositoryId,
    generation_id: GenerationId,
    page_size: ByteCount,
}

impl ManifestBuilder {
    #[must_use]
    pub const fn new(
        repository_id: RepositoryId,
        generation_id: GenerationId,
        page_size: ByteCount,
    ) -> Self {
        Self {
            repository_id,
            generation_id,
            page_size,
        }
    }

    pub fn build(
        self,
        mut input_files: Vec<ManifestFile>,
    ) -> Result<RepositoryManifest, MirageError> {
        input_files.sort_by_key(|file| file.relative_path.to_ascii_lowercase());
        let mut directories = vec![DirectoryRecord {
            parent: None,
            name: String::new(),
        }];
        let mut directory_map: HashMap<String, u32> = HashMap::from([(String::new(), 0)]);
        let mut files = Vec::with_capacity(input_files.len());
        let mut extents = Vec::new();
        let mut pages = Vec::new();
        let mut remote_locations = Vec::new();
        for input in input_files {
            let normalized = normalize_relative(&input.relative_path)?;
            let path = Path::new(&normalized);
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    MirageError::manifest_invalid("manifest file path has no valid name")
                })?;
            let parent_text = path.parent().map_or(String::new(), |value| {
                value.to_string_lossy().replace('\\', "/")
            });
            let parent_directory =
                ensure_directories(&parent_text, &mut directories, &mut directory_map)?;
            let extent_start = u32::try_from(extents.len())
                .map_err(|_| MirageError::manifest_invalid("extent index overflows"))?;
            let page_start = u32::try_from(pages.len())
                .map_err(|_| MirageError::manifest_invalid("page index overflows"))?;
            let input_page_count = input.pages.len();
            let mut page_bytes = 0_u64;
            for page in input.pages {
                let location = u32::try_from(remote_locations.len())
                    .map_err(|_| MirageError::manifest_invalid("location index overflows"))?;
                remote_locations.push(RemoteLocation {
                    object: page.object,
                    offset: page.offset,
                    encoded_length: page.encoded_length,
                    codec: page.codec,
                });
                pages.push(PageRecord {
                    plaintext_hash: page.hash,
                    logical_length: page.logical_length,
                    remote_location: location,
                });
                page_bytes = page_bytes
                    .checked_add(u64::from(page.logical_length))
                    .ok_or_else(|| MirageError::manifest_invalid("file page bytes overflow"))?;
            }
            let extent_count = if input.class.is_virtual() && !input.logical_size.is_zero() {
                if page_bytes != input.logical_size.as_u64() {
                    return Err(MirageError::manifest_invalid(
                        "virtual file pages do not reconstruct its declared length",
                    ));
                }
                extents.push(ExtentRecord {
                    logical_offset: 0,
                    logical_length: input.logical_size,
                    page_start,
                    page_count: u32::try_from(pages.len())
                        .map_err(|_| MirageError::manifest_invalid("page count overflows"))?
                        - page_start,
                });
                1
            } else {
                if input_page_count != 0 {
                    return Err(MirageError::manifest_invalid(
                        "native or empty files cannot carry virtual pages",
                    ));
                }
                0
            };
            files.push(FileRecord {
                parent_directory,
                name: name.to_string(),
                logical_size: input.logical_size,
                stable_id: stable_id(&normalized),
                class: input.class,
                extent_start,
                extent_count,
            });
        }
        let manifest = RepositoryManifest {
            format_version: MANIFEST_FORMAT_VERSION,
            repository_id: self.repository_id,
            generation_id: self.generation_id,
            page_size: self.page_size,
            directories,
            files,
            extents,
            pages,
            remote_locations,
        };
        validate_manifest(&manifest)?;
        Ok(manifest)
    }
}

fn normalize_relative(value: &str) -> Result<String, MirageError> {
    let replaced = value.replace('\\', "/");
    let path = Path::new(&replaced);
    if replaced.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(MirageError::manifest_invalid(
            "manifest path is not a safe relative path",
        ));
    }
    Ok(replaced)
}

fn ensure_directories(
    parent: &str,
    directories: &mut Vec<DirectoryRecord>,
    map: &mut HashMap<String, u32>,
) -> Result<u32, MirageError> {
    if let Some(index) = map.get(parent) {
        return Ok(*index);
    }
    let mut current = String::new();
    let mut parent_index = 0_u32;
    for component in parent.split('/').filter(|part| !part.is_empty()) {
        current = if current.is_empty() {
            component.to_string()
        } else {
            format!("{current}/{component}")
        };
        if let Some(index) = map.get(&current) {
            parent_index = *index;
            continue;
        }
        let index = u32::try_from(directories.len())
            .map_err(|_| MirageError::manifest_invalid("directory index overflows"))?;
        directories.push(DirectoryRecord {
            parent: Some(parent_index),
            name: component.to_string(),
        });
        map.insert(current.clone(), index);
        parent_index = index;
    }
    Ok(parent_index)
}

fn stable_id(path: &str) -> StableFileId {
    let hash = blake3::hash(path.to_ascii_lowercase().as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    StableFileId::from_u64(u64::from_le_bytes(bytes))
}
