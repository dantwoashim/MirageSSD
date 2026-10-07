//! Drive conversion, native restoration, and backup reconciliation.

use super::config::ConversionRecord;
use super::config::save_config;
use super::drive::active_generation;
use super::fs_util::ensure_empty_directory;
use super::fs_util::ensure_no_reparse;
use super::fs_util::ensure_regular_no_reparse;
use super::fs_util::is_windows_reparse;
use super::fs_util::read_json_bounded;
use super::fs_util::validate_single_component;
use super::mount::require_unmounted;
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum NativeBackupResidency {
    #[default]
    Resident,
    Evicted,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeBackupInfo {
    pub(crate) path: PathBuf,
    pub(crate) inventory_blake3: String,
    pub(crate) file_count: u64,
    pub(crate) total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeBackupEvictionIntent {
    pub(super) format_version: u32,
    pub(super) repository_id: RepositoryId,
    pub(super) generation_id: GenerationId,
    pub(super) commit_hash: CommitHash,
    pub(super) backup: PathBuf,
    tombstone: PathBuf,
    pub(super) inventory_blake3: String,
    pub(super) file_count: u64,
    pub(super) total_bytes: u64,
    remote_verified_at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConversionIntentOperation {
    Convert,
    RestoreNative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConversionIntent {
    pub(super) format_version: u32,
    pub(super) repository_id: RepositoryId,
    pub(super) operation: ConversionIntentOperation,
    pub(super) source: PathBuf,
    pub(super) backup: PathBuf,
    pub(super) record: ConversionRecord,
}

pub fn convert(
    database: &Database,
    repository_id: RepositoryId,
    apply: bool,
) -> Result<Value, MirageError> {
    require_unmounted(database, repository_id, "conversion")?;
    let mut config = load_config(database, repository_id)?;
    reconcile_conversion_intent(database, repository_id, &mut config)?;
    if config.mount_subtree.as_os_str().is_empty() || config.mount_subtree == Path::new(".") {
        return Err(MirageError::unsupported_layout(
            "conversion requires an explicit immutable mount subtree",
        ));
    }
    if config.conversion.is_some() {
        return Err(MirageError::repository_conflict(
            "repository subtree is already converted",
        ));
    }
    let source = config.native_root.join(&config.mount_subtree);
    let backup = conversion_backup_path(&config, repository_id)?;
    if backup.exists() {
        return Err(MirageError::repository_conflict(
            "conversion backup path already exists; restore or inspect it before retrying",
        ));
    }
    let inventory = directory_inventory(&source)?;
    let actions = vec![
        format!(
            "rename {} to protected sibling backup",
            config.mount_subtree.display()
        ),
        format!(
            "leave the WinFsp-owned mount path absent at {}",
            config.mount_subtree.display()
        ),
        "retain every original byte until restore-native completes".to_owned(),
    ];
    if !apply {
        return Ok(json!({
            "repository_id": repository_id.to_string(),
            "dry_run": true,
            "mount_subtree": config.mount_subtree,
            "file_count": inventory.file_count,
            "total_bytes": inventory.total_bytes,
            "inventory_blake3": inventory.blake3,
            "actions": actions
        }));
    }

    let record = ConversionRecord {
        backup_root: backup.clone(),
        inventory_blake3: inventory.blake3.clone(),
        file_count: inventory.file_count,
        total_bytes: inventory.total_bytes,
        backup_residency: NativeBackupResidency::Resident,
    };
    let intent = ConversionIntent {
        format_version: 1,
        repository_id,
        operation: ConversionIntentOperation::Convert,
        source: source.clone(),
        backup: backup.clone(),
        record: record.clone(),
    };
    write_json_atomic(&conversion_intent_path(database, repository_id)?, &intent)?;
    std::fs::rename(&source, &backup).map_err(MirageError::from)?;
    config.conversion = Some(record);
    if let Err(error) = save_config(database, repository_id, &config) {
        let _ = std::fs::rename(&backup, &source);
        return Err(error);
    }
    remove_conversion_intent(database, repository_id)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "converted": true,
        "mount_subtree": config.mount_subtree,
        "file_count": inventory.file_count,
        "total_bytes": inventory.total_bytes,
        "inventory_blake3": inventory.blake3,
        "original_bytes_reclaimed": false,
        "actions": actions
    }))
}

pub fn restore_native(
    database: &Database,
    repository_id: RepositoryId,
    apply: bool,
) -> Result<Value, MirageError> {
    require_unmounted(database, repository_id, "native restoration")?;
    let mut config = load_config(database, repository_id)?;
    reconcile_conversion_intent(database, repository_id, &mut config)?;
    let record = config
        .conversion
        .clone()
        .ok_or_else(|| MirageError::repository_conflict("repository subtree is not converted"))?;
    if record.backup_residency == NativeBackupResidency::Evicted {
        return Err(MirageError::backend_unauthenticated(
            "native backup is cloud-only; authenticate Drive and run native activation before permanent restoration",
        ));
    }
    let mount_root = config.native_root.join(&config.mount_subtree);
    if mount_root.exists() {
        ensure_empty_directory(&mount_root, "converted mount directory")?;
    }
    let inventory = directory_inventory(&record.backup_root)?;
    if inventory.blake3 != record.inventory_blake3
        || inventory.file_count != record.file_count
        || inventory.total_bytes != record.total_bytes
    {
        return Err(MirageError::integrity_mismatch(
            "native backup inventory changed; restoration is blocked",
        ));
    }
    let actions = vec![
        format!(
            "remove the empty mount directory if WinFsp left one at {}",
            config.mount_subtree.display()
        ),
        "atomically rename the verified native backup into place".to_owned(),
        "retain repository metadata and remote objects".to_owned(),
    ];
    if !apply {
        return Ok(json!({
            "repository_id": repository_id.to_string(),
            "dry_run": true,
            "mount_subtree": config.mount_subtree,
            "file_count": inventory.file_count,
            "total_bytes": inventory.total_bytes,
            "inventory_blake3": inventory.blake3,
            "actions": actions
        }));
    }

    let intent = ConversionIntent {
        format_version: 1,
        repository_id,
        operation: ConversionIntentOperation::RestoreNative,
        source: mount_root.clone(),
        backup: record.backup_root.clone(),
        record: record.clone(),
    };
    write_json_atomic(&conversion_intent_path(database, repository_id)?, &intent)?;
    if mount_root.exists() {
        std::fs::remove_dir(&mount_root).map_err(MirageError::from)?;
    }
    if let Err(error) = std::fs::rename(&record.backup_root, &mount_root) {
        return Err(MirageError::from(error));
    }
    config.conversion = None;
    if let Err(error) = save_config(database, repository_id, &config) {
        let _ = std::fs::rename(&mount_root, &record.backup_root);
        return Err(error);
    }
    remove_conversion_intent(database, repository_id)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "restored_native": true,
        "mount_subtree": config.mount_subtree,
        "file_count": inventory.file_count,
        "total_bytes": inventory.total_bytes,
        "inventory_blake3": inventory.blake3
    }))
}

