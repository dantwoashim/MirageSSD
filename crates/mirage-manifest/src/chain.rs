use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use mirage_types::{CommitHash, MirageError};

use crate::commit::{RepositoryCommit, commit_hash, validate_commit};
use crate::signature::CommitVerifier;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainConflict {
    pub parent: CommitHash,
    pub children: Vec<CommitHash>,
}

#[derive(Debug)]
pub enum ChainValidationError {
    Invalid(Box<MirageError>),
    Conflict {
        error: Box<MirageError>,
        metadata: ChainConflict,
    },
}

impl fmt::Display for ChainValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Conflict { error, metadata } => write!(
                f,
                "{} (parent {}, {} valid children)",
                error,
                metadata.parent,
                metadata.children.len()
            ),
        }
    }
}

impl Error for ChainValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(error) | Self::Conflict { error, .. } => Some(error.as_ref()),
        }
    }
}

impl From<MirageError> for ChainValidationError {
    fn from(error: MirageError) -> Self {
        Self::Invalid(Box::new(error))
    }
}

pub fn validate_link(
    parent: &RepositoryCommit,
    child: &RepositoryCommit,
    verifier: &dyn CommitVerifier,
) -> Result<(), MirageError> {
    validate_commit(parent, verifier)?;
    validate_commit(child, verifier)?;
    if parent.body.repository_id != child.body.repository_id {
        return Err(MirageError::repository_conflict(
            "commit link crosses repository identities",
        ));
    }
    let expected_sequence = parent
        .body
        .sequence
        .checked_add(1)
        .ok_or_else(|| MirageError::repository_conflict("commit sequence overflows"))?;
    if child.body.sequence != expected_sequence {
        return Err(MirageError::repository_conflict(
            "commit sequence does not increment by exactly one",
        ));
    }
    if child.body.parent_commit != Some(commit_hash(parent)?) {
        return Err(MirageError::repository_conflict(
            "commit parent hash does not match canonical parent bytes",
        ));
    }
    Ok(())
}

pub fn select_highest_valid_chain(
    trust_root: &RepositoryCommit,
    candidates: &[RepositoryCommit],
    verifier: &dyn CommitVerifier,
) -> Result<Vec<RepositoryCommit>, ChainValidationError> {
    validate_commit(trust_root, verifier)?;
    let mut by_parent: BTreeMap<CommitHash, Vec<&RepositoryCommit>> = BTreeMap::new();
    for candidate in candidates {
        if let Some(parent) = candidate.body.parent_commit {
            by_parent.entry(parent).or_default().push(candidate);
        }
    }

    let mut chain = vec![trust_root.clone()];
    loop {
        let current = chain.last().expect("chain always contains trust root");
        let current_hash = commit_hash(current)?;
        let mut valid_children = Vec::new();
        if let Some(children) = by_parent.get(&current_hash) {
            for child in children {
                if validate_link(current, child, verifier).is_ok() {
                    valid_children.push(*child);
                }
            }
        }
        match valid_children.as_slice() {
            [] => return Ok(chain),
            [child] => chain.push((*child).clone()),
            children => {
                let mut hashes = children
                    .iter()
                    .map(|child| commit_hash(child))
                    .collect::<Result<Vec<_>, _>>()?;
                hashes.sort_unstable();
                return Err(ChainValidationError::Conflict {
                    error: Box::new(MirageError::repository_conflict(
                        "multiple valid commits descend from one trusted parent",
                    )),
                    metadata: ChainConflict {
                        parent: current_hash,
                        children: hashes,
                    },
                });
            }
        }
    }
}
