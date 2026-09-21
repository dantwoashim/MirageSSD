use mirage_db::{Database, NewUpdateJournal, RepositorySummary};
use mirage_engine::repair::{LocalMetadataRepairSource, RepairLevel, repair};
use mirage_ipc::{Authorization, Command, Principal, PrincipalRole, Request, ResponseBody};
use mirage_types::{
    MirageError, RepositoryEvent, RepositoryId, RepositoryState, UpdateEvent, UpdateId, UpdateState,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runtime::{self, RegisterSpec};
use crate::{
    MountControl, RequestHandler, capacity, mount_control::UnavailableMountControl, native_session,
};
use crate::{disk_floor, disk_space};

pub struct ControlPlaneHandler {
    database: Database,
    mounts: Mutex<Box<dyn MountControl>>,
    mount_lifecycle: Mutex<()>,
    storage_lifecycle: Mutex<()>,
    launches: Arc<Mutex<BTreeMap<RepositoryId, runtime::RuntimeLaunch>>>,
}

impl ControlPlaneHandler {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self {
            database,
            mounts: Mutex::new(Box::new(UnavailableMountControl)),
            mount_lifecycle: Mutex::new(()),
            storage_lifecycle: Mutex::new(()),
            launches: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn with_mount_control(database: Database, mounts: impl MountControl + 'static) -> Self {
        Self {
            database,
            mounts: Mutex::new(Box::new(mounts)),
            mount_lifecycle: Mutex::new(()),
            storage_lifecycle: Mutex::new(()),
            launches: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn maintain_persistent_mounts(&self) -> Result<usize, MirageError> {
        let _lifecycle = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        let state_root = runtime::service_state_root(&self.database)?;
        let mut restored = 0;
        for repository in self.repositories()? {
            if repository.state != RepositoryState::ReadyMounted {
                continue;
            }
            let Some(record) =
                runtime::load_mount_record(&self.database, repository.repository_id)?
            else {
                continue;
            };
            if !record.explorer_visible {
                continue;
            }
            let running = self
                .mounts
                .lock()
                .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
                .is_running(repository.repository_id)?;
            if running {
                continue;
            }
            let active = self
                .database
                .load_active_generation(repository.repository_id)?
                .ok_or_else(|| MirageError::integrity_mismatch("active generation is missing"))?;
            if active.generation_id != record.generation {
                return Err(MirageError::repository_conflict(
                    "persistent mount generation is no longer active",
                ));
            }
            let index = active.mount_index_path.ok_or_else(|| {
                MirageError::integrity_mismatch("active generation has no compiled mount index")
            })?;
            let letter = record.mount_point.to_str().ok_or_else(|| {
                MirageError::integrity_mismatch("persistent drive letter is invalid")
            })?;
            let (mount_point, _) = runtime::validate_explorer_mount_ready(
                &self.database,
                repository.repository_id,
                &index,
                letter,
            )?;
            let owner_sid = self
                .database
                .load_repository_owner_sid(repository.repository_id)?
                .ok_or_else(|| {
                    MirageError::integrity_mismatch("repository owner SID is missing")
                })?;
            let capacity =
                runtime::volume_capacity(&self.database, repository.repository_id, &index)?;
            let origin_root = local_origin_root(&self.database, repository.repository_id)?;
            let managed = self
                .database
                .load_repository_volume_mode(repository.repository_id)?
                == Some(mirage_db::VolumeMode::Managed);
            let drive_provider =
                drive_provider_paths(&self.database, repository.repository_id, managed)?;
            self.mounts
                .lock()
                .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
                .mount(
                    repository.repository_id,
                    &mount_point,
                    &index,
                    &state_root,
                    &owner_sid,
                    origin_root.as_deref(),
                    capacity,
                    managed,
                    drive_provider
                        .as_ref()
                        .map(|(manifest, key)| (manifest.as_path(), key.as_path())),
                    if managed {
                        state_root_disk_floor(&self.database)
                    } else {
                        None
                    },
                )?;
            restored += 1;
        }
        Ok(restored)
    }

    pub fn recover_native_activations(&self) -> Result<usize, MirageError> {
        let _mount = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        let _storage = self
            .storage_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("storage lifecycle lock poisoned"))?;
        let mut active = 0;
        for repository in self.repositories()? {
            if native_session::reconcile(&self.database, repository.repository_id)?.is_some() {
                active += 1;
            }
        }
        Ok(active)
    }

    fn repositories(&self) -> Result<Vec<RepositorySummary>, MirageError> {
        self.database.list_repositories()
    }

    fn repositories_for(
        &self,
        principal: &Principal,
    ) -> Result<Vec<RepositorySummary>, MirageError> {
        let repositories = self.repositories()?;
        if matches!(
            principal.role,
            PrincipalRole::Administrator | PrincipalRole::Service
        ) {
            return Ok(repositories);
        }
        repositories
            .into_iter()
            .filter_map(|repository| {
                match self
                    .database
                    .load_repository_owner_sid(repository.repository_id)
                {
                    Ok(Some(owner)) if owner == principal.windows_sid => Some(Ok(repository)),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect()
    }

    fn detail(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        let repository = self
            .repositories()?
            .into_iter()
            .find(|item| item.repository_id == repository_id)
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        let mut summary = summary_json(&repository);
        // Truthful durability state: pending journal depth, live divergence,
        // and verified workspace coverage — a failed query fails the status
        // rather than reporting an optimistic zero.
        let volume = repository_id;
        let pending_operations = self.database.replayable_operations(volume)?.len();
        let diverged = self
            .database
            .live_divergence(volume)?
            .is_some_and(|divergence| divergence.status == mirage_db::DivergenceStatus::Diverged);
        let verified_workspace_bytes: u64 = self
            .database
            .workspace_leases(volume)?
            .iter()
            .filter(|lease| lease.status == mirage_db::LeaseStatus::Verified)
            .map(|lease| lease.bytes_verified)
            .sum();
        // Payload publication ledger for managed volumes: pending bytes are
        // local-only data, published objects are cloud-backed and evictable.
        let publication = self.database.payload_publication_stats(
            volume,
            &mirage_db::payload_remote::MANAGED_JOURNAL_FILE_ID,
        )?;
        summary["unpublished_payload_bytes"] = json!(publication.pending_bytes);
        summary["unpublished_payloads"] = json!(publication.pending_payloads);
        summary["published_payload_bytes"] = json!(publication.published_bytes);
        summary["published_payload_objects"] = json!(publication.published_payloads);
        summary["evicted_payloads"] = json!(publication.evicted_payloads);
        summary["pending_local_operations"] = json!(pending_operations);
        summary["diverged"] = json!(diverged);
        summary["verified_workspace_bytes"] = json!(verified_workspace_bytes);
        // Origin/volume mode are only known for runtime-registered
        // repositories; a bare registered fixture reports nulls.
        if let Ok(config) = runtime::load_config(&self.database, repository_id) {
            summary["origin"] = json!(config.origin.as_str());
        }
        summary["volume_mode"] = json!(
            self.database
                .load_repository_volume_mode(repository_id)?
                .map_or("legacy", |mode| mode.as_str())
        );
        Ok(ResponseBody::Json(summary))
    }

    fn reap_completed_launches(&self) -> Result<(), MirageError> {
        let now = Instant::now();
        let mut launches = self
            .launches
            .lock()
            .map_err(|_| MirageError::internal_invariant("launch registry lock poisoned"))?;
        let mut completed = Vec::new();
        for (repository_id, launch) in launches.iter_mut() {
            if launch.exit_observed_at.is_none()
                && launch
                    .maximum_duration
                    .is_some_and(|maximum| now.duration_since(launch.started_at) >= maximum)
            {
                launch.process.child.kill().map_err(service_io)?;
                launch.process.child.wait().map_err(service_io)?;
                launch.exit_observed_at = Some(now);
            }
            if launch.exit_observed_at.is_none()
                && launch
                    .process
                    .child
                    .try_wait()
                    .map_err(service_io)?
                    .is_some()
            {
                launch.exit_observed_at = Some(now);
            }
            if launch
                .exit_observed_at
                .is_some_and(|observed| now.duration_since(observed) >= launch.drain_interval)
            {
                completed.push(*repository_id);
            }
        }
        for repository_id in completed {
            let launch = launches
                .remove(&repository_id)
                .ok_or_else(|| MirageError::internal_invariant("completed launch disappeared"))?;
            self.database.finish_session(
                launch.session_id,
                vec![launch.process.root_pid],
                now_ns(),
            )?;
            self.database
                .release_cache_pins(mirage_db::PersistentPinReason::Session(launch.session_id))?;
            self.database.set_repository_state(
                repository_id,
                RepositoryState::PlayingSealed,
                RepositoryEvent::SessionEnded,
                now_ns(),
            )?;
        }
        Ok(())
    }

    fn launch_command(
        &self,
        repository_id: RepositoryId,
        capsule_id: Option<mirage_types::CapsuleId>,
        maximum_duration_seconds: Option<u64>,
    ) -> Result<ResponseBody, MirageError> {
        if self
            .launches
            .lock()
            .map_err(|_| MirageError::internal_invariant("launch registry lock poisoned"))?
            .contains_key(&repository_id)
        {
            return Err(MirageError::repository_conflict(
                "repository already has a tracked launch",
            ));
        }
        let (value, launch) = runtime::launch(
            &self.database,
            repository_id,
            capsule_id,
            maximum_duration_seconds,
        )?;
        let session_id = launch.session_id;
        self.launches
            .lock()
            .map_err(|_| MirageError::internal_invariant("launch registry lock poisoned"))?
            .insert(repository_id, launch);
        if let Some(seconds) = maximum_duration_seconds {
            let database = self.database.clone();
            let launches = Arc::clone(&self.launches);
            std::thread::spawn(move || {
                if let Err(error) = reap_bounded_launch_after(
                    database,
                    launches,
                    repository_id,
                    session_id,
                    Duration::from_secs(seconds),
                ) {
                    eprintln!("bounded launch cleanup failed: {error}");
                }
            });
        }
        Ok(ResponseBody::Json(value))
    }

    fn set_drive_origin(
        &self,
        repository_id: RepositoryId,
        drive: bool,
    ) -> Result<ResponseBody, MirageError> {
        let _storage = self
            .storage_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("storage lifecycle lock poisoned"))?;
        runtime::set_drive_origin(&self.database, repository_id, drive).map(ResponseBody::Json)
    }

    fn set_volume_mode(
        &self,
        repository_id: RepositoryId,
        managed: bool,
    ) -> Result<ResponseBody, MirageError> {
        let _mount = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        runtime::set_volume_mode(&self.database, repository_id, managed).map(ResponseBody::Json)
    }

    fn acquire_capacity(
        &self,
        repository_id: RepositoryId,
        requested_bytes: u64,
        lifetime_seconds: u64,
        drive_access_token: Option<&str>,
    ) -> Result<ResponseBody, MirageError> {
        let _storage = self
            .storage_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("storage lifecycle lock poisoned"))?;
        capacity::acquire(
            &self.database,
            repository_id,
            random_space_lease_id()?,
            requested_bytes,
            lifetime_seconds,
            drive_access_token,
        )
        .map(ResponseBody::Json)
    }

    fn activate_native(
        &self,
        repository_id: RepositoryId,
        drive_access_token: &str,
    ) -> Result<ResponseBody, MirageError> {
        let _mount = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        let _storage = self
            .storage_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("storage lifecycle lock poisoned"))?;
        if self
            .mounts
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
            .is_running(repository_id)?
        {
            return Err(MirageError::repository_conflict(
                "native activation requires the virtual mount to be stopped",
            ));
        }
        if let Some(active) = native_session::reconcile(&self.database, repository_id)? {
            return Ok(ResponseBody::Json(active));
        }
        let required_bytes = native_session::required_bytes(&self.database, repository_id)?;
        let lease_id = random_space_lease_id()?;
        capacity::acquire(
            &self.database,
            repository_id,
            lease_id,
            required_bytes,
            86_400,
            Some(drive_access_token),
        )?;
        if let Err(error) = capacity::consume(&self.database, repository_id, lease_id) {
            let _ = capacity::release(&self.database, repository_id, lease_id);
            return Err(error);
        }
        match native_session::activate(&self.database, repository_id, lease_id, drive_access_token)
        {
            Ok(value) => Ok(ResponseBody::Json(value)),
            Err(error) => {
                let _ = capacity::release(&self.database, repository_id, lease_id);
                Err(error)
            }
        }
    }
}

fn reap_bounded_launch_after(
    database: Database,
    launches: Arc<Mutex<BTreeMap<RepositoryId, runtime::RuntimeLaunch>>>,
    repository_id: RepositoryId,
    session_id: mirage_types::SessionId,
    maximum_duration: Duration,
) -> Result<(), MirageError> {
    std::thread::sleep(maximum_duration);
    let drain_interval = {
        let mut launches = launches
            .lock()
            .map_err(|_| MirageError::internal_invariant("launch registry lock poisoned"))?;
        let Some(launch) = launches.get_mut(&repository_id) else {
            return Ok(());
        };
        if launch.session_id != session_id {
            return Ok(());
        }
        if launch.exit_observed_at.is_none() {
            if launch
                .process
                .child
                .try_wait()
                .map_err(service_io)?
                .is_none()
            {
                // Child::kill is a forcible termination on the supported platforms.
                launch.process.child.kill().map_err(service_io)?;
                launch.process.child.wait().map_err(service_io)?;
            }
            launch.exit_observed_at = Some(Instant::now());
        }
        launch.drain_interval
    };
    std::thread::sleep(drain_interval);
    let launch = {
        let mut launches = launches
            .lock()
            .map_err(|_| MirageError::internal_invariant("launch registry lock poisoned"))?;
        if launches
            .get(&repository_id)
            .is_some_and(|launch| launch.session_id == session_id)
        {
            launches.remove(&repository_id)
        } else {
            None
        }
    };
    if let Some(launch) = launch {
        finish_launch(&database, repository_id, launch)?;
    }
    Ok(())
}

fn finish_launch(
    database: &Database,
    repository_id: RepositoryId,
    launch: runtime::RuntimeLaunch,
) -> Result<(), MirageError> {
    database.finish_session(launch.session_id, vec![launch.process.root_pid], now_ns())?;
    database.release_cache_pins(mirage_db::PersistentPinReason::Session(launch.session_id))?;
    database.set_repository_state(
        repository_id,
        RepositoryState::PlayingSealed,
        RepositoryEvent::SessionEnded,
        now_ns(),
    )?;
    Ok(())
}

impl RequestHandler for ControlPlaneHandler {
    fn handle(&self, principal: &Principal, request: Request) -> ResponseBody {
        if let Err(error) = self.reap_completed_launches() {
            return error_response(error);
        }
        if matches!(request.command, Command::RepositoryRegister { .. }) {
            if let Err(error) = Authorization::authenticate(principal) {
                return error_response(error);
            }
        } else if matches!(request.command, Command::RepositoryAdopt { .. }) {
            if let Err(error) = Authorization::authenticate(principal) {
                return error_response(error);
            }
            if principal.role != PrincipalRole::Administrator {
                return error_response(MirageError::backend_permission_denied(
                    "repository adoption requires an elevated administrator token",
                ));
            }
        } else if let Some(repository_id) = repository_id(&request.command) {
            let authorized = self
                .database
                .load_repository_owner_sid(repository_id)
                .and_then(|owner| {
                    let owner = owner.ok_or_else(|| {
                        MirageError::invalid_argument("repository is not configured")
                    })?;
                    let effective = if principal.role == PrincipalRole::ReadOnly
                        && principal.windows_sid == owner
                    {
                        Principal {
                            role: PrincipalRole::RepositoryOwner,
                            ..principal.clone()
                        }
                    } else {
                        principal.clone()
                    };
                    Authorization::authorize_repository(&effective, &request.command, &owner)
                });
            if let Err(error) = authorized {
                return error_response(error);
            }
        } else if let Err(error) = Authorization::authorize(principal, &request.command) {
            return error_response(error);
        }
        let result = match request.command {
            Command::Status | Command::RepositoryList => {
                self.repositories_for(principal).map(|items| {
                    ResponseBody::Json(json!({
                        "service": "running",
                        "configured": !items.is_empty(),
                        "repositories": items.iter().map(summary_json).collect::<Vec<_>>()
                    }))
                })
            }
            Command::RepositoryDetail { repository_id } => self.detail(repository_id),
            Command::RepositoryRegister {
                repository_id,
                display_name,
                native_root,
                mount_subtree,
                import_root,
                launcher_relative,
                arguments,
                version_label,
                configuration_label,
                cache_bytes,
            } => runtime::register(
                &self.database,
                &principal.windows_sid,
                RegisterSpec {
                    repository_id,
                    display_name,
                    native_root,
                    mount_subtree,
                    import_root,
                    launcher_relative,
                    arguments,
                    version_label,
                    configuration_label,
                    cache_bytes,
                },
            )
            .map(ResponseBody::Json),
            Command::RepositoryAdopt { repository_id } => self
                .database
                .load_repository_owner_sid(repository_id)
                .and_then(|owner| {
                    let owner = owner.ok_or_else(|| {
                        MirageError::invalid_argument("repository is not configured")
                    })?;
                    if owner != principal.windows_sid {
                        self.database.set_repository_owner_sid(
                            repository_id,
                            &owner,
                            &principal.windows_sid,
                        )?;
                    }
                    Ok(ResponseBody::Json(json!({
                        "repository_id": repository_id.to_string(),
                        "adopted": true
                    })))
                }),
            Command::RepositoryConvert {
                repository_id,
                apply,
            } => runtime::convert(&self.database, repository_id, apply).map(ResponseBody::Json),
            Command::RepositoryRestoreNative {
                repository_id,
                apply,
            } => runtime::restore_native(&self.database, repository_id, apply)
                .map(ResponseBody::Json),
            Command::RepositorySetDriveOrigin {
                repository_id,
                drive,
            } => self.set_drive_origin(repository_id, drive),
            Command::RepositorySetVolumeMode {
                repository_id,
                managed,
            } => self.set_volume_mode(repository_id, managed),
            Command::Mount {
                repository_id,
                generation,
                drive_letter,
                drive_access_token,
            } => self.mount(
                repository_id,
                generation,
                drive_letter.as_deref(),
                drive_access_token
                    .as_ref()
                    .map(mirage_ipc::SensitiveString::expose),
            ),
            Command::DriveTokenSupply {
                repository_id,
                drive_access_token,
            } => self.supply_drive_token(repository_id, drive_access_token.expose()),
            Command::Unmount { repository_id } => self.unmount(repository_id),
            Command::DiskFloorSet {
                volume_root,
                floor_bytes,
                hysteresis_bytes,
            } => self.disk_floor_set(&volume_root, floor_bytes, hysteresis_bytes),
            Command::DiskFloorClear { volume_root } => self.disk_floor_clear(&volume_root),
            Command::DiskStatus => self.disk_status(),
            Command::NamespacePin {
                repository_id,
                path,
            } => self.namespace_pin_set(repository_id, &path, true),
            Command::NamespaceUnpin {
                repository_id,
                path,
            } => self.namespace_pin_set(repository_id, &path, false),
            Command::NamespacePins { repository_id } => self.namespace_pins_list(repository_id),
            Command::ProfileConfigure {
                repository_id,
                launcher_relative,
                arguments,
                version_label,
                configuration_label,
            } => runtime::configure(
                &self.database,
                repository_id,
                launcher_relative,
                arguments,
                version_label,
                configuration_label,
            )
            .map(ResponseBody::Json),
            Command::Profile {
                repository_id,
                maximum_duration_seconds,
            } => runtime::capture(&self.database, repository_id, maximum_duration_seconds)
                .map(ResponseBody::Json),
            Command::Simulate { repository_id } => {
                runtime::simulate(&self.database, repository_id).map(ResponseBody::Json)
            }
            Command::CapacityPlan {
                repository_id,
                requested_bytes,
                drive_access_token,
            } => capacity::plan_response(
                &self.database,
                repository_id,
                requested_bytes,
                drive_access_token
                    .as_ref()
                    .map(mirage_ipc::SensitiveString::expose),
            )
            .map(|plan| ResponseBody::Json(plan.json())),
            Command::CapacityAcquire {
                repository_id,
                requested_bytes,
                lifetime_seconds,
                drive_access_token,
            } => self.acquire_capacity(
                repository_id,
                requested_bytes,
                lifetime_seconds,
                drive_access_token
                    .as_ref()
                    .map(mirage_ipc::SensitiveString::expose),
            ),
            Command::CapacityStatus {
                repository_id,
                lease_id,
            } => capacity::status(&self.database, repository_id, lease_id).map(ResponseBody::Json),
            Command::CapacityConsume {
                repository_id,
                lease_id,
            } => capacity::consume(&self.database, repository_id, lease_id).map(ResponseBody::Json),
            Command::CapacityRelease {
                repository_id,
                lease_id,
            } => capacity::release(&self.database, repository_id, lease_id).map(ResponseBody::Json),
            Command::NativeActivate {
                repository_id,
                drive_access_token,
            } => drive_access_token
                .as_ref()
                .ok_or_else(|| {
                    MirageError::backend_unauthenticated(
                        "native activation requires the trusted user credential broker",
                    )
                })
                .and_then(|token| self.activate_native(repository_id, token.expose())),
            Command::NativeStatus { repository_id } => {
                native_session::status(&self.database, repository_id).map(ResponseBody::Json)
            }
            Command::Plan {
                repository_id,
                full_volume,
            } => runtime::plan(&self.database, repository_id, full_volume).map(ResponseBody::Json),
            Command::Materialize {
                repository_id,
                capsule_id,
                drive_access_token,
                drive_quota,
            } => runtime::materialize(
                &self.database,
                repository_id,
                capsule_id,
                drive_access_token
                    .as_ref()
                    .map(mirage_ipc::SensitiveString::expose),
                drive_quota,
            )
            .map(ResponseBody::Json),
            Command::Admit {
                repository_id,
                capsule_id,
            } => runtime::admit(&self.database, repository_id, capsule_id).map(ResponseBody::Json),
            Command::Launch {
                repository_id,
                capsule_id,
                maximum_duration_seconds,
            } => self.launch_command(repository_id, capsule_id, maximum_duration_seconds),
            Command::UpdateBegin { repository_id } => self.begin_update(repository_id),
            Command::UpdateStatus { repository_id } => self.update_status(repository_id),
            Command::UpdateCommit { repository_id } => self.commit_update(repository_id),
            Command::UpdateRollback { repository_id } => self.rollback_update(repository_id),
            Command::Verify {
                repository_id,
                deep,
            } => self.detail(repository_id).and_then(|_| {
                let level = if deep {
                    RepairLevel::DeepRemote
                } else {
                    RepairLevel::Metadata
                };
                repair_response(&self.database, repository_id, level)
            }),
            Command::Repair { repository_id } => self.detail(repository_id).and_then(|_| {
                repair_response(&self.database, repository_id, RepairLevel::LocalCache)
            }),
            command => repository_id(&command)
                .ok_or_else(|| MirageError::invalid_argument("command has no repository target"))
                .and_then(|id| {
                    self.detail(id)?;
                    Err(MirageError::provider_unavailable(
                        "repository exists but the requested runtime capability is not ready",
                    ))
                }),
        };
        result.unwrap_or_else(error_response)
    }
}

impl ControlPlaneHandler {
    fn mount(
        &self,
        repository_id: RepositoryId,
        generation: mirage_types::GenerationId,
        drive_letter: Option<&str>,
        drive_access_token: Option<&str>,
    ) -> Result<ResponseBody, MirageError> {
        let _lifecycle = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        let state = self
            .database
            .load_repository_state(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        if state != RepositoryState::ReadyUnmounted {
            return Err(MirageError::repository_conflict(
                "repository is not ready to mount",
            ));
        }
        let active = self
            .database
            .load_active_generation(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))?;
        if active.generation_id != generation {
            return Err(MirageError::repository_conflict(
                "requested generation is not active",
            ));
        }
        let index = active.mount_index_path.ok_or_else(|| {
            MirageError::integrity_mismatch("active generation has no compiled mount index")
        })?;
        let database_mount_root = self
            .database
            .load_repository_root(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository root is unavailable"))?;
        let (mount_point, explorer_visible, verified_volume_pages) = match drive_letter {
            Some(letter) => {
                let (mount_point, pages) = runtime::validate_explorer_mount_ready(
                    &self.database,
                    repository_id,
                    &index,
                    letter,
                )?;
                (mount_point, true, Some(pages))
            }
            None => {
                runtime::validate_mount_ready(&self.database, repository_id, &database_mount_root)?;
                (database_mount_root, false, None)
            }
        };
        let state_root = runtime::service_state_root(&self.database)?;
        let owner_sid = self
            .database
            .load_repository_owner_sid(repository_id)?
            .ok_or_else(|| MirageError::integrity_mismatch("repository owner SID is missing"))?;
        let (mut volume_total_bytes, mut volume_free_bytes) =
            runtime::volume_capacity(&self.database, repository_id, &index)?;
        let origin_root = local_origin_root(&self.database, repository_id)?;
        let managed = self.database.load_repository_volume_mode(repository_id)?
            == Some(mirage_db::VolumeMode::Managed);
        // For a managed volume `volume_free_bytes` is the dirty-write budget:
        // without a Drive quota snapshot (on-demand mounts need no
        // materialize) fall back to the repository's reviewed cache budget
        // rather than a zero budget that rejects every write.
        if managed && volume_free_bytes == 0 {
            volume_free_bytes = runtime::load_config(&self.database, repository_id)?.cache_bytes;
            volume_total_bytes = volume_total_bytes.max(volume_free_bytes);
        }
        // A managed Drive repository mounts with the publication manifest and
        // content key so the host can fetch non-resident pages on demand.
        let drive_provider = drive_provider_paths(&self.database, repository_id, managed)?;
        self.database.set_repository_state(
            repository_id,
            state,
            RepositoryEvent::MountRequested,
            now_ns(),
        )?;
        let mounted = self
            .mounts
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
            .mount(
                repository_id,
                &mount_point,
                &index,
                &state_root,
                &owner_sid,
                origin_root.as_deref(),
                (volume_total_bytes, volume_free_bytes),
                managed,
                drive_provider
                    .as_ref()
                    .map(|(manifest, key)| (manifest.as_path(), key.as_path())),
                if managed {
                    state_root_disk_floor(&self.database)
                } else {
                    None
                },
            );
        if let Err(error) = mounted {
            let _ = runtime::clear_mount_record(&self.database, repository_id);
            self.recover_failed_mount(repository_id)?;
            return Err(error);
        }
        // Deliver the bearer token only after the host reported ready; the
        // provider stays uninstalled (reads fail unavailable) until then.
        if let Some(token) = drive_access_token
            && drive_provider.is_some()
            && let Err(error) = self
                .mounts
                .lock()
                .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
                .send_drive_token(repository_id, token)
        {
            let _ = self
                .mounts
                .lock()
                .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
                .unmount(repository_id);
            let _ = runtime::clear_mount_record(&self.database, repository_id);
            self.recover_failed_mount(repository_id)?;
            return Err(error);
        }
        if let Err(error) = runtime::save_mount_record(
            &self.database,
            repository_id,
            generation,
            &mount_point,
            explorer_visible,
        ) {
            let _ = self
                .mounts
                .lock()
                .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
                .unmount(repository_id);
            self.recover_failed_mount(repository_id)?;
            return Err(error);
        }
        if let Err(error) = self.database.set_repository_state(
            repository_id,
            RepositoryState::Mounting,
            RepositoryEvent::MountSucceeded,
            now_ns(),
        ) {
            let _ = self
                .mounts
                .lock()
                .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
                .unmount(repository_id);
            let _ = runtime::clear_mount_record(&self.database, repository_id);
            self.recover_failed_mount(repository_id)?;
            return Err(error);
        }
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "generation": generation.as_u64(),
            "state": "ready_mounted",
            "mount_point": mount_point,
            "explorer_visible": explorer_visible,
            "verified_volume_pages": verified_volume_pages,
            "volume_total_bytes": volume_total_bytes,
            "volume_free_bytes": volume_free_bytes,
            "cloud_reads_in_filesystem_callbacks": drive_provider.is_some()
        })))
    }

    /// Forwards a bearer token to the repository's live filesystem host; the
    /// token is never persisted — it leaves the service on the host's stdin.
    fn supply_drive_token(
        &self,
        repository_id: RepositoryId,
        token: &str,
    ) -> Result<ResponseBody, MirageError> {
        let mut mounts = self
            .mounts
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?;
        if !mounts.is_running(repository_id)? {
            return Err(MirageError::repository_conflict(
                "repository is not mounted; the token cannot be delivered",
            ));
        }
        mounts
            .send_drive_token(repository_id, token)
            .map(|()| ResponseBody::Json(json!({"supplied": true})))
    }

    fn disk_floor_set(
        &self,
        volume_root: &str,
        floor_bytes: u64,
        hysteresis_bytes: Option<u64>,
    ) -> Result<ResponseBody, MirageError> {
        let root = disk_floor::normalize_volume_root(volume_root)?;
        let space = disk_space::query(Path::new(&root))?;
        if floor_bytes >= space.total_bytes {
            return Err(MirageError::invalid_argument(
                "disk floor must be below the volume's total size",
            ));
        }
        let hysteresis =
            hysteresis_bytes.unwrap_or_else(|| disk_floor::default_hysteresis(floor_bytes));
        self.database.set_disk_floor(mirage_db::DiskFloor {
            volume_root: root.clone(),
            floor_bytes,
            hysteresis_bytes: hysteresis,
            updated_ns: now_ns(),
        })?;
        Ok(ResponseBody::Json(json!({
            "volume_root": root,
            "floor_bytes": floor_bytes,
            "hysteresis_bytes": hysteresis,
        })))
    }

    fn disk_floor_clear(&self, volume_root: &str) -> Result<ResponseBody, MirageError> {
        let root = disk_floor::normalize_volume_root(volume_root)?;
        if !self.database.clear_disk_floor(&root)? {
            return Err(MirageError::invalid_argument(
                "no disk floor is configured for that volume",
            ));
        }
        Ok(ResponseBody::Json(json!({
            "volume_root": root,
            "cleared": true,
        })))
    }

    fn disk_status(&self) -> Result<ResponseBody, MirageError> {
        let floors = self.database.disk_floors()?;
        let mut entries = Vec::new();
        for floor in &floors {
            let space = disk_space::query(Path::new(&floor.volume_root))?;
            let free = space.available_bytes;
            let breached = free < floor.floor_bytes;
            let deficit = floor.floor_bytes.saturating_sub(free);
            let mut cache_pages = 0_u64;
            let mut shadow_packs = 0_u64;
            let mut published_payloads = 0_u64;
            for repository in self.repositories()? {
                let Some(native_root) = self
                    .database
                    .load_repository_root(repository.repository_id)?
                else {
                    continue;
                };
                if disk_space::volume_root_of(&native_root).ok().as_deref()
                    != Some(floor.volume_root.as_str())
                {
                    continue;
                }
                if let Ok(source) = capacity::RepositoryCapacitySource::open(
                    &self.database,
                    repository.repository_id,
                    None,
                ) && let Ok((pages, shadow)) = source.evictable_breakdown()
                {
                    cache_pages += pages;
                    shadow_packs += shadow;
                }
                if let Ok(evictable) = self
                    .database
                    .published_payloads_evictable(repository.repository_id)
                {
                    published_payloads += evictable
                        .iter()
                        .map(|(_, length, _)| u64::try_from(*length).unwrap_or(0))
                        .sum::<u64>();
                }
            }
            let last = self.database.latest_disk_floor_run(&floor.volume_root)?;
            entries.push(json!({
                "volume_root": floor.volume_root,
                "total_bytes": space.total_bytes,
                "free_bytes": free,
                "floor_bytes": floor.floor_bytes,
                "hysteresis_bytes": floor.hysteresis_bytes,
                "breached": breached,
                "deficit_bytes": deficit,
                "evictable_bytes": {
                    "cache_pages": cache_pages,
                    "drive_shadow_packs": shadow_packs,
                    "published_payloads": published_payloads,
                },
                "last_reclaim": last.map(|run| json!({
                    "at_ns": run.at_ns,
                    "freed_bytes": run.freed_bytes,
                    "outcome": run.outcome,
                })),
            }));
        }
        Ok(ResponseBody::Json(json!({ "floors": entries })))
    }

    /// Resolves a mount-relative path (`/` or `\` separated) to a namespace
    /// inode; an empty path resolves to the volume root.
    fn resolve_namespace_path(
        &self,
        repository_id: RepositoryId,
        path: &str,
    ) -> Result<mirage_types::InodeId, MirageError> {
        let components: Vec<String> = path
            .split(['\\', '/'])
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
        if components.is_empty() {
            return self.database.namespace_root(repository_id)?.ok_or_else(|| {
                MirageError::invalid_argument("repository has no managed namespace")
            });
        }
        self.database
            .namespace_resolve_components(repository_id, &components)?
            .ok_or_else(|| MirageError::invalid_argument("path not found in the managed namespace"))
    }

    /// Reconstructs the `/`-joined namespace path of an inode by walking
    /// dirents upward; `None` for the root (listed as "/").
    fn namespace_path_of(
        &self,
        repository_id: RepositoryId,
        inode: mirage_types::InodeId,
    ) -> String {
        let mut names = Vec::new();
        let mut current = inode;
        for _ in 0..128 {
            match self
                .database
                .namespace_entry(repository_id, current)
                .ok()
                .flatten()
            {
                Some((parent, name)) => {
                    names.push(name);
                    current = parent;
                }
                None => break,
            }
        }
        names.reverse();
        format!("/{}", names.join("/"))
    }

    fn namespace_pin_set(
        &self,
        repository_id: RepositoryId,
        path: &str,
        pin: bool,
    ) -> Result<ResponseBody, MirageError> {
        let inode = self.resolve_namespace_path(repository_id, path)?;
        if pin {
            self.database
                .namespace_pin(repository_id, inode, now_ns())?;
        } else if !self.database.namespace_unpin(repository_id, inode)? {
            return Err(MirageError::invalid_argument("path is not pinned"));
        }
        // Refresh the live host's pinned set; an unmounted repo picks the
        // rows up at mount time.
        if let Ok(mut mounts) = self.mounts.lock()
            && mounts.is_running(repository_id).unwrap_or(false)
        {
            let _ = mounts.reload_pins(repository_id);
        }
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "path": path,
            "inode": hex_inode(inode),
            "pinned": pin,
        })))
    }

    fn namespace_pins_list(
        &self,
        repository_id: RepositoryId,
    ) -> Result<ResponseBody, MirageError> {
        let pins = self.database.namespace_pins(repository_id)?;
        let entries: Vec<_> = pins
            .iter()
            .map(|inode| {
                json!({
                    "inode": hex_inode(*inode),
                    "path": self.namespace_path_of(repository_id, *inode),
                })
            })
            .collect();
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "pins": entries,
        })))
    }

    /// One disk-floor reclaim pass over every configured volume: reclaim
    /// verified-clean cache/shadow data of unmounted repositories and ask
    /// mounted managed hosts to evict published payloads. Never removes data
    /// that is not safely recoverable.
    pub fn enforce_disk_floors(&self) -> Result<(), MirageError> {
        self.enforce_disk_floors_with(|path| disk_space::query(path).map(|s| s.available_bytes))
    }

    /// The probe reports available bytes on the volume; injectable for tests.
    #[doc(hidden)]
    pub fn enforce_disk_floors_with(
        &self,
        probe: impl Fn(&Path) -> Result<u64, MirageError>,
    ) -> Result<(), MirageError> {
        for floor in self.database.disk_floors()? {
            let Ok(free_bytes) = probe(Path::new(&floor.volume_root)) else {
                continue;
            };
            let Some(mut target) =
                disk_floor::reclaim_target(free_bytes, floor.floor_bytes, floor.hysteresis_bytes)
            else {
                continue;
            };
            let mut freed = 0_u64;
            let mut pinned_blocked = 0_u64;
            for repository in self.repositories()? {
                if freed >= target {
                    break;
                }
                if repository.state == RepositoryState::ReadyMounted {
                    // The managed journal lives under the service state root;
                    // only hosts whose journal volume matches may evict.
                    let state_root = runtime::service_state_root(&self.database)?;
                    if disk_space::volume_root_of(&state_root).ok().as_deref()
                        != Some(floor.volume_root.as_str())
                    {
                        continue;
                    }
                    if let Ok(bytes) = self
                        .mounts
                        .lock()
                        .map_err(|_| {
                            MirageError::internal_invariant("mount coordinator lock poisoned")
                        })?
                        .request_eviction(repository.repository_id, target - freed)
                    {
                        let (bytes, blocked) = bytes;
                        freed += bytes;
                        pinned_blocked += blocked;
                    }
                    continue;
                }
                let Some(native_root) = self
                    .database
                    .load_repository_root(repository.repository_id)?
                else {
                    continue;
                };
                if disk_space::volume_root_of(&native_root).ok().as_deref()
                    != Some(floor.volume_root.as_str())
                {
                    continue;
                }
                if let Ok(source) = capacity::RepositoryCapacitySource::open(
                    &self.database,
                    repository.repository_id,
                    None,
                ) {
                    freed += source.reclaim_up_to(target - freed).unwrap_or(0);
                }
            }
            let outcome = if freed >= target {
                "ok"
            } else if pinned_blocked > 0 {
                "blocked_by_pins"
            } else {
                "insufficient_evictable"
            };
            eprintln!(
                "disk floor breached on {}: free={} floor={} freed={} pinned_blocked={} outcome={outcome}",
                floor.volume_root, free_bytes, floor.floor_bytes, freed, pinned_blocked,
            );
            self.database
                .record_disk_floor_run(mirage_db::DiskFloorRun {
                    volume_root: floor.volume_root.clone(),
                    at_ns: now_ns(),
                    target_bytes: target,
                    freed_bytes: freed,
                    outcome: outcome.to_owned(),
                })?;
            let _ = &mut target;
        }
        Ok(())
    }

    fn unmount(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        let _lifecycle = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        let state = self
            .database
            .load_repository_state(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        if state != RepositoryState::ReadyMounted {
            return Err(MirageError::repository_conflict(
                "repository is not mounted",
            ));
        }
        let database_mount_root = self
            .database
            .load_repository_root(repository_id)?
            .ok_or_else(|| MirageError::integrity_mismatch("repository mount root is missing"))?;
        let record = runtime::load_mount_record(&self.database, repository_id)?;
        let mount_root = record
            .as_ref()
            .map_or(database_mount_root.as_path(), |record| {
                record.mount_point.as_path()
            });
        // Lifecycle ordering: quiesce → flush fence over committed journal
        // operations → unmount. A flush failure blocks the unmount rather
        // than silently dropping the durability boundary.
        mirage_engine::journal::LocalJournal::new(self.database.clone(), repository_id)
            .flush_fence(now_ns())?;
        let unmounted = self
            .mounts
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
            .unmount(repository_id);
        let recovered_after_restart = match unmounted {
            Ok(()) => false,
            Err(_) if !runtime::mount_target_exists(mount_root) => true,
            Err(error) => return Err(error),
        };
        self.database.set_repository_state(
            repository_id,
            RepositoryState::ReadyMounted,
            RepositoryEvent::UnmountRequested,
            now_ns(),
        )?;
        runtime::clear_mount_record(&self.database, repository_id)?;
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "state": "ready_unmounted",
            "recovered_after_restart": recovered_after_restart,
            "mount_point": record.map(|record| record.mount_point)
        })))
    }

    fn recover_failed_mount(&self, repository_id: RepositoryId) -> Result<(), MirageError> {
        let _ = runtime::clear_mount_record(&self.database, repository_id);
        self.database.set_repository_state(
            repository_id,
            RepositoryState::Mounting,
            RepositoryEvent::RecoveryRequested,
            now_ns(),
        )?;
        self.database.set_repository_state(
            repository_id,
            RepositoryState::Recovering,
            RepositoryEvent::RecoverySucceededUnmounted,
            now_ns(),
        )?;
        Ok(())
    }

    fn begin_update(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        let state = self
            .database
            .load_repository_state(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        if !matches!(
            state,
            RepositoryState::ReadyUnmounted | RepositoryState::ReadyMounted
        ) {
            return Err(MirageError::repository_conflict(
                "repository is not ready to begin an update",
            ));
        }
        let active = self
            .database
            .load_active_generation(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository has no active generation"))?;
        if self.database.load_active_update(repository_id)?.is_some() {
            return Err(MirageError::update_active(
                "repository already has an active update",
            ));
        }
        let target = active
            .generation_id
            .as_u64()
            .checked_add(1)
            .map(mirage_types::GenerationId::from_u64)
            .ok_or_else(|| MirageError::invalid_argument("target generation overflow"))?;
        let update_id = random_update_id()?;
        let updates = self
            .database
            .reads()
            .database_path()
            .parent()
            .ok_or_else(|| MirageError::internal_invariant("database has no state root"))?
            .join("updates");
        std::fs::create_dir_all(&updates).map_err(service_io)?;
        let journal_path = updates.join(format!("{update_id}.journal"));
        let mut journal_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&journal_path)
            .map_err(service_io)?;
        use std::io::Write as _;
        journal_file
            .write_all(b"MIRAGE_UPDATE_JOURNAL_V1\n")
            .map_err(service_io)?;
        journal_file.sync_all().map_err(service_io)?;
        self.database.set_repository_state(
            repository_id,
            state,
            RepositoryEvent::BeginUpdate,
            now_ns(),
        )?;
        if let Err(error) = self.database.create_update_journal(NewUpdateJournal {
            update_id,
            repository_id,
            base_generation: active.generation_id,
            target_generation: target,
            journal_path,
            created_at_ns: now_ns(),
        }) {
            self.recover_repository_to_unmounted(repository_id, RepositoryState::Updating)?;
            return Err(error);
        }
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "update_id": update_id.to_string(),
            "base_generation": active.generation_id.as_u64(),
            "target_generation": target.as_u64(),
            "state": UpdateState::Created.as_str()
        })))
    }

    fn update_status(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        self.detail(repository_id)?;
        let update = self.database.load_active_update(repository_id)?;
        Ok(ResponseBody::Json(match update {
            Some(update) => json!({
                "repository_id": repository_id.to_string(),
                "active": true,
                "update_id": update.update_id.to_string(),
                "base_generation": update.base_generation.as_u64(),
                "target_generation": update.target_generation.as_u64(),
                "state": update.state.as_str()
            }),
            None => json!({"repository_id": repository_id.to_string(), "active": false}),
        }))
    }

    fn rollback_update(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        let update = self
            .database
            .load_active_update(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository has no active update"))?;
        if update.state != UpdateState::Created {
            return Err(MirageError::provider_unavailable(
                "update has durable mutations and requires the restore executor",
            ));
        }
        self.database.transition_update_state(
            update.update_id,
            update.state,
            UpdateEvent::RollbackRequested,
            "rollback requested before mutation".into(),
            now_ns(),
        )?;
        self.database.transition_update_state(
            update.update_id,
            UpdateState::RollbackPending,
            UpdateEvent::RollbackCompleted,
            "empty update rolled back".into(),
            now_ns(),
        )?;
        self.database.set_repository_state(
            repository_id,
            RepositoryState::Updating,
            RepositoryEvent::UpdateCommitted,
            now_ns(),
        )?;
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "update_id": update.update_id.to_string(),
            "state": UpdateState::RolledBack.as_str()
        })))
    }

    fn commit_update(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        let update = self
            .database
            .load_active_update(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository has no active update"))?;
        if update.state != UpdateState::LocalActivationPending {
            return Err(MirageError::repository_conflict(
                "update is not ready for activation",
            ));
        }
        let target = self
            .database
            .load_verified_generation(repository_id, update.target_generation)?
            .ok_or_else(|| MirageError::integrity_mismatch("target generation is not verified"))?;
        let active = self.database.load_active_generation(repository_id)?;
        if active.as_ref().map(|value| value.generation_id) != Some(update.target_generation) {
            let expected = active.map(|value| (value.generation_id, value.commit_hash));
            self.database.activate_generation(
                repository_id,
                target.generation_id,
                target.commit_hash,
                expected,
                now_ns(),
            )?;
        }
        self.database.transition_update_state(
            update.update_id,
            update.state,
            UpdateEvent::ActivationCommitted,
            "verified generation activated".into(),
            now_ns(),
        )?;
        self.database.set_repository_state(
            repository_id,
            RepositoryState::Updating,
            RepositoryEvent::UpdateCommitted,
            now_ns(),
        )?;
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "update_id": update.update_id.to_string(),
            "generation": target.generation_id.as_u64(),
            "state": UpdateState::Committed.as_str()
        })))
    }

    fn recover_repository_to_unmounted(
        &self,
        repository_id: RepositoryId,
        state: RepositoryState,
    ) -> Result<(), MirageError> {
        self.database.set_repository_state(
            repository_id,
            state,
            RepositoryEvent::RecoveryRequested,
            now_ns(),
        )?;
        self.database.set_repository_state(
            repository_id,
            RepositoryState::Recovering,
            RepositoryEvent::RecoverySucceededUnmounted,
            now_ns(),
        )?;
        Ok(())
    }
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_nanos().min(i64::MAX as u128) as i64
        })
}

