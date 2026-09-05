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

#[derive(Debug, Clone)]
pub struct ReadPool {
    path: Arc<PathBuf>,
}

impl ReadPool {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
        }
    }

    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, MirageError>,
    ) -> Result<T, MirageError> {
        let connection = read_connection(&self.path)?;
        operation(&connection)
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.path
    }
}
