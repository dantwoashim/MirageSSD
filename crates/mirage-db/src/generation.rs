use std::path::PathBuf;

use mirage_types::{CommitHash, GenerationId, ManifestHash, MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::Database;
use crate::error::{conflict, sqlite};
use crate::value::{fixed, nonnegative, path_text, sqlite_integer, stored_path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGeneration {
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub commit_hash: CommitHash,
    pub manifest_hash: ManifestHash,
    pub manifest_local_path: PathBuf,
    pub mount_index_path: Option<PathBuf>,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveGeneration {
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub commit_hash: CommitHash,
    pub manifest_hash: ManifestHash,
    pub manifest_local_path: PathBuf,
    pub mount_index_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Activation {
    pub repository_id: RepositoryId,
    pub target_generation: GenerationId,
    pub target_commit_hash: CommitHash,
    pub expected_current: Option<(GenerationId, CommitHash)>,
    pub updated_at_ns: i64,
}

impl Database {
    pub fn load_verified_generation(
        &self,
        repository_id: RepositoryId,
        generation_id: GenerationId,
    ) -> Result<Option<VerifiedGeneration>, MirageError> {
        let generation = sqlite_integer(generation_id.as_u64(), "generation")?;
        self.reads.with_connection(|connection| {
            let row = connection
                .query_row(
                    "SELECT commit_hash, manifest_hash, manifest_local_path, mount_index_path, created_at_ns
                     FROM generations WHERE repository_id = ?1 AND generation = ?2 AND verified = 1",
                    params![repository_id.as_bytes().as_slice(), generation],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load verified generation"))?;
            row.map(|(commit, manifest, manifest_path, index_path, created_at_ns)| {
                Ok(VerifiedGeneration {
                    repository_id,
                    generation_id,
                    commit_hash: CommitHash::from_bytes(fixed(commit, "commit hash")?),
                    manifest_hash: ManifestHash::from_bytes(fixed(manifest, "manifest hash")?),
                    manifest_local_path: stored_path(manifest_path)?,
                    mount_index_path: index_path.map(stored_path).transpose()?,
                    created_at_ns,
                })
            })
            .transpose()
        })
    }

    pub fn insert_verified_generation(
        &self,
        generation: VerifiedGeneration,
    ) -> Result<(), MirageError> {
        self.writer.insert_verified_generation(generation)
    }

    pub fn activate_generation(
        &self,
        repository_id: RepositoryId,
        target_generation: GenerationId,
        target_commit_hash: CommitHash,
        expected_current: Option<(GenerationId, CommitHash)>,
        updated_at_ns: i64,
    ) -> Result<(), MirageError> {
        self.writer.activate_generation(Activation {
            repository_id,
            target_generation,
            target_commit_hash,
            expected_current,
            updated_at_ns,
        })
    }

    pub fn load_active_generation(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<ActiveGeneration>, MirageError> {
        self.reads.with_connection(|connection| {
            let row = connection
                .query_row(
                    "SELECT r.active_generation, r.active_commit_hash, g.manifest_hash,
                            g.manifest_local_path, g.mount_index_path
                     FROM repositories r
                     JOIN generations g
                       ON g.repository_id = r.repository_id
                      AND g.generation = r.active_generation
                     WHERE r.repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load active generation"))?;
            row.map(
                |(generation, commit, manifest, manifest_path, index_path)| {
                    Ok(ActiveGeneration {
                        repository_id,
                        generation_id: GenerationId::from_u64(nonnegative(
                            generation,
                            "generation",
                        )?),
                        commit_hash: CommitHash::from_bytes(fixed(commit, "commit hash")?),
                        manifest_hash: ManifestHash::from_bytes(fixed(manifest, "manifest hash")?),
                        manifest_local_path: stored_path(manifest_path)?,
                        mount_index_path: index_path.map(stored_path).transpose()?,
                    })
                },
            )
            .transpose()
        })
    }
}

pub(crate) fn insert_verified(
    connection: &mut Connection,
    generation: VerifiedGeneration,
) -> Result<(), MirageError> {
    let generation_id = sqlite_integer(generation.generation_id.as_u64(), "generation")?;
    let manifest_path = path_text(&generation.manifest_local_path)?;
    let index_path = generation
        .mount_index_path
        .as_deref()
        .map(path_text)
        .transpose()?;
    let existing = connection
        .query_row(
            "SELECT commit_hash, manifest_hash, manifest_local_path, mount_index_path, verified
             FROM generations WHERE repository_id = ?1 AND generation = ?2",
            params![
                generation.repository_id.as_bytes().as_slice(),
                generation_id
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to query generation identity"))?;
    if let Some((commit, manifest, stored_manifest_path, stored_index_path, verified)) = existing {
        if commit == generation.commit_hash.as_bytes()
            && manifest == generation.manifest_hash.as_bytes()
            && stored_manifest_path == manifest_path
            && stored_index_path.as_deref() == index_path
            && verified == 1
        {
            return Ok(());
        }
        return Err(conflict(
            "generation identity conflicts with an existing row",
        ));
    }
    connection
        .execute(
            "INSERT INTO generations(
                repository_id, generation, commit_hash, manifest_hash,
                manifest_local_path, mount_index_path, verified, created_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
            params![
                generation.repository_id.as_bytes().as_slice(),
                generation_id,
                generation.commit_hash.as_bytes().as_slice(),
                generation.manifest_hash.as_bytes().as_slice(),
                manifest_path,
                index_path,
                generation.created_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to insert verified generation"))?;
    Ok(())
}

pub(crate) fn activate(
    connection: &mut Connection,
    activation: Activation,
) -> Result<(), MirageError> {
    let target = sqlite_integer(activation.target_generation.as_u64(), "target generation")?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin generation activation"))?;
    let current = transaction
        .query_row(
            "SELECT active_generation, active_commit_hash
             FROM repositories WHERE repository_id = ?1",
            [activation.repository_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to load repository activation state"))?
        .ok_or_else(|| MirageError::invalid_argument("repository does not exist"))?;
    let current = match current {
        (None, None) => None,
        (Some(generation), Some(commit)) => Some((
            GenerationId::from_u64(nonnegative(generation, "active generation")?),
            CommitHash::from_bytes(fixed(commit, "active commit hash")?),
        )),
        _ => {
            return Err(MirageError::integrity_mismatch(
                "repository active generation columns disagree",
            ));
        }
    };
    if current != activation.expected_current {
        return Err(conflict("active generation changed concurrently"));
    }
    let target_row = transaction
        .query_row(
            "SELECT commit_hash, verified FROM generations
             WHERE repository_id = ?1 AND generation = ?2",
            params![activation.repository_id.as_bytes().as_slice(), target],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to load activation target"))?
        .ok_or_else(|| MirageError::invalid_argument("target generation does not exist"))?;
    if target_row.1 != 1 || target_row.0.as_slice() != activation.target_commit_hash.as_bytes() {
        return Err(conflict(
            "activation target is unverified or has a different commit hash",
        ));
    }
    transaction
        .execute(
            "UPDATE repositories
             SET active_generation = ?1, active_commit_hash = ?2, updated_at_ns = ?3
             WHERE repository_id = ?4",
            params![
                target,
                activation.target_commit_hash.as_bytes().as_slice(),
                activation.updated_at_ns,
                activation.repository_id.as_bytes().as_slice(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to activate generation"))?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit generation activation"))
}
