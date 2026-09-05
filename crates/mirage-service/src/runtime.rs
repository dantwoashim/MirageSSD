use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mirage_backend::{FetchClass, ObjectBackend, RemoteObjectRef};
use mirage_backend_drive::{DriveObjectBackend, NativeHttpTransport, RetryingHttpTransport};
use mirage_cache::{
    ArenaShard, CacheLayout, InsertOutcome, IntegrityClass, ResidentIndex, VerifyOutcome,
    insert_reserved_page, verify_page,
};
use mirage_crypto::repository_key_store::load_repository_key;
use mirage_db::{
    CacheShardSpec, CacheSlotRecord, Database, LeaseSpec, NewRepository, NewSealedSession,
    ReserveCacheSlotOutcome, VerifiedGeneration,
};
use mirage_engine::{
    AdmissionStore, CapsulePageStore, MaterializeProgress, admit_sealed_session,
    materialize_capsule,
};
use mirage_index::{MountIndex, NodeIndex, compile_to_bytes};
use mirage_ipc::DriveQuotaSnapshot;
use mirage_manifest::{
    Codec, DecodeLimits, RepositoryManifest, decode_manifest_bounded, manifest_hash,
};
use mirage_pack::{
    EncryptedFrameAad, PackReadEncryption, PackReader, decode_encrypted_frame,
    encrypted_frame_pack_id,
};
use mirage_predictor::capsule::{
    CapsuleDraft, CapsulePlan, ClusterReason, ProfileKey, ReasonKind, RiskEstimate,
};
use mirage_predictor::hard_set::{HardSetPolicy, PageKey, build as build_hard_set};
use mirage_predictor::{
    GameProfile, ObservationClass, PageObservation, ProcessRecord, ProfileProcessRole,
    TraceBlockDecoder, TraceBlockEncoder, TraceEvent, TraceHeader, normalize_trace,
};
use mirage_simulator::{BaselineReplay, NetworkModel, ReplayConfig};
use mirage_types::{
    ByteCount, CheckedRange, CommitHash, GenerationId, MirageError, RepositoryEvent, RepositoryId,
    RepositoryState, SessionEvent, SessionId, SessionState, StableFileId,
};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::{
    LaunchMode, LaunchPolicy, LaunchReadiness, NativeLaunch, ProfileLaunch, launch_native,
    run_profile_session,
};

const RUNTIME_FORMAT_VERSION: u32 = 1;
const PROFILE_STARTUP_WINDOW_US: u64 = 30_000_000;
const DEFAULT_DRAIN_MS: u64 = 2_000;
const DRIVE_MANIFEST: &str = "drive-manifest.cbor";

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
    conversion: Option<ConversionRecord>,
}

