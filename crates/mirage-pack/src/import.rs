use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use mirage_backend::{BackendId, ImmutableRevision, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_manifest::{
    Codec, FileClass, ManifestBuilder, ManifestFile, ManifestPage, RepositoryManifest,
    encode_manifest,
};
use mirage_types::{ByteCount, GenerationId, MirageError, PageHash, RepositoryId};
use serde::{Deserialize, Serialize};

use crate::index::PackEntry;
use crate::pager::page_path;
use crate::reader::PackReader;
use crate::writer::{CompletedPack, PackEncryption, PackWriter, PackWriterOptions};

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub relative_path: String,
    pub class: FileClass,
}

#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub source_root: PathBuf,
    pub files: Vec<PlannedFile>,
    pub page_size: u32,
    pub pack_target: u64,
    pub output_staging_directory: PathBuf,
    pub encryption: Option<PackEncryption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReport {
    pub report_version: u32,
    pub logical_bytes: u64,
    pub virtual_bytes: u64,
    pub unique_page_bytes: u64,
    pub unique_pages: u64,
    pub reused_pages: u64,
    pub pack_count: u64,
    pub source_change_failures: u64,
    pub native_file_count: u64,
    pub virtual_file_count: u64,
}

#[derive(Debug, Clone)]
pub struct ImportedRepository {
    pub manifest: RepositoryManifest,
    pub manifest_path: PathBuf,
    pub packs: Vec<CompletedPack>,
    pub report: ImportReport,
}

pub fn import_local(plan: &ImportPlan) -> Result<ImportedRepository, MirageError> {
    import_local_with_cancel(plan, || false)
}

pub fn import_local_with_cancel(
    plan: &ImportPlan,
    mut cancelled: impl FnMut() -> bool,
) -> Result<ImportedRepository, MirageError> {
    validate_plan(plan)?;
    std::fs::create_dir_all(&plan.output_staging_directory).map_err(MirageError::from)?;
    let mut packs = load_verified_packs(&plan.output_staging_directory, plan.encryption.is_some())?;
    let mut locations: HashMap<PageHash, (usize, PackEntry)> = HashMap::new();
    for (pack_index, pack) in packs.iter().enumerate() {
        for entry in &pack.entries {
            locations
                .entry(entry.page_hash)
                .or_insert((pack_index, *entry));
        }
    }
    let mut writer: Option<PackWriter> = None;
    let mut file_pages: HashMap<String, Vec<PageHash>> = HashMap::new();
    let mut logical_bytes = 0_u64;
    let mut virtual_bytes = 0_u64;
    let mut reused_pages = 0_u64;
    let mut native_file_count = 0_u64;
    let mut virtual_file_count = 0_u64;
    let mut sizes = HashMap::new();
    for file in &plan.files {
        if cancelled() {
            if let Some(active) = writer.take() {
                active.abort()?;
            }
            return Err(MirageError::invalid_argument("local import was cancelled"));
        }
        let source = safe_source_path(&plan.source_root, &file.relative_path)?;
        let metadata = std::fs::metadata(&source).map_err(MirageError::from)?;
        if !metadata.is_file() {
            return Err(MirageError::unsupported_layout(
                "import input is not a regular file",
            ));
        }
        logical_bytes = logical_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| MirageError::invalid_argument("import byte count overflows"))?;
        sizes.insert(file.relative_path.clone(), metadata.len());
        if !file.class.is_virtual() {
            native_file_count += 1;
            continue;
        }
        virtual_file_count += 1;
        virtual_bytes = virtual_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| MirageError::invalid_argument("virtual byte count overflows"))?;
        let pages = page_path(&source, plan.page_size)?;
        let mut sequence = Vec::with_capacity(pages.len());
        for page in pages {
            sequence.push(page.hash);
            if locations.contains_key(&page.hash)
                || writer
                    .as_ref()
                    .is_some_and(|active| active.contains_page(page.hash))
            {
                reused_pages += 1;
                if let Some(active) = writer.as_mut()
                    && active.contains_page(page.hash)
                {
                    active.append_page(&page)?;
                }
                continue;
            }
            let should_rollover = writer
                .as_ref()
                .map(|active| active.would_rollover(&page))
                .transpose()?
                .unwrap_or(false);
            if should_rollover {
                let complete = writer.take().expect("writer exists").finish()?;
                let pack_index = packs.len();
                for entry in &complete.entries {
                    locations.insert(entry.page_hash, (pack_index, *entry));
                }
                packs.push(complete);
            }
            if writer.is_none() {
                let options = PackWriterOptions {
                    page_size: plan.page_size,
                    target_size: plan.pack_target,
                    align_frames_4k: true,
                };
                writer = Some(match &plan.encryption {
                    Some(encryption) => PackWriter::create_encrypted(
                        &plan.output_staging_directory,
                        options,
                        encryption.clone(),
                    )?,
                    None => PackWriter::create(&plan.output_staging_directory, options)?,
                });
            }
            writer
                .as_mut()
                .expect("writer initialized")
                .append_page(&page)?;
        }
        file_pages.insert(file.relative_path.clone(), sequence);
    }
    if let Some(active) = writer {
        let complete = active.finish()?;
        let pack_index = packs.len();
        for entry in &complete.entries {
            locations.insert(entry.page_hash, (pack_index, *entry));
        }
        packs.push(complete);
    }
    let manifest_files = build_manifest_files(plan, &packs, &locations, &file_pages, &sizes)?;
    let manifest = ManifestBuilder::new(
        plan.repository_id,
        plan.generation_id,
        ByteCount::from_u64(u64::from(plan.page_size)),
    )
    .build(manifest_files)?;
    let manifest_path = plan.output_staging_directory.join("base-manifest.cbor");
    write_atomic(&manifest_path, &encode_manifest(&manifest)?)?;
    let unique: HashSet<PageHash> = locations.keys().copied().collect();
    let unique_page_bytes = unique.iter().try_fold(0_u64, |total, hash| {
        let (_, entry) = locations[hash];
        total
            .checked_add(u64::from(entry.logical_length))
            .ok_or_else(|| MirageError::invalid_argument("unique byte count overflows"))
    })?;
    let report = ImportReport {
        report_version: 1,
        logical_bytes,
        virtual_bytes,
        unique_page_bytes,
        unique_pages: unique.len() as u64,
        reused_pages,
        pack_count: packs.len() as u64,
        source_change_failures: 0,
        native_file_count,
        virtual_file_count,
    };
    let report_path = plan.output_staging_directory.join("import-report.json");
    write_atomic(
        &report_path,
        &serde_json::to_vec_pretty(&report).map_err(|error| {
            MirageError::internal_invariant("import report serialization failed").with_source(error)
        })?,
    )?;
    Ok(ImportedRepository {
        manifest,
        manifest_path,
        packs,
        report,
    })
}

