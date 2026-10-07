use mirage_db::{Database, NewUpdateJournal, RepositorySummary};
use mirage_engine::repair::{LocalMetadataRepairSource, RepairLevel, repair};
use mirage_ipc::{Authorization, Command, Principal, PrincipalRole, Request, ResponseBody};
use mirage_types::{
    MirageError, RepositoryEvent, RepositoryId, RepositoryState, UpdateEvent, UpdateId, UpdateState,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runtime::{self, RegisterSpec};
use crate::{
    MountControl, RequestHandler, capacity, mount_control::UnavailableMountControl, native_session,
};
use crate::{disk_floor, disk_space};

mod disk;
mod explorer_drive_icon;
mod launches;
mod mounts;
mod namespace;
mod repositories;
mod updates;

use repositories::repair_response;
use repositories::repository_id;
use repositories::summary_json;

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
            Command::RepositorySetCacheRoot {
                repository_id,
                cache_root,
            } => self.set_cache_root(repository_id, cache_root.as_deref()),
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
            Command::RepositoryUnregister {
                repository_id,
                force_unmount,
                discard_unpublished,
            } => self.repository_unregister(repository_id, force_unmount, discard_unpublished),
            Command::DiskFloorSet {
                volume_root,
                floor_bytes,
                hysteresis_bytes,
            } => self.disk_floor_set(&volume_root, floor_bytes, hysteresis_bytes),
            Command::DiskFloorClear { volume_root } => self.disk_floor_clear(&volume_root),
            Command::DiskStatus => self.disk_status(),
            Command::DiskReclaimNow => self
                .enforce_disk_floors()
                .map(|_| ResponseBody::Json(json!({"reclaimed": true}))),
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

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_nanos().min(i64::MAX as u128) as i64
        })
}

fn random_space_lease_id() -> Result<mirage_types::SpaceLeaseId, MirageError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("secure Space Lease ID generation failed"))?;
    Ok(mirage_types::SpaceLeaseId::from_bytes(bytes))
}

fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// BLAKE3 of a whole file, streamed â€” cache payloads are up to 32 MiB.
fn blake3_file(path: &Path) -> Result<[u8; 32], MirageError> {
    let mut file = std::fs::File::open(path).map_err(service_io)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).map_err(service_io)?;
    Ok(*hasher.finalize().as_bytes())
}

fn service_io(error: std::io::Error) -> MirageError {
    MirageError::new(
        mirage_types::MirageErrorKind::Io,
        mirage_types::MirageErrorKind::Io.default_code(),
        "service state I/O failed",
    )
    .with_source(error)
}

fn error_response(error: MirageError) -> ResponseBody {
    ResponseBody::Error {
        code: error.code.into(),
        message: error.message,
    }
}