impl RuntimeConfig {
    pub(crate) const fn is_converted(&self) -> bool {
        self.conversion.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionRecord {
    backup_root: PathBuf,
    inventory_blake3: String,
    file_count: u64,
    total_bytes: u64,
    #[serde(default)]
    backup_residency: NativeBackupResidency,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NativeBackupResidency {
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
    format_version: u32,
    repository_id: RepositoryId,
    generation_id: GenerationId,
    commit_hash: CommitHash,
    backup: PathBuf,
    tombstone: PathBuf,
    inventory_blake3: String,
    file_count: u64,
    total_bytes: u64,
    remote_verified_at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConversionIntentOperation {
    Convert,
    RestoreNative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversionIntent {
    format_version: u32,
    repository_id: RepositoryId,
    operation: ConversionIntentOperation,
    source: PathBuf,
    backup: PathBuf,
    record: ConversionRecord,
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

pub fn register(
    database: &Database,
    owner_sid: &str,
    spec: RegisterSpec,
) -> Result<Value, MirageError> {
    validate_label(&spec.display_name, "repository display name")?;
    let native_root = canonical_directory(&spec.native_root, "native game root")?;
    let (mount_subtree, mount_root) = validate_mount_subtree(&native_root, &spec.mount_subtree)?;
    let import_root = canonical_directory(&spec.import_root, "import root")?;
    let launcher_relative = validate_launcher(&native_root, &spec.launcher_relative)?;
    validate_runtime_fields(
        &spec.arguments,
        &spec.version_label,
        &spec.configuration_label,
        spec.cache_bytes,
    )?;

    let manifest_path = import_root.join("base-manifest.cbor");
    let manifest_bytes = bounded_read(&manifest_path, DecodeLimits::default().max_input_bytes)?;
    let manifest = decode_manifest_bounded(&manifest_bytes, DecodeLimits::default())?;
    if manifest.repository_id != spec.repository_id {
        return Err(MirageError::repository_conflict(
            "import manifest repository ID differs from registration",
        ));
    }
    let content_encrypted = verify_local_import(&manifest, &import_root)?;
    let manifest_hash = manifest_hash(&manifest)?;
    let index_bytes = compile_to_bytes(&manifest)?;
    let commit_hash = local_commit_hash(&manifest, &import_root)?;

    let existing_owner = database.load_repository_owner_sid(spec.repository_id)?;
    if existing_owner
        .as_deref()
        .is_some_and(|owner| owner != owner_sid)
    {
        return Err(MirageError::backend_permission_denied(
            "repository is already owned by another Windows SID",
        ));
    }
    if let Some(existing_encrypted) =
        database.load_repository_content_encrypted(spec.repository_id)?
        && existing_encrypted != content_encrypted
    {
        return Err(MirageError::repository_conflict(
            "repository encryption policy cannot be changed by registration",
        ));
    }

    let repository_root = repository_state_root(database, spec.repository_id)?;
    std::fs::create_dir_all(&repository_root).map_err(MirageError::from)?;
    let index_path = repository_root.join("mount-index.bin");
    write_atomic(&index_path, &index_bytes)?;

    match existing_owner {
        Some(_) => {
            let existing_root = database
                .load_repository_root(spec.repository_id)?
                .ok_or_else(|| MirageError::integrity_mismatch("repository root disappeared"))?;
            if existing_root != mount_root {
                return Err(MirageError::repository_conflict(
                    "registered mount subtree differs from the existing repository",
                ));
            }
        }
        None => database.create_repository(NewRepository {
            repository_id: spec.repository_id,
            display_name: spec.display_name,
            local_root: mount_root,
            owner_sid: owner_sid.to_owned(),
            content_encrypted,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: now_ns(),
        })?,
    }

    let generation = VerifiedGeneration {
        repository_id: spec.repository_id,
        generation_id: manifest.generation_id,
        commit_hash,
        manifest_hash,
        manifest_local_path: manifest_path,
        mount_index_path: Some(index_path),
        created_at_ns: now_ns(),
    };
    database.insert_verified_generation(generation.clone())?;
    match database.load_active_generation(spec.repository_id)? {
        Some(active)
            if active.generation_id == generation.generation_id
                && active.commit_hash == generation.commit_hash => {}
        Some(_) => {
            return Err(MirageError::repository_conflict(
                "repository already has a different active generation",
            ));
        }
        None => database.activate_generation(
            spec.repository_id,
            generation.generation_id,
            generation.commit_hash,
            None,
            now_ns(),
        )?,
    }

    let config = RuntimeConfig {
        format_version: RUNTIME_FORMAT_VERSION,
        origin: RuntimeOrigin::Local,
        native_root,
        mount_subtree,
        import_root,
        launcher_relative,
        arguments: spec.arguments,
        version_label: spec.version_label,
        configuration_label: spec.configuration_label,
        cache_bytes: spec.cache_bytes,
        drain_ms: DEFAULT_DRAIN_MS,
        conversion: None,
    };
    save_config(database, spec.repository_id, &config)?;
    Ok(json!({
        "repository_id": spec.repository_id.to_string(),
        "owner_sid_bound": true,
        "generation": manifest.generation_id.as_u64(),
        "manifest_hash": manifest_hash.to_string(),
        "page_count": manifest.pages.len(),
        "cache_bytes": config.cache_bytes,
        "mount_subtree": config.mount_subtree,
        "content_encrypted": content_encrypted,
        "state": RepositoryState::ReadyUnmounted.as_str()
    }))
}

pub fn configure(
    database: &Database,
    repository_id: RepositoryId,
    launcher_relative: PathBuf,
    arguments: Vec<String>,
    version_label: String,
    configuration_label: String,
) -> Result<Value, MirageError> {
    let mut config = load_config(database, repository_id)?;
    config.launcher_relative = validate_launcher(&config.native_root, &launcher_relative)?;
    validate_runtime_fields(
        &arguments,
        &version_label,
        &configuration_label,
        config.cache_bytes,
    )?;
    config.arguments = arguments;
    config.version_label = version_label;
    config.configuration_label = configuration_label;
    save_config(database, repository_id, &config)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "configured": true,
        "launcher_relative": config.launcher_relative,
        "version_label": config.version_label,
        "configuration_label": config.configuration_label
    }))
}

pub fn set_drive_origin(
    database: &Database,
    repository_id: RepositoryId,
    drive: bool,
) -> Result<Value, MirageError> {
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    if state != RepositoryState::ReadyUnmounted {
        return Err(MirageError::repository_conflict(
            "repository origin can change only while ready and unmounted",
        ));
    }
    let active = active_generation(database, repository_id)?;
    let local_manifest = decode_manifest_bounded(
        &bounded_read(
            &active.manifest_local_path,
            DecodeLimits::default().max_input_bytes,
        )?,
        DecodeLimits::default(),
    )?;
    let mut config = load_config(database, repository_id)?;
    config.origin = if drive {
        if database.load_repository_content_encrypted(repository_id)? != Some(true) {
            return Err(MirageError::backend_permission_denied(
                "Drive origin requires an encrypted repository",
            ));
        }
        let _ = load_drive_manifest(&config, &local_manifest)?;
        RuntimeOrigin::Drive
    } else {
        verify_local_import(&local_manifest, &config.import_root)?;
        RuntimeOrigin::Local
    };
    save_config(database, repository_id, &config)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "origin": config.origin.as_str(),
        "cloud_reads_in_filesystem_callbacks": false
    }))
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

pub fn capture(
    database: &Database,
    repository_id: RepositoryId,
    maximum_duration_seconds: u32,
) -> Result<Value, MirageError> {
    if !(1..=21_600).contains(&maximum_duration_seconds) {
        return Err(MirageError::invalid_argument(
            "profile maximum duration must be between 1 second and 6 hours",
        ));
    }
    let config = load_config(database, repository_id)?;
    let active = active_generation(database, repository_id)?;
    let index_path = active.mount_index_path.as_ref().ok_or_else(|| {
        MirageError::integrity_mismatch("active generation has no compiled mount index")
    })?;
    let index = MountIndex::open(index_path)?;
    let launcher = config.native_root.join(&config.launcher_relative);
    let result = run_profile_session(&ProfileLaunch {
        game_root: config.native_root.clone(),
        launcher,
        arguments: config.arguments.clone(),
        maximum_runtime: Duration::from_secs(u64::from(maximum_duration_seconds)),
        drain_interval: Duration::from_millis(config.drain_ms),
        version_label: config.version_label.clone(),
        configuration_label: config.configuration_label.clone(),
    })?;

    let mut trace_events = Vec::new();
    let profile_root = config.native_root.join(&config.mount_subtree);
    let filter = mirage_etw::filter::RootFilter::new(&profile_root).map_err(MirageError::from)?;
    let first_timestamp = result
        .events
        .iter()
        .filter(|event| !event.write)
        .map(|event| event.timestamp_100ns)
        .min()
        .unwrap_or(0);
    let mut matching_process_reads = 0_u64;
    let mut named_process_reads = 0_u64;
    let mut in_root_reads = 0_u64;
    for event in result
        .events
        .iter()
        .filter(|event| event.process_id == result.process_id && !event.write && event.size != 0)
    {
        matching_process_reads = matching_process_reads.saturating_add(1);
        if event.path.is_some() {
            named_process_reads = named_process_reads.saturating_add(1);
        }
        let Some(path) = event.path.as_deref().and_then(|path| filter.include(path)) else {
            continue;
        };
        in_root_reads = in_root_reads.saturating_add(1);
        let path = path.to_string_lossy().replace('/', "\\");
        let Some(NodeIndex::File(file_index)) = index.lookup_path(&path)? else {
            continue;
        };
        let file = index.file_by_index(file_index)?;
        trace_events.push(TraceEvent {
            timestamp_ns: event
                .timestamp_100ns
                .saturating_sub(first_timestamp)
                .saturating_mul(100),
            stable_file_id: file.stable_id(),
            offset: event.offset,
            length: event.size,
            flags: 0,
        });
    }
    trace_events.sort_by_key(|event| event.timestamp_ns);
    if trace_events.is_empty() {
        return Err(MirageError::provider_unavailable(format!(
            "ETW captured no manifest-backed reads: total_events={}, matching_process_reads={matching_process_reads}, named_process_reads={named_process_reads}, in_root_reads={in_root_reads}, unknown_paths={}, dropped_events={}, events_lost={}, buffers_lost={}, timed_out={}, exit_code={:?}",
            result.events.len(),
            result.unknown_path_events,
            result.dropped_capture_events,
            result.etw_events_lost,
            result.etw_buffers_lost,
            result.timed_out,
            result.exit_code,
        )));
    }
    let dropped = u64::from(result.etw_events_lost)
        .saturating_add(u64::from(result.etw_buffers_lost))
        .saturating_add(result.dropped_capture_events);
    let normalized = normalize_trace(&index, &trace_events, None)?;
    if normalized.touches.is_empty() {
        return Err(MirageError::provider_unavailable(
            "captured file I/O did not resolve to any virtual manifest page",
        ));
    }
    let file_ordinals = file_ordinals(&index)?;
    let observations = normalized
        .touches
        .iter()
        .map(|touch| {
            let file_index = *file_ordinals.get(&touch.stable_file_id).ok_or_else(|| {
                MirageError::internal_invariant("normalized trace file disappeared")
            })?;
            let page_ordinal = relative_page_ordinal(
                index.file_by_index(file_index)?,
                touch.page_ordinal.as_u32(),
            )?;
            Ok(PageObservation {
                file_index,
                page_ordinal,
                first_touch_delta_us: touch.timestamp_ns / 1_000,
                class: ObservationClass::Demand,
            })
        })
        .collect::<Result<Vec<_>, MirageError>>()?;
    let label = format!("{}:{}", config.version_label, config.configuration_label);
    let profile = GameProfile {
        format_version: 1,
        repository_id,
        manifest_hash: active.manifest_hash,
        label,
        page_observations: observations,
        processes: vec![ProcessRecord {
            stable_index: result.process_id,
            role: ProfileProcessRole::Game,
            redacted_image_path: config
                .launcher_relative
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("launcher")
                .to_owned(),
        }],
        dropped_event_count: dropped,
    };
    profile.validate()?;
    let stamp = now_ns();
    let profile_root = repository_state_root(database, repository_id)?.join("profiles");
    std::fs::create_dir_all(&profile_root).map_err(MirageError::from)?;
    let base = format!("{stamp}");
    write_json_atomic(&profile_root.join(format!("{base}.profile.json")), &profile)?;
    let trace_path = profile_root.join(format!("{base}.trace"));
    write_trace(
        &trace_path,
        repository_id,
        active.generation_id,
        u32::try_from(index.header().page_size)
            .map_err(|_| MirageError::unsupported_layout("page size exceeds u32"))?,
        dropped,
        &trace_events,
    )?;
    let summary = profile.summary()?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "profile_id": base,
        "process_id": result.process_id,
        "exit_code": result.exit_code,
        "timed_out": result.timed_out,
        "elapsed_ms": result.elapsed.as_millis(),
        "observations": summary.observation_count,
        "unique_pages": summary.unique_page_count,
        "dropped_events": dropped,
        "unknown_path_events": result.unknown_path_events,
        "trace_path": trace_path
    }))
}

pub fn simulate(database: &Database, repository_id: RepositoryId) -> Result<Value, MirageError> {
    let config = load_config(database, repository_id)?;
    let active = active_generation(database, repository_id)?;
    let index = MountIndex::open(active.mount_index_path.as_ref().ok_or_else(|| {
        MirageError::integrity_mismatch("active generation has no compiled mount index")
    })?)?;
    let profiles = load_profiles(database, repository_id)?;
    let latest = latest_trace_path(database, repository_id)?;
    let trace = read_trace(&latest, repository_id, active.generation_id)?;
    let normalized = normalize_trace(&index, &trace, None)?;
    let page_size = index.header().page_size;
    let cache_pages = usize::try_from((config.cache_bytes / page_size).max(1))
        .unwrap_or(usize::MAX)
        .min(
            usize::try_from(index.page_count())
                .unwrap_or(usize::MAX)
                .max(1),
        );
    let mut replay = BaselineReplay::new(ReplayConfig {
        cache_pages,
        page_bytes: page_size,
        pinned_pages: BTreeSet::new(),
        network: NetworkModel {
            base_latency_ns: 50_000_000,
            jitter_ns: 5_000_000,
            jitter_seed: 0x4d49_5241_4745,
            bandwidth_bytes_per_second: 12_500_000,
            max_concurrency: 1,
            fail_fetches: BTreeSet::new(),
        },
    })?;
    for touch in normalized.touches {
        replay.process(touch)?;
    }
    let metrics = replay.finish();
    let report = mirage_predictor::analyze::analyze(
        &profiles,
        page_size,
        PROFILE_STARTUP_WINDOW_US,
        index.page_count(),
    )?;
    let value = json!({
        "repository_id": repository_id.to_string(),
        "profile_sessions": report.session_count,
        "unique_pages": report.unique_pages,
        "union_bytes": report.union_bytes,
        "intersection_pages": report.intersection_pages,
        "startup_pages": report.startup_pages,
        "broad_scan_detected": report.broad_scan_detected,
        "dropped_events": report.dropped_events,
        "simulation": {
            "cache_pages": cache_pages,
            "network_mbps": 100,
            "latency_ms": 50,
            "accesses": metrics.accesses,
            "hits": metrics.hits,
            "misses": metrics.misses,
            "blocking_ns": metrics.blocking_ns,
            "p95_stall_ns": metrics.p95_stall_ns,
            "p99_stall_ns": metrics.p99_stall_ns,
            "remote_bytes": metrics.remote_bytes,
            "peak_resident_pages": metrics.peak_resident_pages
        }
    });
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("simulation.json"),
        &value,
    )?;
    Ok(value)
}

