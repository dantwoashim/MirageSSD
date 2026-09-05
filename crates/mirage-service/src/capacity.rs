use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use mirage_backend::{ObjectBackend, RemoteObjectRef};
use mirage_cache::{
    ArenaShard, CacheLayout, CapacitySnapshot, PinReason, ReclaimCandidate, ReclaimId,
    ResidentIndex, SpaceLeasePlan, plan_space_lease,
};
use mirage_crypto::repository_key_store::load_repository_key;
use mirage_db::{CacheSlotRecord, Database, SpaceLeaseRecord, SpaceLeaseState};
use mirage_engine::space_lease::{
    PreparedSpaceLease, SpaceLeaseRequest, SpaceLeaseSource, consume_space_lease,
    prepare_space_lease, release_space_lease,
};
use mirage_index::MountIndex;
use mirage_manifest::{DecodeLimits, decode_manifest_bounded};
use mirage_pack::{PackReadEncryption, PackReader};
use mirage_types::{ByteCount, GenerationId, MirageError, PageHash, RepositoryId, SpaceLeaseId};
use serde_json::{Value, json};

use crate::disk_space::{self, VolumeSpace};
use crate::runtime::{self, RuntimeConfig, RuntimeOrigin};

const RESERVE_FLOOR_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const RESERVE_CEILING_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Clone)]
enum RemotePageProof {
    Local {
        object_id: String,
        object_hash: [u8; 32],
        logical_length: u32,
    },
    Drive {
        object: RemoteObjectRef,
        logical_length: u32,
    },
}

#[derive(Clone)]
enum ReclaimUnit {
    CachePage(PageHash),
    DriveShadowPack {
        path: PathBuf,
        object: RemoteObjectRef,
        physical_bytes: u64,
        last_write_sequence: u64,
    },
    NativeBackup {
        info: runtime::NativeBackupInfo,
        physical_bytes: u64,
        last_write_sequence: u64,
    },
}

pub(crate) struct RepositoryCapacitySource {
    database: Database,
    repository_id: RepositoryId,
    generation_id: GenerationId,
    config: RuntimeConfig,
    target: VolumeSpace,
    cache_on_target_volume: bool,
    shard: Option<Arc<ArenaShard>>,
    resident: Option<Arc<ResidentIndex>>,
    proofs: BTreeMap<PageHash, RemotePageProof>,
    drive_objects: BTreeMap<[u8; 32], RemoteObjectRef>,
    reclaim_units: BTreeMap<ReclaimId, ReclaimUnit>,
    drive_backend: Option<Arc<dyn ObjectBackend>>,
    verified_drive_objects: Mutex<BTreeSet<[u8; 32]>>,
    encryption: Option<PackReadEncryption>,
    reserve_bytes: u64,
}

impl RepositoryCapacitySource {
    pub(crate) fn open(
        database: &Database,
        repository_id: RepositoryId,
        drive_access_token: Option<&str>,
    ) -> Result<Self, MirageError> {
        Self::open_with_backend(database, repository_id, drive_access_token, None)
    }

    fn open_with_backend(
        database: &Database,
        repository_id: RepositoryId,
        drive_access_token: Option<&str>,
        drive_backend_override: Option<Arc<dyn ObjectBackend>>,
    ) -> Result<Self, MirageError> {
        runtime::reconcile_native_backup_eviction(database, repository_id)?;
        let config = runtime::load_config(database, repository_id)?;
        let active = database
            .load_active_generation(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))?;
        let index = MountIndex::open(active.mount_index_path.as_ref().ok_or_else(|| {
            MirageError::integrity_mismatch("active generation has no compiled mount index")
        })?)?;
        if index.header().repository_id != repository_id
            || index.header().generation_id != active.generation_id
        {
            return Err(MirageError::integrity_mismatch(
                "active mount index identity is contradictory",
            ));
        }

        let target = disk_space::query(&config.native_root)?;
        let reserve_bytes = target
            .total_bytes
            .checked_div(20)
            .unwrap_or(0)
            .max(RESERVE_FLOOR_BYTES.min(target.total_bytes / 10))
            .min(RESERVE_CEILING_BYTES);

        let key_path = config.import_root.join("repository-key.dpapi");
        let encryption = key_path
            .is_file()
            .then(|| {
                Ok::<_, MirageError>(PackReadEncryption {
                    repository_id,
                    key: Arc::new(load_repository_key(&key_path, repository_id)?),
                })
            })
            .transpose()?;

        let manifest_bytes = bounded_read(
            &active.manifest_local_path,
            DecodeLimits::default().max_input_bytes,
        )?;
        let local_manifest = decode_manifest_bounded(&manifest_bytes, DecodeLimits::default())?;
        if local_manifest.repository_id != repository_id
            || local_manifest.generation_id != active.generation_id
        {
            return Err(MirageError::integrity_mismatch(
                "active local manifest identity is contradictory",
            ));
        }

