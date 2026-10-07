//! Disk floor and capacity accounting for managed volumes.

use super::*;

impl ControlPlaneHandler {
    pub(super) fn acquire_capacity(
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

    pub(super) fn disk_floor_set(
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

    pub(super) fn disk_floor_clear(&self, volume_root: &str) -> Result<ResponseBody, MirageError> {
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

    pub(super) fn disk_status(&self) -> Result<ResponseBody, MirageError> {
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
            // Managed journal payloads physically live under the service
            // state root â€” attribute them to that volume like reclaim does,
            // not to each repository's native_root.
            let state_root_volume = runtime::service_state_root(&self.database)
                .ok()
                .and_then(|root| disk_space::volume_root_of(&root).ok());
            for repository in self.repositories()? {
                if let Some(native_root) = self
                    .database
                    .load_repository_root(repository.repository_id)?
                    && disk_space::volume_root_of(&native_root).ok().as_deref()
                        == Some(floor.volume_root.as_str())
                    && let Ok(source) = capacity::RepositoryCapacitySource::open(
                        &self.database,
                        repository.repository_id,
                        None,
                    )
                    && let Ok((pages, shadow)) = source.evictable_breakdown()
                {
                    cache_pages += pages;
                    shadow_packs += shadow;
                }
                if state_root_volume.as_deref() == Some(floor.volume_root.as_str())
                    && let Ok(evictable) = self
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
                    // Only hosts whose journal (cache root) disk matches the
                    // floor's disk may evict for it.
                    let cache_root =
                        runtime::repository_cache_root(&self.database, repository.repository_id)?;
                    if disk_space::volume_root_of(&cache_root).ok().as_deref()
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
}

/// Immutable origin directory used for degraded read-through on a cache miss:
/// the repository's import root when its origin is local; `None` for Drive origins.
/// The managed Drive provider inputs for a mounted repository:
/// `drive-manifest.cbor` + `repository-key.dpapi` inside the verified import
/// root. Only meaningful for a managed volume on the Drive origin; both files
/// must exist or the mount proceeds without a provider (non-resident reads
/// fail unavailable until a token-bearing remount).
/// Advertised `(total, free)` for a mount plus the separate local staging
/// budget. A managed volume with a recorded Drive quota shows the cloud
/// capacity in Explorer and keeps its reviewed cache budget internal; without
/// a quota the budget doubles as the advertised size (the old behavior).
/// Non-managed mounts are unchanged and get no separate budget.
pub(super) fn managed_capacity(
    database: &Database,
    repository_id: RepositoryId,
    index: &Path,
    managed: bool,
) -> Result<((u64, u64), Option<u64>), MirageError> {
    if !managed {
        return Ok((
            runtime::volume_capacity(database, repository_id, index)?,
            None,
        ));
    }
    let budget = runtime::load_config(database, repository_id)?.cache_bytes;
    if let Some(capacity) = runtime::drive_capacity_snapshot(database, repository_id)? {
        return Ok((capacity, Some(budget)));
    }
    let (total, _) = runtime::volume_capacity(database, repository_id, index)?;
    Ok(((total.max(budget), budget), None))
}

/// The configured disk floor for the disk holding the repository's journal
/// (its cache root, or the service state root by default), if any.
pub(super) fn cache_disk_floor(database: &Database, repository_id: RepositoryId) -> Option<u64> {
    let cache_root = runtime::repository_cache_root(database, repository_id).ok()?;
    let root = disk_space::volume_root_of(&cache_root).ok()?;
    database
        .disk_floor(&root)
        .ok()?
        .map(|floor| floor.floor_bytes)
}
