use mirage_backend::{ObjectBackend, ObjectKind, RemoteObjectRef};
use mirage_manifest::{
    ChainValidationError, CommitVerifier, RepositoryCommit, RepositoryManifest, commit_hash,
    decode_commit_bounded, decode_manifest_bounded, manifest_hash, select_highest_valid_chain,
    validate_commit,
};
use mirage_types::{CommitHash, MirageError, RepositoryId};

use crate::object_plan::pack_set_hash;
use crate::repository_writer::read_complete;

#[derive(Debug, Clone)]
pub struct RecoveredRepository {
    pub chain: Vec<RepositoryCommit>,
    pub head_hash: CommitHash,
    pub manifest: RepositoryManifest,
    pub manifest_object: RemoteObjectRef,
    pub verified_pack_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryHints {
    pub cached_commit: Option<RemoteObjectRef>,
    pub latest_hint: Option<RemoteObjectRef>,
}

pub async fn recover_repository(
    backend: &dyn ObjectBackend,
    repository_id: RepositoryId,
    verifier: &dyn CommitVerifier,
) -> Result<RecoveredRepository, MirageError> {
    recover_repository_with_hints(backend, repository_id, verifier, RecoveryHints::default()).await
}

pub async fn recover_repository_with_hints(
    backend: &dyn ObjectBackend,
    repository_id: RepositoryId,
    verifier: &dyn CommitVerifier,
    hints: RecoveryHints,
) -> Result<RecoveredRepository, MirageError> {
    let mut references = Vec::new();
    if let Some(cached) = hints.cached_commit {
        references.push(cached);
    }
    if let Some(latest) = hints.latest_hint {
        references.push(latest);
    }
    references.extend(backend.enumerate_commits(repository_id).await?);
    references.sort_by(|left, right| {
        left.provider_object_id
            .as_str()
            .cmp(right.provider_object_id.as_str())
    });
    references.dedup_by(|left, right| left.provider_object_id == right.provider_object_id);
    let mut valid = Vec::new();
    for object in references {
        if object.kind != ObjectKind::Commit {
            continue;
        }
        let Ok(bytes) = read_complete(backend, &object, 64 * 1024).await else {
            continue;
        };
        let Ok(commit) = decode_commit_bounded(&bytes) else {
            continue;
        };
        if commit.body.repository_id == repository_id && validate_commit(&commit, verifier).is_ok()
        {
            valid.push(commit);
        }
    }
    let mut roots: Vec<_> = valid
        .iter()
        .filter(|commit| commit.body.sequence == 0 && commit.body.parent_commit.is_none())
        .cloned()
        .collect();
    roots.sort_by_key(|commit| commit_hash(commit).unwrap_or(CommitHash::from_bytes([0xff; 32])));
    if roots.is_empty() {
        return Err(MirageError::remote_object_missing(
            "no valid trust-root commit was discovered",
        ));
    }
    if roots.len() != 1 {
        return Err(MirageError::repository_conflict(
            "multiple valid trust-root commits were discovered",
        ));
    }
    let trust_root = roots.remove(0);
    let candidates: Vec<_> = valid
        .into_iter()
        .filter(|commit| commit.body.sequence != 0)
        .collect();
    let chain = select_highest_valid_chain(&trust_root, &candidates, verifier).map_err(
        |error| match error {
            ChainValidationError::Invalid(error) => *error,
            ChainValidationError::Conflict { error, .. } => *error,
        },
    )?;
    let head = chain
        .last()
        .ok_or_else(|| MirageError::internal_invariant("recovered chain is empty"))?;
    let manifest_object = head.body.manifest_object.clone();
    let manifest_bytes = read_complete(backend, &manifest_object, 256 * 1024 * 1024).await?;
    let manifest =
        decode_manifest_bounded(&manifest_bytes, mirage_manifest::DecodeLimits::default())?;
    if manifest.repository_id != repository_id
        || manifest_hash(&manifest)? != head.body.manifest_hash
    {
        return Err(MirageError::integrity_mismatch(
            "recovered manifest does not match the authoritative commit",
        ));
    }
    let mut packs: Vec<_> = manifest
        .remote_locations
        .iter()
        .map(|location| location.object.clone())
        .collect();
    packs.sort_by_key(|object| object.content_hash);
    packs.dedup_by_key(|object| object.content_hash);
    for pack in &packs {
        let stat = backend.stat(pack).await?;
        if stat.content_hash != pack.content_hash || stat.byte_length != pack.byte_length {
            return Err(MirageError::integrity_mismatch(
                "manifest pack metadata failed verification",
            ));
        }
    }
    if pack_set_hash(packs.iter().map(|object| object.content_hash).collect())
        != head.body.referenced_pack_set_hash
    {
        return Err(MirageError::integrity_mismatch(
            "commit pack-set hash differs from recovered manifest",
        ));
    }
    Ok(RecoveredRepository {
        head_hash: commit_hash(head)?,
        chain,
        manifest,
        manifest_object,
        verified_pack_count: packs.len(),
    })
}