        let (drive_objects, drive_backend) = match config.origin {
            RuntimeOrigin::Local => {
                if drive_access_token.is_some() || drive_backend_override.is_some() {
                    return Err(MirageError::invalid_argument(
                        "Drive credentials were supplied for a local-origin repository",
                    ));
                }
                (BTreeMap::new(), None)
            }
            RuntimeOrigin::Drive => {
                if encryption.is_none() {
                    return Err(MirageError::backend_unauthenticated(
                        "Drive capacity reclaim requires the protected repository content key",
                    ));
                }
                let drive_manifest = runtime::load_drive_manifest(&config, &local_manifest)?;
                let mut objects = BTreeMap::new();
                for location in drive_manifest.remote_locations {
                    let hash = *location.object.content_hash.as_bytes();
                    if let Some(previous) = objects.insert(hash, location.object.clone())
                        && previous != location.object
                    {
                        return Err(MirageError::integrity_mismatch(
                            "Drive publication maps one content hash to conflicting objects",
                        ));
                    }
                }
                let backend = match drive_backend_override {
                    Some(backend) => {
                        if drive_access_token.is_some() {
                            return Err(MirageError::invalid_argument(
                                "Drive backend and access token cannot both be supplied",
                            ));
                        }
                        Some(backend)
                    }
                    None => drive_access_token
                        .map(|token| runtime::drive_backend(repository_id, token))
                        .transpose()?
                        .map(|(backend, _transport)| backend),
                };
                (objects, backend)
            }
        };

        let mut proofs = BTreeMap::new();
        for ordinal in 0..index.page_count() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| MirageError::unsupported_layout("mount index exceeds u32 pages"))?;
            let page = index.page_by_ordinal(ordinal)?;
            let location = page.remote_location()?;
            let proof = match config.origin {
                RuntimeOrigin::Local => {
                    if location.backend_id()? != "local" {
                        return Err(MirageError::integrity_mismatch(
                            "local-origin index references a non-local backend",
                        ));
                    }
                    let object_id = location.provider_object_id()?;
                    validate_object_component(object_id)?;
                    RemotePageProof::Local {
                        object_id: object_id.to_owned(),
                        object_hash: location.object_hash(),
                        logical_length: page.logical_length(),
                    }
                }
                RuntimeOrigin::Drive => {
                    let object = drive_objects.get(&location.object_hash()).ok_or_else(|| {
                        MirageError::integrity_mismatch(
                            "Drive publication omits an indexed pack object",
                        )
                    })?;
                    RemotePageProof::Drive {
                        object: object.clone(),
                        logical_length: page.logical_length(),
                    }
                }
            };
            match proofs.entry(page.plaintext_hash()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(proof);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    if proof_logical_length(entry.get()) != page.logical_length() {
                        return Err(MirageError::integrity_mismatch(
                            "deduplicated page has conflicting logical lengths",
                        ));
                    }
                }
            }
        }

        let mut reclaim_units = proofs
            .keys()
            .map(|hash| (cache_reclaim_id(*hash), ReclaimUnit::CachePage(*hash)))
            .collect::<BTreeMap<_, _>>();
        if config.origin == RuntimeOrigin::Drive {
            let import_volume = disk_space::query(&config.import_root)?;
            if import_volume.volume_id == target.volume_id {
                let mut seen = BTreeSet::new();
                for location in &local_manifest.remote_locations {
                    if location.object.backend_id.as_str() != "local"
                        || location.object.kind != mirage_backend::ObjectKind::Pack
                    {
                        return Err(MirageError::integrity_mismatch(
                            "local manifest contains an unsupported shadow object",
                        ));
                    }
                    let content_hash = *location.object.content_hash.as_bytes();
                    if !seen.insert(content_hash) {
                        continue;
                    }
                    let object_id = location.object.provider_object_id.as_str();
                    validate_object_component(object_id)?;
                    let path = config.import_root.join(object_id);
                    if !path.is_file() {
                        continue;
                    }
                    let object = drive_objects.get(&content_hash).ok_or_else(|| {
                        MirageError::integrity_mismatch(
                            "Drive publication omits a local shadow object",
                        )
                    })?;
                    let physical_bytes = disk_space::allocated_file_bytes(&path)?;
                    reclaim_units.insert(
                        pack_reclaim_id(content_hash),
                        ReclaimUnit::DriveShadowPack {
                            path: path.clone(),
                            object: object.clone(),
                            physical_bytes,
                            last_write_sequence: last_write_sequence(&path)?,
                        },
                    );
                }
            }
            if let Some(info) = runtime::native_backup_info(&config)
                && info.path.is_dir()
            {
                let backup_volume = disk_space::query(&info.path)?;
                if backup_volume.volume_id == target.volume_id {
                    let allocation = disk_space::allocated_tree(&info.path)?;
                    if allocation.physical_bytes > 0 {
                        reclaim_units.insert(
                            native_backup_reclaim_id(
                                repository_id,
                                active.generation_id,
                                &info.inventory_blake3,
                            ),
                            ReclaimUnit::NativeBackup {
                                info,
                                physical_bytes: allocation.physical_bytes,
                                last_write_sequence: allocation.latest_write_sequence,
                            },
                        );
                    }
                }
            }
        }
        let (shard, resident, cache_on_target_volume) =
            open_existing_cache(database, &index, &target)?;
        Ok(Self {
            database: database.clone(),
            repository_id,
            generation_id: active.generation_id,
            config,
            target,
            cache_on_target_volume,
            shard,
            resident,
            proofs,
            drive_objects,
            reclaim_units,
            drive_backend,
            verified_drive_objects: Mutex::new(BTreeSet::new()),
            encryption,
            reserve_bytes,
        })
    }

    pub(crate) fn plan(&self, requested_bytes: u64) -> Result<SpaceLeasePlan, MirageError> {
        let at_ns = now_ns()?;
        plan_space_lease(
            &CapacitySnapshot {
                physical_free_bytes: self.physical_free_bytes()?,
                filesystem_reserve_bytes: self.filesystem_reserve_bytes()?,
                outstanding_space_lease_bytes: self
                    .database
                    .active_space_lease_bytes_for_volume(&self.target.volume_id, at_ns)?,
                candidates: self.reclaim_candidates()?,
            },
            requested_bytes,
        )
    }

    pub(crate) fn cache_on_target_volume(&self) -> bool {
        self.cache_on_target_volume
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.target.total_bytes
    }

    pub(crate) fn total_free_bytes(&self) -> u64 {
        self.target.total_free_bytes
    }

    pub(crate) fn origin(&self) -> &'static str {
        match self.config.origin {
            RuntimeOrigin::Local => "local",
            RuntimeOrigin::Drive => "drive",
        }
    }

    pub(crate) fn drive_authenticated(&self) -> bool {
        self.drive_backend.is_some()
    }

    fn verify_remote(&self, hash: PageHash) -> Result<(), MirageError> {
        let proof = self.proofs.get(&hash).ok_or_else(|| {
            MirageError::repository_conflict("resident page is absent from the active generation")
        })?;
        match proof {
            RemotePageProof::Local {
                object_id,
                object_hash,
                logical_length,
            } => {
                let path = self.config.import_root.join(object_id);
                let mut reader = match &self.encryption {
                    Some(encryption) => {
                        PackReader::open_verified_encrypted(&path, encryption.clone())?
                    }
                    None => PackReader::open_verified(&path)?,
                };
                if reader.content_hash().as_bytes() != object_hash {
                    return Err(MirageError::integrity_mismatch(
                        "local pack hash differs from the active mount index",
                    ));
                }
                let decoded = reader.read_page(hash)?;
                if decoded.page.bytes.len() != *logical_length as usize {
                    return Err(MirageError::integrity_mismatch(
                        "local recovery page length differs from the active mount index",
                    ));
                }
                Ok(())
            }
            RemotePageProof::Drive {
                object,
                logical_length: _,
            } => self.verify_drive_object(object),
        }
    }

    fn verify_drive_object(&self, object: &RemoteObjectRef) -> Result<(), MirageError> {
        let backend = self.drive_backend.as_ref().ok_or_else(|| {
            MirageError::backend_unauthenticated(
                "Drive reclaim requires authenticated client credentials",
            )
        })?;
        let key = *object.content_hash.as_bytes();
        let mut verified = self.verified_drive_objects.lock().map_err(|_| {
            MirageError::internal_invariant("Drive verification cache lock poisoned")
        })?;
        if !verified.contains(&key) {
            futures_executor::block_on(backend.stat(object)).map_err(MirageError::from)?;
            verified.insert(key);
        }
        Ok(())
    }

    fn live_candidate(&self, hash: PageHash) -> Result<ReclaimCandidate, MirageError> {
        let shard = self.shard.as_ref().ok_or_else(|| {
            MirageError::cache_full("repository has no existing local cache shard")
        })?;
        let resident = self
            .resident
            .as_ref()
            .ok_or_else(|| MirageError::cache_full("repository has no resident cache index"))?;
        let record = self
            .database
            .load_resident_cache_slots()?
            .into_iter()
            .find(|record| record.page_hash == Some(hash))
            .ok_or_else(|| MirageError::cache_full("reclaim candidate is no longer resident"))?;
        candidate_for(
            record,
            shard,
            resident,
            &self.proofs,
            self.drive_backend.is_some(),
        )
    }
}

