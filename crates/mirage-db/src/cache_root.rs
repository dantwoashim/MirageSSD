//! Per-repository local cache placement: which directory (on which disk)
//! holds a managed volume's journal payloads. No row means the historical
//! default `<state root>\journal`.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

pub(crate) fn set_cache_root(
    connection: &mut Connection,
    repository_id: RepositoryId,
    cache_root: Option<&str>,
    now_ns: i64,
) -> Result<(), MirageError> {
    match cache_root {
        Some(root) => {
            if root.trim().len() < 3 {
                return Err(MirageError::invalid_argument(
                    "cache root must be an absolute directory path",
                ));
            }
            connection
                .execute(
                    "INSERT INTO repository_cache_roots(repository_id, cache_root, updated_ns)
                     VALUES(?1, ?2, ?3)
                     ON CONFLICT(repository_id) DO UPDATE SET
                       cache_root = excluded.cache_root,
                       updated_ns = excluded.updated_ns",
                    params![repository_id.as_bytes().as_slice(), root, now_ns],
                )
                .map_err(|e| sqlite(e, "cache root upsert failed"))?;
        }
        None => {
            connection
                .execute(
                    "DELETE FROM repository_cache_roots WHERE repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                )
                .map_err(|e| sqlite(e, "cache root delete failed"))?;
        }
    }
    Ok(())
}

impl Database {
    /// Sets (or clears with `None`) the repository's cache root directory.
    pub fn set_repository_cache_root(
        &self,
        repository_id: RepositoryId,
        cache_root: Option<&str>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.writer()
            .cache_root_set(repository_id, cache_root.map(str::to_owned), now_ns)
    }

    /// The configured cache root, if the repository has one.
    pub fn repository_cache_root(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<String>, MirageError> {
        self.reads().with_connection(|connection| {
            connection
                .query_row(
                    "SELECT cache_root FROM repository_cache_roots WHERE repository_id = ?1",
                    [repository_id.as_bytes().as_slice()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| sqlite(e, "cache root lookup failed"))
        })
    }
}