pub(crate) fn native_backup_info(config: &RuntimeConfig) -> Option<NativeBackupInfo> {
    let record = config.conversion.as_ref()?;
    (record.backup_residency == NativeBackupResidency::Resident).then(|| NativeBackupInfo {
        path: record.backup_root.clone(),
        inventory_blake3: record.inventory_blake3.clone(),
        file_count: record.file_count,
        total_bytes: record.total_bytes,
    })
}

pub(crate) fn evict_verified_native_backup(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    reconcile_native_backup_eviction(database, repository_id)?;
    let mut config = load_config(database, repository_id)?;
    if config.origin != RuntimeOrigin::Drive {
        return Err(MirageError::repository_conflict(
            "native backup eviction requires a verified Drive origin",
        ));
    }
    if database.load_repository_content_encrypted(repository_id)? == Some(true) {
        require_verified_recovery_envelope(&config, repository_id)?;
    }
    let record = config
        .conversion
        .as_mut()
        .ok_or_else(|| MirageError::repository_conflict("repository subtree is not converted"))?;
    if record.backup_residency != NativeBackupResidency::Resident {
        return Err(MirageError::cache_full(
            "native backup is no longer resident",
        ));
    }
    verify_inventory_record(&record.backup_root, record)?;
    let active = active_generation(database, repository_id)?;
    let tombstone = native_backup_tombstone(&record.backup_root, repository_id)?;
    if tombstone.exists() {
        return Err(MirageError::repository_conflict(
            "native backup eviction tombstone already exists",
        ));
    }
    let intent = NativeBackupEvictionIntent {
        format_version: 1,
        repository_id,
        generation_id: active.generation_id,
        commit_hash: active.commit_hash,
        backup: record.backup_root.clone(),
        tombstone: tombstone.clone(),
        inventory_blake3: record.inventory_blake3.clone(),
        file_count: record.file_count,
        total_bytes: record.total_bytes,
        remote_verified_at_ns: now_ns(),
    };
    write_json_atomic(
        &native_backup_intent_path(database, repository_id)?,
        &intent,
    )?;
    std::fs::rename(&intent.backup, &intent.tombstone).map_err(MirageError::from)?;
    record.backup_residency = NativeBackupResidency::Evicted;
    save_config(database, repository_id, &config)?;
    safe_remove_verified_tree(&intent.tombstone, &intent)?;
    remove_native_backup_intent(database, repository_id)
}

