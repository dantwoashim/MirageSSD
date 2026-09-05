use mirage_backend::{BackendId, ImmutableRevision, ObjectKind, ProviderObjectId};
use mirage_types::{ByteCount, ContentHash, MirageError, UpdateId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::{conflict, sqlite};
use crate::value::{bounded_text, fixed, nonnegative, sqlite_integer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAccount {
    pub backend_id: BackendId,
    pub provider: String,
    pub account_subject_hash: ContentHash,
    pub state: String,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteObjectRecord {
    pub backend_id: BackendId,
    pub object_key: ContentHash,
    pub provider_object_id: ProviderObjectId,
    pub immutable_revision: Option<ImmutableRevision>,
    pub object_kind: ObjectKind,
    pub byte_length: ByteCount,
    pub content_hash: ContentHash,
    pub state: String,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertRemoteObjectOutcome {
    Inserted,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSession {
    pub backend_id: BackendId,
    pub upload_id: UpdateId,
    pub object_key: ContentHash,
    pub provider_session_id: String,
    pub committed_offset: ByteCount,
    pub total_length: ByteCount,
    pub state: String,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct UploadAdvance {
    pub backend_id: BackendId,
    pub upload_id: UpdateId,
    pub expected_offset: ByteCount,
    pub new_offset: ByteCount,
    pub state: String,
    pub updated_at_ns: i64,
}

impl Database {
    pub fn register_backend_account(&self, account: BackendAccount) -> Result<(), MirageError> {
        self.writer.register_backend_account(account)
    }

    pub fn upsert_remote_object(
        &self,
        object: RemoteObjectRecord,
    ) -> Result<UpsertRemoteObjectOutcome, MirageError> {
        self.writer.upsert_remote_object(object)
    }

    pub fn load_remote_object(
        &self,
        backend_id: &BackendId,
        object_key: ContentHash,
    ) -> Result<Option<RemoteObjectRecord>, MirageError> {
        self.reads.with_connection(|connection| {
            let row = connection
                .query_row(
                    "SELECT provider_file_id, provider_revision, object_kind, byte_length,
                            content_hash, state, created_at_ns
                     FROM remote_objects WHERE backend_id = ?1 AND object_key = ?2",
                    params![backend_id.as_str(), object_key.as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Vec<u8>>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load immutable remote object"))?;
            row.map(
                |(provider, revision, kind, length, hash, state, created_at_ns)| {
                    Ok(RemoteObjectRecord {
                        backend_id: backend_id.clone(),
                        object_key,
                        provider_object_id: ProviderObjectId::new(provider)?,
                        immutable_revision: revision.map(ImmutableRevision::new).transpose()?,
                        object_kind: decode_kind(kind)?,
                        byte_length: ByteCount::from_u64(nonnegative(length, "object length")?),
                        content_hash: ContentHash::from_bytes(fixed(hash, "content hash")?),
                        state,
                        created_at_ns,
                    })
                },
            )
            .transpose()
        })
    }

    pub fn begin_upload_session(&self, session: UploadSession) -> Result<(), MirageError> {
        self.writer.begin_upload_session(session)
    }

    pub fn advance_upload_session(
        &self,
        backend_id: BackendId,
        upload_id: UpdateId,
        expected_offset: ByteCount,
        new_offset: ByteCount,
        state: String,
        updated_at_ns: i64,
    ) -> Result<UploadSession, MirageError> {
        self.writer.advance_upload_session(UploadAdvance {
            backend_id,
            upload_id,
            expected_offset,
            new_offset,
            state,
            updated_at_ns,
        })
    }

    pub fn load_upload_session(
        &self,
        backend_id: &BackendId,
        upload_id: UpdateId,
    ) -> Result<Option<UploadSession>, MirageError> {
        self.reads
            .with_connection(|connection| load_upload(connection, backend_id, upload_id))
    }
}

pub(crate) fn register_account(
    connection: &mut Connection,
    account: BackendAccount,
) -> Result<(), MirageError> {
    bounded_text(&account.provider, 1, 64, "backend provider")?;
    bounded_text(&account.state, 1, 64, "backend account state")?;
    let existing = connection
        .query_row(
            "SELECT provider, account_subject_hash FROM backend_accounts WHERE backend_id = ?1",
            [account.backend_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to query backend account"))?;
    if let Some((provider, subject_hash)) = existing {
        if provider != account.provider || subject_hash != account.account_subject_hash.as_bytes() {
            return Err(conflict("backend account immutable identity changed"));
        }
        connection
            .execute(
                "UPDATE backend_accounts SET state = ?1, updated_at_ns = ?2 WHERE backend_id = ?3",
                params![
                    account.state,
                    account.updated_at_ns,
                    account.backend_id.as_str()
                ],
            )
            .map_err(|error| sqlite(error, "failed to update backend account state"))?;
        return Ok(());
    }
    connection
        .execute(
            "INSERT INTO backend_accounts(
                backend_id, provider, account_subject_hash, state, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                account.backend_id.as_str(),
                account.provider,
                account.account_subject_hash.as_bytes().as_slice(),
                account.state,
                account.updated_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to register backend account"))?;
    Ok(())
}

pub(crate) fn upsert_object(
    connection: &mut Connection,
    object: RemoteObjectRecord,
) -> Result<UpsertRemoteObjectOutcome, MirageError> {
    bounded_text(&object.state, 1, 64, "remote object state")?;
    let byte_length = sqlite_integer(object.byte_length.as_u64(), "remote object length")?;
    if byte_length == 0 {
        return Err(MirageError::invalid_argument(
            "remote object length must be positive",
        ));
    }
    let existing = connection
        .query_row(
            "SELECT provider_file_id, provider_revision, object_kind, byte_length, content_hash
             FROM remote_objects WHERE backend_id = ?1 AND object_key = ?2",
            params![
                object.backend_id.as_str(),
                object.object_key.as_bytes().as_slice()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to query immutable remote object"))?;
    if let Some((provider, revision, kind, length, hash)) = existing {
        if provider == object.provider_object_id.as_str()
            && revision.as_deref()
                == object
                    .immutable_revision
                    .as_ref()
                    .map(ImmutableRevision::as_str)
            && kind == encode_kind(object.object_kind)
            && length == byte_length
            && hash == object.content_hash.as_bytes()
        {
            return Ok(UpsertRemoteObjectOutcome::AlreadyPresent);
        }
        return Err(conflict(
            "immutable remote object identity conflicts with existing metadata",
        ));
    }
    connection
        .execute(
            "INSERT INTO remote_objects(
                backend_id, object_key, provider_file_id, provider_revision,
                object_kind, byte_length, content_hash, state, created_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                object.backend_id.as_str(),
                object.object_key.as_bytes().as_slice(),
                object.provider_object_id.as_str(),
                object
                    .immutable_revision
                    .as_ref()
                    .map(ImmutableRevision::as_str),
                encode_kind(object.object_kind),
                byte_length,
                object.content_hash.as_bytes().as_slice(),
                object.state,
                object.created_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist immutable remote object"))?;
    Ok(UpsertRemoteObjectOutcome::Inserted)
}

pub(crate) fn begin_upload(
    connection: &mut Connection,
    session: UploadSession,
) -> Result<(), MirageError> {
    validate_upload(&session)?;
    let existing = load_upload(connection, &session.backend_id, session.upload_id)?;
    if let Some(existing) = existing {
        if existing == session {
            return Ok(());
        }
        return Err(conflict(
            "upload session identity conflicts with existing state",
        ));
    }
    connection
        .execute(
            "INSERT INTO upload_sessions(
                backend_id, upload_id, object_key, provider_session_id,
                committed_offset, total_length, state, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session.backend_id.as_str(),
                session.upload_id.as_bytes().as_slice(),
                session.object_key.as_bytes().as_slice(),
                session.provider_session_id,
                sqlite_integer(session.committed_offset.as_u64(), "upload offset")?,
                sqlite_integer(session.total_length.as_u64(), "upload length")?,
                session.state,
                session.updated_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist upload session"))?;
    Ok(())
}

pub(crate) fn advance_upload(
    connection: &mut Connection,
    advance: UploadAdvance,
) -> Result<UploadSession, MirageError> {
    bounded_text(&advance.state, 1, 64, "upload state")?;
    if advance.new_offset.as_u64() < advance.expected_offset.as_u64() {
        return Err(MirageError::invalid_argument(
            "upload committed offset cannot move backward",
        ));
    }
    let current = load_upload(connection, &advance.backend_id, advance.upload_id)?
        .ok_or_else(|| MirageError::invalid_argument("upload session does not exist"))?;
    if current.committed_offset != advance.expected_offset {
        return Err(conflict("upload offset changed concurrently"));
    }
    if advance.new_offset.as_u64() > current.total_length.as_u64() {
        return Err(MirageError::invalid_argument(
            "upload committed offset exceeds total length",
        ));
    }
    connection
        .execute(
            "UPDATE upload_sessions
             SET committed_offset = ?1, state = ?2, updated_at_ns = ?3
             WHERE backend_id = ?4 AND upload_id = ?5 AND committed_offset = ?6",
            params![
                sqlite_integer(advance.new_offset.as_u64(), "upload offset")?,
                advance.state,
                advance.updated_at_ns,
                advance.backend_id.as_str(),
                advance.upload_id.as_bytes().as_slice(),
                sqlite_integer(advance.expected_offset.as_u64(), "upload offset")?,
            ],
        )
        .map_err(|error| sqlite(error, "failed to advance upload session"))?;
    load_upload(connection, &advance.backend_id, advance.upload_id)?
        .ok_or_else(|| MirageError::internal_invariant("updated upload session disappeared"))
}

fn load_upload(
    connection: &Connection,
    backend_id: &BackendId,
    upload_id: UpdateId,
) -> Result<Option<UploadSession>, MirageError> {
    let row = connection
        .query_row(
            "SELECT object_key, provider_session_id, committed_offset,
                    total_length, state, updated_at_ns
             FROM upload_sessions WHERE backend_id = ?1 AND upload_id = ?2",
            params![backend_id.as_str(), upload_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to load upload session"))?;
    row.map(|(key, provider, offset, total, state, updated)| {
        Ok(UploadSession {
            backend_id: backend_id.clone(),
            upload_id,
            object_key: ContentHash::from_bytes(fixed(key, "upload object key")?),
            provider_session_id: provider,
            committed_offset: ByteCount::from_u64(nonnegative(offset, "upload offset")?),
            total_length: ByteCount::from_u64(nonnegative(total, "upload length")?),
            state,
            updated_at_ns: updated,
        })
    })
    .transpose()
}

fn validate_upload(session: &UploadSession) -> Result<(), MirageError> {
    bounded_text(
        &session.provider_session_id,
        1,
        1024,
        "provider upload session ID",
    )?;
    bounded_text(&session.state, 1, 64, "upload state")?;
    if session.total_length.is_zero()
        || session.committed_offset.as_u64() > session.total_length.as_u64()
    {
        return Err(MirageError::invalid_argument(
            "upload offsets contradict total length",
        ));
    }
    Ok(())
}

fn encode_kind(kind: ObjectKind) -> i64 {
    match kind {
        ObjectKind::RepositoryConfig => 0,
        ObjectKind::Pack => 1,
        ObjectKind::Manifest => 2,
        ObjectKind::Commit => 3,
        ObjectKind::Profile => 4,
    }
}

fn decode_kind(kind: i64) -> Result<ObjectKind, MirageError> {
    match kind {
        0 => Ok(ObjectKind::RepositoryConfig),
        1 => Ok(ObjectKind::Pack),
        2 => Ok(ObjectKind::Manifest),
        3 => Ok(ObjectKind::Commit),
        4 => Ok(ObjectKind::Profile),
        _ => Err(MirageError::integrity_mismatch(
            "database object kind is unknown",
        )),
    }
}
