use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirage_backend::ObjectBackend;
use mirage_crypto::repository_key_store::load_repository_key;
use mirage_db::{Database, SpaceLeaseState};
use mirage_engine::extract_virtual_files_with_encryption;
use mirage_manifest::{DecodeLimits, RepositoryManifest, decode_manifest_bounded};
use mirage_pack::PackReadEncryption;
use mirage_types::{GenerationId, MirageError, RepositoryId, RepositoryState, SpaceLeaseId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{capacity, runtime};

const ACTIVATION_FORMAT_VERSION: u32 = 1;
const ACTIVATION_FILE: &str = "native-activation.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActivationState {
    Materializing,
    Publishing,
    Active,
}

impl ActivationState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Materializing => "materializing",
            Self::Publishing => "publishing",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationRecord {
    format_version: u32,
    repository_id: RepositoryId,
    generation_id: GenerationId,
    lease_id: SpaceLeaseId,
    state: ActivationState,
    target: PathBuf,
    staging: PathBuf,
    required_bytes: u64,
    inventory_blake3: Option<String>,
    file_count: Option<u64>,
    activated_at_ns: Option<i64>,
}

pub(crate) fn required_bytes(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<u64, MirageError> {
    let (_, manifest) = active_manifest(database, repository_id)?;
    virtual_bytes(&manifest)
}

pub(crate) fn activate(
    database: &Database,
    repository_id: RepositoryId,
    lease_id: SpaceLeaseId,
    drive_access_token: &str,
) -> Result<Value, MirageError> {
    let (backend, _transport) = runtime::drive_backend(repository_id, drive_access_token)?;
    activate_with_backend(database, repository_id, lease_id, backend)
}

fn activate_with_backend(
    database: &Database,
    repository_id: RepositoryId,
    lease_id: SpaceLeaseId,
    backend: Arc<dyn ObjectBackend>,
) -> Result<Value, MirageError> {
    if let Some(active) = reconcile(database, repository_id)? {
        return Ok(active);
    }
    let state = database
        .load_repository_state(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
    if state != RepositoryState::ReadyUnmounted {
        return Err(MirageError::repository_conflict(
            "native activation requires a ready, unmounted repository",
        ));
    }
    let config = runtime::load_config(database, repository_id)?;
    if !config.is_converted() {
        return Err(MirageError::repository_conflict(
            "native activation requires a converted immutable subtree",
        ));
    }
    if config.origin != runtime::RuntimeOrigin::Drive {
        return Err(MirageError::repository_conflict(
            "native activation is already local unless the repository uses its verified Drive generation",
        ));
    }
    let lease = database
        .load_space_lease(lease_id)?
        .ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
    if lease.repository_id != repository_id || lease.state != SpaceLeaseState::Consumed {
        return Err(MirageError::repository_conflict(
            "native activation requires a consumed Space Lease for this repository",
        ));
    }

    let (generation_id, local_manifest) = active_manifest(database, repository_id)?;
    let manifest = runtime::load_drive_manifest(&config, &local_manifest)?;
    let required_bytes = virtual_bytes(&manifest)?;
    if required_bytes == 0 || lease.requested_bytes < required_bytes {
        return Err(MirageError::repository_conflict(
            "Space Lease is smaller than the complete native activation",
        ));
    }
    let target = config.native_root.join(&config.mount_subtree);
    let staging = staging_path(&target, repository_id, generation_id)?;
    prepare_absent_target(&target, "native activation target")?;
    prepare_absent_target(&staging, "native activation staging directory")?;

    let key_path = config.import_root.join("repository-key.dpapi");
    let encryption = PackReadEncryption {
        repository_id,
        key: Arc::new(load_repository_key(&key_path, repository_id)?),
    };
    let mut record = ActivationRecord {
        format_version: ACTIVATION_FORMAT_VERSION,
        repository_id,
        generation_id,
        lease_id,
        state: ActivationState::Materializing,
        target: target.clone(),
        staging: staging.clone(),
        required_bytes,
        inventory_blake3: None,
        file_count: None,
        activated_at_ns: None,
    };
    save_record(database, &record)?;

    let result = materialize_and_publish(database, &manifest, backend, &encryption, &mut record);
    if result.is_err() && target.exists() {
        // Publication may have completed immediately before a metadata write failed.
        // Preserve the ordinary NTFS tree and its recovery record unconditionally.
        return result;
    }
    if result.is_err() {
        let _ = safe_remove_staging(&staging);
        let _ = remove_record(database, repository_id);
    }
    result
}

fn materialize_and_publish(
    database: &Database,
    manifest: &RepositoryManifest,
    backend: Arc<dyn ObjectBackend>,
    encryption: &PackReadEncryption,
    record: &mut ActivationRecord,
) -> Result<Value, MirageError> {
    let report = futures_executor::block_on(extract_virtual_files_with_encryption(
        backend.as_ref(),
        manifest,
        &record.staging,
        Some(encryption),
    ))?;
    if report.bytes_written != record.required_bytes {
        return Err(MirageError::integrity_mismatch(
            "native activation wrote a different byte count than the verified manifest",
        ));
    }
    let inventory = runtime::directory_inventory(&record.staging)?;
    if inventory.total_bytes != record.required_bytes
        || inventory.file_count != report.files_written
    {
        return Err(MirageError::integrity_mismatch(
            "native activation inventory differs from extracted content",
        ));
    }
    record.state = ActivationState::Publishing;
    record.inventory_blake3 = Some(inventory.blake3.clone());
    record.file_count = Some(inventory.file_count);
    save_record(database, record)?;
    std::fs::rename(&record.staging, &record.target).map_err(MirageError::from)?;
    record.state = ActivationState::Active;
    record.activated_at_ns = Some(runtime::now_ns());
    save_record(database, record)?;
    release_lease_if_active(database, record.lease_id)?;
    Ok(record_json(record, true))
}

pub(crate) fn status(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Value, MirageError> {
    match reconcile(database, repository_id)? {
        Some(value) => Ok(value),
        None => Ok(json!({
            "repository_id": repository_id.to_string(),
            "active": false,
            "state": "inactive"
        })),
    }
}

/// Replays only transitions that are safe without provider credentials.
/// Partial staging is disposable; an already-published native tree is never removed.
pub(crate) fn reconcile(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Option<Value>, MirageError> {
    let Some(mut record) = load_record(database, repository_id)? else {
        return Ok(None);
    };
    validate_record(database, &record)?;
    match record.state {
        ActivationState::Materializing => {
            if record.target.exists() {
                return Err(MirageError::integrity_mismatch(
                    "native activation target appeared before publication was journaled",
                ));
            }
            safe_remove_staging(&record.staging)?;
            release_lease_if_active(database, record.lease_id)?;
            remove_record(database, repository_id)?;
            Ok(None)
        }
        ActivationState::Publishing => {
            let (root, publish_needed) = match (record.staging.exists(), record.target.exists()) {
                (true, false) => (&record.staging, true),
                (false, true) => (&record.target, false),
                _ => {
                    return Err(MirageError::integrity_mismatch(
                        "native activation publication paths are contradictory",
                    ));
                }
            };
            verify_recorded_inventory(root, &record)?;
            if publish_needed {
                std::fs::rename(&record.staging, &record.target).map_err(MirageError::from)?;
            }
            record.state = ActivationState::Active;
            record.activated_at_ns = Some(runtime::now_ns());
            save_record(database, &record)?;
            release_lease_if_active(database, record.lease_id)?;
            Ok(Some(record_json(&record, true)))
        }
        ActivationState::Active => {
            if !record.target.is_dir() || record.staging.exists() {
                return Err(MirageError::integrity_mismatch(
                    "active native tree is missing or has a stale staging sibling",
                ));
            }
            release_lease_if_active(database, record.lease_id)?;
            Ok(Some(record_json(&record, true)))
        }
    }
}

fn active_manifest(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<(GenerationId, RepositoryManifest), MirageError> {
    let active = database
        .load_active_generation(repository_id)?
        .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))?;
    let bytes = runtime::bounded_read(
        &active.manifest_local_path,
        DecodeLimits::default().max_input_bytes,
    )?;
    let manifest = decode_manifest_bounded(&bytes, DecodeLimits::default())?;
    if manifest.repository_id != repository_id || manifest.generation_id != active.generation_id {
        return Err(MirageError::integrity_mismatch(
            "active manifest identity is contradictory",
        ));
    }
    Ok((active.generation_id, manifest))
}

fn virtual_bytes(manifest: &RepositoryManifest) -> Result<u64, MirageError> {
    manifest
        .files
        .iter()
        .filter(|file| file.class.is_virtual())
        .try_fold(0_u64, |total, file| {
            total
                .checked_add(file.logical_size.as_u64())
                .ok_or_else(|| MirageError::unsupported_layout("native activation size overflows"))
        })
}

fn activation_path(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<PathBuf, MirageError> {
    Ok(runtime::repository_state_root(database, repository_id)?.join(ACTIVATION_FILE))
}

fn staging_path(
    target: &Path,
    repository_id: RepositoryId,
    generation_id: GenerationId,
) -> Result<PathBuf, MirageError> {
    let parent = target
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("native activation target has no parent"))?;
    Ok(parent.join(format!(
        ".mirage-native-{}-{}",
        repository_id,
        generation_id.as_u64()
    )))
}

fn prepare_absent_target(path: &Path, label: &str) -> Result<(), MirageError> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(MirageError::from)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(MirageError::repository_conflict(format!(
            "{label} exists and is not a regular directory"
        )));
    }
    if std::fs::read_dir(path)
        .map_err(MirageError::from)?
        .next()
        .is_some()
    {
        return Err(MirageError::repository_conflict(format!(
            "{label} already contains data"
        )));
    }
    std::fs::remove_dir(path).map_err(MirageError::from)
}

fn safe_remove_staging(path: &Path) -> Result<(), MirageError> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(MirageError::from)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(MirageError::integrity_mismatch(
            "native activation staging path became a reparse point or non-directory",
        ));
    }
    let _ = runtime::directory_inventory(path)?;
    std::fs::remove_dir_all(path).map_err(MirageError::from)
}

