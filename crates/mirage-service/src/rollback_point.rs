use mirage_types::{ContentHash, MirageError};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackEntry {
    pub relative_path: PathBuf,
    pub length: u64,
    pub hash: ContentHash,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPoint {
    entries: Vec<RollbackEntry>,
}
impl RollbackPoint {
    pub fn verified(mut entries: Vec<RollbackEntry>) -> Result<Self, MirageError> {
        for entry in &entries {
            if entry.relative_path.is_absolute()
                || entry
                    .relative_path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(MirageError::invalid_argument(
                    "rollback entry escapes repository root",
                ));
            }
        }
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        if entries
            .windows(2)
            .any(|pair| pair[0].relative_path == pair[1].relative_path)
        {
            return Err(MirageError::invalid_argument("duplicate rollback entry"));
        }
        Ok(Self { entries })
    }
    #[must_use]
    pub fn entries(&self) -> &[RollbackEntry] {
        &self.entries
    }
}
