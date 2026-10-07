//! Repository listing, detail, and mutation commands.

use super::*;

impl ControlPlaneHandler {
    pub(super) fn repositories(&self) -> Result<Vec<RepositorySummary>, MirageError> {
        self.database.list_repositories()
    }

    pub(super) fn repositories_for(
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

    pub(super) fn detail(&self, repository_id: RepositoryId) -> Result<ResponseBody, MirageError> {
        let repository = self
            .repositories()?
            .into_iter()
            .find(|item| item.repository_id == repository_id)
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        let mut summary = summary_json(&repository);
        // Truthful durability state: pending journal depth, live divergence,
        // and verified workspace coverage â€” a failed query fails the status
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
        if let Ok(Some(record)) = runtime::load_mount_record(&self.database, repository_id) {
            summary["mount_path"] = json!(record.mount_point.to_string_lossy());
        }
        // Local cache placement: where journal payloads live, how much room
        // that disk has, and the floor guarding it.
        if let Ok(cache_root) = runtime::repository_cache_root(&self.database, repository_id) {
            summary["cache_root"] = json!(cache_root.to_string_lossy());
            if let Ok(disk_root) = disk_space::volume_root_of(&cache_root) {
                summary["cache_disk_root"] = json!(disk_root);
                if let Ok(space) = disk_space::query(&cache_root) {
                    summary["cache_disk_free_bytes"] = json!(space.available_bytes);
                    summary["cache_disk_total_bytes"] = json!(space.total_bytes);
                }
                if let Ok(Some(floor)) = self.database.disk_floor(&disk_root) {
                    summary["cache_disk_floor_bytes"] = json!(floor.floor_bytes);
                }
            }
        }
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

    pub(super) fn set_drive_origin(
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

    pub(super) fn set_volume_mode(
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

    /// Relocates a managed volume's local cache (journal payloads) to the
    /// requested disk/directory, or back to the default under the state root
    /// with `None`. The repository must be unmounted. Every payload file is
    /// copied and verified (length + BLAKE3) before the new location is
    /// recorded; the old copies are removed only after that record lands, so
    /// a failure at any step leaves the previous location fully intact.
    pub(super) fn set_cache_root(
        &self,
        repository_id: RepositoryId,
        requested: Option<&str>,
    ) -> Result<ResponseBody, MirageError> {
        let _mount = self
            .mount_lifecycle
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount lifecycle lock poisoned"))?;
        let state = self
            .database
            .load_repository_state(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        let running = self
            .mounts
            .lock()
            .map_err(|_| MirageError::internal_invariant("mount coordinator lock poisoned"))?
            .is_running(repository_id)?;
        if state == RepositoryState::ReadyMounted || running {
            return Err(MirageError::repository_conflict(
                "unmount the drive before moving its local cache",
            ));
        }
        let state_root = runtime::service_state_root(&self.database)?;
        let current_root = runtime::repository_cache_root(&self.database, repository_id)?;
        let target_root = match requested {
            Some(request) => runtime::resolve_cache_root_request(request, repository_id)?,
            None => state_root.clone(),
        };
        let unchanged = target_root
            .to_string_lossy()
            .eq_ignore_ascii_case(&current_root.to_string_lossy());
        let source_journal = current_root.join("journal");
        let target_journal = target_root.join("journal");
        let mut moved_files = 0_u64;
        let mut moved_bytes = 0_u64;
        if !unchanged {
            if target_root != state_root {
                let fresh = !target_root.is_dir();
                std::fs::create_dir_all(&target_journal).map_err(service_io)?;
                // Hardened only under the service identity; a user-context
                // caller would lock itself out of the directory.
                if fresh && mirage_crypto::file_acl::running_as_local_system() {
                    mirage_crypto::file_acl::restrict_directory_to_system_admins(&target_root)?;
                }
            } else {
                std::fs::create_dir_all(&target_journal).map_err(service_io)?;
            }
            // Only this volume's payloads move; the default journal directory
            // is shared with other volumes and must not be drained.
            let referenced = self.database.referenced_payload_ids(repository_id)?;
            let mut copied: Vec<PathBuf> = Vec::new();
            let mut sources: Vec<PathBuf> = Vec::new();
            let mut copy_all = || -> Result<(), MirageError> {
                for payload_id in &referenced {
                    let name = format!("{}.payload", hex16(payload_id));
                    let source = source_journal.join(&name);
                    let Ok(metadata) = std::fs::metadata(&source) else {
                        continue; // evicted (remote-only) payload: nothing local to move
                    };
                    let destination = target_journal.join(&name);
                    std::fs::copy(&source, &destination).map_err(service_io)?;
                    copied.push(destination.clone());
                    let (source_hash, destination_hash) =
                        (blake3_file(&source)?, blake3_file(&destination)?);
                    let destination_len =
                        std::fs::metadata(&destination).map_err(service_io)?.len();
                    if destination_len != metadata.len() || source_hash != destination_hash {
                        return Err(MirageError::integrity_mismatch(format!(
                            "cache payload {name} did not copy intact"
                        )));
                    }
                    moved_files += 1;
                    moved_bytes += metadata.len();
                    sources.push(source);
                }
                Ok(())
            };
            if let Err(error) = copy_all() {
                for path in &copied {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
            let record = if target_root == state_root {
                None
            } else {
                Some(target_root.to_string_lossy().into_owned())
            };
            self.database
                .set_repository_cache_root(repository_id, record.as_deref(), now_ns())?;
            for source in &sources {
                let _ = std::fs::remove_file(source);
            }
            crate::logging::log_event(
                "cache.relocated",
                &format!(
                    "{repository_id} -> {} ({moved_files} payloads, {moved_bytes} bytes)",
                    target_root.display()
                ),
            );
        }
        Ok(ResponseBody::Json(json!({
            "repository_id": repository_id.to_string(),
            "cache_root": target_root.to_string_lossy(),
            "cache_disk_root": disk_space::volume_root_of(&target_root).ok(),
            "changed": !unchanged,
            "moved_payloads": moved_files,
            "moved_bytes": moved_bytes,
        })))
    }

    /// Removes a repository's local registration: optionally unmounts first,
    /// refuses to silently drop unpublished payload bytes, then deletes every
    /// local row plus this volume's journal payload files. Remote Drive
    /// objects are never touched.
    pub(super) fn repository_unregister(
        &self,
        repository_id: RepositoryId,
        force_unmount: bool,
        discard_unpublished: bool,
    ) -> Result<ResponseBody, MirageError> {
        let state = self
            .database
            .load_repository_state(repository_id)?
            .ok_or_else(|| MirageError::invalid_argument("repository is not configured"))?;
        let was_mounted = state == RepositoryState::ReadyMounted;
        if was_mounted && !force_unmount {
            return Err(MirageError::repository_conflict(
                "repository is still mounted; retry with force_unmount to unmount it first",
            ));
        }
        if was_mounted {
            self.unmount(repository_id)?;
        }
        let publication = self.database.payload_publication_stats(
            repository_id,
            &mirage_db::payload_remote::MANAGED_JOURNAL_FILE_ID,
        )?;
        if publication.pending_bytes > 0 && !discard_unpublished {
            return Err(MirageError::repository_conflict(format!(
                "{} bytes have not been uploaded to Drive yet; retry with discard_unpublished to drop them",
                publication.pending_bytes
            )));
        }
        // Drop any Explorer drive icon registered for this repository's
        // mount letter before the rows disappear.
        if let Ok(Some(record)) = runtime::load_mount_record(&self.database, repository_id)
            && record.explorer_visible
            && let Ok(Some(owner_sid)) = self.database.load_repository_owner_sid(repository_id)
        {
            explorer_drive_icon::unregister(&owner_sid, &record.mount_point);
            explorer_drive_icon::unregister_verbs(&owner_sid, &record.mount_point);
        }
        // Payload ids that belonged to this volume â€” after the rows are gone
        // their journal files are pure orphans, so delete them now rather
        // than waiting for the next startup sweep.
        let owned_payloads = self
            .database
            .referenced_payload_ids(repository_id)
            .unwrap_or_default();
        let removed = self.database.unregister_repository(repository_id)?;
        let mut journal_files_removed = 0_u64;
        if let Ok(state_root) = runtime::service_state_root(&self.database) {
            let journal = state_root.join("journal");
            for payload in owned_payloads {
                let mut name = String::with_capacity(40);
                for byte in payload {
                    name.push_str(&format!("{byte:02x}"));
                }
                name.push_str(".payload");
                if journal.join(&name).is_file()
                    && std::fs::remove_file(journal.join(&name)).is_ok()
                {
                    journal_files_removed += 1;
                }
            }
        }
        crate::logging::log_event(
            "repository.unregistered",
            &format!("{repository_id} journal_files={journal_files_removed}"),
        );
        Ok(ResponseBody::Json(json!({
            "unregistered": removed,
            "was_mounted": was_mounted,
            "discarded_pending_bytes": if discard_unpublished { publication.pending_bytes } else { 0 },
            "journal_files_removed": journal_files_removed,
        })))
    }
}

pub(super) fn summary_json(repository: &RepositorySummary) -> serde_json::Value {
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

pub(super) fn repair_response(
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

pub(super) fn repository_id(command: &Command) -> Option<RepositoryId> {
    match command {
        Command::RepositoryDetail { repository_id }
        | Command::RepositoryConvert { repository_id, .. }
        | Command::RepositoryRestoreNative { repository_id, .. }
        | Command::RepositorySetDriveOrigin { repository_id, .. }
        | Command::RepositorySetVolumeMode { repository_id, .. }
        | Command::RepositorySetCacheRoot { repository_id, .. }
        | Command::Mount { repository_id, .. }
        | Command::DriveTokenSupply { repository_id, .. }
        | Command::Unmount { repository_id }
        | Command::RepositoryUnregister { repository_id, .. }
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
        | Command::DiskReclaimNow
        | Command::RepositoryRegister { .. }
        | Command::RepositoryAdopt { .. } => None,
    }
}
