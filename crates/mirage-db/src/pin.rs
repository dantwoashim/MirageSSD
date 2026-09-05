use mirage_types::{MirageError, PageHash, SessionId, UpdateId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;
use crate::value::fixed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PersistentPinReason {
    Mandatory,
    Session(SessionId),
    Dirty(UpdateId),
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePinRecord {
    pub page_hash: PageHash,
    pub reason: PersistentPinReason,
}

impl Database {
    pub fn pin_cache_pages(
        &self,
        reason: PersistentPinReason,
        pages: Vec<PageHash>,
    ) -> Result<(), MirageError> {
        self.writer.pin_cache_pages(reason, pages)
    }
    pub fn release_cache_pins(&self, reason: PersistentPinReason) -> Result<(), MirageError> {
        self.writer.release_cache_pins(reason)
    }
    pub fn load_cache_pins(&self) -> Result<Vec<CachePinRecord>, MirageError> {
        self.reads.with_connection(load)
    }
}

pub(crate) fn pin(
    connection: &mut Connection,
    reason: PersistentPinReason,
    mut pages: Vec<PageHash>,
) -> Result<(), MirageError> {
    pages.sort();
    pages.dedup();
    if pages.is_empty() {
        return Err(MirageError::invalid_argument("cache pin batch is empty"));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| sqlite(error, "failed to begin cache pin transaction"))?;
    validate_owner(&transaction, reason)?;
    for page in &pages {
        let resident = transaction
            .query_row(
                "SELECT 1 FROM cache_slots WHERE page_hash=?1 AND state=2",
                [page.as_bytes().as_slice()],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| sqlite(error, "failed to verify cache pin resident"))?
            .is_some();
        if !resident {
            return Err(MirageError::cache_full(
                "cache pin batch contains a non-resident page",
            ));
        }
    }
    let (kind, owner) = encode_reason(reason);
    for page in pages {
        transaction
            .execute(
                "INSERT OR IGNORE INTO cache_pins(page_hash, reason, owner_id) VALUES (?1, ?2, ?3)",
                params![page.as_bytes().as_slice(), kind, owner],
            )
            .map_err(|error| sqlite(error, "failed to insert cache pin"))?;
    }
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit cache pin batch"))
}

pub(crate) fn release(
    connection: &mut Connection,
    reason: PersistentPinReason,
) -> Result<(), MirageError> {
    let (kind, owner) = encode_reason(reason);
    connection
        .execute(
            "DELETE FROM cache_pins WHERE reason=?1 AND owner_id=?2",
            params![kind, owner],
        )
        .map_err(|error| sqlite(error, "failed to release cache pins"))?;
    Ok(())
}

fn load(connection: &Connection) -> Result<Vec<CachePinRecord>, MirageError> {
    let mut statement = connection.prepare("SELECT page_hash, reason, owner_id FROM cache_pins ORDER BY page_hash, reason, owner_id").map_err(|error| sqlite(error, "failed to prepare cache pin load"))?;
    statement
        .query_map([], |row| {
            let hash = PageHash::from_bytes(
                fixed::<32>(row.get::<_, Vec<u8>>(0)?, "cache pin hash")
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
            );
            let kind = row.get::<_, String>(1)?;
            let owner = row.get::<_, Vec<u8>>(2)?;
            let reason = decode_reason(&kind, owner).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(CachePinRecord {
                page_hash: hash,
                reason,
            })
        })
        .map_err(|error| sqlite(error, "failed to load cache pins"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite(error, "failed to decode cache pins"))
}

fn validate_owner(connection: &Connection, reason: PersistentPinReason) -> Result<(), MirageError> {
    let query = match reason {
        PersistentPinReason::Session(id) => Some((
            "SELECT 1 FROM sessions WHERE session_id=?1",
            id.as_bytes().to_vec(),
        )),
        PersistentPinReason::Dirty(id) => Some((
            "SELECT 1 FROM update_journals WHERE update_id=?1",
            id.as_bytes().to_vec(),
        )),
        _ => None,
    };
    if let Some((sql, owner)) = query
        && connection
            .query_row(sql, [owner], |_| Ok(()))
            .optional()
            .map_err(|error| sqlite(error, "failed to verify cache pin owner"))?
            .is_none()
    {
        return Err(MirageError::invalid_argument(
            "cache pin owner does not exist",
        ));
    }
    Ok(())
}
fn encode_reason(reason: PersistentPinReason) -> (&'static str, Vec<u8>) {
    match reason {
        PersistentPinReason::Mandatory => ("mandatory", Vec::new()),
        PersistentPinReason::Session(id) => ("session", id.as_bytes().to_vec()),
        PersistentPinReason::Dirty(id) => ("dirty", id.as_bytes().to_vec()),
        PersistentPinReason::Recovery => ("recovery", Vec::new()),
    }
}
fn decode_reason(kind: &str, owner: Vec<u8>) -> Result<PersistentPinReason, MirageError> {
    match (kind, owner.as_slice()) {
        ("mandatory", []) => Ok(PersistentPinReason::Mandatory),
        ("recovery", []) => Ok(PersistentPinReason::Recovery),
        ("session", _) => Ok(PersistentPinReason::Session(SessionId::from_bytes(
            fixed::<16>(owner, "session pin owner")?,
        ))),
        ("dirty", _) => Ok(PersistentPinReason::Dirty(UpdateId::from_bytes(
            fixed::<16>(owner, "dirty pin owner")?,
        ))),
        _ => Err(MirageError::integrity_mismatch(
            "cache pin reason and owner disagree",
        )),
    }
}
