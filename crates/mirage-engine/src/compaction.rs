//! Bounded-history compaction and provable-unreachable remote reclamation.
//! Namespace deltas are retained at/after the newest checkpoint that a
//! bound allows; journal operations and pending uploads keep at least
//! `min_keep` entries; a remote object may only be physically deleted when
//! it is provably unreachable from the retained commit set, a newer
//! verified commit has landed, and the reclamation window elapsed.

use mirage_db::{Database, GcBound, GcKind};
use mirage_types::{MirageError, RepositoryId};

/// History-compaction driver honoring durable retention bounds.
pub struct Compaction<'a> {
    db: &'a Database,
    volume: RepositoryId,
}

/// The set of remote objects reachable from retained commits; anything not
/// in this set is an unreachable candidate.
pub trait ReachableSet: Send + Sync {
    /// True when `object_key` is referenced by a retained commit.
    fn reachable(&self, object_key: &str) -> bool;
    /// The newest verified commit — reclamation requires a commit strictly
    /// newer than the one that made the candidate unreachable.
    fn latest_verified_commit(&self) -> [u8; 32];
    /// Enumerates remote object keys under management.
    fn remote_object_keys(&self) -> Vec<String>;
}

impl<'a> Compaction<'a> {
    pub fn new(db: &'a Database, volume: RepositoryId) -> Self {
        Self { db, volume }
    }

    /// Sets the namespace-history bound: deltas before `retain_from`
    /// checkpoint may be dropped once at least `min_keep` deltas survive.
    pub fn bound_namespace_history(
        &self,
        retain_from: i64,
        min_keep: i64,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.db.writer().gc_bound_set(GcBound {
            volume_id: self.volume,
            kind: GcKind::NamespaceHistory,
            retain_from,
            min_keep,
            set_ns: now_ns,
        })
    }

    /// Marks remote objects unreachable when they are not reachable from the
    /// retained commit set. Physical deletion is deferred: the reclaimable
    /// scan requires a newer verified commit plus the reclamation window.
    pub fn mark_unreachable(
        &self,
        reachable: &dyn ReachableSet,
        reclamation_window_ns: i64,
        now_ns: i64,
    ) -> Result<u64, MirageError> {
        let mut marked = 0u64;
        for key in reachable.remote_object_keys() {
            if !reachable.reachable(&key) {
                self.db.writer().gc_unreachable_mark(
                    self.volume,
                    &key,
                    reachable.latest_verified_commit(),
                    now_ns.saturating_add(reclamation_window_ns),
                    now_ns,
                )?;
                marked += 1;
            }
        }
        Ok(marked)
    }

    /// Remote object keys safe to delete now: unreachable since an older
    /// verified commit and past the reclamation window. Each candidate is
    /// rechecked against the live reachability set at deletion time — an
    /// object re-pinned or re-created since marking is cleared instead of
    /// deleted.
    pub fn reclaimable(
        &self,
        reachable: &dyn ReachableSet,
        now_ns: i64,
    ) -> Result<Vec<String>, MirageError> {
        let candidates =
            self.db
                .gc_reclaimable(self.volume, &reachable.latest_verified_commit(), now_ns)?;
        let mut safe = Vec::with_capacity(candidates.len());
        for key in candidates {
            if reachable.reachable(&key) {
                self.db.writer().gc_unreachable_clear(self.volume, &key)?;
            } else {
                safe.push(key);
            }
        }
        Ok(safe)
    }

    /// Clears a reclaimed candidate after the remote delete lands.
    pub fn clear_reclaimed(&self, object_key: &str) -> Result<(), MirageError> {
        self.db
            .writer()
            .gc_unreachable_clear(self.volume, object_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct FakeReachable {
        reachable: HashSet<String>,
        remote: Vec<String>,
        head: [u8; 32],
    }

    impl ReachableSet for FakeReachable {
        fn reachable(&self, key: &str) -> bool {
            self.reachable.contains(key)
        }
        fn latest_verified_commit(&self) -> [u8; 32] {
            self.head
        }
        fn remote_object_keys(&self) -> Vec<String> {
            self.remote.clone()
        }
    }

    #[test]
    fn unreachable_marked_but_reclaim_only_after_window_and_new_commit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("control.db")).unwrap();
        let volume = RepositoryId::from_bytes([0x61; 16]);
        let compaction = Compaction::new(&db, volume);
        let commit_at_mark = [0x01; 32];
        let reachable = FakeReachable {
            reachable: HashSet::from(["keep".to_string()]),
            remote: vec!["keep".into(), "dead".into()],
            head: commit_at_mark,
        };
        // One object unreachable → marked, but never reclaimable at the same
        // commit nor before the window.
        assert_eq!(compaction.mark_unreachable(&reachable, 100, 10).unwrap(), 1);
        assert!(compaction.reclaimable(&reachable, 10).unwrap().is_empty());
        // Window passes but head is still the marking commit → still held.
        assert!(compaction.reclaimable(&reachable, 200).unwrap().is_empty());
        // A newer verified commit lands → reclaimable.
        let newer = FakeReachable {
            head: [0x02; 32],
            ..reachable
        };
        let reclaim = compaction.reclaimable(&newer, 200).unwrap();
        assert_eq!(reclaim, vec!["dead".to_string()]);
        compaction.clear_reclaimed("dead").unwrap();
        assert!(compaction.reclaimable(&newer, 300).unwrap().is_empty());
    }
}