impl SpaceLeaseSource for RepositoryCapacitySource {
    fn target_volume_id(&self) -> Result<String, MirageError> {
        Ok(self.target.volume_id.clone())
    }

    fn physical_free_bytes(&self) -> Result<u64, MirageError> {
        let current = disk_space::query(&self.config.native_root)?;
        if current.volume_id != self.target.volume_id {
            return Err(MirageError::repository_conflict(
                "capacity target moved to a different physical volume",
            ));
        }
        Ok(current.available_bytes)
    }

    fn filesystem_reserve_bytes(&self) -> Result<u64, MirageError> {
        Ok(self.reserve_bytes)
    }

    fn reclaim_candidates(&self) -> Result<Vec<ReclaimCandidate>, MirageError> {
        let mut candidates = Vec::new();
        if self.cache_on_target_volume
            && let (Some(shard), Some(resident)) = (&self.shard, &self.resident)
        {
            candidates.extend(
                self.database
                    .load_resident_cache_slots()?
                    .into_iter()
                    .filter(|record| record.page_hash.is_some())
                    .map(|record| {
                        candidate_for(
                            record,
                            shard,
                            resident,
                            &self.proofs,
                            self.drive_backend.is_some(),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        for (reclaim_id, unit) in &self.reclaim_units {
            match unit {
                ReclaimUnit::DriveShadowPack {
                    physical_bytes,
                    last_write_sequence,
                    ..
                }
                | ReclaimUnit::NativeBackup {
                    physical_bytes,
                    last_write_sequence,
                    ..
                } => candidates.push(ReclaimCandidate {
                    reclaim_id: *reclaim_id,
                    physical_bytes: *physical_bytes,
                    last_access_sequence: *last_write_sequence,
                    remote_verified: self.drive_backend.is_some(),
                    dirty: false,
                    pinned: false,
                    active_read_leases: 0,
                }),
                ReclaimUnit::CachePage(_) => {}
            }
        }
        Ok(candidates)
    }

    fn reclaim_verified_clean(&self, candidate: ReclaimCandidate) -> Result<(), MirageError> {
        let unit = self
            .reclaim_units
            .get(&candidate.reclaim_id)
            .ok_or_else(|| MirageError::cache_full("reclaim candidate is no longer managed"))?;
        match unit {
            ReclaimUnit::CachePage(hash) => {
                let live = self.live_candidate(*hash)?;
                if !live.reclaimable() || live.physical_bytes < candidate.physical_bytes {
                    return Err(MirageError::cache_full(
                        "reclaim candidate changed after capacity planning",
                    ));
                }
                self.verify_remote(*hash)?;
                let resident = self.resident.as_ref().ok_or_else(|| {
                    MirageError::internal_invariant("resident cache index disappeared")
                })?;
                if !resident.evict(&self.database, *hash)? {
                    return Err(MirageError::cache_full(
                        "reclaim candidate disappeared before deallocation",
                    ));
                }
            }
            ReclaimUnit::DriveShadowPack {
                path,
                object,
                physical_bytes: _,
                last_write_sequence: _,
            } => {
                let current_config = runtime::load_config(&self.database, self.repository_id)?;
                let current_generation = self
                    .database
                    .load_active_generation(self.repository_id)?
                    .ok_or_else(|| {
                        MirageError::repository_conflict(
                            "repository lost its active generation during reclaim",
                        )
                    })?;
                if current_config.origin != RuntimeOrigin::Drive
                    || current_config.import_root != self.config.import_root
                    || current_generation.generation_id != self.generation_id
                    || path.parent() != Some(self.config.import_root.as_path())
                {
                    return Err(MirageError::repository_conflict(
                        "repository origin or generation changed during reclaim",
                    ));
                }
                if !path.is_file()
                    || disk_space::query(path.parent().ok_or_else(|| {
                        MirageError::integrity_mismatch("shadow pack has no parent directory")
                    })?)?
                    .volume_id
                        != self.target.volume_id
                    || disk_space::allocated_file_bytes(path)? < candidate.physical_bytes
                {
                    return Err(MirageError::cache_full(
                        "Drive shadow pack changed after capacity planning",
                    ));
                }
                self.verify_drive_object(object)?;
                {
                    let encryption = self.encryption.clone().ok_or_else(|| {
                        MirageError::backend_unauthenticated(
                            "Drive shadow reclaim requires the repository content key",
                        )
                    })?;
                    let reader = PackReader::open_verified_encrypted(path, encryption)?;
                    if reader.content_hash() != object.content_hash {
                        return Err(MirageError::integrity_mismatch(
                            "local shadow pack identity differs from its verified Drive object",
                        ));
                    }
                }
                std::fs::remove_file(path).map_err(MirageError::from)?;
            }
            ReclaimUnit::NativeBackup {
                info,
                physical_bytes: _,
                last_write_sequence: _,
            } => {
                let current_config = runtime::load_config(&self.database, self.repository_id)?;
                let current = runtime::native_backup_info(&current_config).ok_or_else(|| {
                    MirageError::cache_full("native backup is no longer resident")
                })?;
                let allocation = disk_space::allocated_tree(&current.path)?;
                if current.path != info.path
                    || current.inventory_blake3 != info.inventory_blake3
                    || current.file_count != info.file_count
                    || current.total_bytes != info.total_bytes
                    || allocation.physical_bytes < candidate.physical_bytes
                    || disk_space::query(&current.path)?.volume_id != self.target.volume_id
                {
                    return Err(MirageError::cache_full(
                        "native backup changed after capacity planning",
                    ));
                }
                for object in self.drive_objects.values() {
                    self.verify_drive_object(object)?;
                }
                runtime::evict_verified_native_backup(&self.database, self.repository_id)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn plan_response(
    database: &Database,
    repository_id: RepositoryId,
    requested_bytes: u64,
    drive_access_token: Option<&str>,
) -> Result<ResponsePlan, MirageError> {
    validate_request_bytes(requested_bytes)?;
    let source = RepositoryCapacitySource::open(database, repository_id, drive_access_token)?;
    let plan = source.plan(requested_bytes)?;
    Ok(ResponsePlan { source, plan })
}

pub(crate) struct ResponsePlan {
    source: RepositoryCapacitySource,
    plan: SpaceLeasePlan,
}

impl ResponsePlan {
    pub(crate) fn json(&self) -> Value {
        plan_json(&self.source, &self.plan)
    }
}

pub(crate) fn acquire(
    database: &Database,
    repository_id: RepositoryId,
    lease_id: SpaceLeaseId,
    requested_bytes: u64,
    lifetime_seconds: u64,
    drive_access_token: Option<&str>,
) -> Result<Value, MirageError> {
    validate_request_bytes(requested_bytes)?;
    if !(30..=86_400).contains(&lifetime_seconds) {
        return Err(MirageError::invalid_argument(
            "Space Lease lifetime must be between 30 seconds and 24 hours",
        ));
    }
    let source = RepositoryCapacitySource::open(database, repository_id, drive_access_token)?;
    let expires_at_ns = now_ns()?
        .checked_add(
            i64::try_from(lifetime_seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(1_000_000_000))
                .ok_or_else(|| MirageError::invalid_argument("Space Lease lifetime overflows"))?,
        )
        .ok_or_else(|| MirageError::invalid_argument("Space Lease expiry overflows"))?;
    let prepared = prepare_space_lease(
        database,
        &source,
        SpaceLeaseRequest {
            lease_id,
            repository_id,
            requested_bytes,
            expires_at_ns,
        },
    )?;
    Ok(prepared_json(&source, &prepared))
}

pub(crate) fn status(
    database: &Database,
    repository_id: RepositoryId,
    lease_id: Option<SpaceLeaseId>,
) -> Result<Value, MirageError> {
    let source = RepositoryCapacitySource::open(database, repository_id, None)?;
    let now = now_ns()?;
    let active_promised_bytes =
        database.active_space_lease_bytes_for_volume(&source.target.volume_id, now)?;
    let lease = lease_id
        .map(|id| {
            let record = database
                .load_space_lease(id)?
                .ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
            require_repository(&record, repository_id)?;
            Ok::<_, MirageError>(lease_json(&record, now))
        })
        .transpose()?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "target_volume_id": source.target.volume_id,
        "physical_free_bytes": source.physical_free_bytes()?,
        "filesystem_reserve_bytes": source.filesystem_reserve_bytes()?,
        "active_promised_bytes": active_promised_bytes,
        "cache_on_target_volume": source.cache_on_target_volume,
        "lease": lease
    }))
}

pub(crate) fn release(
    database: &Database,
    repository_id: RepositoryId,
    lease_id: SpaceLeaseId,
) -> Result<Value, MirageError> {
    let record = database
        .load_space_lease(lease_id)?
        .ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
    require_repository(&record, repository_id)?;
    let state = release_space_lease(database, lease_id)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "lease_id": lease_id.to_string(),
        "state": state.as_str(),
        "released": state == SpaceLeaseState::Released
    }))
}

pub(crate) fn consume(
    database: &Database,
    repository_id: RepositoryId,
    lease_id: SpaceLeaseId,
) -> Result<Value, MirageError> {
    let record = database
        .load_space_lease(lease_id)?
        .ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
    require_repository(&record, repository_id)?;
    let state = consume_space_lease(database, lease_id)?;
    Ok(json!({
        "repository_id": repository_id.to_string(),
        "lease_id": lease_id.to_string(),
        "state": state.as_str(),
        "consumed": state == SpaceLeaseState::Consumed
    }))
}

fn candidate_for(
    record: CacheSlotRecord,
    shard: &ArenaShard,
    resident: &ResidentIndex,
    proofs: &BTreeMap<PageHash, RemotePageProof>,
    drive_authenticated: bool,
) -> Result<ReclaimCandidate, MirageError> {
    let hash = record
        .page_hash
        .ok_or_else(|| MirageError::integrity_mismatch("resident cache slot has no hash"))?;
    let reasons = resident.pins().reasons(hash)?;
    let dirty = reasons
        .iter()
        .any(|reason| matches!(reason, PinReason::Dirty(_)));
    let remote_verified = match proofs.get(&hash) {
        Some(RemotePageProof::Local { .. }) => true,
        Some(RemotePageProof::Drive { .. }) => drive_authenticated,
        None => false,
    };
    Ok(ReclaimCandidate {
        reclaim_id: cache_reclaim_id(hash),
        physical_bytes: shard.reclaimable_slot_bytes(record.slot_index)?,
        last_access_sequence: record.generation,
        remote_verified,
        dirty,
        pinned: !reasons.is_empty(),
        active_read_leases: resident.active_read_leases(hash)?.unwrap_or(0),
    })
}

fn cache_reclaim_id(hash: PageHash) -> ReclaimId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD/reclaim/cache-page/v1\0");
    hasher.update(hash.as_bytes());
    ReclaimId::from_bytes(*hasher.finalize().as_bytes())
}

fn pack_reclaim_id(content_hash: [u8; 32]) -> ReclaimId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD/reclaim/drive-shadow-pack/v1\0");
    hasher.update(&content_hash);
    ReclaimId::from_bytes(*hasher.finalize().as_bytes())
}

fn native_backup_reclaim_id(
    repository_id: RepositoryId,
    generation_id: GenerationId,
    inventory_blake3: &str,
) -> ReclaimId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD/reclaim/native-backup/v1\0");
    hasher.update(repository_id.as_bytes());
    hasher.update(&generation_id.as_u64().to_le_bytes());
    hasher.update(inventory_blake3.as_bytes());
    ReclaimId::from_bytes(*hasher.finalize().as_bytes())
}

