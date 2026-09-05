use mirage_types::{GenerationId, MirageError, RepositoryId, UpdateId};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateContext {
    pub update_id: UpdateId,
    pub repository_id: RepositoryId,
    pub base_generation: GenerationId,
    pub target_generation: GenerationId,
    pub page_size: u32,
}
impl UpdateContext {
    pub fn validate(self) -> Result<(), MirageError> {
        if self.target_generation.0 <= self.base_generation.0
            || self.page_size < 65536
            || !self.page_size.is_power_of_two()
        {
            Err(MirageError::invalid_argument("invalid update context"))
        } else {
            Ok(())
        }
    }
}