pub(crate) fn reconcile_native_backup_eviction(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    let path = native_backup_intent_path(database, repository_id)?;
    if !path.exists() {
        return Ok(());
    }
    let intent: NativeBackupEvictionIntent =
        read_json_bounded(&path, 1024 * 1024, "native backup eviction intent")?;
    let mut config = load_config(database, repository_id)?;
    let active = active_generation(database, repository_id)?;
    let expected_backup = conversion_backup_path(&config, repository_id)?;
    let expected_tombstone = native_backup_tombstone(&expected_backup, repository_id)?;
    if intent.format_version != 1
        || intent.repository_id != repository_id
        || intent.generation_id != active.generation_id
        || intent.commit_hash != active.commit_hash
        || intent.backup != expected_backup
        || intent.tombstone != expected_tombstone
        || intent.remote_verified_at_ns < 0
    {
        return Err(MirageError::integrity_mismatch(
            "native backup eviction intent identity is invalid",
        ));
    }
    let record = config
        .conversion
        .as_mut()
        .ok_or_else(|| MirageError::integrity_mismatch("eviction intent lost conversion state"))?;
    if record.inventory_blake3 != intent.inventory_blake3
        || record.file_count != intent.file_count
        || record.total_bytes != intent.total_bytes
    {
        return Err(MirageError::integrity_mismatch(
            "native backup eviction intent differs from conversion inventory",
        ));
    }
    match record.backup_residency {
        NativeBackupResidency::Resident => {
            match (intent.backup.exists(), intent.tombstone.exists()) {
                (true, false) => {}
                (false, true) => {
                    verify_eviction_intent_tree(&intent.tombstone, &intent)?;
                    std::fs::rename(&intent.tombstone, &intent.backup)
                        .map_err(MirageError::from)?;
                }
                _ => {
                    return Err(MirageError::integrity_mismatch(
                        "resident native backup eviction paths are contradictory",
                    ));
                }
            }
        }
        NativeBackupResidency::Evicted => match (intent.backup.exists(), intent.tombstone.exists())
        {
            (false, true) => safe_remove_verified_tree(&intent.tombstone, &intent)?,
            (false, false) => {}
            (true, false) => {
                verify_eviction_intent_tree(&intent.backup, &intent)?;
                record.backup_residency = NativeBackupResidency::Resident;
                save_config(database, repository_id, &config)?;
            }
            (true, true) => {
                return Err(MirageError::integrity_mismatch(
                    "evicted native backup exists in two locations",
                ));
            }
        },
    }
    remove_native_backup_intent(database, repository_id)
}

