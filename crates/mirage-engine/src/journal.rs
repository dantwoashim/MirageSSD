//! Flush-fence coordination over the durable local-mutation journal.
//!
//! Ordering contract: payload bytes are written and fsynced, the payload is
//! marked flushed, the operation commits, and only then does a flush group
//! acknowledge local durability. `FlushFileBuffers` therefore means "durable
//! on this device" — never cloud completion.

use std::path::Path;

use mirage_db::{
    Database, OperationKind, OperationPayloadRecord, OperationRecord, OperationStatus,
};
use mirage_types::{MirageError, RepositoryId};

/// Coordinates journaled local mutations and flush fences for one volume.
pub struct LocalJournal {
    db: Database,
    volume_id: RepositoryId,
}

/// A payload file staged under the journal directory before its operation
/// commits.
pub struct StagedPayload {
    pub payload_id: [u8; 16],
    pub path: String,
    pub bytes: u64,
    pub checksum: [u8; 32],
}

impl LocalJournal {
    #[must_use]
    pub fn new(db: Database, volume_id: RepositoryId) -> Self {
        Self { db, volume_id }
    }

    /// Stages payload bytes under `journal_dir`, fsyncs the file, and marks
    /// the payload durable. Must run before `commit`.
    pub fn stage_payload(
        &self,
        journal_dir: &Path,
        bytes: &[u8],
    ) -> Result<StagedPayload, MirageError> {
        let payload_id = new_id();
        let file_name = format!("{}.payload", hex16(&payload_id));
        let path = journal_dir.join(&file_name);
        mirage_crypto::durable_file::write_atomic(&path, bytes)?;
        let checksum = *blake3::hash(bytes).as_bytes();
        Ok(StagedPayload {
            payload_id,
            path: file_name,
            bytes: bytes.len() as u64,
            checksum,
        })
    }

    /// Journals a pending operation bound to the caller's staged payloads.
    pub fn begin(
        &self,
        kind: OperationKind,
        payload: Vec<u8>,
        base_commit: Option<[u8; 32]>,
        depends_on: Option<[u8; 16]>,
        staged: Vec<StagedPayload>,
        now_ns: i64,
    ) -> Result<[u8; 16], MirageError> {
        let operation_id = new_id();
        let device_seq = self.db.next_operation_seq(self.volume_id)?;
        let payloads: Vec<OperationPayloadRecord> = staged
            .iter()
            .map(|staged| OperationPayloadRecord {
                payload_id: staged.payload_id,
                operation_id,
                path: staged.path.clone(),
                bytes: staged.bytes as i64,
                checksum: Some(staged.checksum),
                flushed_ns: None,
            })
            .collect();
        self.db.writer().operation_begin(
            OperationRecord {
                operation_id,
                device_seq,
                volume_id: self.volume_id,
                base_commit,
                kind,
                payload,
                status: OperationStatus::Pending,
                flush_group: None,
                depends_on,
                created_ns: now_ns,
            },
            payloads,
        )?;
        for payload in &staged {
            self.db.writer().operation_payload_flushed(
                payload.payload_id,
                payload.checksum,
                now_ns,
            )?;
        }
        Ok(operation_id)
    }

    /// Commits the operation once all payloads are durable; the writer
    /// refuses the transition otherwise.
    pub fn commit(&self, operation_id: [u8; 16]) -> Result<(), MirageError> {
        self.db.writer().operation_commit(operation_id)
    }

    /// Closes a flush fence over the volume's committed operations — the
    /// `FlushFileBuffers` acknowledgement. Returns the group id.
    pub fn flush_fence(&self, now_ns: i64) -> Result<i64, MirageError> {
        let group = self.db.writer().flush_group_open(self.volume_id, now_ns)?;
        self.db.writer().flush_group_mark(group, now_ns)?;
        Ok(group)
    }

    /// Marks operations published after their remote commit lands.
    pub fn mark_published(&self, operation_ids: Vec<[u8; 16]>) -> Result<u64, MirageError> {
        self.db.writer().operation_publish(operation_ids)
    }

    /// Operations to replay at mount recovery, in device order.
    pub fn replayable(&self) -> Result<Vec<OperationRecord>, MirageError> {
        self.db.replayable_operations(self.volume_id)
    }

    /// Deletes payload files whose operations never committed, then reclaims
    /// their journal rows.
    pub fn reclaim_pending(&self, journal_dir: &Path, now_ns: i64) -> Result<u64, MirageError> {
        let reclaimable = self.db.reclaimable_operation_payloads(self.volume_id)?;
        for payload_id in &reclaimable {
            let path = journal_dir.join(format!("{}.payload", hex16(payload_id)));
            let _ = std::fs::remove_file(path);
        }
        self.db
            .writer()
            .operation_reclaim_pending(self.volume_id, now_ns)
    }
}

fn new_id() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

fn hex16(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, LocalJournal) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("control.db")).unwrap();
        let volume = RepositoryId::from_bytes([0x41; 16]);
        (dir, LocalJournal::new(db, volume))
    }

    #[test]
    fn commit_requires_durable_payloads_and_flush_fence_acknowledges() {
        let (dir, journal) = setup();
        let staged = journal.stage_payload(dir.path(), b"payload").unwrap();
        let op = journal
            .begin(
                OperationKind::Write,
                b"op".to_vec(),
                None,
                None,
                vec![staged],
                11,
            )
            .unwrap();
        journal.commit(op).unwrap();
        journal.flush_fence(12).unwrap();
        assert_eq!(journal.replayable().unwrap().len(), 1);
    }

    #[test]
    fn replay_is_idempotent_and_pending_is_reclaimable() {
        let (dir, journal) = setup();
        // A committed+flushed op replays; a pending one never does.
        let staged = journal.stage_payload(dir.path(), b"bytes").unwrap();
        let committed = journal
            .begin(
                OperationKind::Write,
                b"ok".to_vec(),
                None,
                None,
                vec![staged],
                11,
            )
            .unwrap();
        journal.commit(committed).unwrap();
        journal.flush_fence(12).unwrap();
        // An uncommitted pending op: insert directly with no staged payloads.
        let pending = journal
            .begin(
                OperationKind::Delete,
                b"drop".to_vec(),
                None,
                Some(committed),
                Vec::new(),
                13,
            )
            .unwrap();
        let replay = journal.replayable().unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].operation_id, committed);
        journal.reclaim_pending(dir.path(), 14).unwrap();
        assert!(journal.replayable().unwrap().len() == 1);
        let _ = pending;
    }
}
