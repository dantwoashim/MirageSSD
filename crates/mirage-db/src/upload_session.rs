//! Durable upload-session ledger. A session row is created before the first
//! byte is sent and tracks the resumable-upload cursor — upload id, session
//! URI, chunk offset, committed bytes — plus ordered next operations and the
//! last classified error. Recovery scans unfinished sessions per volume and
//! resumes or aborts them idempotently.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Pack,
    Manifest,
    Commit,
    Tombstone,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::Manifest => "manifest",
            Self::Commit => "commit",
            Self::Tombstone => "tombstone",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "pack" => Ok(Self::Pack),
            "manifest" => Ok(Self::Manifest),
            "commit" => Ok(Self::Commit),
            "tombstone" => Ok(Self::Tombstone),
            _ => Err(MirageError::integrity_mismatch("session kind is unknown")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// Row created; no remote call made yet.
    Created,
    /// Resumable session initiated; upload id and URI recorded.
    Initiated,
    /// Chunks flowing; `chunk_offset`/`committed_bytes` advance.
    Uploading,
    /// All bytes committed remotely; finalize/commit not yet sent.
    Uploaded,
    /// Remote commit acknowledged.
    Committed,
    /// Rolled back; no remote bytes remain.
    Aborted,
    /// Fully settled; eligible for retention cleanup.
    Done,
}

impl SessionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Initiated => "initiated",
            Self::Uploading => "uploading",
            Self::Uploaded => "uploaded",
            Self::Committed => "committed",
            Self::Aborted => "aborted",
            Self::Done => "done",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "created" => Ok(Self::Created),
            "initiated" => Ok(Self::Initiated),
            "uploading" => Ok(Self::Uploading),
            "uploaded" => Ok(Self::Uploaded),
            "committed" => Ok(Self::Committed),
            "aborted" => Ok(Self::Aborted),
            "done" => Ok(Self::Done),
            _ => Err(MirageError::integrity_mismatch("session phase is unknown")),
        }
    }
    /// Terminal phases never resume.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Committed | Self::Aborted | Self::Done)
    }
}

#[derive(Debug, Clone)]
pub struct UploadSession {
    pub session_id: [u8; 16],
    pub volume_id: RepositoryId,
    pub kind: SessionKind,
    pub object_key: String,
    pub content_hash: Option<[u8; 32]>,
    pub phase: SessionPhase,
    pub remote_upload_id: Option<String>,
    pub session_uri: Option<String>,
    pub chunk_offset: u64,
    pub committed_bytes: u64,
    pub total_bytes: Option<u64>,
    pub next_ops: Vec<u8>,
    pub error_class: Option<String>,
    pub attempts: u32,
    pub created_ns: i64,
    pub updated_ns: i64,
}

/// Creates a session row — must complete before the first byte is sent so a
/// crash can never orphan remote state with no local record.
pub fn create_session(
    connection: &mut Connection,
    session: &UploadSession,
) -> Result<(), MirageError> {
    if session.phase != SessionPhase::Created
        || session.chunk_offset != 0
        || session.committed_bytes != 0
        || session.remote_upload_id.is_some()
        || session.session_uri.is_some()
    {
        return Err(MirageError::invalid_argument(
            "a new upload session must start in the created phase",
        ));
    }
    connection
        .execute(
            "INSERT INTO publication_sessions
             (session_id, volume_id, kind, object_key, content_hash, phase,
              remote_upload_id, session_uri, chunk_offset, committed_bytes,
              total_bytes, next_ops, error_class, attempts, created_ns, updated_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, 'created', NULL, NULL, 0, 0,
                     ?6, ?7, NULL, 0, ?8, ?8)",
            params![
                session.session_id.as_slice(),
                session.volume_id.as_bytes().as_slice(),
                session.kind.as_str(),
                session.object_key,
                session.content_hash.map(|hash| hash.to_vec()),
                session.total_bytes.map(|bytes| bytes as i64),
                session.next_ops,
                session.created_ns,
            ],
        )
        .map_err(|e| sqlite(e, "upload session insert failed"))?;
    Ok(())
}

/// Idempotent create: returns the existing session when the object key is
/// already tracked for this volume.
pub fn find_session_by_key(
    connection: &Connection,
    volume_id: RepositoryId,
    object_key: &str,
) -> Result<Option<UploadSession>, MirageError> {
    connection
        .query_row(
            "SELECT session_id FROM publication_sessions
             WHERE volume_id = ?1 AND object_key = ?2
             ORDER BY created_ns DESC LIMIT 1",
            params![volume_id.as_bytes().as_slice(), object_key],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "upload session lookup failed"))?
        .map(|bytes| {
            let id: [u8; 16] = bytes
                .try_into()
                .map_err(|_| MirageError::integrity_mismatch("session id is malformed"))?;
            load_session(connection, &id)
        })
        .transpose()
        .map(|inner| inner.flatten())
}