/// Reclaiming the last original bytes of an encrypted repository is only safe
/// after a portable recovery envelope was verified against the exact content
/// key this installation still holds. The record binds the key hash so a
/// rotated or replaced key cannot ride on a stale verification.
fn require_verified_recovery_envelope(
    config: &RuntimeConfig,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct VerifiedRecovery {
        format_version: u32,
        repository_id: RepositoryId,
        content_key_blake3: String,
        has_signer_authority: bool,
        envelope_sha256: String,
        verified_at_ns: i64,
    }
    let record_path = config.import_root.join("recovery-verified.json");
    if !record_path.exists() {
        return Err(MirageError::repository_conflict(
            "encrypted originals cannot be reclaimed until a complete recovery \
             envelope is verified: run 'mirage repo recovery verify --import <dir>'",
        ));
    }
    let record: VerifiedRecovery =
        read_json_bounded(&record_path, 64 * 1024, "recovery verification record")?;
    let _ = record.has_signer_authority;
    if record.format_version != 1
        || record.repository_id != repository_id
        || record.envelope_sha256.len() != 64
        || record.content_key_blake3.len() != 64
        || record.verified_at_ns <= 0
    {
        return Err(MirageError::integrity_mismatch(
            "recovery verification record is invalid",
        ));
    }
    let key = load_repository_key(
        &config.import_root.join("repository-key.dpapi"),
        repository_id,
    )?;
    if blake3::hash(key.secret_bytes().as_ref()).to_hex().as_str() != record.content_key_blake3 {
        return Err(MirageError::integrity_mismatch(
            "the verified recovery envelope covers a different content key",
        ));
    }
    Ok(())
}

fn native_backup_tombstone(
    backup: &Path,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    let parent = backup
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("native backup has no parent"))?;
    Ok(parent.join(format!(".miragessd-evicting-{repository_id}")))
}

fn native_backup_intent_path(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    Ok(repository_state_root(database, repository_id)?.join("native-backup-eviction.json"))
}

fn remove_native_backup_intent(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    match std::fs::remove_file(native_backup_intent_path(database, repository_id)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MirageError::from(error)),
    }
}

fn verify_eviction_intent_tree(
    path: &Path,
    intent: &NativeBackupEvictionIntent,
) -> Result<(), MirageError> {
    let inventory = directory_inventory(path)?;
    if inventory.blake3 != intent.inventory_blake3
        || inventory.file_count != intent.file_count
        || inventory.total_bytes != intent.total_bytes
    {
        return Err(MirageError::integrity_mismatch(
            "native backup changed during eviction",
        ));
    }
    Ok(())
}

fn safe_remove_verified_tree(
    path: &Path,
    intent: &NativeBackupEvictionIntent,
) -> Result<(), MirageError> {
    verify_eviction_intent_tree(path, intent)?;
    std::fs::remove_dir_all(path).map_err(MirageError::from)
}

pub(super) fn conversion_backup_path(
    config: &RuntimeConfig,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    let leaf = config
        .mount_subtree
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| MirageError::invalid_argument("mount subtree has no safe leaf name"))?;
    Ok(config
        .native_root
        .join(format!(".miragessd-native-{leaf}-{repository_id}")))
}

pub(super) fn conversion_intent_path(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    Ok(repository_state_root(database, repository_id)?.join("conversion-intent.json"))
}

fn remove_conversion_intent(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    let path = conversion_intent_path(database, repository_id)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MirageError::from(error)),
    }
}