fn last_write_sequence(path: &Path) -> Result<u64, MirageError> {
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(MirageError::from)?;
    let duration = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
}

fn open_existing_cache(
    database: &Database,
    index: &MountIndex,
    target: &VolumeSpace,
) -> Result<(Option<Arc<ArenaShard>>, Option<Arc<ResidentIndex>>, bool), MirageError> {
    let state_root = database
        .reads()
        .database_path()
        .parent()
        .ok_or_else(|| MirageError::internal_invariant("database has no state root"))?;
    let cache_volume = disk_space::query(state_root)?;
    let same_volume = cache_volume.volume_id == target.volume_id;
    let shards = database.load_cache_shards()?;
    let spec = match shards.as_slice() {
        [] => return Ok((None, None, same_volume)),
        [spec] if spec.shard_id == 0 => spec,
        _ => {
            return Err(MirageError::unsupported_layout(
                "capacity accounting currently requires exactly one cache shard",
            ));
        }
    };
    if spec.page_size.as_u64() != index.header().page_size {
        return Err(MirageError::repository_conflict(
            "cache page size differs from the active generation",
        ));
    }
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(index.header().page_size),
        slot_count: spec.slot_count,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let shard = Arc::new(ArenaShard::open(
        &state_root.join("cache").join(&spec.relative_path),
        layout,
    )?);
    let resident = Arc::new(ResidentIndex::rebuild(database, Arc::clone(&shard))?);
    Ok((Some(shard), Some(resident), same_volume))
}