/// Loads one session by id, including terminal sessions.
pub fn find_session_by_id(
    connection: &Connection,
    session_id: &[u8; 16],
) -> Result<Option<UploadSession>, MirageError> {
    load_session(connection, session_id)
}

fn load_session(
    connection: &Connection,
    session_id: &[u8; 16],
) -> Result<Option<UploadSession>, MirageError> {
    connection
        .query_row(
            "SELECT session_id, volume_id, kind, object_key, content_hash,
                    phase, remote_upload_id, session_uri, chunk_offset,
                    committed_bytes, total_bytes, next_ops, error_class,
                    attempts, created_ns, updated_ns
             FROM publication_sessions WHERE session_id = ?1",
            [session_id.as_slice()],
            |row| {
                let hash: Option<Vec<u8>> = row.get(4)?;
                Ok(UploadSession {
                    session_id: row
                        .get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                    volume_id: RepositoryId::from_bytes(
                        row.get::<_, Vec<u8>>(1)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 16))?,
                    ),
                    kind: SessionKind::parse(&row.get::<_, String>(2)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 4))?,
                    object_key: row.get(3)?,
                    content_hash: hash
                        .map(|bytes| bytes.try_into())
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 32))?,
                    phase: SessionPhase::parse(&row.get::<_, String>(5)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, 8))?,
                    remote_upload_id: row.get(6)?,
                    session_uri: row.get(7)?,
                    chunk_offset: u64::try_from(row.get::<_, i64>(8)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, 0))?,
                    committed_bytes: u64::try_from(row.get::<_, i64>(9)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, 0))?,
                    total_bytes: row
                        .get::<_, Option<i64>>(10)?
                        .map(|bytes| u64::try_from(bytes))
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, 0))?,
                    next_ops: row.get::<_, Option<Vec<u8>>>(11)?.unwrap_or_default(),
                    error_class: row.get(12)?,
                    attempts: u32::try_from(row.get::<_, i64>(13)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(13, 0))?,
                    created_ns: row.get(14)?,
                    updated_ns: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "upload session load failed"))
}

/// Advances the session phase; transitions are forward-only except
/// `uploading`→`initiated`/`created` restarts allowed by recovery.
pub fn advance_phase(
    connection: &mut Connection,
    session_id: &[u8; 16],
    phase: SessionPhase,
    remote_upload_id: Option<&str>,
    session_uri: Option<&str>,
    chunk_offset: u64,
    committed_bytes: u64,
    error_class: Option<&str>,
    now_ns: i64,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE publication_sessions SET
                phase = ?1, remote_upload_id = COALESCE(?2, remote_upload_id),
                session_uri = COALESCE(?3, session_uri), chunk_offset = ?4,
                committed_bytes = ?5, error_class = ?6, updated_ns = ?7,
                attempts = attempts + 1
             WHERE session_id = ?8",
            params![
                phase.as_str(),
                remote_upload_id,
                session_uri,
                chunk_offset as i64,
                committed_bytes as i64,
                error_class,
                now_ns,
                session_id.as_slice(),
            ],
        )
        .map_err(|e| sqlite(e, "upload session advance failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "upload session is missing",
        ));
    }
    Ok(())
}

/// Sessions that recovery must resume or roll back: every non-terminal row
/// for the volume in creation order.
pub fn unfinished_sessions(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Vec<UploadSession>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT session_id FROM publication_sessions
             WHERE volume_id = ?1
               AND phase NOT IN ('committed', 'aborted', 'done')
             ORDER BY created_ns",
        )
        .map_err(|e| sqlite(e, "unfinished session scan prepare failed"))?;
    let ids: Vec<[u8; 16]> = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            row.get::<_, Vec<u8>>(0)?
                .try_into()
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))
        })
        .map_err(|e| sqlite(e, "unfinished session scan failed"))?
        .collect::<Result<_, _>>()
        .map_err(|e| sqlite(e, "unfinished session id decode failed"))?;
    ids.iter()
        .map(|id| {
            load_session(connection, id)?
                .ok_or_else(|| MirageError::internal_invariant("session vanished mid-scan"))
        })
        .collect()
}

impl Database {
    /// Unfinished sessions for recovery resume/rollback.
    pub fn unfinished_publication_sessions(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Vec<UploadSession>, MirageError> {
        self.reads()
            .with_connection(|connection| unfinished_sessions(connection, volume_id))
    }

    /// Finds the newest session for an object key — idempotent publication
    /// reuses it instead of starting a second upload.
    pub fn publication_session_by_key(
        &self,
        volume_id: RepositoryId,
        object_key: &str,
    ) -> Result<Option<UploadSession>, MirageError> {
        self.reads()
            .with_connection(|connection| find_session_by_key(connection, volume_id, object_key))
    }

    /// Loads one session by id, including terminal sessions.
    pub fn publication_session_by_id(
        &self,
        session_id: &[u8; 16],
    ) -> Result<Option<UploadSession>, MirageError> {
        self.reads()
            .with_connection(|connection| find_session_by_id(connection, session_id))
    }
}
