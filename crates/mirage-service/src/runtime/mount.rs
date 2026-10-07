//! Mount readiness validation and persisted mount records.

use super::conversion::reconcile_conversion_intent;
use super::drive::DRIVE_MANIFEST;
use super::fs_util::canonical_absent_target;
use super::fs_util::canonical_directory;
use super::fs_util::ensure_empty_directory;
use super::fs_util::read_json_bounded;
use super::*;

pub fn validate_mount_ready(
    database: &Database,
    repository_id: RepositoryId,
    database_mount_root: &Path,
) -> Result<(), MirageError> {
    let runtime_path = repository_state_root(database, repository_id)?.join("runtime.json");
    if !runtime_path.exists() {
        return Err(MirageError::integrity_mismatch(
            "repository runtime configuration is missing",
        ));
    }
    let mut config = load_config(database, repository_id)?;
    reconcile_conversion_intent(database, repository_id, &mut config)?;
    if config.conversion.is_none() {
        return Err(MirageError::repository_conflict(
            "mount subtree is not converted; run repo convert and review its dry-run first",
        ));
    }
    let configured_mount_root = config.native_root.join(&config.mount_subtree);
    if configured_mount_root.exists() {
        ensure_empty_directory(&configured_mount_root, "converted mount directory")?;
        std::fs::remove_dir(&configured_mount_root).map_err(MirageError::from)?;
    }
    let expected = canonical_absent_target(&configured_mount_root, "configured mount path")?;
    let actual = canonical_absent_target(database_mount_root, "database mount path")?;
    if expected != actual {
        return Err(MirageError::integrity_mismatch(
            "registered mount root differs from runtime configuration",
        ));
    }
    if actual.exists() {
        return Err(MirageError::repository_conflict(
            "converted mount path must be absent before WinFsp creates it",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountRecord {
    pub(super) format_version: u32,
    pub repository_id: RepositoryId,
    pub generation: GenerationId,
    pub mount_point: PathBuf,
    pub explorer_visible: bool,
}

pub(super) fn recover_to_mounted(
    database: &Database,
    repository_id: RepositoryId,
    state: RepositoryState,
) {
    if database
        .set_repository_state(
            repository_id,
            state,
            RepositoryEvent::RecoveryRequested,
            now_ns(),
        )
        .is_ok()
    {
        let _ = database.set_repository_state(
            repository_id,
            RepositoryState::Recovering,
            RepositoryEvent::RecoverySucceededMounted,
            now_ns(),
        );
    }
}

pub fn validate_explorer_mount_ready(
    database: &Database,
    repository_id: RepositoryId,
    index_path: &Path,
    drive_letter: &str,
) -> Result<(PathBuf, u64), MirageError> {
    let value = drive_letter.trim_end_matches(':');
    let bytes = value.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii_alphabetic() {
        return Err(MirageError::invalid_argument(
            "drive letter must be one ASCII letter, for example M",
        ));
    }
    let letter = (bytes[0] as char).to_ascii_uppercase();
    let mount_point = PathBuf::from(format!("{letter}:"));
    let root = PathBuf::from(format!("{letter}:\\"));
    match std::fs::metadata(&root) {
        Ok(_) => {
            return Err(MirageError::repository_conflict(
                "requested Explorer drive letter is already in use",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(MirageError::provider_unavailable(
                "requested Explorer drive letter could not be inspected",
            )
            .with_source(error));
        }
    }
    let index = MountIndex::open(index_path)?;
    let mut hashes = BTreeSet::new();
    for ordinal in 0..index.page_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 pages"))?;
        hashes.insert(index.page_by_ordinal(ordinal)?.plaintext_hash());
    }
    let config = load_config(database, repository_id)?;
    // A managed Drive volume fetches non-resident pages on demand, so the
    // Explorer gate only needs the provider inputs (manifest + key), not
    // upfront residency. The mount path provisions the provider's empty
    // arena before starting the host. Legacy mounts still require the shard
    // and full verification here.
    let on_demand = database.load_repository_volume_mode(repository_id)?
        == Some(mirage_db::VolumeMode::Managed)
        && config.origin == RuntimeOrigin::Drive
        && config.import_root.join(DRIVE_MANIFEST).is_file()
        && config.import_root.join("repository-key.dpapi").is_file();
    if !on_demand {
        let shards = database.load_cache_shards()?;
        let [spec] = shards.as_slice() else {
            return Err(MirageError::unsupported_layout(
                "Explorer volume requires exactly one local cache shard",
            ));
        };
        if spec.shard_id != 0 {
            return Err(MirageError::unsupported_layout(
                "Explorer volume requires cache shard zero",
            ));
        }
        let layout = CacheLayout {
            page_size: spec.page_size,
            slot_count: spec.slot_count,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        };
        let shard = Arc::new(ArenaShard::open(
            &service_state_root(database)?
                .join("cache")
                .join(&spec.relative_path),
            layout,
        )?);
        let resident = ResidentIndex::rebuild(database, shard)?;
        for hash in &hashes {
            if verify_page(&resident, database, *hash, IntegrityClass::Clean)?
                != VerifyOutcome::Verified
            {
                return Err(MirageError::repository_conflict(
                    "Explorer volume requires every repository page to be materialized and verified",
                ));
            }
        }
    }
    if config.origin == RuntimeOrigin::Drive && !config.import_root.join(DRIVE_MANIFEST).is_file() {
        return Err(MirageError::integrity_mismatch(
            "Drive publication metadata is unavailable",
        ));
    }
    Ok((mount_point, hashes.len() as u64))
}

pub fn save_mount_record(
    database: &Database,
    repository_id: RepositoryId,
    generation: GenerationId,
    mount_point: &Path,
    explorer_visible: bool,
) -> Result<(), MirageError> {
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("active-mount.json"),
        &MountRecord {
            format_version: 1,
            repository_id,
            generation,
            mount_point: mount_point.to_path_buf(),
            explorer_visible,
        },
    )
}

pub fn load_mount_record(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Option<MountRecord>, MirageError> {
    let path = repository_state_root(database, repository_id)?.join("active-mount.json");
    if !path.exists() {
        return Ok(None);
    }
    let record: MountRecord = read_json_bounded(&path, 64 * 1024, "active mount record")?;
    if record.format_version != 1 || record.repository_id != repository_id {
        return Err(MirageError::integrity_mismatch(
            "active mount record identity is invalid",
        ));
    }
    Ok(Some(record))
}

pub fn clear_mount_record(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    let path = repository_state_root(database, repository_id)?.join("active-mount.json");
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MirageError::from(error)),
    }
}

pub fn mount_target_exists(path: &Path) -> bool {
    let value = path.to_string_lossy();
    let bytes = value.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return PathBuf::from(format!("{}:\\", bytes[0] as char)).exists();
    }
    path.exists()
}

pub(super) fn validate_mount_subtree(
    native_root: &Path,
    relative: &Path,
) -> Result<(PathBuf, PathBuf), MirageError> {
    let normalized = if relative == Path::new(".") {
        PathBuf::from(".")
    } else {
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || relative
                .components()
                .filter_map(|component| match component {
                    Component::Normal(value) => value.to_str(),
                    _ => None,
                })
                .any(|component| component.contains(':'))
        {
            return Err(MirageError::invalid_argument(
                "mount subtree must be a normal native-root-relative directory without ADS syntax",
            ));
        }
        relative.to_path_buf()
    };
    let candidate = native_root.join(&normalized);
    let canonical = canonical_directory(&candidate, "mount subtree")?;
    if !canonical.starts_with(native_root) {
        return Err(MirageError::invalid_argument(
            "mount subtree escapes the native game root",
        ));
    }
    Ok((normalized, canonical))
}

pub(super) fn require_unmounted(
    database: &Database,
    repository_id: RepositoryId,
    operation: &str,
) -> Result<(), MirageError> {
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    if state != RepositoryState::ReadyUnmounted {
        return Err(MirageError::repository_conflict(format!(
            "{operation} requires a ready, unmounted repository"
        )));
    }
    Ok(())
}