pub fn plan(
    database: &Database,
    repository_id: RepositoryId,
    full_volume: bool,
) -> Result<Value, MirageError> {
    let config = load_config(database, repository_id)?;
    let active = active_generation(database, repository_id)?;
    let index = MountIndex::open(active.mount_index_path.as_ref().ok_or_else(|| {
        MirageError::integrity_mismatch("active generation has no compiled mount index")
    })?)?;
    let profiles = load_profiles(database, repository_id)?;
    let hard = build_hard_set(
        &profiles,
        &HardSetPolicy {
            startup_window_us: PROFILE_STARTUP_WINDOW_US,
            minimum_session_count: 1,
            minimum_session_ratio_millionths: 1_000_000,
        },
        &BTreeSet::new(),
        &BTreeSet::new(),
        &BTreeSet::new(),
    )?;
    let mut all = RoaringBitmap::new();
    if full_volume {
        let page_count = u32::try_from(index.page_count())
            .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 pages"))?;
        all.insert_range(0..page_count);
    } else {
        for profile in &profiles {
            for observation in &profile.page_observations {
                all.insert(global_page_ordinal(
                    &index,
                    PageKey {
                        file_index: observation.file_index,
                        page_ordinal: observation.page_ordinal,
                    },
                )?);
            }
        }
    }
    let mut mandatory = RoaringBitmap::new();
    for page in hard {
        mandatory.insert(global_page_ordinal(&index, page.page)?);
    }
    if all.is_empty() {
        return Err(MirageError::invalid_argument(
            "profile set contains no resolvable pages",
        ));
    }
    let page_size = u32::try_from(index.header().page_size)
        .map_err(|_| MirageError::unsupported_layout("page size exceeds u32"))?;
    let total_bytes = all
        .len()
        .checked_mul(u64::from(page_size))
        .ok_or_else(|| MirageError::invalid_argument("capsule byte count overflows"))?;
    if total_bytes > config.cache_bytes {
        return Err(MirageError::cache_full(format!(
            "profile union requires {total_bytes} bytes but cache budget is {}",
            config.cache_bytes
        )));
    }
    let violation = held_out_violation_millionths(&profiles);
    let dropped: u64 = profiles
        .iter()
        .map(|profile| profile.dropped_event_count)
        .sum();
    let observations: u64 = profiles
        .iter()
        .map(|profile| profile.page_observations.len() as u64)
        .sum();
    let data_loss = millionths(dropped, dropped.saturating_add(observations));
    let version_confidence = if profiles
        .windows(2)
        .all(|pair| pair[0].label == pair[1].label)
    {
        1_000_000
    } else {
        500_000
    };
    let mut reasons = vec![ClusterReason {
        cluster_id: 1,
        kind: ReasonKind::RecentUnion,
        pages: all.clone(),
        evidence_millionths: 1_000_000_u32.saturating_sub(violation),
    }];
    if !mandatory.is_empty() {
        reasons.push(ClusterReason {
            cluster_id: 0,
            kind: ReasonKind::HardSet,
            pages: mandatory.clone(),
            evidence_millionths: 1_000_000,
        });
    }
    let plan = CapsulePlan::new(CapsuleDraft {
        repository_id,
        generation: active.generation_id,
        profile_key: ProfileKey(format!(
            "{}:{}",
            config.version_label, config.configuration_label
        )),
        page_set: all,
        mandatory_set: mandatory,
        frontier_set: RoaringBitmap::new(),
        page_size,
        risk: RiskEstimate {
            held_out_violation_millionths: violation,
            unseen_branch_mass_millionths: violation,
            data_quality_millionths: 1_000_000_u32.saturating_sub(data_loss),
            version_transfer_confidence_millionths: version_confidence,
        },
        reasons,
    })?;
    let capsule_root = repository_state_root(database, repository_id)?.join("capsules");
    std::fs::create_dir_all(&capsule_root).map_err(MirageError::from)?;
    write_json_atomic(
        &capsule_root.join(format!("{}.json", plan.capsule_id)),
        &plan,
    )?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "capsule_id": plan.capsule_id.to_string(),
        "full_volume": full_volume,
        "generation": plan.generation.as_u64(),
        "page_count": plan.page_set.len(),
        "mandatory_pages": plan.mandatory_set.len(),
        "total_bytes": plan.total_bytes,
        "cache_budget_bytes": config.cache_bytes,
        "held_out_violation_millionths": plan.risk.held_out_violation_millionths,
        "data_quality_millionths": plan.risk.data_quality_millionths
    }))
}

pub fn materialize(
    database: &Database,
    repository_id: RepositoryId,
    capsule_id: mirage_types::CapsuleId,
    drive_access_token: Option<&str>,
    drive_quota: Option<DriveQuotaSnapshot>,
) -> Result<Value, MirageError> {
    let plan = load_plan(database, repository_id, capsule_id)?;
    let store = LocalCapsuleStore::open(database, repository_id, &plan, drive_access_token)?;
    let progress = futures_executor::block_on(materialize_capsule(
        &plan,
        &store,
        &CancellationToken::new(),
    ))?;
    if let Some(quota) = drive_quota {
        quota.validate()?;
        if store.origin != RuntimeOrigin::Drive {
            return Err(MirageError::invalid_argument(
                "Drive quota was supplied for a non-Drive repository",
            ));
        }
        save_drive_capacity(database, repository_id, quota)?;
    }
    let drive_requests = store
        .drive_transport
        .as_ref()
        .map_or(0, |transport| transport.request_count());
    let drive_retries = store
        .drive_transport
        .as_ref()
        .map_or(0, |transport| transport.retry_count());
    let drive_downloaded_bytes = store
        .drive_transport
        .as_ref()
        .map_or(0, |transport| transport.downloaded_bytes());
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "capsule_id": capsule_id.to_string(),
        "generation": plan.generation.as_u64(),
        "total_pages": progress.total_pages,
        "already_resident": progress.already_resident,
        "downloaded_pages": progress.downloaded_pages,
        "verified_pages": progress.verified_pages,
        "failed_pages": progress.failed_pages,
        "downloaded_bytes": progress.downloaded_bytes,
        "origin": store.origin.as_str(),
        "drive_requests": drive_requests,
        "drive_retries": drive_retries,
        "drive_downloaded_bytes": drive_downloaded_bytes,
        "drive_quota_limit_bytes": drive_quota.and_then(|quota| quota.limit_bytes),
        "drive_quota_usage_bytes": drive_quota.map(|quota| quota.usage_bytes),
        "drive_quota_available_bytes": drive_quota.and_then(DriveQuotaSnapshot::available_bytes),
        "complete": progress.verified_pages == progress.total_pages
    }))
}

pub fn admit(
    database: &Database,
    repository_id: RepositoryId,
    capsule_id: mirage_types::CapsuleId,
) -> Result<Value, MirageError> {
    let plan = load_plan(database, repository_id, capsule_id)?;
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    if state != RepositoryState::ReadyMounted {
        return Err(MirageError::repository_conflict(
            "sealed admission requires the active generation to be mounted",
        ));
    }
    database.set_repository_state(
        repository_id,
        state,
        RepositoryEvent::BeginAdmission,
        now_ns(),
    )?;
    let store = LocalCapsuleStore::open(database, repository_id, &plan, None)?;
    let result = futures_executor::block_on(admit_sealed_session(&plan, &store));
    let admitted = match result {
        Ok(admitted) => admitted,
        Err(error) => {
            store.abort_created_session();
            recover_to_mounted(database, repository_id, RepositoryState::AdmittingSession);
            return Err(error);
        }
    };
    if let Err(error) = database.set_repository_state(
        repository_id,
        RepositoryState::AdmittingSession,
        RepositoryEvent::CapsuleSealed,
        now_ns(),
    ) {
        store.abort_created_session();
        recover_to_mounted(database, repository_id, RepositoryState::AdmittingSession);
        return Err(error);
    }
    let artifact = AdmittedArtifact {
        format_version: 1,
        repository_id,
        capsule_id,
        generation: admitted.generation,
        session_id: admitted.session_id,
        pinned_pages: admitted.pinned_pages,
        admitted_at_ns: now_ns(),
    };
    write_json_atomic(
        &admitted_path(database, repository_id, capsule_id)?,
        &artifact,
    )?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "capsule_id": capsule_id.to_string(),
        "session_id": admitted.session_id.to_string(),
        "generation": admitted.generation.as_u64(),
        "pinned_pages": admitted.pinned_pages,
        "state": SessionState::SealedReady.as_str(),
        "offline_ready": true
    }))
}

