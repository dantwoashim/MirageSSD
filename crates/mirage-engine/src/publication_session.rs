//! Publication sessions: idempotent, resumable remote publication driven by
//! the durable upload-session ledger. A session row exists before the first
//! byte is sent; each phase transition is recorded before the remote call so
//! a crash can always resume or roll back. Content upload is idempotent by
//! object key — a second publication of the same bytes reuses the session.

use mirage_db::{Database, PublicationSession as UploadSession, SessionKind, SessionPhase};
use mirage_types::{MirageError, RepositoryId};

/// Remote resumable-upload transport the session drives. Implementations
/// must treat `initiate`/`upload`/`commit`/`abort` as idempotent.
pub trait UploadTransport: Send + Sync {
    /// Starts a resumable session for `object_key`; returns the remote upload
    /// id and session URI. Retrying an existing upload must return the same
    /// identifiers.
    fn initiate(
        &self,
        object_key: &str,
        total_bytes: Option<u64>,
    ) -> Result<RemoteSession, MirageError>;

    /// Sends bytes `[offset, offset+len)`; returns bytes the remote confirms
    /// committed. Must be safe to repeat for the same range.
    fn upload(&self, session_uri: &str, offset: u64, bytes: &[u8]) -> Result<u64, MirageError>;

    /// Commits the object: the remote durably binds `object_key` to the
    /// uploaded bytes. Signed commits happen last — after every byte lands.
    fn commit(&self, session_uri: &str) -> Result<(), MirageError>;

    /// Rolls back the remote session; safe when the session never existed.
    fn abort(&self, session_uri: &str) -> Result<(), MirageError>;
}

/// Remote-side resumable identifiers returned by `initiate`.
pub struct RemoteSession {
    pub upload_id: String,
    pub session_uri: String,
}

/// Drives one volume's upload sessions over a transport.
pub struct PublicationSession<'a> {
    db: &'a Database,
    volume: RepositoryId,
    transport: &'a dyn UploadTransport,
}

impl<'a> PublicationSession<'a> {
    pub fn new(db: &'a Database, volume: RepositoryId, transport: &'a dyn UploadTransport) -> Self {
        Self {
            db,
            volume,
            transport,
        }
    }

    /// Publishes `bytes` to `object_key`, resuming an existing session when
    /// one is open. Idempotent by object key: a second call for the same key
    /// completes without re-uploading committed bytes.
    pub fn publish(
        &self,
        object_key: &str,
        content_hash: [u8; 32],
        bytes: &[u8],
        now_ns: i64,
    ) -> Result<UploadSession, MirageError> {
        let session = self.open_or_create(object_key, content_hash, bytes.len() as u64, now_ns)?;
        self.step(session.session_id, bytes, now_ns)
    }

    /// Resumes a session by id: picks up at its recorded phase and finishes
    /// or aborts it. Recovery calls this on every unfinished session.
    pub fn step(
        &self,
        session_id: [u8; 16],
        bytes: &[u8],
        now_ns: i64,
    ) -> Result<UploadSession, MirageError> {
        loop {
            let session = self.load(&session_id)?;
            match session.phase {
                SessionPhase::Committed | SessionPhase::Done => return Ok(session),
                SessionPhase::Aborted => {
                    return Err(MirageError::repository_conflict(
                        "upload session was aborted",
                    ));
                }
                SessionPhase::Created => {
                    let remote = self
                        .transport
                        .initiate(&session.object_key, session.total_bytes)?;
                    self.advance(
                        &session_id,
                        SessionPhase::Initiated,
                        Some(&remote.upload_id),
                        Some(&remote.session_uri),
                        0,
                        0,
                        None,
                        now_ns,
                    )?;
                }
                SessionPhase::Initiated | SessionPhase::Uploading => {
                    let uri = session.session_uri.clone().ok_or_else(|| {
                        MirageError::internal_invariant("initiated session has no upload URI")
                    })?;
                    let offset = session.committed_bytes;
                    let total = session.total_bytes.unwrap_or(bytes.len() as u64);
                    if offset >= total {
                        self.advance(
                            &session_id,
                            SessionPhase::Uploaded,
                            None,
                            None,
                            offset,
                            offset,
                            None,
                            now_ns,
                        )?;
                        continue;
                    }
                    let chunk_end = (offset + 8 * 1024 * 1024).min(total);
                    let chunk =
                        bytes
                            .get(offset as usize..chunk_end as usize)
                            .ok_or_else(|| {
                                MirageError::integrity_mismatch(
                                    "session byte range exceeds the payload",
                                )
                            })?;
                    let committed = self.transport.upload(&uri, offset, chunk)?;
                    self.advance(
                        &session_id,
                        SessionPhase::Uploading,
                        None,
                        None,
                        committed,
                        committed,
                        None,
                        now_ns,
                    )?;
                }
                SessionPhase::Uploaded => {
                    let uri = session.session_uri.clone().ok_or_else(|| {
                        MirageError::internal_invariant("uploaded session has no upload URI")
                    })?;
                    self.transport.commit(&uri)?;
                    self.advance(
                        &session_id,
                        SessionPhase::Committed,
                        None,
                        None,
                        session.committed_bytes,
                        session.committed_bytes,
                        None,
                        now_ns,
                    )?;
                }
            }
        }
    }

    /// Every unfinished session for the volume — the recovery scan that
    /// resumes or aborts each one.
    pub fn resume_unfinished(&self) -> Result<Vec<UploadSession>, MirageError> {
        self.db.unfinished_publication_sessions(self.volume)
    }