fn plan_json(source: &RepositoryCapacitySource, plan: &SpaceLeasePlan) -> Value {
    let (selected_cache_pages, selected_drive_shadow_packs, selected_native_backups) =
        selected_unit_counts(source, plan);
    json!({
        "repository_id": source.repository_id.to_string(),
        "origin": source.origin(),
        "drive_authenticated": source.drive_authenticated(),
        "target_volume_id": source.target.volume_id,
        "physical_total_bytes": source.total_bytes(),
        "physical_free_bytes": source.target.available_bytes,
        "physical_total_free_bytes": source.total_free_bytes(),
        "filesystem_reserve_bytes": source.reserve_bytes,
        "cache_on_target_volume": source.cache_on_target_volume(),
        "requested_bytes": plan.requested_bytes,
        "immediately_available_bytes": plan.immediately_available_bytes,
        "reclaim_required_bytes": plan.reclaim_required_bytes,
        "total_reclaimable_bytes": plan.total_reclaimable_bytes,
        "selected_reclaim_bytes": plan.selected_reclaim_bytes,
        "selected_reclaim_unit_count": plan.selected.len(),
        "selected_cache_page_count": selected_cache_pages,
        "selected_drive_shadow_pack_count": selected_drive_shadow_packs,
        "selected_native_backup_count": selected_native_backups,
        "available_after_selected_reclaim_bytes": plan.available_after_selected_reclaim_bytes,
        "shortfall_bytes": plan.shortfall_bytes,
        "grantable": plan.grantable(),
        "blocked": {
            "unique_bytes": plan.blocked.unique_bytes,
            "unverified_bytes": plan.blocked.unverified_bytes,
            "dirty_bytes": plan.blocked.dirty_bytes,
            "pinned_bytes": plan.blocked.pinned_bytes,
            "active_read_bytes": plan.blocked.active_read_bytes
        },
        "remote_quota_counted_as_local_capacity": false
    })
}