fn verify_recorded_inventory(root: &Path, record: &ActivationRecord) -> Result<(), MirageError> {
    let inventory = runtime::directory_inventory(root)?;
    if record.inventory_blake3.as_deref() != Some(inventory.blake3.as_str())
        || record.file_count != Some(inventory.file_count)
        || inventory.total_bytes != record.required_bytes
    {
        return Err(MirageError::integrity_mismatch(
            "journaled native activation inventory no longer matches its files",
        ));
    }
    Ok(())
}

fn save_record(database: &Database, record: &ActivationRecord) -> Result<(), MirageError> {
    runtime::write_json_atomic(&activation_path(database, record.repository_id)?, record)
}

fn load_record(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Option<ActivationRecord>, MirageError> {
    let path = activation_path(database, repository_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = runtime::bounded_read(&path, 1024 * 1024)?;
    let record: ActivationRecord = serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch("native activation journal is invalid").with_source(error)
    })?;
    Ok(Some(record))
}

fn remove_record(database: &Database, repository_id: RepositoryId) -> Result<(), MirageError> {
    let path = activation_path(database, repository_id)?;
    if path.exists() {
        std::fs::remove_file(path).map_err(MirageError::from)?;
    }
    Ok(())
}

fn validate_record(database: &Database, record: &ActivationRecord) -> Result<(), MirageError> {
    if record.format_version != ACTIVATION_FORMAT_VERSION || record.required_bytes == 0 {
        return Err(MirageError::unsupported_layout(
            "native activation journal version or size is invalid",
        ));
    }
    let config = runtime::load_config(database, record.repository_id)?;
    let active = database
        .load_active_generation(record.repository_id)?
        .ok_or_else(|| {
            MirageError::integrity_mismatch("native activation generation is missing")
        })?;
    let expected_target = config.native_root.join(&config.mount_subtree);
    let expected_staging =
        staging_path(&expected_target, record.repository_id, active.generation_id)?;
    if !config.is_converted()
        || record.generation_id != active.generation_id
        || record.target != expected_target
        || record.staging != expected_staging
    {
        return Err(MirageError::integrity_mismatch(
            "native activation journal does not match current repository configuration",
        ));
    }
    Ok(())
}

