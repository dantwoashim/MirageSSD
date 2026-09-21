use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mirage_types::{MirageError, MirageErrorKind};
use rusqlite::{Connection, OpenFlags};

use crate::error::sqlite;

pub const APPLICATION_ID: i32 = 0x4D49_5247;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn writer_connection(path: &Path) -> Result<Connection, MirageError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite(error, "failed to open database"))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite(error, "failed to configure database busy timeout"))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA trusted_schema = OFF;",
        )
        .map_err(|error| sqlite(error, "failed to configure durable database pragmas"))?;
    validate_or_initialize_application_id(&connection, true)?;
    Ok(connection)
}

pub(crate) fn read_connection(path: &Path) -> Result<Connection, MirageError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite(error, "failed to open read-only database"))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite(error, "failed to configure read-only busy timeout"))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA query_only = ON;
             PRAGMA trusted_schema = OFF;",
        )
        .map_err(|error| sqlite(error, "failed to configure read-only database pragmas"))?;
    validate_or_initialize_application_id(&connection, false)?;
    Ok(connection)
}

fn validate_or_initialize_application_id(
    connection: &Connection,
    initialize: bool,
) -> Result<(), MirageError> {
    let observed: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|error| sqlite(error, "failed to read database application identity"))?;
    if observed == 0 && initialize {
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .map_err(|error| sqlite(error, "failed to set database application identity"))?;
        return Ok(());
    }
    if observed != APPLICATION_ID {
        return Err(MirageError::new(
            MirageErrorKind::UnsupportedLayout,
            MirageErrorKind::UnsupportedLayout.default_code(),
            "database application identity is not MirageSSD",
        ));
    }
    Ok(())
}

/// Maximum simultaneously-held read connections. Beyond this callers wait on
/// the pool lock instead of opening unbounded connections.
const READ_POOL_CAPACITY: usize = 8;

/// A bounded pool of reused read-only connections. Returning a connection to
/// the pool keeps its page cache and statement cache warm.
#[derive(Debug, Clone)]
pub struct ReadPool {
    path: Arc<PathBuf>,
    idle: Arc<std::sync::Mutex<Vec<Connection>>>,
}

struct PooledConnection<'pool> {
    pool: &'pool ReadPool,
    connection: Option<Connection>,
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take()
            && let Ok(mut idle) = self.pool.idle.lock()
            && idle.len() < READ_POOL_CAPACITY
        {
            idle.push(connection);
        }
    }
}

impl ReadPool {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            idle: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, MirageError>,
    ) -> Result<T, MirageError> {
        let connection = {
            let mut idle = self.idle.lock().map_err(|_| {
                MirageError::internal_invariant("read connection pool lock poisoned")
            })?;
            match idle.pop() {
                Some(connection) => connection,
                None => read_connection(&self.path)?,
            }
        };
        let pooled = PooledConnection {
            pool: self,
            connection: Some(connection),
        };
        operation(pooled.connection.as_ref().expect("connection present"))
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.path
    }
}