fn random_update_id() -> Result<UpdateId, MirageError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("secure update ID generation failed"))?;
    Ok(UpdateId::from_bytes(bytes))
}

fn random_space_lease_id() -> Result<mirage_types::SpaceLeaseId, MirageError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("secure Space Lease ID generation failed"))?;
    Ok(mirage_types::SpaceLeaseId::from_bytes(bytes))
}

fn service_io(error: std::io::Error) -> MirageError {
    MirageError::new(
        mirage_types::MirageErrorKind::Io,
        mirage_types::MirageErrorKind::Io.default_code(),
        "service state I/O failed",
    )
    .with_source(error)
}

fn summary_json(repository: &RepositorySummary) -> serde_json::Value {
    let (generation, commit) = repository
        .active
        .map_or((None, None), |(generation, commit)| {
            (Some(generation.as_u64()), Some(commit.to_string()))
        });
    json!({
        "repository_id": repository.repository_id.to_string(),
        "display_name": repository.display_name,
        "state": repository.state.as_str(),
        "active_generation": generation,
        "active_commit": commit
    })
}

fn repair_response(
    database: &Database,
    repository_id: RepositoryId,
    level: RepairLevel,
) -> Result<ResponseBody, MirageError> {
    let report = repair(
        &LocalMetadataRepairSource::new(database.clone(), repository_id),
        level,
    )?;
    Ok(ResponseBody::Json(json!({
        "repository_id": repository_id.to_string(),
        "verified_objects": report.verified_objects,
        "missing_objects": report.missing_objects.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "dirty_pages_preserved": report.dirty_pages_preserved
    })))
}