fn release_lease_if_active(database: &Database, lease_id: SpaceLeaseId) -> Result<(), MirageError> {
    let Some(lease) = database.load_space_lease(lease_id)? else {
        return Err(MirageError::integrity_mismatch(
            "native activation Space Lease disappeared",
        ));
    };
    if lease.state.active() {
        capacity::release(database, lease.repository_id, lease_id)?;
    }
    Ok(())
}

fn record_json(record: &ActivationRecord, active: bool) -> Value {
    json!({
        "repository_id": record.repository_id.to_string(),
        "generation": record.generation_id.as_u64(),
        "state": record.state.as_str(),
        "active": active,
        "ordinary_ntfs": active,
        "target": record.target,
        "required_bytes": record.required_bytes,
        "file_count": record.file_count,
        "inventory_blake3": record.inventory_blake3,
        "survives_service_or_drive_outage": active
    })
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

// This end-to-end fixture persists its repository key with Windows DPAPI.
#[cfg(all(test, windows))]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use bytes::Bytes;
    use mirage_backend::{
        BackendByteStream, BackendError, BackendId, BackendRead, BackendResponseMetadata,
        DeletionProof, FetchClass, ImmutableRevision, ObjectKind, ObjectStat, ProviderObjectId,
        RemoteObjectRef, UploadSource,
    };
    use mirage_crypto::{
        aead::RepositoryKey, dpapi::ProtectionScope, repository_key_store::save_repository_key,
    };
    use mirage_db::{NewSpaceLease, SpaceLeaseEvent};
    use mirage_manifest::{FileClass, encode_manifest};
    use mirage_pack::{ImportPlan, PackEncryption, PlannedFile, import_local};
    use mirage_types::{BackendHealthState, ByteCount, CheckedRange, ContentHash};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::runtime::{RegisterSpec, convert, register, set_drive_origin};

    #[derive(Default)]
    struct ReadOnlyBackend {
        objects: Mutex<BTreeMap<String, Bytes>>,
    }

    #[async_trait]
    impl ObjectBackend for ReadOnlyBackend {
        async fn read_range(
            &self,
            object: &RemoteObjectRef,
            range: CheckedRange,
            _class: FetchClass,
            cancel: CancellationToken,
        ) -> Result<BackendRead, BackendError> {
            if cancel.is_cancelled() {
                return Err(BackendError::permanent("cancelled"));
            }
            let objects = self.objects.lock().unwrap();
            let bytes = objects
                .get(object.provider_object_id.as_str())
                .ok_or_else(|| BackendError::missing("missing test object"))?;
            let start = usize::try_from(range.start())
                .map_err(|_| BackendError::permanent("range start overflows"))?;
            let end = usize::try_from(range.end_exclusive())
                .map_err(|_| BackendError::permanent("range end overflows"))?;
            let value = bytes
                .get(start..end)
                .ok_or_else(|| BackendError::permanent("range exceeds test object"))?;
            BackendRead::new(
                range,
                ByteCount::from_u64(value.len() as u64),
                BackendResponseMetadata {
                    observed_revision: object.immutable_revision.clone(),
                    ..BackendResponseMetadata::default()
                },
                BackendByteStream::from_bytes(Bytes::copy_from_slice(value)),
            )
        }

        async fn put_immutable(
            &self,
            _kind: ObjectKind,
            _source: UploadSource,
            _expected_hash: ContentHash,
            _cancel: CancellationToken,
        ) -> Result<RemoteObjectRef, BackendError> {
            Err(BackendError::permanent("read-only test backend"))
        }

        async fn stat(&self, _object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
            Err(BackendError::permanent("unused test stat"))
        }

        async fn enumerate_commits(
            &self,
            _repository: RepositoryId,
        ) -> Result<Vec<RemoteObjectRef>, BackendError> {
            Ok(Vec::new())
        }

        async fn delete_immutable(
            &self,
            _object: &RemoteObjectRef,
            _proof: &DeletionProof,
            _cancel: CancellationToken,
        ) -> Result<(), BackendError> {
            Err(BackendError::permanent("read-only test backend"))
        }

        async fn health(&self) -> BackendHealthState {
            BackendHealthState::Healthy
        }
    }

    #[test]
    fn verified_drive_generation_becomes_an_ordinary_crash_resilient_tree() {
        let directory = tempfile::tempdir().unwrap();
        let native_root = directory.path().join("application");
        let source = native_root.join("assets");
        let import_root = directory.path().join("import");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(native_root.join("application.exe"), b"native launcher").unwrap();
        std::fs::write(
            source.join("content.bin"),
            b"verified native activation bytes",
        )
        .unwrap();

        let repository_id = RepositoryId::from_bytes([0x91; 16]);
        let generation_id = GenerationId::ZERO;
        std::fs::create_dir_all(&import_root).unwrap();
        let key = RepositoryKey::generate().unwrap();
        save_repository_key(
            &import_root.join("repository-key.dpapi"),
            repository_id,
            &key,
            ProtectionScope::LocalMachine,
        )
        .unwrap();
        let imported = import_local(&ImportPlan {
            repository_id,
            generation_id,
            source_root: source.clone(),
            files: vec![PlannedFile {
                relative_path: "content.bin".into(),
                class: FileClass::VirtualAsset,
            }],
            page_size: 64 * 1024,
            pack_target: 256 * 1024,
            output_staging_directory: import_root.clone(),
            encryption: Some(PackEncryption {
                repository_id,
                key: Arc::new(key),
            }),
        })
        .unwrap();
        let database = Database::open(&directory.path().join("control.db")).unwrap();
        register(
            &database,
            "S-1-5-21-1111111111-2222222222-3333333333-1001",
            RegisterSpec {
                repository_id,
                display_name: "native activation test".into(),
                native_root: native_root.clone(),
                mount_subtree: "assets".into(),
                import_root: import_root.clone(),
                launcher_relative: "application.exe".into(),
                arguments: Vec::new(),
                version_label: "1".into(),
                configuration_label: "default".into(),
                cache_bytes: 1024 * 1024,
            },
        )
        .unwrap();

        let backend = Arc::new(ReadOnlyBackend::default());
        let mut drive_manifest = imported.manifest.clone();
        let mut mapped = BTreeMap::<[u8; 32], RemoteObjectRef>::new();
        let mut next_object = 0_u64;
        for location in &mut drive_manifest.remote_locations {
            let hash = *location.object.content_hash.as_bytes();
            let reference = mapped.entry(hash).or_insert_with(|| {
                let id = format!("drive-object-{next_object}");
                next_object += 1;
                let bytes =
                    std::fs::read(import_root.join(location.object.provider_object_id.as_str()))
                        .unwrap();
                backend
                    .objects
                    .lock()
                    .unwrap()
                    .insert(id.clone(), bytes.into());
                RemoteObjectRef {
                    backend_id: BackendId::new("drive").unwrap(),
                    provider_object_id: ProviderObjectId::new(id).unwrap(),
                    immutable_revision: Some(ImmutableRevision::new("immutable-revision").unwrap()),
                    ..location.object.clone()
                }
            });
            location.object = reference.clone();
        }
        std::fs::write(
            import_root.join("drive-manifest.cbor"),
            encode_manifest(&drive_manifest).unwrap(),
        )
        .unwrap();
        set_drive_origin(&database, repository_id, true).unwrap();
        convert(&database, repository_id, true).unwrap();

        let needed = required_bytes(&database, repository_id).unwrap();
        let lease_id = SpaceLeaseId::from_bytes([0x33; 16]);
        let at_ns = runtime::now_ns();
        let volume = crate::disk_space::query(&native_root).unwrap();
        database
            .create_space_lease(NewSpaceLease {
                lease_id,
                repository_id,
                target_volume_id: volume.volume_id,
                requested_bytes: needed,
                planned_reclaim_bytes: 0,
                created_at_ns: at_ns,
                expires_at_ns: at_ns + 60_000_000_000,
            })
            .unwrap();
        database
            .transition_space_lease(
                lease_id,
                SpaceLeaseState::Preparing,
                SpaceLeaseEvent::ReclaimFinished,
                at_ns + 1,
            )
            .unwrap();
        database
            .transition_space_lease(
                lease_id,
                SpaceLeaseState::Ready,
                SpaceLeaseEvent::Consume,
                at_ns + 2,
            )
            .unwrap();

        let response = activate_with_backend(&database, repository_id, lease_id, backend).unwrap();
        assert_eq!(response["ordinary_ntfs"], true);
        assert_eq!(
            std::fs::read(source.join("content.bin")).unwrap(),
            b"verified native activation bytes"
        );
        assert_eq!(
            database.load_space_lease(lease_id).unwrap().unwrap().state,
            SpaceLeaseState::Released
        );
        assert_eq!(status(&database, repository_id).unwrap()["active"], true);
    }
}
