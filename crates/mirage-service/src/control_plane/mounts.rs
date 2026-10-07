//! Mount lifecycle: persistent mounts, mount/unmount, recovery.

use super::disk::cache_disk_floor;
use super::disk::managed_capacity;
use super::*;

impl ControlPlaneHandler {
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
            let origin_root = local_origin_root(&self.database, repository.repository_id)?;
            let managed = self
                .database
                .load_repository_volume_mode(repository.repository_id)?
                == Some(mirage_db::VolumeMode::Managed);
            // Same capacity rule as an explicit mount (the last recorded
            // Drive quota, or the budget when none exists yet).
            let (capacity, dirty_budget) =
                managed_capacity(&self.database, repository.repository_id, &index, managed)?;
            let drive_provider =
                drive_provider_paths(&self.database, repository.repository_id, managed)?;
            let journal_root = if managed {
                runtime::prepare_journal_root(&self.database, repository.repository_id)?
            } else {
                None
            };
            crate::logging::log_event(
                "mount.restoring",
                &format!("{} at {}", repository.repository_id, mount_point.display()),
            );
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
                        cache_disk_floor(&self.database, repository.repository_id)
                    } else {
                        None
                    },
                    &repository.display_name,
                    dirty_budget,
                    journal_root.as_deref(),
                )?;
            // Restored letters get the same Explorer icon and shell verbs as
            // an explicit mount; failures are cosmetic only.
            explorer_drive_icon::register(&owner_sid, &mount_point);
            explorer_drive_icon::register_verbs(
                &owner_sid,
                &mount_point,
                &repository.repository_id.to_string(),
            );
            crate::logging::log_event(
                "mount.restored_volume",
                &format!("{} at {}", repository.repository_id, mount_point.display()),
            );
            restored += 1;
        }
        Ok(restored)
    }
    pub(super) fn mount(
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
        let display_name = self
            .repositories()?
            .into_iter()
            .find(|item| item.repository_id == repository_id)
            .map(|item| item.display_name)
            .unwrap_or_else(|| "MirageSSD".to_owned());
        let origin_root = local_origin_root(&self.database, repository_id)?;
        let managed = self.database.load_repository_volume_mode(repository_id)?
            == Some(mirage_db::VolumeMode::Managed);
        // A managed Drive repository mounts with the publication manifest and
        // content key so the host can fetch non-resident pages on demand.
        let drive_provider = drive_provider_paths(&self.database, repository_id, managed)?;
        // With a fresh token, record the account's Drive quota so Explorer
        // shows cloud capacity. Best effort: a stale snapshot or the budget
        // fallback is still a correct mount.
        if managed
            && drive_provider.is_some()
            && let Some(token) = drive_access_token
            && let Err(error) =
                runtime::refresh_drive_capacity(&self.database, repository_id, token)
        {
            crate::logging::log_event("mount.quota_unavailable", &error.to_string());
        }
        let ((volume_total_bytes, volume_free_bytes), dirty_budget) =
            managed_capacity(&self.database, repository_id, &index, managed)?;
        let journal_root = if managed {
            runtime::prepare_journal_root(&self.database, repository_id)?
        } else {
            None
        };
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
                    cache_disk_floor(&self.database, repository_id)
                } else {
                    None
                },
                &display_name,
                dirty_budget,
                journal_root.as_deref(),
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
        // Explorer letter mounts get the per-user MirageSSD drive icon and
        // the pin/free shell verbs so This PC shows a branded drive;
        // failures are cosmetic only.
        if explorer_visible {
            explorer_drive_icon::register(&owner_sid, &mount_point);
            explorer_drive_icon::register_verbs(
                &owner_sid,
                &mount_point,
                &repository_id.to_string(),
            );
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
    /// token is never persisted â€” it leaves the service on the host's stdin.
    pub(super) fn supply_drive_token(
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

    pub(super) fn unmount(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
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
        // Lifecycle ordering: quiesce â†’ flush fence over committed journal
        // operations â†’ unmount. A flush failure blocks the unmount rather
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
        // The letter is free again: drop its icon and shell verbs so whatever
        // takes the letter next is not branded as a MirageSSD drive.
        if let Some(record) = record.as_ref()
            && let Ok(Some(owner_sid)) = self.database.load_repository_owner_sid(repository_id)
        {
            explorer_drive_icon::unregister(&owner_sid, &record.mount_point);
            explorer_drive_icon::unregister_verbs(&owner_sid, &record.mount_point);
        }
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

    pub(super) fn recover_repository_to_unmounted(
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
        // Both explicit mounts and logon recovery pass here. The Drive host
        // needs an arena even when no pages have been fetched yet.
        let active = database
            .load_active_generation(repository_id)?
            .ok_or_else(|| {
                MirageError::integrity_mismatch("managed Drive generation is missing")
            })?;
        let index_path = active.mount_index_path.ok_or_else(|| {
            MirageError::integrity_mismatch("managed Drive mount index is missing")
        })?;
        let index = mirage_index::MountIndex::open(&index_path)?;
        let page_size = u32::try_from(index.header().page_size)
            .map_err(|_| MirageError::unsupported_layout("repository page size exceeds u32"))?;
        // The arena is service-wide. Reuse its established capacity rather
        // than resize it when a second volume chooses a different budget.
        let shards = database.load_cache_shards()?;
        let cache_bytes = match shards.as_slice() {
            [] => config.cache_bytes,
            [spec] => spec.page_size.as_u64() * u64::from(spec.slot_count),
            _ => {
                return Err(MirageError::unsupported_layout(
                    "service cache requires one shard",
                ));
            }
        };
        runtime::open_cache(database, page_size, cache_bytes)?;
        Ok(Some((manifest, key)))
    } else {
        Err(MirageError::integrity_mismatch(
            "managed Drive volume requires its publication manifest and repository key; restore the missing metadata before mounting",
        ))
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