pub struct RuntimeLaunch {
    pub process: crate::launch::LaunchedProcess,
    pub session_id: SessionId,
    pub started_at: std::time::Instant,
    pub maximum_duration: Option<Duration>,
    pub exit_observed_at: Option<std::time::Instant>,
    pub drain_interval: Duration,
}

pub fn launch(
    database: &Database,
    repository_id: RepositoryId,
    capsule_id: Option<mirage_types::CapsuleId>,
    maximum_duration_seconds: Option<u64>,
) -> Result<(Value, RuntimeLaunch), MirageError> {
    if maximum_duration_seconds.is_some_and(|seconds| !(1..=21_600).contains(&seconds)) {
        return Err(MirageError::invalid_argument(
            "maximum launch duration must be between 1 and 21600 seconds",
        ));
    }
    let capsule_id = capsule_id.ok_or_else(|| {
        MirageError::invalid_argument(
            "balanced launch is not admitted until a measured hard set is materialized; supply --capsule-id for sealed launch",
        )
    })?;
    let plan = load_plan(database, repository_id, capsule_id)?;
    let artifact: AdmittedArtifact = read_json_bounded(
        &admitted_path(database, repository_id, capsule_id)?,
        1024 * 1024,
        "admitted session artifact",
    )?;
    if artifact.format_version != 1
        || artifact.repository_id != repository_id
        || artifact.capsule_id != capsule_id
        || artifact.generation != plan.generation
        || database.load_session_state(artifact.session_id)? != Some(SessionState::SealedReady)
    {
        return Err(MirageError::repository_conflict(
            "capsule does not have a matching sealed-ready session",
        ));
    }
    let active = active_generation(database, repository_id)?;
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    LaunchPolicy::validate(
        LaunchMode::Sealed,
        &LaunchReadiness {
            mounted_generation: active.generation_id,
            requested_generation: plan.generation,
            admitted_capsule: Some(artifact.capsule_id),
            requested_capsule: Some(capsule_id),
            capsule_complete: artifact.pinned_pages == plan.page_set.len(),
            hard_set_complete: true,
            update_or_recovery_active: state != RepositoryState::PlayingSealed,
            provider_healthy: true,
            risk_millionths: plan.risk.held_out_violation_millionths,
        },
    )?;
    let config = load_config(database, repository_id)?;
    database.transition_session_state(
        artifact.session_id,
        SessionState::SealedReady,
        SessionEvent::LaunchRequested,
        now_ns(),
    )?;
    let environment = safe_launch_environment();
    let drain_interval = Duration::from_millis(config.drain_ms.min(120_000));
    let mut process = match launch_native(&NativeLaunch {
        game_root: config.native_root.clone(),
        launcher: config.native_root.join(&config.launcher_relative),
        arguments: config.arguments,
        environment,
    }) {
        Ok(process) => process,
        Err(error) => {
            let _ = database.transition_session_state(
                artifact.session_id,
                SessionState::Launching,
                SessionEvent::AbortRequested,
                now_ns(),
            );
            let _ = database
                .release_cache_pins(mirage_db::PersistentPinReason::Session(artifact.session_id));
            recover_to_mounted(database, repository_id, RepositoryState::PlayingSealed);
            return Err(error);
        }
    };
    let record = database
        .record_session_process(mirage_db::SessionProcess {
            session_id: artifact.session_id,
            process_id: process.root_pid,
            started_at_ns: now_ns(),
        })
        .and_then(|_| {
            database.transition_session_state(
                artifact.session_id,
                SessionState::Launching,
                SessionEvent::LaunchObserved,
                now_ns(),
            )?;
            Ok(())
        });
    if let Err(error) = record {
        let _ = process.child.kill();
        let _ = process.child.wait();
        let _ = database.transition_session_state(
            artifact.session_id,
            SessionState::Launching,
            SessionEvent::AbortRequested,
            now_ns(),
        );
        let _ = database
            .release_cache_pins(mirage_db::PersistentPinReason::Session(artifact.session_id));
        recover_to_mounted(database, repository_id, RepositoryState::PlayingSealed);
        return Err(error);
    }
    let value = json!({
        "repository_id": repository_id.to_string(),
        "capsule_id": capsule_id.to_string(),
        "session_id": artifact.session_id.to_string(),
        "root_pid": process.root_pid,
        "mode": "sealed",
        "risk_millionths": plan.risk.held_out_violation_millionths,
        "maximum_duration_seconds": maximum_duration_seconds,
        "state": SessionState::Active.as_str()
    });
    Ok((
        value,
        RuntimeLaunch {
            process,
            session_id: artifact.session_id,
            started_at: std::time::Instant::now(),
            maximum_duration: maximum_duration_seconds.map(Duration::from_secs),
            exit_observed_at: None,
            drain_interval,
        },
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmittedArtifact {
    format_version: u32,
    repository_id: RepositoryId,
    capsule_id: mirage_types::CapsuleId,
    generation: GenerationId,
    session_id: SessionId,
    pinned_pages: u64,
    admitted_at_ns: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountRecord {
    format_version: u32,
    pub repository_id: RepositoryId,
    pub generation: GenerationId,
    pub mount_point: PathBuf,
    pub explorer_visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeCapacitySnapshot {
    format_version: u32,
    repository_id: RepositoryId,
    provider: String,
    limit_bytes: Option<u64>,
    usage_bytes: u64,
    observed_at_ns: i64,
}

struct LocalCapsuleStore {
    database: Database,
    repository_id: RepositoryId,
    generation: GenerationId,
    index: Arc<MountIndex>,
    shard: Arc<ArenaShard>,
    resident: Arc<ResidentIndex>,
    import_root: PathBuf,
    origin: RuntimeOrigin,
    drive_objects: BTreeMap<[u8; 32], RemoteObjectRef>,
    drive_backend: Option<Arc<dyn ObjectBackend>>,
    drive_transport: Option<Arc<RetryingHttpTransport>>,
    provider_ready: bool,
    encryption: Option<PackReadEncryption>,
    reservations: Mutex<BTreeMap<mirage_types::PageHash, CacheSlotRecord>>,
    created_session: Mutex<Option<SessionId>>,
    checkpoint_path: PathBuf,
}

impl LocalCapsuleStore {
    fn open(
        database: &Database,
        repository_id: RepositoryId,
        plan: &CapsulePlan,
        drive_access_token: Option<&str>,
    ) -> Result<Self, MirageError> {
        let active = active_generation(database, repository_id)?;
        if active.generation_id != plan.generation {
            return Err(MirageError::repository_conflict(
                "capsule generation is not active",
            ));
        }
        let index = Arc::new(MountIndex::open(
            active.mount_index_path.as_ref().ok_or_else(|| {
                MirageError::integrity_mismatch("active generation has no mount index")
            })?,
        )?);
        let config = load_config(database, repository_id)?;
        let key_path = config.import_root.join("repository-key.dpapi");
        let encryption = key_path
            .exists()
            .then(|| {
                Ok::<PackReadEncryption, MirageError>(PackReadEncryption {
                    repository_id,
                    key: Arc::new(load_repository_key(&key_path, repository_id)?),
                })
            })
            .transpose()?;
        let (drive_objects, drive_backend, drive_transport, provider_ready) = match config.origin {
            RuntimeOrigin::Local => {
                if drive_access_token.is_some() {
                    return Err(MirageError::invalid_argument(
                        "Drive access token was supplied for a local-origin repository",
                    ));
                }
                (BTreeMap::new(), None, None, config.import_root.is_dir())
            }
            RuntimeOrigin::Drive => {
                if encryption.is_none() {
                    return Err(MirageError::backend_unauthenticated(
                        "Drive origin requires the repository content key",
                    ));
                }
                let local_manifest = decode_manifest_bounded(
                    &bounded_read(
                        &active.manifest_local_path,
                        DecodeLimits::default().max_input_bytes,
                    )?,
                    DecodeLimits::default(),
                )?;
                let drive_manifest = load_drive_manifest(&config, &local_manifest)?;
                let mut objects = BTreeMap::new();
                for location in drive_manifest.remote_locations {
                    let key = *location.object.content_hash.as_bytes();
                    if let Some(existing) = objects.insert(key, location.object.clone())
                        && existing != location.object
                    {
                        return Err(MirageError::integrity_mismatch(
                            "Drive publication maps one content hash to multiple objects",
                        ));
                    }
                }
                let drive = drive_access_token
                    .map(|token| drive_backend(repository_id, token))
                    .transpose()?;
                let (backend, transport) = match drive {
                    Some((backend, transport)) => (Some(backend), Some(transport)),
                    None => (None, None),
                };
                (objects, backend, transport, true)
            }
        };
        let (shard, resident) = open_cache(database, plan.page_size, config.cache_bytes)?;
        Ok(Self {
            database: database.clone(),
            repository_id,
            generation: plan.generation,
            index,
            shard,
            resident,
            import_root: config.import_root,
            origin: config.origin,
            drive_objects,
            drive_backend,
            drive_transport,
            provider_ready,
            encryption,
            reservations: Mutex::new(BTreeMap::new()),
            created_session: Mutex::new(None),
            checkpoint_path: repository_state_root(database, repository_id)?
                .join("capsules")
                .join(format!("{}.progress.json", plan.capsule_id)),
        })
    }

    fn page(&self, ordinal: u32) -> Result<mirage_index::PageView<'_>, MirageError> {
        self.index.page_by_ordinal(ordinal)
    }

    fn hashes(&self, pages: &RoaringBitmap) -> Result<Vec<mirage_types::PageHash>, MirageError> {
        let mut hashes = pages
            .iter()
            .map(|page| self.page(page).map(|page| page.plaintext_hash()))
            .collect::<Result<Vec<_>, _>>()?;
        hashes.sort();
        hashes.dedup();
        Ok(hashes)
    }

    fn abort_created_session(&self) {
        let Ok(mut created) = self.created_session.lock() else {
            return;
        };
        let Some(session) = created.take() else {
            return;
        };
        if let Ok(Some(state)) = self.database.load_session_state(session)
            && matches!(state, SessionState::Verifying | SessionState::SealedReady)
        {
            let _ = self.database.transition_session_state(
                session,
                state,
                SessionEvent::AbortRequested,
                now_ns(),
            );
        }
        let _ = self
            .resident
            .pins()
            .release_session(&self.database, session);
    }
}

#[async_trait]
impl CapsulePageStore for LocalCapsuleStore {
    fn generation(&self) -> GenerationId {
        self.generation
    }

    async fn reserve_all(&self, pages: &RoaringBitmap) -> Result<(), MirageError> {
        let mut unique = BTreeMap::new();
        for ordinal in pages {
            let page = self.page(ordinal)?;
            if let Some(existing) = unique.insert(page.plaintext_hash(), page.logical_length())
                && existing != page.logical_length()
            {
                return Err(MirageError::integrity_mismatch(
                    "deduplicated page hash has conflicting lengths",
                ));
            }
        }
        let requests = unique
            .iter()
            .map(|(hash, length)| (*hash, *length))
            .collect::<Vec<_>>();
        let outcomes = self.database.reserve_cache_slots_batch(requests)?;
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| MirageError::internal_invariant("capsule reservation lock poisoned"))?;
        for ((hash, _), outcome) in unique.into_iter().zip(outcomes) {
            if let ReserveCacheSlotOutcome::Reserved(record) = outcome {
                reservations.insert(hash, record);
            }
        }
        Ok(())
    }

    async fn is_verified_resident(&self, page: u32) -> Result<bool, MirageError> {
        let hash = self.page(page)?.plaintext_hash();
        Ok(matches!(
            verify_page(&self.resident, &self.database, hash, IntegrityClass::Clean,)?,
            VerifyOutcome::Verified
        ))
    }

    async fn fetch_verify_commit(
        &self,
        page: u32,
        mandatory: bool,
        cancel: &CancellationToken,
    ) -> Result<u64, MirageError> {
        if cancel.is_cancelled() {
            return Err(MirageError::cancelled("capsule materialization cancelled"));
        }
        let page_view = self.page(page)?;
        let hash = page_view.plaintext_hash();
        let location = page_view.remote_location()?;
        let object_id = location.provider_object_id()?;
        validate_single_component(object_id, "pack object ID")?;
        let bytes = match self.origin {
            RuntimeOrigin::Local => {
                let path = self.import_root.join(object_id);
                let mut reader = match &self.encryption {
                    Some(encryption) => {
                        PackReader::open_verified_encrypted(&path, encryption.clone())?
                    }
                    None => PackReader::open_verified(&path)?,
                };
                reader.read_page(hash)?.page.bytes.to_vec()
            }
            RuntimeOrigin::Drive => {
                if location.codec() != Codec::None {
                    return Err(MirageError::unsupported_layout(
                        "Drive materialization currently requires uncompressed pack frames",
                    ));
                }
                let backend = self.drive_backend.as_ref().ok_or_else(|| {
                    MirageError::backend_unauthenticated(
                        "Drive materialization requires --drive-client-credentials",
                    )
                })?;
                let object = self
                    .drive_objects
                    .get(&location.object_hash())
                    .ok_or_else(|| {
                        MirageError::integrity_mismatch(
                            "Drive publication omits a required pack object",
                        )
                    })?;
                let frame = backend
                    .read_range(
                        object,
                        CheckedRange::new(location.pack_offset(), location.encoded_length())?,
                        if mandatory {
                            FetchClass::MandatoryAdmission
                        } else {
                            FetchClass::CapsuleAdmission
                        },
                        cancel.child_token(),
                    )
                    .await?
                    .collect_bounded(location.encoded_length())
                    .await?;
                let encryption = self.encryption.as_ref().ok_or_else(|| {
                    MirageError::backend_unauthenticated(
                        "Drive materialization requires the repository content key",
                    )
                })?;
                let pack_id = encrypted_frame_pack_id(&frame)?;
                decode_encrypted_frame(
                    &encryption.key,
                    &frame,
                    EncryptedFrameAad {
                        repository: self.repository_id,
                        pack_id,
                        frame_index: location.pack_offset(),
                        plaintext_hash: hash,
                        plaintext_length: page_view.logical_length(),
                    },
                )?
            }
        };
        if bytes.len() != page_view.logical_length() as usize
            || blake3::hash(&bytes).as_bytes() != hash.as_bytes()
        {
            return Err(MirageError::integrity_mismatch(
                "materialized page differs from mount index",
            ));
        }
        let record = self
            .reservations
            .lock()
            .map_err(|_| MirageError::internal_invariant("capsule reservation lock poisoned"))?
            .remove(&hash)
            .ok_or_else(|| {
                MirageError::repository_conflict("capsule page has no durable cache reservation")
            })?;
        let outcome = insert_reserved_page(
            &self.database,
            Arc::clone(&self.shard),
            record,
            hash,
            &bytes,
            &(),
        )?;
        let resident = match outcome {
            InsertOutcome::Inserted(record) | InsertOutcome::Existing(record) => record,
        };
        self.resident.install(resident, Arc::clone(&self.shard))?;
        Ok(bytes.len() as u64)
    }

    async fn checkpoint(&self, progress: MaterializeProgress) -> Result<(), MirageError> {
        write_json_atomic(
            &self.checkpoint_path,
            &json!({
                "format_version": 1,
                "repository_id": self.repository_id.to_string(),
                "generation": self.generation.as_u64(),
                "total_pages": progress.total_pages,
                "already_resident": progress.already_resident,
                "downloaded_pages": progress.downloaded_pages,
                "verified_pages": progress.verified_pages,
                "failed_pages": progress.failed_pages,
                "downloaded_bytes": progress.downloaded_bytes
            }),
        )
    }
}

#[async_trait]
impl AdmissionStore for LocalCapsuleStore {
    fn generation(&self) -> GenerationId {
        self.generation
    }

    async fn state_allows_admission(&self) -> Result<bool, MirageError> {
        Ok(self.database.load_repository_state(self.repository_id)?
            == Some(RepositoryState::AdmittingSession)
            && self
                .database
                .load_active_update(self.repository_id)?
                .is_none())
    }

    async fn all_verified_resident(&self, pages: &RoaringBitmap) -> Result<bool, MirageError> {
        for hash in self.hashes(pages)? {
            if !matches!(
                verify_page(
                    &self.resident,
                    &self.database,
                    hash,
                    IntegrityClass::PinnedClean,
                )?,
                VerifyOutcome::Verified
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn create_session_and_leases(
        &self,
        plan: &CapsulePlan,
    ) -> Result<SessionId, MirageError> {
        let session_id = random_session_id()?;
        let mandatory = self
            .hashes(&plan.mandatory_set)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let hashes = self.hashes(&plan.page_set)?;
        let leases = hashes
            .iter()
            .map(|hash| LeaseSpec {
                page_hash: *hash,
                reason: if mandatory.contains(hash) {
                    "mandatory".to_owned()
                } else {
                    "capsule".to_owned()
                },
            })
            .collect::<Vec<_>>();
        self.database.create_session_with_leases(NewSealedSession {
            session_id,
            repository_id: self.repository_id,
            generation_id: plan.generation,
            capsule_id: Some(plan.capsule_id),
            expected_lease_count: u32::try_from(leases.len())
                .map_err(|_| MirageError::invalid_argument("capsule lease count exceeds u32"))?,
            leases,
            started_at_ns: now_ns(),
        })?;
        *self
            .created_session
            .lock()
            .map_err(|_| MirageError::internal_invariant("session creation lock poisoned"))? =
            Some(session_id);
        Ok(session_id)
    }

    async fn apply_memory_pins(
        &self,
        session: SessionId,
        pages: &RoaringBitmap,
    ) -> Result<(), MirageError> {
        self.resident
            .pins()
            .pin_session(&self.database, session, &self.hashes(pages)?)
    }

    async fn provider_ready(&self) -> Result<bool, MirageError> {
        Ok(self.provider_ready)
    }

    async fn mark_sealed_ready(&self, session: SessionId) -> Result<(), MirageError> {
        self.database.transition_session_state(
            session,
            SessionState::Verifying,
            SessionEvent::VerificationPassed,
            now_ns(),
        )?;
        Ok(())
    }
}

pub(crate) fn open_cache(
    database: &Database,
    page_size: u32,
    cache_bytes: u64,
) -> Result<(Arc<ArenaShard>, Arc<ResidentIndex>), MirageError> {
    let requested_slots = cache_bytes / u64::from(page_size);
    let slot_count = u32::try_from(requested_slots)
        .map_err(|_| MirageError::invalid_argument("cache slot count exceeds u32"))?;
    if slot_count == 0 {
        return Err(MirageError::cache_full(
            "cache budget is smaller than one repository page",
        ));
    }
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(u64::from(page_size)),
        slot_count,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    layout.validate()?;
    let state_root = database
        .reads()
        .database_path()
        .parent()
        .ok_or_else(|| MirageError::internal_invariant("database has no state root"))?;
    let cache_root = state_root.join("cache");
    std::fs::create_dir_all(&cache_root).map_err(MirageError::from)?;
    let shards = database.load_cache_shards()?;
    let spec = match shards.as_slice() {
        [] => {
            let spec = CacheShardSpec {
                shard_id: 0,
                relative_path: "shard-0.bin".to_owned(),
                page_size: layout.page_size,
                slot_count,
            };
            let path = cache_root.join(&spec.relative_path);
            let shard = if path.exists() {
                ArenaShard::open(&path, layout)?
            } else {
                ArenaShard::create(&path, layout)?
            };
            database.register_cache_shard(spec.clone())?;
            let shard = Arc::new(shard);
            let resident = Arc::new(ResidentIndex::rebuild(database, Arc::clone(&shard))?);
            return Ok((shard, resident));
        }
        [spec] if spec.shard_id == 0 => spec.clone(),
        _ => {
            return Err(MirageError::unsupported_layout(
                "service cache currently requires exactly one shard",
            ));
        }
    };
    if spec.page_size != layout.page_size || spec.slot_count != layout.slot_count {
        return Err(MirageError::repository_conflict(
            "configured cache layout differs from the existing service cache",
        ));
    }
    let shard = Arc::new(ArenaShard::open(
        &cache_root.join(&spec.relative_path),
        layout,
    )?);
    let resident = Arc::new(ResidentIndex::rebuild(database, Arc::clone(&shard))?);
    Ok((shard, resident))
}

fn admitted_path(
    database: &Database,
    repository_id: RepositoryId,
    capsule_id: mirage_types::CapsuleId,
) -> Result<PathBuf, MirageError> {
    Ok(repository_state_root(database, repository_id)?
        .join("capsules")
        .join(format!("{capsule_id}.admitted.json")))
}

fn recover_to_mounted(database: &Database, repository_id: RepositoryId, state: RepositoryState) {
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

fn random_session_id() -> Result<SessionId, MirageError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("secure session ID generation failed"))?;
    Ok(SessionId::from_bytes(bytes))
}

fn safe_launch_environment() -> BTreeMap<String, String> {
    ["SystemRoot", "WINDIR", "TEMP", "TMP", "PATH", "USERPROFILE"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect()
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

pub fn load_plan(
    database: &Database,
    repository_id: RepositoryId,
    capsule_id: mirage_types::CapsuleId,
) -> Result<CapsulePlan, MirageError> {
    let path = repository_state_root(database, repository_id)?
        .join("capsules")
        .join(format!("{capsule_id}.json"));
    let bytes = bounded_read(&path, 256 * 1024 * 1024)?;
    let plan: CapsulePlan = serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch("capsule plan is malformed").with_source(error)
    })?;
    let validated = CapsulePlan::new(CapsuleDraft {
        repository_id: plan.repository_id,
        generation: plan.generation,
        profile_key: plan.profile_key.clone(),
        page_set: plan.page_set.clone(),
        mandatory_set: plan.mandatory_set.clone(),
        frontier_set: plan.frontier_set.clone(),
        page_size: plan.page_size,
        risk: plan.risk,
        reasons: plan.reasons.clone(),
    })?;
    if validated != plan || plan.repository_id != repository_id || plan.capsule_id != capsule_id {
        return Err(MirageError::integrity_mismatch(
            "capsule plan identity is invalid",
        ));
    }
    Ok(plan)
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
    let mut hashes = BTreeSet::new();
    for ordinal in 0..index.page_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 pages"))?;
        hashes.insert(index.page_by_ordinal(ordinal)?.plaintext_hash());
    }
    for hash in &hashes {
        if verify_page(&resident, database, *hash, IntegrityClass::Clean)?
            != VerifyOutcome::Verified
        {
            return Err(MirageError::repository_conflict(
                "Explorer volume requires every repository page to be materialized and verified",
            ));
        }
    }
    let config = load_config(database, repository_id)?;
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

fn save_drive_capacity(
    database: &Database,
    repository_id: RepositoryId,
    quota: DriveQuotaSnapshot,
) -> Result<(), MirageError> {
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("volume-capacity.json"),
        &VolumeCapacitySnapshot {
            format_version: 1,
            repository_id,
            provider: "google-drive".to_owned(),
            limit_bytes: quota.limit_bytes,
            usage_bytes: quota.usage_bytes,
            observed_at_ns: now_ns(),
        },
    )
}

pub fn volume_capacity(
    database: &Database,
    repository_id: RepositoryId,
    index_path: &Path,
) -> Result<(u64, u64), MirageError> {
    let path = repository_state_root(database, repository_id)?.join("volume-capacity.json");
    if path.exists() {
        let snapshot: VolumeCapacitySnapshot =
            read_json_bounded(&path, 64 * 1024, "volume capacity snapshot")?;
        if snapshot.format_version != 1
            || snapshot.repository_id != repository_id
            || snapshot.provider != "google-drive"
            || snapshot
                .limit_bytes
                .is_some_and(|limit| limit == 0 || snapshot.usage_bytes > limit)
        {
            return Err(MirageError::integrity_mismatch(
                "volume capacity snapshot is invalid",
            ));
        }
        if let Some(limit) = snapshot.limit_bytes {
            return Ok((limit, limit.saturating_sub(snapshot.usage_bytes)));
        }
    }
    let index = MountIndex::open(index_path)?;
    let mut logical_bytes = 0_u64;
    for ordinal in 0..index.file_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 files"))?;
        logical_bytes = logical_bytes
            .checked_add(index.file_by_index(ordinal)?.logical_size())
            .ok_or_else(|| MirageError::unsupported_layout("volume size overflows u64"))?;
    }
    Ok((logical_bytes.max(1), 0))
}

fn save_config(
    database: &Database,
    repository_id: RepositoryId,
    config: &RuntimeConfig,
) -> Result<(), MirageError> {
    write_json_atomic(
        &repository_state_root(database, repository_id)?.join("runtime.json"),
        config,
    )
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

fn conversion_backup_path(
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

fn conversion_intent_path(
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

fn reconcile_conversion_intent(
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

fn active_generation(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<mirage_db::ActiveGeneration, MirageError> {
    database
        .load_active_generation(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))
}

fn verify_local_import(
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

pub(crate) fn load_drive_manifest(
    config: &RuntimeConfig,
    local_manifest: &RepositoryManifest,
) -> Result<RepositoryManifest, MirageError> {
    let path = config.import_root.join(DRIVE_MANIFEST);
    let manifest = decode_manifest_bounded(
        &bounded_read(&path, DecodeLimits::default().max_input_bytes)?,
        DecodeLimits::default(),
    )?;
    validate_drive_manifest(local_manifest, &manifest)?;
    Ok(manifest)
}

fn validate_drive_manifest(
    local: &RepositoryManifest,
    drive: &RepositoryManifest,
) -> Result<(), MirageError> {
    if local.remote_locations.len() != drive.remote_locations.len() {
        return Err(MirageError::integrity_mismatch(
            "Drive publication location count differs from the verified local manifest",
        ));
    }
    for (local_location, drive_location) in
        local.remote_locations.iter().zip(&drive.remote_locations)
    {
        if drive_location.object.backend_id.as_str() != "drive"
            || drive_location.object.kind != mirage_backend::ObjectKind::Pack
            || drive_location.object.content_hash != local_location.object.content_hash
            || drive_location.object.byte_length != local_location.object.byte_length
            || drive_location.offset != local_location.offset
            || drive_location.encoded_length != local_location.encoded_length
            || drive_location.codec != local_location.codec
        {
            return Err(MirageError::integrity_mismatch(
                "Drive publication differs from the verified local pack layout",
            ));
        }
    }
    let mut normalized = drive.clone();
    normalized.remote_locations = local.remote_locations.clone();
    if &normalized != local {
        return Err(MirageError::integrity_mismatch(
            "Drive publication changes immutable repository content",
        ));
    }
    Ok(())
}

pub(crate) fn drive_backend(
    repository_id: RepositoryId,
    access_token: &str,
) -> Result<(Arc<dyn ObjectBackend>, Arc<RetryingHttpTransport>), MirageError> {
    let native = Arc::new(NativeHttpTransport::new()?);
    let retrying = Arc::new(RetryingHttpTransport::new(native, 5)?);
    let backend = DriveObjectBackend::new(
        retrying.clone(),
        Zeroizing::new(access_token.to_owned()),
        repository_id,
    )?;
    Ok((Arc::new(backend), retrying))
}

fn local_commit_hash(
    manifest: &RepositoryManifest,
    import_root: &Path,
) -> Result<CommitHash, MirageError> {
    let mut objects = BTreeSet::new();
    for location in &manifest.remote_locations {
        objects.insert(location.object.provider_object_id.as_str().to_owned());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD/local-verified-generation/v1\0");
    hasher.update(manifest_hash(manifest)?.as_bytes());
    for object in objects {
        let reader = PackReader::open_verified(&import_root.join(&object))?;
        hasher.update(reader.content_hash().as_bytes());
    }
    Ok(CommitHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn load_profiles(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Vec<GameProfile>, MirageError> {
    let root = repository_state_root(database, repository_id)?.join("profiles");
    let mut paths = std::fs::read_dir(&root)
        .map_err(MirageError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".profile.json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        return Err(MirageError::invalid_argument(
            "repository has no captured profiles",
        ));
    }
    paths
        .into_iter()
        .map(|path| {
            let bytes = bounded_read(&path, 256 * 1024 * 1024)?;
            let profile: GameProfile = serde_json::from_slice(&bytes).map_err(|error| {
                MirageError::integrity_mismatch("stored game profile is malformed")
                    .with_source(error)
            })?;
            profile.validate()?;
            Ok(profile)
        })
        .collect()
}

fn latest_trace_path(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    let root = repository_state_root(database, repository_id)?.join("profiles");
    let mut paths = std::fs::read_dir(root)
        .map_err(MirageError::from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("trace"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .pop()
        .ok_or_else(|| MirageError::invalid_argument("repository has no captured trace"))
}

fn write_trace(
    path: &Path,
    repository_id: RepositoryId,
    generation: GenerationId,
    page_size: u32,
    dropped: u64,
    events: &[TraceEvent],
) -> Result<(), MirageError> {
    let mut bytes = Vec::new();
    let mut encoder = TraceBlockEncoder::new(
        &mut bytes,
        &TraceHeader {
            schema_version: 1,
            repository_id,
            manifest_generation: generation,
            page_size,
            machine_profile: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "windows".into()),
            dropped_event_count: dropped,
        },
    )?;
    for block in events.chunks(65_536) {
        encoder.write_block(block)?;
    }
    let _ = encoder.finish();
    write_atomic(path, &bytes)
}

fn read_trace(
    path: &Path,
    repository_id: RepositoryId,
    generation: GenerationId,
) -> Result<Vec<TraceEvent>, MirageError> {
    let file = File::open(path).map_err(MirageError::from)?;
    let mut decoder = TraceBlockDecoder::new(file)?;
    if decoder.header.repository_id != repository_id
        || decoder.header.manifest_generation != generation
    {
        return Err(MirageError::repository_conflict(
            "stored trace targets a different repository generation",
        ));
    }
    let mut events = Vec::new();
    while let Some(block) = decoder.read_block()? {
        events.extend(block);
    }
    Ok(events)
}

fn file_ordinals(index: &MountIndex) -> Result<BTreeMap<StableFileId, u32>, MirageError> {
    let mut result = BTreeMap::new();
    for ordinal in 0..index.file_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::unsupported_layout("file count exceeds u32"))?;
        result.insert(index.file_by_index(ordinal)?.stable_id(), ordinal);
    }
    Ok(result)
}

fn relative_page_ordinal(
    file: mirage_index::FileView<'_>,
    global: u32,
) -> Result<u32, MirageError> {
    let mut relative = 0_u32;
    for extent_index in 0..file.extent_count() {
        let extent = file.extent(extent_index)?;
        if global >= extent.page_start()
            && global < extent.page_start().saturating_add(extent.page_count())
        {
            return relative
                .checked_add(global - extent.page_start())
                .ok_or_else(|| MirageError::manifest_invalid("relative page ordinal overflows"));
        }
        relative = relative
            .checked_add(extent.page_count())
            .ok_or_else(|| MirageError::manifest_invalid("relative page count overflows"))?;
    }
    Err(MirageError::manifest_invalid(
        "resolved page does not belong to file",
    ))
}

fn global_page_ordinal(index: &MountIndex, key: PageKey) -> Result<u32, MirageError> {
    let file = index.file_by_index(key.file_index)?;
    let mut remaining = key.page_ordinal;
    for extent_index in 0..file.extent_count() {
        let extent = file.extent(extent_index)?;
        if remaining < extent.page_count() {
            return extent
                .page_start()
                .checked_add(remaining)
                .ok_or_else(|| MirageError::manifest_invalid("global page ordinal overflows"));
        }
        remaining -= extent.page_count();
    }
    Err(MirageError::integrity_mismatch(
        "profile page ordinal exceeds its file",
    ))
}

fn held_out_violation_millionths(profiles: &[GameProfile]) -> u32 {
    if profiles.len() < 2 {
        return 1_000_000;
    }
    profiles
        .iter()
        .enumerate()
        .map(|(held_out, profile)| {
            let expected = profile
                .page_observations
                .iter()
                .map(|page| (page.file_index, page.page_ordinal))
                .collect::<BTreeSet<_>>();
            let trained = profiles
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != held_out)
                .flat_map(|(_, profile)| {
                    profile
                        .page_observations
                        .iter()
                        .map(|page| (page.file_index, page.page_ordinal))
                })
                .collect::<BTreeSet<_>>();
            let missing = expected.difference(&trained).count() as u64;
            millionths(missing, expected.len() as u64)
        })
        .max()
        .unwrap_or(1_000_000)
}

fn millionths(numerator: u64, denominator: u64) -> u32 {
    if denominator == 0 {
        return 0;
    }
    u32::try_from(
        (u128::from(numerator).saturating_mul(1_000_000) / u128::from(denominator)).min(1_000_000),
    )
    .unwrap_or(1_000_000)
}

fn validate_runtime_fields(
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

fn validate_mount_subtree(
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

fn require_unmounted(
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

fn ensure_empty_directory(path: &Path, label: &str) -> Result<(), MirageError> {
    ensure_no_reparse(path, label)?;
    if !path.is_dir() {
        return Err(MirageError::repository_conflict(format!(
            "{label} is not a directory"
        )));
    }
    if std::fs::read_dir(path)
        .map_err(MirageError::from)?
        .next()
        .is_some()
    {
        return Err(MirageError::repository_conflict(format!(
            "{label} is not empty"
        )));
    }
    Ok(())
}

fn canonical_absent_target(path: &Path, label: &str) -> Result<PathBuf, MirageError> {
    let parent = path
        .parent()
        .ok_or_else(|| MirageError::invalid_argument(format!("{label} has no parent directory")))?;
    let leaf = path.file_name().ok_or_else(|| {
        MirageError::invalid_argument(format!("{label} has no final path component"))
    })?;
    if !matches!(
        Path::new(leaf).components().next(),
        Some(Component::Normal(_))
    ) {
        return Err(MirageError::invalid_argument(format!(
            "{label} has an unsafe final path component"
        )));
    }
    let canonical_parent = canonical_directory(parent, label)?;
    Ok(canonical_parent.join(leaf))
}

fn validate_label(value: &str, label: &str) -> Result<(), MirageError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(MirageError::invalid_argument(format!("{label} is invalid")))
    } else {
        Ok(())
    }
}

fn validate_launcher(root: &Path, relative: &Path) -> Result<PathBuf, MirageError> {
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
            "launcher path must be a normal repository-relative path without ADS syntax",
        ));
    }
    let launcher = root.join(relative);
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        cursor.push(component.as_os_str());
        ensure_no_reparse(&cursor, "launcher path")?;
    }
    let canonical = launcher.canonicalize().map_err(MirageError::from)?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(MirageError::invalid_argument(
            "launcher escapes the registered native root or is not a file",
        ));
    }
    canonical
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| MirageError::invalid_argument("launcher escapes native root"))
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, MirageError> {
    ensure_no_reparse(path, label)?;
    let canonical = path.canonicalize().map_err(MirageError::from)?;
    if !canonical.is_dir() {
        return Err(MirageError::invalid_argument(format!(
            "{label} is not a directory"
        )));
    }
    ensure_no_reparse(&canonical, label)?;
    Ok(canonical)
}

fn ensure_regular_no_reparse(path: &Path, label: &str) -> Result<(), MirageError> {
    ensure_no_reparse(path, label)?;
    if !path.is_file() {
        return Err(MirageError::invalid_argument(format!(
            "{label} is not a regular file"
        )));
    }
    Ok(())
}

fn ensure_no_reparse(path: &Path, label: &str) -> Result<(), MirageError> {
    let metadata = std::fs::symlink_metadata(path).map_err(MirageError::from)?;
    if metadata.file_type().is_symlink() || is_windows_reparse(&metadata) {
        return Err(MirageError::invalid_argument(format!(
            "{label} contains a reparse point"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn is_windows_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse(_: &std::fs::Metadata) -> bool {
    false
}

fn validate_single_component(value: &str, label: &str) -> Result<(), MirageError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(':')
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(MirageError::invalid_argument(format!(
            "{label} is not a safe file name"
        )));
    }
    Ok(())
}

pub(crate) fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, MirageError> {
    let length = std::fs::metadata(path).map_err(MirageError::from)?.len();
    if length > limit as u64 {
        return Err(MirageError::invalid_argument(
            "artifact exceeds its byte bound",
        ));
    }
    std::fs::read(path).map_err(MirageError::from)
}

fn read_json_bounded<T: for<'de> Deserialize<'de>>(
    path: &Path,
    limit: usize,
    label: &str,
) -> Result<T, MirageError> {
    let bytes = bounded_read(path, limit)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch(format!("{label} is malformed")).with_source(error)
    })
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), MirageError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        MirageError::internal_invariant("runtime artifact serialization failed").with_source(error)
    })?;
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MirageError> {
    mirage_crypto::durable_file::write_atomic(path, bytes)
}

pub(crate) fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_nanos().min(i64::MAX as u128) as i64
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirage_backend::{BackendId, ImmutableRevision, ProviderObjectId};
    use mirage_pack::{ImportPlan, PlannedFile, import_local};

    #[test]
    fn subtree_conversion_is_dry_run_first_and_byte_reversible() {
        let directory = tempfile::tempdir().unwrap();
        let native_root = directory.path().join("game");
        let subtree = native_root.join("assets");
        let import_root = directory.path().join("import");
        std::fs::create_dir_all(&subtree).unwrap();
        std::fs::write(native_root.join("game.exe"), b"native executable").unwrap();
        std::fs::write(subtree.join("content.pak"), b"immutable asset bytes").unwrap();
        let repository_id = RepositoryId::from_bytes([0x42; 16]);
        import_local(&ImportPlan {
            repository_id,
            generation_id: GenerationId::ZERO,
            source_root: subtree.clone(),
            files: vec![PlannedFile {
                relative_path: "content.pak".into(),
                class: mirage_manifest::FileClass::VirtualContainer,
            }],
            page_size: 64 * 1024,
            pack_target: 256 * 1024,
            output_staging_directory: import_root.clone(),
            encryption: None,
        })
        .unwrap();
        let database = Database::open(&directory.path().join("control.db")).unwrap();
        register(
            &database,
            "S-1-5-21-1111111111-2222222222-3333333333-1001",
            RegisterSpec {
                repository_id,
                display_name: "conversion test".into(),
                native_root: native_root.clone(),
                mount_subtree: "assets".into(),
                import_root,
                launcher_relative: "game.exe".into(),
                arguments: Vec::new(),
                version_label: "1".into(),
                configuration_label: "default".into(),
                cache_bytes: 1024 * 1024,
            },
        )
        .unwrap();

        assert_eq!(
            convert(&database, repository_id, false).unwrap()["dry_run"],
            true
        );
        assert_eq!(
            std::fs::read(subtree.join("content.pak")).unwrap(),
            b"immutable asset bytes"
        );

        let config = load_config(&database, repository_id).unwrap();
        let conversion_source = config.native_root.join(&config.mount_subtree);
        let backup = conversion_backup_path(&config, repository_id).unwrap();
        let inventory = directory_inventory(&conversion_source).unwrap();
        let record = ConversionRecord {
            backup_root: backup.clone(),
            inventory_blake3: inventory.blake3,
            file_count: inventory.file_count,
            total_bytes: inventory.total_bytes,
            backup_residency: NativeBackupResidency::Resident,
        };
        write_json_atomic(
            &conversion_intent_path(&database, repository_id).unwrap(),
            &ConversionIntent {
                format_version: 1,
                repository_id,
                operation: ConversionIntentOperation::Convert,
                source: conversion_source.clone(),
                backup: backup.clone(),
                record,
            },
        )
        .unwrap();
        std::fs::rename(&conversion_source, &backup).unwrap();
        std::fs::create_dir(&conversion_source).unwrap();
        assert_eq!(
            convert(&database, repository_id, false).unwrap()["dry_run"],
            true
        );
        assert_eq!(
            std::fs::read(subtree.join("content.pak")).unwrap(),
            b"immutable asset bytes"
        );

        assert_eq!(
            convert(&database, repository_id, true).unwrap()["converted"],
            true
        );
        assert!(!subtree.exists());
        validate_mount_ready(&database, repository_id, &subtree).unwrap();
        assert!(!subtree.exists());
        assert_eq!(
            restore_native(&database, repository_id, false).unwrap()["dry_run"],
            true
        );
        assert_eq!(
            restore_native(&database, repository_id, true).unwrap()["restored_native"],
            true
        );
        assert_eq!(
            std::fs::read(subtree.join("content.pak")).unwrap(),
            b"immutable asset bytes"
        );
        assert_eq!(
            std::fs::read(native_root.join("game.exe")).unwrap(),
            b"native executable"
        );
    }

    #[test]
    fn drive_manifest_can_only_replace_remote_object_identity() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let import = directory.path().join("import");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("content.pak"), b"immutable asset bytes").unwrap();
        let repository_id = RepositoryId::from_bytes([0x51; 16]);
        import_local(&ImportPlan {
            repository_id,
            generation_id: GenerationId::ZERO,
            source_root: source,
            files: vec![PlannedFile {
                relative_path: "content.pak".into(),
                class: mirage_manifest::FileClass::VirtualContainer,
            }],
            page_size: 64 * 1024,
            pack_target: 256 * 1024,
            output_staging_directory: import.clone(),
            encryption: None,
        })
        .unwrap();
        let local = decode_manifest_bounded(
            &std::fs::read(import.join("base-manifest.cbor")).unwrap(),
            DecodeLimits::default(),
        )
        .unwrap();
        let mut drive = local.clone();
        for (index, location) in drive.remote_locations.iter_mut().enumerate() {
            location.object.backend_id = BackendId::new("drive").unwrap();
            location.object.provider_object_id =
                ProviderObjectId::new(format!("drive-file-{index}")).unwrap();
            location.object.immutable_revision =
                Some(ImmutableRevision::new(format!("revision-{index}")).unwrap());
        }
        validate_drive_manifest(&local, &drive).unwrap();
        drive.remote_locations[0].offset += 1;
        assert!(validate_drive_manifest(&local, &drive).is_err());
    }
}
