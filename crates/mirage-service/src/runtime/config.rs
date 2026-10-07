//! Runtime configuration types, persistence, and shared constants.

use super::conversion::NativeBackupResidency;
use super::fs_util::validate_label;
use super::*;

pub(super) const RUNTIME_FORMAT_VERSION: u32 = 1;

/// Declared planning assumptions for the origin estimate, shared by the
/// readiness compiler and the simulator's network model. They are assumptions,
/// not measurements; origin reachability itself is re-checked at admission.
pub(super) const ASSUMED_ORIGIN_TTFB_NS: u64 = 50_000_000;

pub(super) const ASSUMED_ORIGIN_GOODPUT_BYTES_PER_SECOND: u64 = 12_500_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOrigin {
    #[default]
    Local,
    Drive,
}

impl RuntimeOrigin {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Drive => "drive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub format_version: u32,
    #[serde(default)]
    pub origin: RuntimeOrigin,
    pub native_root: PathBuf,
    #[serde(default)]
    pub mount_subtree: PathBuf,
    pub import_root: PathBuf,
    pub launcher_relative: PathBuf,
    pub arguments: Vec<String>,
    pub version_label: String,
    pub configuration_label: String,
    pub cache_bytes: u64,
    pub drain_ms: u64,
    #[serde(default)]
    pub(super) conversion: Option<ConversionRecord>,
}

impl RuntimeConfig {
    pub(crate) const fn is_converted(&self) -> bool {
        self.conversion.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConversionRecord {
    pub(super) backup_root: PathBuf,
    pub(super) inventory_blake3: String,
    pub(super) file_count: u64,
    pub(super) total_bytes: u64,
    #[serde(default)]
    pub(super) backup_residency: NativeBackupResidency,
}

pub struct RegisterSpec {
    pub repository_id: RepositoryId,
    pub display_name: String,
    pub native_root: PathBuf,
    pub mount_subtree: PathBuf,
    pub import_root: PathBuf,
    pub launcher_relative: PathBuf,
    pub arguments: Vec<String>,
    pub version_label: String,
    pub configuration_label: String,
    pub cache_bytes: u64,
}

pub fn load_config(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<RuntimeConfig, MirageError> {
    let path = repository_state_root(database, repository_id)?.join("runtime.json");
    let bytes = bounded_read(&path, 1024 * 1024)?;
    let config: RuntimeConfig = serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch("repository runtime configuration is malformed")
            .with_source(error)
    })?;
    if config.format_version != RUNTIME_FORMAT_VERSION {
        return Err(MirageError::unsupported_layout(
            "repository runtime configuration version is unsupported",
        ));
    }
    Ok(config)
}

pub fn repository_state_root(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    Ok(database
        .reads()
        .database_path()
        .parent()
        .ok_or_else(|| MirageError::internal_invariant("database has no state root"))?
        .join("repositories")
        .join(repository_id.to_string()))
}

pub fn service_state_root(database: &Database) -> Result<PathBuf, MirageError> {
    database
        .reads()
        .database_path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| MirageError::internal_invariant("database has no state root"))
}

/// The directory whose disk holds the repository's local cache (journal
/// payloads): the user-chosen cache root, or the service state root when
/// none was ever chosen. The journal itself is `<root>\journal`.
pub fn repository_cache_root(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    match database.repository_cache_root(repository_id)? {
        Some(root) => Ok(PathBuf::from(root)),
        None => service_state_root(database),
    }
}

/// The journal directory to hand the filesystem host, or `None` when the
/// repository uses the engine default under the state root. A configured
/// root is created (SYSTEM/Administrators only) if it is missing.
pub fn prepare_journal_root(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Option<PathBuf>, MirageError> {
    let Some(root) = database.repository_cache_root(repository_id)? else {
        return Ok(None);
    };
    let root = PathBuf::from(root);
    let journal = root.join("journal");
    let fresh = !root.is_dir();
    std::fs::create_dir_all(&journal).map_err(|error| {
        MirageError::new(
            mirage_types::MirageErrorKind::Io,
            mirage_types::MirageErrorKind::Io.default_code(),
            format!("cache directory {} is unavailable", root.display()),
        )
        .with_source(error)
    })?;
    if fresh && mirage_crypto::file_acl::running_as_local_system() {
        mirage_crypto::file_acl::restrict_directory_to_system_admins(&root)?;
    }
    Ok(Some(journal))
}

/// Validates a user-requested cache root for a repository: an absolute path
/// on a fixed local disk (`X:\...`), never inside a MirageSSD mount, never
/// the root of a disk itself. Returns the canonical directory to record —
/// `<disk>\MirageSSD\<repository-id>` when only a disk root was given.
pub fn resolve_cache_root_request(
    requested: &str,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    let trimmed = requested.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(MirageError::invalid_argument(
            "cache location must be a local disk path such as D:\\",
        ));
    }
    if trimmed.len() > 2 && !matches!(bytes[2], b'\\' | b'/') {
        return Err(MirageError::invalid_argument(
            "cache location must be an absolute path such as D:\\MirageSSD",
        ));
    }
    let disk_root = format!("{}:\\", bytes[0].to_ascii_uppercase() as char);
    if crate::disk_space::drive_kind(Path::new(&disk_root))? != crate::disk_space::DriveKind::Fixed
    {
        return Err(MirageError::invalid_argument(
            "cache location must be a local NTFS or ReFS disk (not removable, network, or a virtual drive such as MirageSSD or Google Drive)",
        ));
    }
    let path = if trimmed.len() <= 3 {
        Path::new(&disk_root)
            .join("MirageSSD")
            .join(repository_id.to_string())
    } else {
        PathBuf::from(trimmed)
    };
    if path.components().count() < 2 {
        return Err(MirageError::invalid_argument(
            "cache location cannot be the root of a disk",
        ));
    }
    Ok(path)
}

pub(super) fn save_config(
    database: &Database,
    repository_id: RepositoryId,
    config: &RuntimeConfig,
) -> Result<(), MirageError> {
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("runtime.json"),
        config,
    )
}

pub(super) fn validate_runtime_fields(
    arguments: &[String],
    version_label: &str,
    configuration_label: &str,
    cache_bytes: u64,
) -> Result<(), MirageError> {
    if arguments.len() > 256
        || arguments.iter().any(|argument| argument.len() > 32_767)
        || cache_bytes == 0
        || cache_bytes > 16 * 1024 * 1024 * 1024 * 1024
    {
        return Err(MirageError::invalid_argument(
            "runtime arguments or cache budget are outside bounds",
        ));
    }
    validate_label(version_label, "game version label")?;
    validate_label(configuration_label, "game configuration label")
}
