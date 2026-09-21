//! Remote reconciliation: classifies local vs remote heads, records
//! divergences (both histories preserved — never overwritten), and replays
//! remote namespace changes into the durable observation ledger.

use mirage_db::{Database, Divergence, DivergenceStatus, RemoteChange, RemoteHead};
use mirage_types::{MirageError, RepositoryId};

/// How a local head relates to the observed remote head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Heads are identical — nothing to do.
    Same,
    /// Remote head is a strict descendant of local — local publishes freely.
    LocalAhead,
    /// Local head is a strict ancestor of remote — replay remote changes.
    RemoteAhead,
    /// Neither is the other's ancestor — divergence; both stay visible.
    Diverged,
}

/// Drives observation recording and divergence detection for one volume.
pub struct Reconciler<'a> {
    db: &'a Database,
    volume: RepositoryId,
}

impl<'a> Reconciler<'a> {
    pub fn new(db: &'a Database, volume: RepositoryId) -> Self {
        Self { db, volume }
    }

    /// Observes a remote head and a batch of changes, then classifies the
    /// relationship against `local_head`. `remote_is_descendant_of_local`
    /// and `local_is_ancestor_of_remote` are the *same* ancestry fact seen
    /// from either direction — either being true means the remote head is
    /// ahead. Only when neither holds (and the heads differ) is the volume
    /// diverged; a stale remote head is `LocalAhead` via
    /// [`observe_with_relation`](Self::observe_with_relation).
    ///
    /// On `Diverged` a divergence record is written and both heads stay
    /// visible — publication must not silently overwrite the remote.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &self,
        remote_head: RemoteHead,
        changes: Vec<RemoteChange>,
        local_head: [u8; 32],
        remote_is_descendant_of_local: bool,
        local_is_ancestor_of_remote: bool,
        base_commit: Option<[u8; 32]>,
        now_ns: i64,
    ) -> Result<Relation, MirageError> {
        let relation = if remote_head.head_commit == local_head {
            Relation::Same
        } else if remote_is_descendant_of_local || local_is_ancestor_of_remote {
            Relation::RemoteAhead
        } else {
            Relation::Diverged
        };
        self.observe_with_relation(
            remote_head,
            changes,
            local_head,
            relation,
            base_commit,
            now_ns,
        )
    }

    /// Records an observation with a caller-verified relation — the commit
    /// graph is owned by the caller, so a remote head that is a strict
    /// ancestor of local reports `LocalAhead` and stays divergence-free.
    pub fn observe_with_relation(
        &self,
        remote_head: RemoteHead,
        changes: Vec<RemoteChange>,
        local_head: [u8; 32],
        relation: Relation,
        base_commit: Option<[u8; 32]>,
        now_ns: i64,
    ) -> Result<Relation, MirageError> {
        for change in &changes {
            self.db.writer().remote_change_record(change.clone())?;
        }
        self.db.writer().remote_head_observe(remote_head.clone())?;
        if relation == Relation::Diverged {
            self.db.writer().divergence_record(Divergence {
                volume_id: self.volume,
                base_commit,
                local_head,
                remote_head: remote_head.head_commit,
                status: DivergenceStatus::Diverged,
                detected_ns: now_ns,
                resolved_ns: None,
            })?;
        }
        Ok(relation)
    }

    /// The volume's live divergence — callers use this to refuse remote
    /// publication or surface both heads.
    pub fn live_divergence(&self) -> Result<Option<Divergence>, MirageError> {
        self.db.live_divergence(self.volume)
    }

    /// Remote changes after `cursor` — the replay stream a reconciler feeds
    /// into namespace replay.
    pub fn changes_after(&self, cursor: &str) -> Result<Vec<RemoteChange>, MirageError> {
        self.db.remote_changes_after(self.volume, cursor)
    }

    /// Resolves a divergence while keeping the record for audit.
    pub fn resolve(&self, status: DivergenceStatus, now_ns: i64) -> Result<(), MirageError> {
        self.db
            .writer()
            .divergence_resolve(self.volume, status, now_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Database, RepositoryId) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("control.db")).unwrap();
        let volume = RepositoryId::from_bytes([0x77; 16]);
        (dir, db, volume)
    }

    fn head(cursor: &str, commit: u8, seq: i64, volume: RepositoryId) -> RemoteHead {
        RemoteHead {
            volume_id: volume,
            head_commit: [commit; 32],
            head_seq: seq,
            changes_cursor: cursor.into(),
            observed_ns: 1,
        }
    }

    #[test]
    fn identical_head_is_noop_and_divergence_records_both() {
        let (_d, db, volume) = setup();
        let reconciler = Reconciler::new(&db, volume);
        let local = [0xaa; 32];
        // Same head → Same, no divergence.
        let rel = reconciler
            .observe(
                head("c1", 0xaa, 5, volume),
                vec![],
                local,
                true,
                true,
                None,
                1,
            )
            .unwrap();
        assert_eq!(rel, Relation::Same);
        assert!(reconciler.live_divergence().unwrap().is_none());
        // Concurrent commit: neither ancestor → divergence, both preserved.
        let rel = reconciler
            .observe(
                head("c2", 0xbb, 6, volume),
                vec![],
                local,
                false,
                false,
                Some(local),
                2,
            )
            .unwrap();
        assert_eq!(rel, Relation::Diverged);
        let div = reconciler.live_divergence().unwrap().unwrap();
        assert_eq!(div.local_head, local);
        assert_eq!(div.remote_head, [0xbb; 32]);
        assert_eq!(div.status, DivergenceStatus::Diverged);
    }

    #[test]
    fn opaque_cursor_tokens_are_stored_verbatim() {
        let (_d, db, volume) = setup();
        let reconciler = Reconciler::new(&db, volume);
        reconciler
            .observe(
                head("9", 1, 5, volume),
                vec![],
                [0; 32],
                true,
                false,
                None,
                1,
            )
            .unwrap();
        // "10" sorts before "9" lexically but is a valid opaque successor.
        reconciler
            .observe(
                head("10", 2, 6, volume),
                vec![],
                [0; 32],
                true,
                false,
                None,
                2,
            )
            .unwrap();
    }

    #[test]
    fn remote_replay_stream_is_ordered_and_resolvable() {
        let (_d, db, volume) = setup();
        let reconciler = Reconciler::new(&db, volume);
        let changes = vec![
            RemoteChange {
                volume_id: volume,
                cursor: "c1".into(),
                change_kind: "commit".into(),
                payload: b"a".to_vec(),
                observed_ns: 1,
            },
            RemoteChange {
                volume_id: volume,
                cursor: "c2".into(),
                change_kind: "commit".into(),
                payload: b"b".to_vec(),
                observed_ns: 1,
            },
        ];
        reconciler
            .observe(
                head("c2", 9, 2, volume),
                changes,
                [0; 32],
                true,
                true,
                None,
                1,
            )
            .unwrap();
        let replayed = reconciler.changes_after("").unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].cursor, "c1");
        // Replay after the first cursor resumes at the second.
        assert_eq!(reconciler.changes_after("c1").unwrap()[0].cursor, "c2");
        // Divergence then resolve.
        reconciler
            .observe(
                head("c3", 7, 3, volume),
                vec![],
                [0xaa; 32],
                false,
                false,
                None,
                2,
            )
            .unwrap();
        reconciler.resolve(DivergenceStatus::Reconciled, 3).unwrap();
        assert!(matches!(
            reconciler.live_divergence().unwrap().unwrap().status,
            DivergenceStatus::Reconciled
        ));
    }
}
