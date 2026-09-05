use std::path::{Path, PathBuf};

use mirage_backend::ObjectBackend;
use mirage_cache::{ArenaShard, CacheLayout};
use mirage_manifest::CommitVerifier;
use mirage_types::{ByteCount, MirageError, RepositoryId};

use crate::{RecoveredRepository, recover_repository};

#[derive(Debug)]
pub struct FreshRestore {
    pub repository: RecoveredRepository,
    pub index_path: PathBuf,
    pub arena_path: PathBuf,
    pub required_native_files: Vec<PathBuf>,
}

pub async fn restore_fresh(
    backend: &dyn ObjectBackend,
    repository_id: RepositoryId,
    verifier: &dyn CommitVerifier,
    state_root: &Path,
    cache_slots: u32,
) -> Result<FreshRestore, MirageError> {
    if state_root.exists()
        && state_root
            .read_dir()
            .map_err(MirageError::from)?
            .next()
            .is_some()
    {
        return Err(MirageError::invalid_argument(
            "fresh restore root is not empty",
        ));
    }
    std::fs::create_dir_all(state_root).map_err(MirageError::from)?;
    let repository = recover_repository(backend, repository_id, verifier).await?;
    let index_path = state_root.join(format!("{}.midx", repository.manifest.generation_id));
    mirage_index::compile_to_path(&repository.manifest, &index_path)?;
    let arena_path = state_root.join("cache-000.mca");
    ArenaShard::create(
        &arena_path,
        CacheLayout {
            page_size: repository.manifest.page_size,
            slot_count: cache_slots,
            db_journal_allowance: ByteCount::from_u64(64 * 1024 * 1024),
            filesystem_reserve: ByteCount::from_u64(256 * 1024 * 1024),
        },
    )?;
    let required_native_files = native_paths(&repository.manifest)?;
    Ok(FreshRestore {
        repository,
        index_path,
        arena_path,
        required_native_files,
    })
}

fn native_paths(
    manifest: &mirage_manifest::RepositoryManifest,
) -> Result<Vec<PathBuf>, MirageError> {
    let mut directory_paths: Vec<PathBuf> = Vec::with_capacity(manifest.directories.len());
    for (index, directory) in manifest.directories.iter().enumerate() {
        let path = match directory.parent {
            None => PathBuf::new(),
            Some(parent) if (parent as usize) < index => {
                directory_paths[parent as usize].join(&directory.name)
            }
            _ => {
                return Err(MirageError::manifest_invalid(
                    "native shell directory graph is invalid",
                ));
            }
        };
        directory_paths.push(path);
    }
    let mut files = manifest
        .files
        .iter()
        .filter(|file| !file.class.is_virtual())
        .map(|file| directory_paths[file.parent_directory as usize].join(&file.name))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    Ok(files)
}
