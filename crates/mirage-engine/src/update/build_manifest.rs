use super::StagedPageMapping;
use mirage_manifest::{ManifestBuilder, ManifestFile, ManifestPage, RepositoryManifest};
use mirage_types::{ByteCount, GenerationId, MirageError, StableFileId};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Default)]
pub struct ManifestOverlay {
    pub page_mappings: Vec<StagedPageMapping>,
    pub size_overrides: BTreeMap<StableFileId, u64>,
    pub deleted_files: BTreeSet<StableFileId>,
    pub created_files: Vec<ManifestFile>,
}
pub fn build_updated_manifest(
    base: &RepositoryManifest,
    target: GenerationId,
    overlay: ManifestOverlay,
) -> Result<RepositoryManifest, MirageError> {
    if target.0 <= base.generation_id.0 {
        return Err(MirageError::invalid_argument(
            "update target generation must advance",
        ));
    }
    let mapping: BTreeMap<_, _> = overlay
        .page_mappings
        .into_iter()
        .map(|m| ((m.file_id, m.page_index), m))
        .collect();
    let directory_paths = directory_paths(base)?;
    let mut files = Vec::new();
    for file in &base.files {
        if overlay.deleted_files.contains(&file.stable_id) {
            continue;
        }
        let parent = &directory_paths[file.parent_directory as usize];
        let relative = if parent.is_empty() {
            file.name.clone()
        } else {
            format!("{parent}/{}", file.name)
        };
        let logical_size = ByteCount::from_u64(
            overlay
                .size_overrides
                .get(&file.stable_id)
                .copied()
                .unwrap_or(file.logical_size.as_u64()),
        );
        let mut pages = Vec::new();
        if file.class.is_virtual() && file.extent_count > 0 {
            if file.extent_count != 1 {
                return Err(MirageError::unsupported_layout(
                    "updated multi-extent files are not supported",
                ));
            }
            let extent = &base.extents[file.extent_start as usize];
            for relative_page in 0..extent.page_count {
                let base_page = &base.pages[(extent.page_start + relative_page) as usize];
                if let Some(staged) = mapping.get(&(file.stable_id, relative_page)) {
                    pages.push(ManifestPage {
                        hash: staged.page_hash,
                        logical_length: staged.logical_length,
                        object: staged.object.clone(),
                        offset: staged.frame_offset,
                        encoded_length: ByteCount::from_u64(staged.frame_length),
                        codec: staged.codec,
                    });
                } else {
                    let location = &base.remote_locations[base_page.remote_location as usize];
                    pages.push(ManifestPage {
                        hash: base_page.plaintext_hash,
                        logical_length: base_page.logical_length,
                        object: location.object.clone(),
                        offset: location.offset,
                        encoded_length: location.encoded_length,
                        codec: location.codec,
                    });
                }
            }
        }
        files.push(ManifestFile {
            relative_path: relative,
            logical_size,
            class: file.class,
            pages,
        });
    }
    files.extend(overlay.created_files);
    ManifestBuilder::new(base.repository_id, target, base.page_size).build(files)
}
fn directory_paths(manifest: &RepositoryManifest) -> Result<Vec<String>, MirageError> {
    let mut paths = Vec::with_capacity(manifest.directories.len());
    for (index, directory) in manifest.directories.iter().enumerate() {
        let value = match directory.parent {
            None => String::new(),
            Some(parent) if (parent as usize) < index => {
                let prefix: &String = &paths[parent as usize];
                if prefix.is_empty() {
                    directory.name.clone()
                } else {
                    format!("{prefix}/{}", directory.name)
                }
            }
            _ => {
                return Err(MirageError::manifest_invalid(
                    "manifest directory graph is invalid",
                ));
            }
        };
        paths.push(value);
    }
    Ok(paths)
}
