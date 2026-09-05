use crate::mark_set::MarkSet;
use mirage_types::{ContentHash, MirageError};

#[derive(Debug, Clone, Copy)]
pub struct RemoteCandidate {
    pub hash: ContentHash,
    pub created_sequence: u64,
}

/// Produces a dry-run deletion plan only after two identical root scans.
pub fn plan_unreferenced(
    objects: &[RemoteCandidate],
    first_roots: &MarkSet,
    second_roots: &MarkSet,
    root_set_unchanged: bool,
    current_sequence: u64,
    grace_sequences: u64,
) -> Result<Vec<ContentHash>, MirageError> {
    if !root_set_unchanged {
        return Err(MirageError::repository_conflict(
            "GC roots changed between mark passes",
        ));
    }
    Ok(objects
        .iter()
        .filter(|object| {
            current_sequence.saturating_sub(object.created_sequence) >= grace_sequences
                && !first_roots.contains(&object.hash)
                && !second_roots.contains(&object.hash)
        })
        .map(|object| object.hash)
        .collect())
}