fn hex_inode(inode: mirage_types::InodeId) -> String {
    let mut out = String::with_capacity(32);
    for byte in inode.as_bytes() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn repository_id(command: &Command) -> Option<RepositoryId> {
    match command {
        Command::RepositoryDetail { repository_id }
        | Command::RepositoryConvert { repository_id, .. }
        | Command::RepositoryRestoreNative { repository_id, .. }
        | Command::RepositorySetDriveOrigin { repository_id, .. }
        | Command::RepositorySetVolumeMode { repository_id, .. }
        | Command::Mount { repository_id, .. }
        | Command::DriveTokenSupply { repository_id, .. }
        | Command::Unmount { repository_id }
        | Command::Profile { repository_id, .. }
        | Command::ProfileConfigure { repository_id, .. }
        | Command::Simulate { repository_id }
        | Command::CapacityPlan { repository_id, .. }
        | Command::CapacityAcquire { repository_id, .. }
        | Command::CapacityStatus { repository_id, .. }
        | Command::CapacityConsume { repository_id, .. }
        | Command::CapacityRelease { repository_id, .. }
        | Command::NativeActivate { repository_id, .. }
        | Command::NativeStatus { repository_id }
        | Command::Plan { repository_id, .. }
        | Command::Materialize { repository_id, .. }
        | Command::Admit { repository_id, .. }
        | Command::Launch { repository_id, .. }
        | Command::Verify { repository_id, .. }
        | Command::UpdateBegin { repository_id }
        | Command::UpdateStatus { repository_id }
        | Command::UpdateCommit { repository_id }
        | Command::UpdateRollback { repository_id }
        | Command::NamespacePin { repository_id, .. }
        | Command::NamespaceUnpin { repository_id, .. }
        | Command::NamespacePins { repository_id }
        | Command::Repair { repository_id }
        | Command::Cancel { repository_id, .. } => Some(*repository_id),
        Command::Status
        | Command::RepositoryList
        | Command::DiskFloorSet { .. }
        | Command::DiskFloorClear { .. }
        | Command::DiskStatus
        | Command::RepositoryRegister { .. }
        | Command::RepositoryAdopt { .. } => None,
    }
}

/// Immutable origin directory used for degraded read-through on a cache miss:
/// the repository's import root when its origin is local; `None` for Drive origins.
/// The managed Drive provider inputs for a mounted repository:
/// `drive-manifest.cbor` + `repository-key.dpapi` inside the verified import
/// root. Only meaningful for a managed volume on the Drive origin; both files
/// must exist or the mount proceeds without a provider (non-resident reads
/// fail unavailable until a token-bearing remount).
/// The configured disk floor for the volume holding the service state root
/// (where managed journals live), if any.
fn state_root_disk_floor(database: &Database) -> Option<u64> {
    let state_root = runtime::service_state_root(database).ok()?;
    let root = disk_space::volume_root_of(&state_root).ok()?;
    database
        .disk_floor(&root)
        .ok()?
        .map(|floor| floor.floor_bytes)
}

fn drive_provider_paths(
    database: &Database,
    repository_id: RepositoryId,
    managed: bool,
) -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>, MirageError> {
    if !managed {
        return Ok(None);
    }
    let config = runtime::load_config(database, repository_id)?;
    if config.origin != runtime::RuntimeOrigin::Drive {
        return Ok(None);
    }
    let manifest = config.import_root.join("drive-manifest.cbor");
    let key = config.import_root.join("repository-key.dpapi");
    if manifest.is_file() && key.is_file() {
        Ok(Some((manifest, key)))
    } else {
        Ok(None)
    }
}

fn local_origin_root(
    database: &Database,
    repository_id: RepositoryId,
) -> Result<Option<std::path::PathBuf>, MirageError> {
    let config = runtime::load_config(database, repository_id)?;
    Ok(match config.origin {
        runtime::RuntimeOrigin::Local => Some(config.import_root),
        runtime::RuntimeOrigin::Drive => None,
    })
}

fn error_response(error: MirageError) -> ResponseBody {
    ResponseBody::Error {
        code: error.code.into(),
        message: error.message,
    }
}
