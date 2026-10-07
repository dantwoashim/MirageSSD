//! Launch bookkeeping: bounded launches and native activation.

use super::*;

impl ControlPlaneHandler {
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

    pub(super) fn reap_completed_launches(&self) -> Result<(), MirageError> {
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

    pub(super) fn launch_command(
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

    pub(super) fn activate_native(
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
