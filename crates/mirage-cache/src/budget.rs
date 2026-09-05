use mirage_types::MirageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservationClass {
    Blocking,
    Capsule,
    Prefetch,
    DirtyUpdate,
    Staging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetConfig {
    pub hard_bytes: u64,
    pub prefetch_soft_bytes: u64,
    pub update_safety_reserve: u64,
    pub dirty_update_bytes: u64,
}

impl BudgetConfig {
    pub fn validate(self) -> Result<(), MirageError> {
        if self.hard_bytes == 0
            || self.prefetch_soft_bytes > self.hard_bytes
            || self.update_safety_reserve > self.hard_bytes
            || self.dirty_update_bytes > self.update_safety_reserve
        {
            return Err(MirageError::invalid_argument(
                "cache reservation budget is inconsistent",
            ));
        }
        Ok(())
    }
}
