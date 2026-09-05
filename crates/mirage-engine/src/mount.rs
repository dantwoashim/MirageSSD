use crate::MountGeneration;
use mirage_types::MirageError;
use std::sync::{Arc, RwLock};

pub struct MountedRepository {
    active: RwLock<Arc<MountGeneration>>,
}
impl MountedRepository {
    #[must_use]
    pub fn new(generation: Arc<MountGeneration>) -> Self {
        Self {
            active: RwLock::new(generation),
        }
    }
    pub fn current(&self) -> Result<Arc<MountGeneration>, MirageError> {
        self.active
            .read()
            .map(|generation| Arc::clone(&generation))
            .map_err(|_| MirageError::internal_invariant("mount generation lock poisoned"))
    }
    pub fn swap(
        &self,
        generation: Arc<MountGeneration>,
    ) -> Result<Arc<MountGeneration>, MirageError> {
        let mut active = self
            .active
            .write()
            .map_err(|_| MirageError::internal_invariant("mount generation lock poisoned"))?;
        Ok(std::mem::replace(&mut *active, generation))
    }
}