fn prepared_json(source: &RepositoryCapacitySource, prepared: &PreparedSpaceLease) -> Value {
    let (selected_cache_pages, selected_drive_shadow_packs, selected_native_backups) =
        selected_unit_counts(source, &prepared.plan);
    json!({
        "repository_id": prepared.record.repository_id.to_string(),
        "lease_id": prepared.record.lease_id.to_string(),
        "state": prepared.record.state.as_str(),
        "target_volume_id": prepared.record.target_volume_id,
        "requested_bytes": prepared.record.requested_bytes,
        "planned_reclaim_bytes": prepared.record.planned_reclaim_bytes,
        "actual_selected_reclaim_bytes": prepared.plan.selected_reclaim_bytes,
        "reclaimed_unit_count": prepared.plan.selected.len(),
        "reclaimed_cache_page_count": selected_cache_pages,
        "reclaimed_drive_shadow_pack_count": selected_drive_shadow_packs,
        "reclaimed_native_backup_count": selected_native_backups,
        "physical_free_after_reclaim_bytes": prepared.physical_free_after_reclaim_bytes,
        "filesystem_reserve_bytes": source.reserve_bytes,
        "active_promised_bytes": prepared.active_promised_bytes,
        "expires_at_ns": prepared.record.expires_at_ns,
        "ready": prepared.record.state == SpaceLeaseState::Ready,
        "remote_quota_counted_as_local_capacity": false
    })
}

fn selected_unit_counts(
    source: &RepositoryCapacitySource,
    plan: &SpaceLeasePlan,
) -> (usize, usize, usize) {
    let mut cache_pages = 0;
    let mut drive_shadow_packs = 0;
    let mut native_backups = 0;
    for candidate in &plan.selected {
        match source.reclaim_units.get(&candidate.reclaim_id) {
            Some(ReclaimUnit::CachePage(_)) => cache_pages += 1,
            Some(ReclaimUnit::DriveShadowPack { .. }) => drive_shadow_packs += 1,
            Some(ReclaimUnit::NativeBackup { .. }) => native_backups += 1,
            None => {}
        }
    }
    (cache_pages, drive_shadow_packs, native_backups)
}

fn lease_json(record: &SpaceLeaseRecord, now_ns: i64) -> Value {
    json!({
        "lease_id": record.lease_id.to_string(),
        "repository_id": record.repository_id.to_string(),
        "target_volume_id": record.target_volume_id,
        "requested_bytes": record.requested_bytes,
        "planned_reclaim_bytes": record.planned_reclaim_bytes,
        "state": record.state.as_str(),
        "created_at_ns": record.created_at_ns,
        "updated_at_ns": record.updated_at_ns,
        "expires_at_ns": record.expires_at_ns,
        "expired": now_ns >= record.expires_at_ns
    })
}

fn require_repository(
    record: &SpaceLeaseRecord,
    repository_id: RepositoryId,
) -> Result<(), MirageError> {
    if record.repository_id != repository_id {
        return Err(MirageError::backend_permission_denied(
            "Space Lease belongs to another repository",
        ));
    }
    Ok(())
}

fn proof_logical_length(proof: &RemotePageProof) -> u32 {
    match proof {
        RemotePageProof::Local { logical_length, .. }
        | RemotePageProof::Drive { logical_length, .. } => *logical_length,
    }
}

fn validate_object_component(value: &str) -> Result<(), MirageError> {
    let mut components = Path::new(value).components();
    if value.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(MirageError::integrity_mismatch(
            "local pack object ID is not a single path component",
        ));
    }
    Ok(())
}

fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, MirageError> {
    let metadata = std::fs::metadata(path).map_err(MirageError::from)?;
    if metadata.len() > limit as u64 {
        return Err(MirageError::manifest_invalid(
            "repository manifest exceeds its decode limit",
        ));
    }
    std::fs::read(path).map_err(MirageError::from)
}

