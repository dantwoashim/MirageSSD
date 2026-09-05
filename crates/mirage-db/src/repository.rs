use std::path::PathBuf;

use mirage_types::{
    CommitHash, GenerationId, MirageError, RepositoryEvent, RepositoryId, RepositoryState,
    transition_repository,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::{conflict, sqlite, transition};
use crate::state_codec;
use crate::value::{bounded_text, fixed, nonnegative, path_text};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRepository {
    pub repository_id: RepositoryId,
    pub display_name: String,
    pub local_root: PathBuf,
    pub owner_sid: String,
    pub content_encrypted: bool,
    pub initial_state: RepositoryState,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySummary {
    pub repository_id: RepositoryId,
    pub display_name: String,
    pub state: RepositoryState,
    pub active: Option<(GenerationId, CommitHash)>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RepositoryStateChange {
    pub repository_id: RepositoryId,
    pub expected: RepositoryState,
    pub event: RepositoryEvent,
    pub updated_at_ns: i64,
}

impl Database {
    pub fn load_repository_owner_sid(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<String>, MirageError> {
        self.reads.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT owner_sid FROM repositories WHERE repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load repository owner SID"))
        })
    }

    pub fn load_repository_content_encrypted(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<bool>, MirageError> {
        self.reads.with_connection(|connection| {
            let value = connection
                .query_row(
                    "SELECT content_encrypted FROM repositories WHERE repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load repository encryption policy"))?;
            value
                .map(|value| match value {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(MirageError::integrity_mismatch(
                        "repository encryption policy is invalid",
                    )),
                })
                .transpose()
        })
    }

    pub fn load_repository_root(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<PathBuf>, MirageError> {
        self.reads.with_connection(|connection| {
            let value = connection
                .query_row(
                    "SELECT local_root FROM repositories WHERE repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load repository root"))?;
            value.map(crate::value::stored_path).transpose()
        })
    }

    pub fn list_repositories(&self) -> Result<Vec<RepositorySummary>, MirageError> {
        self.reads.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT repository_id, display_name, state, active_generation, active_commit_hash
                     FROM repositories ORDER BY repository_id",
                )
                .map_err(|error| sqlite(error, "failed to prepare repository inventory"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                })
                .map_err(|error| sqlite(error, "failed to query repository inventory"))?;
            let mut repositories = Vec::new();
            for row in rows {
                let (id, name, state, generation, commit) = row
                    .map_err(|error| sqlite(error, "failed to read repository inventory"))?;
                let active = match (generation, commit) {
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
                repositories.push(RepositorySummary {
                    repository_id: RepositoryId::from_bytes(fixed(id, "repository ID")?),
                    display_name: name,
                    state: state_codec::repository(&state)?,
                    active,
                });
            }
            Ok(repositories)
        })
    }

    pub fn create_repository(&self, repository: NewRepository) -> Result<(), MirageError> {
        self.writer.create_repository(repository)
    }

    pub fn set_repository_owner_sid(
        &self,
        repository_id: RepositoryId,
        expected_owner_sid: &str,
        new_owner_sid: &str,
    ) -> Result<(), MirageError> {
        validate_owner_sid(expected_owner_sid)?;
        validate_owner_sid(new_owner_sid)?;
        self.writer.set_repository_owner_sid(
            repository_id,
            expected_owner_sid.to_owned(),
            new_owner_sid.to_owned(),
        )
    }

    pub fn set_repository_state(
        &self,
        repository_id: RepositoryId,
        expected: RepositoryState,
        event: RepositoryEvent,
        updated_at_ns: i64,
    ) -> Result<RepositoryState, MirageError> {
        self.writer.set_repository_state(RepositoryStateChange {
            repository_id,
            expected,
            event,
            updated_at_ns,
        })
    }

    pub fn load_repository_state(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<RepositoryState>, MirageError> {
        self.reads.with_connection(|connection| {
            let value: Option<String> = connection
                .query_row(
                    "SELECT state FROM repositories WHERE repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load repository state"))?;
            value
                .map(|value| state_codec::repository(&value))
                .transpose()
        })
    }
}

pub(crate) fn create(
    connection: &mut Connection,
    repository: NewRepository,
) -> Result<(), MirageError> {
    bounded_text(&repository.display_name, 1, 256, "repository display name")?;
    validate_owner_sid(&repository.owner_sid)?;
    let local_root = path_text(&repository.local_root)?;
    connection
        .execute(
            "INSERT INTO repositories(
                repository_id, display_name, local_root, active_generation,
                active_commit_hash, state, created_at_ns, updated_at_ns, owner_sid,
                content_encrypted
             ) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5, ?5, ?6, ?7)",
            params![
                repository.repository_id.as_bytes().as_slice(),
                repository.display_name,
                local_root,
                repository.initial_state.as_str(),
                repository.created_at_ns,
                repository.owner_sid,
                i64::from(repository.content_encrypted),
            ],
        )
        .map_err(|error| sqlite(error, "failed to create repository"))?;
    Ok(())
}

pub(crate) fn set_owner_sid(
    connection: &mut Connection,
    repository_id: RepositoryId,
    expected_owner_sid: String,
    new_owner_sid: String,
) -> Result<(), MirageError> {
    validate_owner_sid(&expected_owner_sid)?;
    validate_owner_sid(&new_owner_sid)?;
    let changed = connection
        .execute(
            "UPDATE repositories SET owner_sid = ?1
             WHERE repository_id = ?2 AND owner_sid = ?3",
            params![
                new_owner_sid,
                repository_id.as_bytes().as_slice(),
                expected_owner_sid,
            ],
        )
        .map_err(|error| sqlite(error, "failed to update repository owner SID"))?;
    if changed != 1 {
        return Err(conflict("repository owner changed concurrently"));
    }
    Ok(())
}

fn validate_owner_sid(value: &str) -> Result<(), MirageError> {
    bounded_text(value, 5, 256, "repository owner SID")?;
    if !value.starts_with("S-")
        || value.chars().any(char::is_whitespace)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'S' || byte == b'-')
    {
        return Err(MirageError::invalid_argument(
            "repository owner SID is malformed",
        ));
    }
    Ok(())
}

pub(crate) fn set_state(
    connection: &mut Connection,
    change: RepositoryStateChange,
) -> Result<RepositoryState, MirageError> {
    let actual: String = connection
        .query_row(
            "SELECT state FROM repositories WHERE repository_id = ?1",
            [change.repository_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to load repository for state transition"))?
        .ok_or_else(|| MirageError::invalid_argument("repository does not exist"))?;
    let actual = state_codec::repository(&actual)?;
    if actual != change.expected {
        return Err(conflict("repository state changed concurrently"));
    }
    let next = transition_repository(actual, change.event).map_err(transition)?;
    let changed = connection
        .execute(
            "UPDATE repositories SET state = ?1, updated_at_ns = ?2
             WHERE repository_id = ?3 AND state = ?4",
            params![
                next.as_str(),
                change.updated_at_ns,
                change.repository_id.as_bytes().as_slice(),
                actual.as_str(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist repository state transition"))?;
    if changed != 1 {
        return Err(conflict("repository state changed concurrently"));
    }
    Ok(next)
}
