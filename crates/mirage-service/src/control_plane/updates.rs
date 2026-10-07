//! Repository update begin/status/rollback/commit.

use super::*;

impl ControlPlaneHandler {
    pub(super) fn begin_update(
        &self,
        repository_id: RepositoryId,
    ) -> Result<ResponseBody, MirageError> {
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

    pub(super) fn update_status(
        &self,
        repository_id: RepositoryId,
    ) -> Result<ResponseBody, MirageError> {
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

    pub(super) fn rollback_update(
        &self,
        repository_id: RepositoryId,
    ) -> Result<ResponseBody, MirageError> {
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

    pub(super) fn commit_update(
        &self,
        repository_id: RepositoryId,
    ) -> Result<ResponseBody, MirageError> {
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
}

fn random_update_id() -> Result<UpdateId, MirageError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("secure update ID generation failed"))?;
    Ok(UpdateId::from_bytes(bytes))
}