fn now_ns() -> Result<i64, MirageError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            MirageError::internal_invariant("system clock predates Unix epoch").with_source(error)
        })?;
    i64::try_from(duration.as_nanos())
        .map_err(|_| MirageError::internal_invariant("system time exceeds i64 nanoseconds"))
}

fn validate_request_bytes(requested_bytes: u64) -> Result<(), MirageError> {
    if requested_bytes == 0 || requested_bytes > (1_u64 << 50) {
        return Err(MirageError::invalid_argument(
            "Space Lease request must be between 1 byte and 1 PiB",
        ));
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mirage_backend::{
        BackendError, BackendRead, DeletionProof, FetchClass, ObjectKind, ObjectStat, UploadSource,
    };
    use mirage_cache::insert_page;
    use mirage_crypto::aead::RepositoryKey;
    use mirage_crypto::dpapi::ProtectionScope;
    use mirage_crypto::repository_key_store::save_repository_key;
    use mirage_pack::{ImportPlan, PackEncryption, PlannedFile, import_local};
    use mirage_types::{BackendHealthState, CheckedRange, ContentHash, GenerationId};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_util::sync::CancellationToken;

    struct StatOnlyBackend {
        stats: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ObjectBackend for StatOnlyBackend {
        async fn read_range(
            &self,
            _object: &RemoteObjectRef,
            _range: CheckedRange,
            _class: FetchClass,
            _cancel: CancellationToken,
        ) -> Result<BackendRead, BackendError> {
            Err(BackendError::permanent("read is not used by this test"))
        }

        async fn put_immutable(
            &self,
            _kind: ObjectKind,
            _source: UploadSource,
            _expected_hash: ContentHash,
            _cancel: CancellationToken,
        ) -> Result<RemoteObjectRef, BackendError> {
            Err(BackendError::permanent("put is not used by this test"))
        }

        async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
            self.stats.fetch_add(1, Ordering::SeqCst);
            Ok(ObjectStat {
                byte_length: object.byte_length,
                content_hash: object.content_hash,
                immutable_revision: object.immutable_revision.clone(),
                kind: object.kind,
            })
        }

        async fn enumerate_commits(
            &self,
            _repository: RepositoryId,
        ) -> Result<Vec<RemoteObjectRef>, BackendError> {
            Err(BackendError::permanent(
                "enumeration is not used by this test",
            ))
        }

        async fn delete_immutable(
            &self,
            _object: &RemoteObjectRef,
            _proof: &DeletionProof,
            _cancel: CancellationToken,
        ) -> Result<(), BackendError> {
            Err(BackendError::permanent("delete is not used by this test"))
        }

        async fn health(&self) -> BackendHealthState {
            BackendHealthState::Healthy
        }
    }

    #[test]
    fn local_capacity_source_plans_and_revalidates_real_sparse_reclaim() {
        let directory = tempfile::tempdir().expect("directory");
        let native_root = directory.path().join("game");
        let assets = native_root.join("assets");
        let import_root = directory.path().join("import");
        std::fs::create_dir_all(&assets).expect("assets");
        std::fs::write(native_root.join("game.exe"), b"launcher").expect("launcher");
        let bytes = (0_u8..=254).cycle().take(1024 * 1024).collect::<Vec<_>>();
        std::fs::write(assets.join("content.pak"), &bytes).expect("asset");
        let repository_id = RepositoryId::from_bytes([0x66; 16]);
        import_local(&ImportPlan {
            repository_id,
            generation_id: GenerationId::ZERO,
            source_root: assets.clone(),
            files: vec![PlannedFile {
                relative_path: "content.pak".into(),
                class: mirage_manifest::FileClass::VirtualContainer,
            }],
            page_size: 1024 * 1024,
            pack_target: 2 * 1024 * 1024,
            output_staging_directory: import_root.clone(),
            encryption: None,
        })
        .expect("import");
        let database = Database::open(&directory.path().join("control.db")).expect("database");
        runtime::register(
            &database,
            "S-1-5-21-1001",
            runtime::RegisterSpec {
                repository_id,
                display_name: "capacity fixture".into(),
                native_root,
                mount_subtree: "assets".into(),
                import_root,
                launcher_relative: "game.exe".into(),
                arguments: Vec::new(),
                version_label: "1".into(),
                configuration_label: "test".into(),
                cache_bytes: 2 * 1024 * 1024,
            },
        )
        .expect("register");
        let active = database
            .load_active_generation(repository_id)
            .expect("active query")
            .expect("active");
        let index =
            MountIndex::open(active.mount_index_path.as_ref().expect("index path")).expect("index");
        let hash = index.page_by_ordinal(0).expect("page").plaintext_hash();
        let (shard, _resident) = runtime::open_cache(
            &database,
            u32::try_from(index.header().page_size).expect("page size"),
            2 * 1024 * 1024,
        )
        .expect("cache");
        insert_page(&database, shard, hash, &bytes, &()).expect("insert page");
        assert_eq!(database.load_resident_cache_slots().unwrap().len(), 1);

        let source = RepositoryCapacitySource::open(&database, repository_id, None)
            .expect("capacity source");
        assert!(source.cache_on_target_volume());
        let baseline = source.plan(1).expect("baseline plan");
        let plan = source
            .plan(baseline.immediately_available_bytes + 1)
            .expect("reclaim plan");
        assert!(plan.grantable());
        assert_eq!(plan.selected.len(), 1);
        assert!(plan.selected_reclaim_bytes > 0);

        source
            .reclaim_verified_clean(plan.selected[0])
            .expect("verified reclaim");
        assert!(database.load_resident_cache_slots().unwrap().is_empty());
        drop(source);

        let lease_id = SpaceLeaseId::from_bytes([0x77; 16]);
        let acquired = acquire(&database, repository_id, lease_id, 1, 30, None)
            .expect("acquire immediate lease");
        assert_eq!(acquired["state"], "ready");
        assert_eq!(
            consume(&database, repository_id, lease_id).unwrap()["state"],
            "consumed"
        );
        assert_eq!(
            release(&database, repository_id, lease_id).unwrap()["state"],
            "released"
        );
    }

    #[test]
    fn verified_drive_shadow_pack_is_reclaimable_without_deleting_control_metadata() {
        let directory = tempfile::tempdir().expect("directory");
        let native_root = directory.path().join("game");
        let assets = native_root.join("assets");
        let import_root = directory.path().join("import");
        std::fs::create_dir_all(&assets).expect("assets");
        std::fs::create_dir_all(&import_root).expect("import");
        std::fs::write(native_root.join("game.exe"), b"launcher").expect("launcher");
        std::fs::write(assets.join("content.pak"), vec![0x5a; 1024 * 1024]).expect("asset");
        let repository_id = RepositoryId::from_bytes([0x68; 16]);
        let key = Arc::new(RepositoryKey::from_bytes([0x91; 32]));
        let key_path = import_root.join("repository-key.dpapi");
        save_repository_key(
            &key_path,
            repository_id,
            key.as_ref(),
            ProtectionScope::CurrentUser,
        )
        .expect("save key");
        let imported = import_local(&ImportPlan {
            repository_id,
            generation_id: GenerationId::ZERO,
            source_root: assets.clone(),
            files: vec![PlannedFile {
                relative_path: "content.pak".into(),
                class: mirage_manifest::FileClass::VirtualContainer,
            }],
            page_size: 1024 * 1024,
            pack_target: 2 * 1024 * 1024,
            output_staging_directory: import_root.clone(),
            encryption: Some(PackEncryption {
                repository_id,
                key: Arc::clone(&key),
            }),
        })
        .expect("encrypted import");
        assert_eq!(imported.packs.len(), 1);
        let shadow_pack = imported.packs[0].path.clone();

        let mut drive_manifest = imported.manifest.clone();
        for location in &mut drive_manifest.remote_locations {
            let suffix = location.object.content_hash.to_string();
            location.object.backend_id = mirage_backend::BackendId::new("drive").unwrap();
            location.object.provider_object_id =
                mirage_backend::ProviderObjectId::new(format!("drive-pack-{suffix}")).unwrap();
            location.object.immutable_revision =
                Some(mirage_backend::ImmutableRevision::new(format!("revision-{suffix}")).unwrap());
        }
        std::fs::write(
            import_root.join("drive-manifest.cbor"),
            mirage_manifest::encode_manifest(&drive_manifest).expect("drive manifest"),
        )
        .expect("write drive manifest");

        let database = Database::open(&directory.path().join("control.db")).expect("database");
        runtime::register(
            &database,
            "S-1-5-21-1001",
            runtime::RegisterSpec {
                repository_id,
                display_name: "Drive shadow fixture".into(),
                native_root,
                mount_subtree: "assets".into(),
                import_root: import_root.clone(),
                launcher_relative: "game.exe".into(),
                arguments: Vec::new(),
                version_label: "1".into(),
                configuration_label: "test".into(),
                cache_bytes: 2 * 1024 * 1024,
            },
        )
        .expect("register");
        runtime::set_drive_origin(&database, repository_id, true).expect("Drive origin");
        runtime::convert(&database, repository_id, true).expect("convert native subtree");
        assert!(!assets.exists());
        let stats = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn ObjectBackend> = Arc::new(StatOnlyBackend {
            stats: Arc::clone(&stats),
        });
        let source = RepositoryCapacitySource::open_with_backend(
            &database,
            repository_id,
            None,
            Some(backend.clone()),
        )
        .expect("capacity source");
        let candidates = source.reclaim_candidates().expect("candidates");
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| candidate.reclaimable()));
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.physical_bytes > 0)
        );
        let pack_candidate = candidates
            .iter()
            .copied()
            .find(|candidate| {
                matches!(
                    source.reclaim_units.get(&candidate.reclaim_id),
                    Some(ReclaimUnit::DriveShadowPack { .. })
                )
            })
            .expect("pack candidate");
        let backup_candidate = candidates
            .iter()
            .copied()
            .find(|candidate| {
                matches!(
                    source.reclaim_units.get(&candidate.reclaim_id),
                    Some(ReclaimUnit::NativeBackup { .. })
                )
            })
            .expect("native backup candidate");

        source
            .reclaim_verified_clean(pack_candidate)
            .expect("reclaim shadow pack");
        source
            .reclaim_verified_clean(backup_candidate)
            .expect("reclaim native backup");
        assert_eq!(stats.load(Ordering::SeqCst), 1);
        assert!(!shadow_pack.exists());
        assert!(
            runtime::native_backup_info(&runtime::load_config(&database, repository_id).unwrap())
                .is_none()
        );
        assert!(import_root.join("base-manifest.cbor").is_file());
        assert!(import_root.join("drive-manifest.cbor").is_file());
        assert!(key_path.is_file());
        drop(source);

        let reopened = RepositoryCapacitySource::open_with_backend(
            &database,
            repository_id,
            None,
            Some(backend),
        )
        .expect("reopen without local pack");
        assert!(reopened.reclaim_candidates().unwrap().is_empty());
        assert!(runtime::set_drive_origin(&database, repository_id, false).is_err());
    }
}
