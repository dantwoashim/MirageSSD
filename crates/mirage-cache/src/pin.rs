use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use mirage_db::{Database, PersistentPinReason};
use mirage_types::{MirageError, PageHash, SessionId, UpdateId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PinReason {
    Mandatory,
    Session(SessionId),
    Dirty(UpdateId),
    Recovery,
    ReadLease,
}

#[derive(Debug, Clone, Default)]
pub struct PinRegistry {
    inner: Arc<RwLock<BTreeMap<PageHash, BTreeSet<PinReason>>>>,
}

impl PinRegistry {
    pub fn rebuild(db: &Database) -> Result<Self, MirageError> {
        let registry = Self::default();
        {
            let mut pins = registry
                .inner
                .write()
                .map_err(|_| MirageError::internal_invariant("pin registry lock poisoned"))?;
            for record in db.load_cache_pins()? {
                pins.entry(record.page_hash)
                    .or_default()
                    .insert(from_persistent(record.reason));
            }
        }
        Ok(registry)
    }
    pub fn pin_batch(
        &self,
        db: &Database,
        reason: PinReason,
        pages: &[PageHash],
    ) -> Result<(), MirageError> {
        let persistent = to_persistent(reason)?;
        let mut unique = pages.to_vec();
        unique.sort();
        unique.dedup();
        let mut pins = self
            .inner
            .write()
            .map_err(|_| MirageError::internal_invariant("pin registry lock poisoned"))?;
        db.pin_cache_pages(persistent, unique.clone())?;
        for page in unique {
            pins.entry(page).or_default().insert(reason);
        }
        Ok(())
    }
    pub fn release(&self, db: &Database, reason: PinReason) -> Result<(), MirageError> {
        let persistent = to_persistent(reason)?;
        let mut pins = self
            .inner
            .write()
            .map_err(|_| MirageError::internal_invariant("pin registry lock poisoned"))?;
        db.release_cache_pins(persistent)?;
        pins.retain(|_, reasons| {
            reasons.remove(&reason);
            !reasons.is_empty()
        });
        Ok(())
    }
    pub fn is_pinned(&self, hash: PageHash) -> Result<bool, MirageError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| MirageError::internal_invariant("pin registry lock poisoned"))?
            .get(&hash)
            .is_some_and(|reasons| !reasons.is_empty()))
    }
    pub fn reasons(&self, hash: PageHash) -> Result<BTreeSet<PinReason>, MirageError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| MirageError::internal_invariant("pin registry lock poisoned"))?
            .get(&hash)
            .cloned()
            .unwrap_or_default())
    }
}

fn to_persistent(reason: PinReason) -> Result<PersistentPinReason, MirageError> {
    match reason {
        PinReason::Mandatory => Ok(PersistentPinReason::Mandatory),
        PinReason::Session(id) => Ok(PersistentPinReason::Session(id)),
        PinReason::Dirty(id) => Ok(PersistentPinReason::Dirty(id)),
        PinReason::Recovery => Ok(PersistentPinReason::Recovery),
        PinReason::ReadLease => Err(MirageError::invalid_argument(
            "read leases are transient and cannot be persisted",
        )),
    }
}
fn from_persistent(reason: PersistentPinReason) -> PinReason {
    match reason {
        PersistentPinReason::Mandatory => PinReason::Mandatory,
        PersistentPinReason::Session(id) => PinReason::Session(id),
        PersistentPinReason::Dirty(id) => PinReason::Dirty(id),
        PersistentPinReason::Recovery => PinReason::Recovery,
    }
}