    /// Records a classified error on the session without changing its phase,
    /// so retry backoff decisions survive a crash.
    pub fn record_error(
        &self,
        session_id: [u8; 16],
        error_class: &str,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        let session = self.load(&session_id)?;
        self.advance(
            &session_id,
            session.phase,
            None,
            None,
            session.chunk_offset,
            session.committed_bytes,
            Some(error_class),
            now_ns,
        )
    }

    fn open_or_create(
        &self,
        object_key: &str,
        content_hash: [u8; 32],
        total_bytes: u64,
        now_ns: i64,
    ) -> Result<UploadSession, MirageError> {
        if let Some(existing) = self
            .db
            .publication_session_by_key(self.volume, object_key)?
        {
            // Idempotent publication: an already-committed session for the
            // same bytes is a success, not a new upload.
            if existing.phase.is_terminal() && existing.content_hash == Some(content_hash) {
                return Ok(existing);
            }
            if !existing.phase.is_terminal() {
                return Ok(existing);
            }
        }
        let mut session_id = [0u8; 16];
        getrandom::fill(&mut session_id)
            .map_err(|_| MirageError::internal_invariant("session id entropy failed"))?;
        let session = UploadSession {
            session_id,
            volume_id: self.volume,
            kind: SessionKind::Pack,
            object_key: object_key.to_string(),
            content_hash: Some(content_hash),
            phase: SessionPhase::Created,
            remote_upload_id: None,
            session_uri: None,
            chunk_offset: 0,
            committed_bytes: 0,
            total_bytes: Some(total_bytes),
            next_ops: Vec::new(),
            error_class: None,
            attempts: 0,
            created_ns: now_ns,
            updated_ns: now_ns,
        };
        self.db.writer().upload_session_create(session.clone())?;
        Ok(session)
    }

    fn load(&self, session_id: &[u8; 16]) -> Result<UploadSession, MirageError> {
        self.db
            .unfinished_publication_sessions(self.volume)?
            .into_iter()
            .find(|session| &session.session_id == session_id)
            .or_else(|| self.db.publication_session_by_id(session_id).ok().flatten())
            .ok_or_else(|| MirageError::repository_conflict("upload session is missing"))
    }

    #[allow(clippy::too_many_arguments)]
    fn advance(
        &self,
        session_id: &[u8; 16],
        phase: SessionPhase,
        remote_upload_id: Option<&str>,
        session_uri: Option<&str>,
        chunk_offset: u64,
        committed_bytes: u64,
        error_class: Option<&str>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.db.writer().upload_session_advance(
            *session_id,
            phase,
            remote_upload_id.map(str::to_string),
            session_uri.map(str::to_string),
            chunk_offset,
            committed_bytes,
            error_class.map(str::to_string),
            now_ns,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// In-memory resumable transport: commits bytes into a store, can be
    /// told to fail partway to simulate a crash.
    struct FakeTransport {
        committed: Mutex<Vec<u8>>,
        fail_on_call: AtomicUsize,
        calls: AtomicUsize,
        commits: AtomicUsize,
    }

    impl UploadTransport for FakeTransport {
        fn initiate(&self, _key: &str, _total: Option<u64>) -> Result<RemoteSession, MirageError> {
            Ok(RemoteSession {
                upload_id: "u1".into(),
                session_uri: "uri://u1".into(),
            })
        }
        fn upload(&self, _uri: &str, offset: u64, bytes: &[u8]) -> Result<u64, MirageError> {
            let mut store = self.committed.lock().unwrap();
            if self.calls.fetch_add(1, Ordering::SeqCst) == self.fail_on_call.load(Ordering::SeqCst)
            {
                // Crash mid-upload: a remote prefix may land, but the durable
                // cursor only ever records confirmed bytes.
                let partial = 3.min(bytes.len());
                store.resize(offset as usize + partial, 0);
                store[offset as usize..offset as usize + partial]
                    .copy_from_slice(&bytes[..partial]);
                return Err(MirageError::backend_unavailable("upload interrupted"));
            }
            store.resize(offset as usize + bytes.len(), 0);
            store[offset as usize..offset as usize + bytes.len()].copy_from_slice(bytes);
            Ok(offset + bytes.len() as u64)
        }
        fn commit(&self, _uri: &str) -> Result<(), MirageError> {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn abort(&self, _uri: &str) -> Result<(), MirageError> {
            Ok(())
        }
    }

    #[test]
    fn publish_resumes_after_mid_upload_crash() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("control.db")).unwrap();
        let volume = RepositoryId::from_bytes([0x42; 16]);
        let transport = FakeTransport {
            committed: Mutex::new(Vec::new()),
            fail_on_call: AtomicUsize::new(1), // second chunk fails
            calls: AtomicUsize::new(0),
            commits: AtomicUsize::new(0),
        };
        let session = PublicationSession::new(&db, volume, &transport);
        let payload = vec![0xabu8; 12 * 1024 * 1024 + 7]; // > one 8 MiB chunk
        let hash = *blake3::hash(&payload).as_bytes();
        // First run crashes inside the second chunk — publish drives the
        // session and surfaces the transport failure.
        session.publish("objects/ab", hash, &payload, 1).ok();
        let open = session.resume_unfinished().unwrap();
        assert!(!open.is_empty());
        assert!(open[0].committed_bytes > 0);
        // Recovery resumes and finishes without re-sending committed bytes.
        session.step(open[0].session_id, &payload, 3).unwrap();
        assert_eq!(transport.committed.lock().unwrap().as_slice(), payload);
        assert_eq!(transport.commits.load(Ordering::SeqCst), 1);
        // Idempotent republish of the same bytes is a no-op.
        let again = session.publish("objects/ab", hash, &payload, 4).unwrap();
        assert_eq!(again.phase, SessionPhase::Committed);
        assert_eq!(transport.commits.load(Ordering::SeqCst), 1);
    }
}
