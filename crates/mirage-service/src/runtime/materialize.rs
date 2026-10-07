//! Artifact admission, capsule materialization, and launches.

use super::capacity::save_drive_capacity;
use super::drive::active_generation;
use super::fs_util::read_json_bounded;
use super::fs_util::validate_single_component;
use super::mount::recover_to_mounted;
use super::*;

pub(super) const DEFAULT_DRAIN_MS: u64 = 2_000;

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
        "complete": progress.verified_pages == progress.total_pages,
        "timing_ms": store
            .timing
            .lock()
            .map(|timing| json!({
                "resident_check": timing.resident_check_ms,
                "fetch_decode": timing.fetch_decode_ms,
                "hash_verify": timing.hash_verify_ms,
                "insert": timing.insert_ms,
                "checkpoint": timing.checkpoint_ms
            }))
            .unwrap_or_else(|_| Value::Null)
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
    let store = match LocalCapsuleStore::open(database, repository_id, &plan, None) {
        Ok(store) => store,
        Err(error) => {
            recover_to_mounted(database, repository_id, RepositoryState::AdmittingSession);
            return Err(error);
        }
    };
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
    pub(super) format_version: u32,
    pub(super) repository_id: RepositoryId,
    pub(super) capsule_id: mirage_types::CapsuleId,
    pub(super) generation: GenerationId,
    session_id: SessionId,
    pub(super) pinned_pages: u64,
    admitted_at_ns: i64,
}

struct LocalCapsuleStore {
    database: Database,
    pub(super) repository_id: RepositoryId,
    pub(super) generation: GenerationId,
    pub(super) index: Arc<MountIndex>,
    shard: Arc<ArenaShard>,
    resident: Arc<ResidentIndex>,
    pub(super) import_root: PathBuf,
    pub(super) origin: RuntimeOrigin,
    drive_objects: BTreeMap<[u8; 32], RemoteObjectRef>,
    drive_backend: Option<Arc<dyn ObjectBackend>>,
    drive_transport: Option<Arc<RetryingHttpTransport>>,
    provider_ready: bool,
    pub(super) encryption: Option<PackReadEncryption>,
    /// Verified pack readers keyed by pack object ID. `PackReader::open_verified` hashes the
    /// whole pack, so opening once per pack instead of once per page keeps materialization
    /// from paying that cost for every page.
    pack_readers: Mutex<BTreeMap<String, PackReader>>,
    reservations: Mutex<BTreeMap<mirage_types::PageHash, CacheSlotRecord>>,
    created_session: Mutex<Option<SessionId>>,
    checkpoint_path: PathBuf,
    timing: Mutex<MaterializeTiming>,
}

/// Per-phase wall time accumulated during capsule materialization; reported in the
/// materialize response as `timing_ms`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterializeTiming {
    pub resident_check_ms: f64,
    pub fetch_decode_ms: f64,
    pub hash_verify_ms: f64,
    pub insert_ms: f64,
    pub checkpoint_ms: f64,
}