fn build_manifest_files(
    plan: &ImportPlan,
    packs: &[CompletedPack],
    locations: &HashMap<PageHash, (usize, PackEntry)>,
    file_pages: &HashMap<String, Vec<PageHash>>,
    sizes: &HashMap<String, u64>,
) -> Result<Vec<ManifestFile>, MirageError> {
    let backend_id = BackendId::new("local")?;
    plan.files
        .iter()
        .map(|file| {
            let pages = file_pages
                .get(&file.relative_path)
                .into_iter()
                .flatten()
                .map(|hash| {
                    let (pack_index, entry) = locations.get(hash).ok_or_else(|| {
                        MirageError::internal_invariant("import page location disappeared")
                    })?;
                    let pack = &packs[*pack_index];
                    let file_name = pack
                        .path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .ok_or_else(|| {
                            MirageError::internal_invariant("pack path has no file name")
                        })?;
                    Ok(ManifestPage {
                        hash: *hash,
                        logical_length: entry.logical_length,
                        object: RemoteObjectRef {
                            backend_id: backend_id.clone(),
                            provider_object_id: ProviderObjectId::new(file_name)?,
                            immutable_revision: Some(ImmutableRevision::new(
                                pack.content_hash.to_string(),
                            )?),
                            byte_length: ByteCount::from_u64(pack.byte_length),
                            content_hash: pack.content_hash,
                            kind: ObjectKind::Pack,
                        },
                        offset: entry.frame_offset,
                        encoded_length: ByteCount::from_u64(entry.frame_length),
                        codec: Codec::None,
                    })
                })
                .collect::<Result<Vec<_>, MirageError>>()?;
            Ok(ManifestFile {
                relative_path: file.relative_path.clone(),
                logical_size: ByteCount::from_u64(*sizes.get(&file.relative_path).ok_or_else(
                    || MirageError::internal_invariant("import file size disappeared"),
                )?),
                class: file.class,
                pages,
            })
        })
        .collect()
}

fn validate_plan(plan: &ImportPlan) -> Result<(), MirageError> {
    if plan.files.is_empty() {
        return Err(MirageError::invalid_argument(
            "import plan contains no files",
        ));
    }
    let root = plan.source_root.canonicalize().map_err(MirageError::from)?;
    let output_parent = plan
        .output_staging_directory
        .parent()
        .unwrap_or(Path::new("."));
    let output_parent = output_parent.canonicalize().map_err(MirageError::from)?;
    if output_parent.starts_with(&root) {
        return Err(MirageError::invalid_argument(
            "import output must be outside the source tree",
        ));
    }
    let mut paths = HashSet::new();
    for file in &plan.files {
        let folded = file.relative_path.replace('\\', "/").to_ascii_lowercase();
        if !paths.insert(folded) {
            return Err(MirageError::invalid_argument(
                "import plan has a duplicate or case-colliding path",
            ));
        }
    }
    Ok(())
}

fn safe_source_path(root: &Path, relative: &str) -> Result<PathBuf, MirageError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(MirageError::invalid_argument(
            "import path is not safely relative",
        ));
    }
    let path = root.join(relative_path);
    let canonical = path.canonicalize().map_err(MirageError::from)?;
    let canonical_root = root.canonicalize().map_err(MirageError::from)?;
    if !canonical.starts_with(canonical_root) {
        return Err(MirageError::invalid_argument(
            "import path escapes its source root",
        ));
    }
    Ok(canonical)
}

fn load_verified_packs(
    directory: &Path,
    encrypted: bool,
) -> Result<Vec<CompletedPack>, MirageError> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(MirageError::from)? {
        let path = entry.map_err(MirageError::from)?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("pack-") && name.ends_with(".bin"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let reader = PackReader::open_verified(&path)?;
            if reader.is_encrypted() != encrypted {
                return Err(MirageError::repository_conflict(
                    "import directory mixes encrypted and unencrypted packs",
                ));
            }
            Ok(CompletedPack {
                path,
                content_hash: reader.content_hash(),
                byte_length: reader.file_length(),
                entries: reader.entries().to_vec(),
            })
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MirageError> {
    mirage_crypto::durable_file::write_atomic(path, bytes)
}