pub(super) fn reconcile_conversion_intent(
    database: &Database,
    repository_id: RepositoryId,
    config: &mut RuntimeConfig,
) -> Result<(), MirageError> {
    let path = conversion_intent_path(database, repository_id)?;
    if !path.exists() {
        return Ok(());
    }
    let intent: ConversionIntent = read_json_bounded(&path, 1024 * 1024, "conversion intent")?;
    let expected_source = config.native_root.join(&config.mount_subtree);
    let expected_backup = conversion_backup_path(config, repository_id)?;
    if intent.format_version != 1
        || intent.repository_id != repository_id
        || intent.source != expected_source
        || intent.backup != expected_backup
        || intent.record.backup_root != expected_backup
    {
        return Err(MirageError::integrity_mismatch(
            "conversion intent identity or paths are invalid",
        ));
    }

    match intent.operation {
        ConversionIntentOperation::Convert => {
            if config.conversion.as_ref() == Some(&intent.record) {
                if intent.source.exists() {
                    ensure_empty_directory(&intent.source, "converted mount directory")?;
                    std::fs::remove_dir(&intent.source).map_err(MirageError::from)?;
                }
                verify_inventory_record(&intent.backup, &intent.record)?;
                return remove_conversion_intent(database, repository_id);
            }
            if config.conversion.is_some() {
                return Err(MirageError::integrity_mismatch(
                    "conversion intent conflicts with durable conversion state",
                ));
            }
            match (intent.source.exists(), intent.backup.exists()) {
                (true, false) => {}
                (false, true) => {
                    verify_inventory_record(&intent.backup, &intent.record)?;
                    std::fs::rename(&intent.backup, &intent.source).map_err(MirageError::from)?;
                }
                (true, true) => {
                    ensure_empty_directory(&intent.source, "partial conversion mount directory")?;
                    verify_inventory_record(&intent.backup, &intent.record)?;
                    std::fs::remove_dir(&intent.source).map_err(MirageError::from)?;
                    std::fs::rename(&intent.backup, &intent.source).map_err(MirageError::from)?;
                }
                (false, false) => {
                    return Err(MirageError::integrity_mismatch(
                        "conversion intent has neither source nor backup subtree",
                    ));
                }
            }
            remove_conversion_intent(database, repository_id)
        }
        ConversionIntentOperation::RestoreNative => {
            if config.conversion.is_none() {
                if !intent.source.is_dir() || intent.backup.exists() {
                    return Err(MirageError::integrity_mismatch(
                        "completed restoration paths disagree with durable state",
                    ));
                }
                verify_inventory_record(&intent.source, &intent.record)?;
                return remove_conversion_intent(database, repository_id);
            }
            if config.conversion.as_ref() != Some(&intent.record) {
                return Err(MirageError::integrity_mismatch(
                    "restoration intent conflicts with durable conversion state",
                ));
            }
            match (intent.source.exists(), intent.backup.exists()) {
                (true, true) => {
                    ensure_empty_directory(&intent.source, "converted mount directory")?;
                    std::fs::remove_dir(&intent.source).map_err(MirageError::from)?;
                }
                (false, true) => {
                    verify_inventory_record(&intent.backup, &intent.record)?;
                }
                (true, false) => {
                    verify_inventory_record(&intent.source, &intent.record)?;
                    config.conversion = None;
                    save_config(database, repository_id, config)?;
                }
                (false, false) => {
                    return Err(MirageError::integrity_mismatch(
                        "restoration intent has neither source nor backup subtree",
                    ));
                }
            }
            remove_conversion_intent(database, repository_id)
        }
    }
}

fn verify_inventory_record(path: &Path, record: &ConversionRecord) -> Result<(), MirageError> {
    let inventory = directory_inventory(path)?;
    if inventory.blake3 != record.inventory_blake3
        || inventory.file_count != record.file_count
        || inventory.total_bytes != record.total_bytes
    {
        return Err(MirageError::integrity_mismatch(
            "conversion subtree inventory differs from its durable record",
        ));
    }
    Ok(())
}

