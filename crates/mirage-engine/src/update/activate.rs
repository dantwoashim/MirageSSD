use mirage_types::{CommitHash, GenerationId, MirageError, RepositoryId};

/// Durable activation operations. Implementations must make each switch atomic.
pub trait ActivationBackend {
    fn active(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<(GenerationId, CommitHash)>, MirageError>;
    fn switch(
        &self,
        repository: RepositoryId,
        target: (GenerationId, CommitHash),
        expected: Option<(GenerationId, CommitHash)>,
        timestamp_ns: i64,
    ) -> Result<(), MirageError>;
}

/// Quiesces handles and mounts an already compiled, verified generation.
pub trait GenerationMounter {
    fn quiesce(&mut self) -> Result<(), MirageError>;
    fn mount_and_smoke_test(&mut self, generation: GenerationId) -> Result<(), MirageError>;
}

#[derive(Debug, Clone, Copy)]
pub struct ActivationPlan {
    pub repository: RepositoryId,
    pub generation: GenerationId,
    pub commit: CommitHash,
    pub timestamp_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationReport {
    pub previous: Option<(GenerationId, CommitHash)>,
    pub active: (GenerationId, CommitHash),
}

/// Activates a verified generation and rolls the durable pointer back if mounting fails.
pub fn activate_generation(
    backend: &impl ActivationBackend,
    mounter: &mut impl GenerationMounter,
    plan: ActivationPlan,
) -> Result<ActivationReport, MirageError> {
    let previous = backend.active(plan.repository)?;
    mounter.quiesce()?;
    let target = (plan.generation, plan.commit);
    backend.switch(plan.repository, target, previous, plan.timestamp_ns)?;
    if let Err(mount_error) = mounter.mount_and_smoke_test(plan.generation) {
        if let Some(old) = previous {
            backend.switch(plan.repository, old, Some(target), plan.timestamp_ns)?;
            // Best effort restores service availability; the original failure remains authoritative.
            let _ = mounter.mount_and_smoke_test(old.0);
        }
        return Err(mount_error);
    }
    Ok(ActivationReport {
        previous,
        active: target,
    })
}

impl ActivationBackend for mirage_db::Database {
    fn active(
        &self,
        repository: RepositoryId,
    ) -> Result<Option<(GenerationId, CommitHash)>, MirageError> {
        Ok(self
            .load_active_generation(repository)?
            .map(|active| (active.generation_id, active.commit_hash)))
    }

    fn switch(
        &self,
        repository: RepositoryId,
        target: (GenerationId, CommitHash),
        expected: Option<(GenerationId, CommitHash)>,
        timestamp_ns: i64,
    ) -> Result<(), MirageError> {
        self.activate_generation(repository, target.0, target.1, expected, timestamp_ns)
    }
}
