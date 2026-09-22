use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
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
    shared: Arc<PoolShared>,
}

#[derive(Debug, Default)]
struct PoolState {
    idle: Vec<Connection>,
    /// Includes connections being opened and checked out, not just idle ones.
    total: usize,
}

#[derive(Debug, Default)]
struct PoolShared {
    state: Mutex<PoolState>,
    available: Condvar,
}

struct PooledConnection<'pool> {
    pool: &'pool ReadPool,
    connection: Option<Connection>,
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let mut state = self
                .pool
                .shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.idle.push(connection);
            self.pool.shared.available.notify_one();
        }
    }
}

impl ReadPool {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            shared: Arc::new(PoolShared::default()),
        }
    }

    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, MirageError>,
    ) -> Result<T, MirageError> {
        let connection = loop {
            let mut state = self.shared.state.lock().map_err(|_| {
                MirageError::internal_invariant("read connection pool lock poisoned")
            })?;
            if let Some(connection) = state.idle.pop() {
                break connection;
            }
            if state.total < READ_POOL_CAPACITY {
                state.total += 1;
                drop(state);
                match read_connection(&self.path) {
                    Ok(connection) => break connection,
                    Err(error) => {
                        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.total -= 1;
                        self.shared.available.notify_one();
                        return Err(error);
                    }
                }
            }
            drop(self.shared.available.wait(state).map_err(|_| {
                MirageError::internal_invariant("read connection pool wait poisoned")
            })?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    #[test]
    fn checked_out_connections_are_bounded_and_waiters_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.db");
        let _writer = writer_connection(&path).unwrap();
        let pool = ReadPool::new(path);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (entered, receiver) = mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..READ_POOL_CAPACITY * 2 {
            let (pool, release, active, peak, entered) = (
                pool.clone(),
                Arc::clone(&release),
                Arc::clone(&active),
                Arc::clone(&peak),
                entered.clone(),
            );
            workers.push(std::thread::spawn(move || {
                pool.with_connection(|connection| {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    entered.send(()).unwrap();
                    let mut guard = release.0.lock().unwrap();
                    while !*guard {
                        guard = release.1.wait(guard).unwrap();
                    }
                    assert_eq!(
                        connection
                            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                            .unwrap(),
                        1
                    );
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }));
        }
        for _ in 0..READ_POOL_CAPACITY {
            receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        let overflow = receiver.recv_timeout(Duration::from_millis(100)).is_ok();
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert!(!overflow, "a ninth read connection was admitted");
        assert_eq!(peak.load(Ordering::SeqCst), READ_POOL_CAPACITY);
        assert_eq!(pool.shared.state.lock().unwrap().total, READ_POOL_CAPACITY);
    }

    #[test]
    fn failed_connection_open_releases_its_permit() {
        let dir = tempfile::tempdir().unwrap();
        let pool = ReadPool::new(dir.path().join("missing.db"));
        for _ in 0..READ_POOL_CAPACITY * 2 {
            assert!(pool.with_connection(|_| Ok(())).is_err());
        }
        assert_eq!(pool.shared.state.lock().unwrap().total, 0);
    }
}