impl LocalCapsuleStore {
    pub(super) fn open(
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
            pack_readers: Mutex::new(BTreeMap::new()),
            reservations: Mutex::new(BTreeMap::new()),
            created_session: Mutex::new(None),
            timing: Mutex::new(MaterializeTiming::default()),
            checkpoint_path: repository_state_root(database, repository_id)?
                .join("capsules")
                .join(format!("{}.progress.json", plan.capsule_id)),
        })
    }

    pub(super) fn page(&self, ordinal: u32) -> Result<mirage_index::PageView<'_>, MirageError> {
        self.index.page_by_ordinal(ordinal)
    }

    pub(super) fn hashes(
        &self,
        pages: &RoaringBitmap,
    ) -> Result<Vec<mirage_types::PageHash>, MirageError> {
        let mut hashes = pages
            .iter()
            .map(|page| self.page(page).map(|page| page.plaintext_hash()))
            .collect::<Result<Vec<_>, _>>()?;
        hashes.sort();
        hashes.dedup();
        Ok(hashes)
    }

    async fn fetch_verified_bytes(
        &self,
        page: u32,
        mandatory: bool,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, MirageError> {
        if cancel.is_cancelled() {
            return Err(MirageError::cancelled("capsule materialization cancelled"));
        }
        let page_view = self.page(page)?;
        let hash = page_view.plaintext_hash();
        let location = page_view.remote_location()?;
        let object_id = location.provider_object_id()?;
        validate_single_component(object_id, "pack object ID")?;
        let fetch_start = Instant::now();
        let bytes = match self.origin {
            RuntimeOrigin::Local => {
                // Synchronous section only: the std mutex guard must never be held across an
                // `.await` (clippy `await_holding_lock`).
                let mut readers = self.pack_readers.lock().map_err(|_| {
                    MirageError::internal_invariant("pack reader cache lock poisoned")
                })?;
                if !readers.contains_key(object_id) {
                    let path = self.import_root.join(object_id);
                    let reader = match &self.encryption {
                        Some(encryption) => {
                            PackReader::open_indexed_encrypted(&path, encryption.clone())?
                        }
                        None => PackReader::open_indexed(&path)?,
                    };
                    readers.insert(object_id.to_owned(), reader);
                }
                let reader = readers.get(object_id).ok_or_else(|| {
                    MirageError::internal_invariant("pack reader vanished from cache")
                })?;
                let bytes = reader.read_page(hash)?.page.bytes.to_vec();
                drop(readers);
                bytes
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
        let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;
        let verify_start = Instant::now();
        if bytes.len() != page_view.logical_length() as usize
            || blake3::hash(&bytes).as_bytes() != hash.as_bytes()
        {
            return Err(MirageError::integrity_mismatch(
                "materialized page differs from mount index",
            ));
        }
        if let Ok(mut timing) = self.timing.lock() {
            timing.fetch_decode_ms += fetch_ms;
            timing.hash_verify_ms += verify_start.elapsed().as_secs_f64() * 1000.0;
        }
        Ok(bytes)
    }

    fn take_reservation(
        &self,
        hash: mirage_types::PageHash,
        logical_length: u32,
    ) -> Result<CacheSlotRecord, MirageError> {
        let reserved = self
            .reservations
            .lock()
            .map_err(|_| MirageError::internal_invariant("capsule reservation lock poisoned"))?
            .remove(&hash);
        if let Some(record) = reserved {
            return Ok(record);
        }
        match self.database.reserve_cache_slot(hash, logical_length)? {
            ReserveCacheSlotOutcome::Reserved(record) => Ok(record),
            ReserveCacheSlotOutcome::Existing(_) => Err(MirageError::repository_conflict(
                "cache page became resident during materialization; retry",
            )),
        }
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
        let mut requests = Vec::new();
        for ordinal in pages {
            let page = self.page(ordinal)?;
            let hash = page.plaintext_hash();
            let length = page.logical_length();
            match unique.insert(hash, length) {
                Some(existing) if existing != length => {
                    return Err(MirageError::integrity_mismatch(
                        "deduplicated page hash has conflicting lengths",
                    ));
                }
                Some(_) => {}
                None => requests.push((hash, length)),
            }
        }
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
        let start = Instant::now();
        let hash = self.page(page)?.plaintext_hash();
        let verified = matches!(
            verify_page(&self.resident, &self.database, hash, IntegrityClass::Clean,)?,
            VerifyOutcome::Verified
        );
        if let Ok(mut timing) = self.timing.lock() {
            timing.resident_check_ms += start.elapsed().as_secs_f64() * 1000.0;
        }
        Ok(verified)
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
        let bytes = self.fetch_verified_bytes(page, mandatory, cancel).await?;
        let hash = self.page(page)?.plaintext_hash();
        let record = self.take_reservation(hash, bytes.len() as u32)?;
        let insert_start = Instant::now();
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
        if let Ok(mut timing) = self.timing.lock() {
            timing.insert_ms += insert_start.elapsed().as_secs_f64() * 1000.0;
        }
        Ok(bytes.len() as u64)
    }

    fn batch_pages(&self) -> usize {
        (32 * 1024 * 1024 / self.shard.layout().page_size.as_u64()).clamp(1, 32) as usize
    }

    async fn materialize_pages(
        &self,
        pages: &[u32],
        mandatory: bool,
        progress: &mut MaterializeProgress,
        cancel: &CancellationToken,
    ) -> Result<(), MirageError> {
        let result = async {
            let mut payloads = BTreeMap::new();
            let mut duplicates = 0;
            for &ordinal in pages {
                if cancel.is_cancelled() {
                    return Err(MirageError::cancelled("capsule materialization cancelled"));
                }
                let hash = self.page(ordinal)?.plaintext_hash();
                match payloads.entry(hash) {
                    std::collections::btree_map::Entry::Occupied(_) => duplicates += 1,
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        if self.is_verified_resident(ordinal).await? {
                            progress.already_resident += 1;
                            progress.verified_pages += 1;
                        } else {
                            let bytes = self
                                .fetch_verified_bytes(ordinal, mandatory, cancel)
                                .await?;
                            entry.insert(bytes);
                        }
                    }
                }
            }
            if payloads.is_empty() {
                return Ok(());
            }
            if cancel.is_cancelled() {
                return Err(MirageError::cancelled("capsule materialization cancelled"));
            }
            let mut batch: Vec<_> = payloads
                .iter()
                .map(|(hash, bytes)| {
                    let record = self.take_reservation(*hash, bytes.len() as u32)?;
                    Ok((record, bytes.as_slice()))
                })
                .collect::<Result<_, MirageError>>()?;
            batch.sort_unstable_by_key(|(record, _)| (record.shard_id, record.slot_index));
            let insert_start = Instant::now();
            let records = mirage_cache::insert_reserved_pages(
                &self.database,
                Arc::clone(&self.shard),
                &batch,
                &(),
            )?;
            for record in records {
                self.resident.install(record, Arc::clone(&self.shard))?;
            }
            if let Ok(mut timing) = self.timing.lock() {
                timing.insert_ms += insert_start.elapsed().as_secs_f64() * 1000.0;
            }
            progress.downloaded_pages += batch.len() as u64;
            progress.downloaded_bytes += payloads
                .values()
                .map(|bytes| bytes.len() as u64)
                .sum::<u64>();
            progress.already_resident += duplicates;
            progress.verified_pages += batch.len() as u64 + duplicates;
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            progress.failed_pages += u64::from(error.code != "MIRAGE_CANCELLED");
        }
        result
    }

    async fn checkpoint(&self, progress: MaterializeProgress) -> Result<(), MirageError> {
        let start = Instant::now();
        let result = write_json_atomic(
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
        );
        if let Ok(mut timing) = self.timing.lock() {
            timing.checkpoint_ms += start.elapsed().as_secs_f64() * 1000.0;
        }
        result
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