pub(super) fn verify_local_import(
    manifest: &RepositoryManifest,
    import_root: &Path,
) -> Result<bool, MirageError> {
    let mut readers = BTreeMap::<String, PackReader>::new();
    let mut encryption: Option<PackReadEncryption> = None;
    let key_path = import_root.join("repository-key.dpapi");
    let encrypted_policy = key_path.exists();
    for location in &manifest.remote_locations {
        if location.object.backend_id.as_str() != "local" {
            return Err(MirageError::unsupported_layout(
                "service registration currently requires a verified local import",
            ));
        }
        let object_id = location.object.provider_object_id.as_str();
        validate_single_component(object_id, "pack object ID")?;
        if !readers.contains_key(object_id) {
            let path = import_root.join(object_id);
            ensure_regular_no_reparse(&path, "pack object")?;
            let inspection = PackReader::open_verified(&path)?;
            if inspection.is_encrypted() != encrypted_policy {
                return Err(MirageError::repository_conflict(
                    "import key record and pack encryption policy disagree",
                ));
            }
            let reader = if inspection.is_encrypted() {
                let encryption = match &encryption {
                    Some(encryption) => encryption.clone(),
                    None => {
                        let key = load_repository_key(&key_path, manifest.repository_id)?;
                        let value = PackReadEncryption {
                            repository_id: manifest.repository_id,
                            key: Arc::new(key),
                        };
                        encryption = Some(value.clone());
                        value
                    }
                };
                PackReader::open_verified_encrypted(&path, encryption)?
            } else {
                inspection
            };
            if reader.file_length() != location.object.byte_length.as_u64()
                || reader.content_hash() != location.object.content_hash
            {
                return Err(MirageError::integrity_mismatch(
                    "verified pack identity differs from manifest",
                ));
            }
            readers.insert(object_id.to_owned(), reader);
        }
    }
    for page in &manifest.pages {
        let location = manifest
            .remote_locations
            .get(page.remote_location as usize)
            .ok_or_else(|| MirageError::manifest_invalid("page location is missing"))?;
        let reader = readers
            .get_mut(location.object.provider_object_id.as_str())
            .ok_or_else(|| MirageError::integrity_mismatch("pack reader disappeared"))?;
        let entry = reader
            .lookup(page.plaintext_hash)
            .ok_or_else(|| MirageError::integrity_mismatch("pack is missing a manifest page"))?;
        if entry.frame_offset != location.offset
            || entry.frame_length != location.encoded_length.as_u64()
            || entry.logical_length != page.logical_length
        {
            return Err(MirageError::integrity_mismatch(
                "pack index location differs from manifest",
            ));
        }
        let decoded = reader.read_page(page.plaintext_hash)?;
        if decoded.page.logical_len != page.logical_length {
            return Err(MirageError::integrity_mismatch(
                "authenticated pack page length differs from manifest",
            ));
        }
    }
    Ok(encrypted_policy)
}

#[derive(Debug)]
pub(crate) struct DirectoryInventory {
    pub(crate) blake3: String,
    pub(crate) file_count: u64,
    pub(crate) total_bytes: u64,
}

pub(crate) fn directory_inventory(root: &Path) -> Result<DirectoryInventory, MirageError> {
    const MAX_ENTRIES: usize = 2_000_000;
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        ensure_no_reparse(&directory, "conversion inventory directory")?;
        let mut entries = std::fs::read_dir(&directory)
            .map_err(MirageError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(MirageError::from)?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(MirageError::from)?;
            if metadata.file_type().is_symlink() || is_windows_reparse(&metadata) {
                return Err(MirageError::invalid_argument(
                    "conversion inventory contains a reparse point",
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(path);
                if files.len() > MAX_ENTRIES {
                    return Err(MirageError::invalid_argument(
                        "conversion inventory exceeds the file-count bound",
                    ));
                }
            } else {
                return Err(MirageError::unsupported_layout(
                    "conversion inventory contains a non-file entry",
                ));
            }
        }
    }
    files.sort_by(|left, right| {
        left.to_string_lossy()
            .to_lowercase()
            .cmp(&right.to_string_lossy().to_lowercase())
            .then_with(|| left.cmp(right))
    });
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD/native-subtree-inventory/v1\0");
    let mut total_bytes = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    for path in &files {
        let relative = path.strip_prefix(root).map_err(|_| {
            MirageError::internal_invariant("inventory file escaped the selected subtree")
        })?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        let path_length = u32::try_from(relative.len())
            .map_err(|_| MirageError::unsupported_layout("inventory path is too long"))?;
        let metadata = std::fs::metadata(path).map_err(MirageError::from)?;
        total_bytes = total_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| MirageError::unsupported_layout("inventory byte count overflows"))?;
        hasher.update(&path_length.to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(&metadata.len().to_le_bytes());
        let mut file = File::open(path).map_err(MirageError::from)?;
        loop {
            let read = file.read(&mut buffer).map_err(MirageError::from)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(DirectoryInventory {
        blake3: hasher.finalize().to_hex().to_string(),
        file_count: files.len() as u64,
        total_bytes,
    })
}
