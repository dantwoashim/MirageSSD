use mirage_types::{ByteCount, MirageError, PageHash};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;

use crate::Database;
use crate::error::{conflict, sqlite};
use crate::value::{fixed, nonnegative, sqlite_integer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheShardSpec {
    pub shard_id: i64,
    pub relative_path: String,
    pub page_size: ByteCount,
    pub slot_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum CacheSlotState {
    Free = 0,
    Reserved = 1,
    Resident = 2,
    Evicting = 3,
    RetryDeallocate = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSlotRecord {
    pub shard_id: i64,
    pub slot_index: u32,
    pub generation: u64,
    pub state: CacheSlotState,
    pub page_hash: Option<PageHash>,
    pub logical_length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSnapshot {
    pub shards: Vec<CacheShardSpec>,
    pub resident_slots: Vec<CacheSlotRecord>,
}

pub fn load_cache_snapshot(path: &Path) -> Result<CacheSnapshot, MirageError> {
    let connection = crate::open::read_connection(path)?;
    Ok(CacheSnapshot {
        shards: load_shards(&connection)?,
        resident_slots: load_residents(&connection)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveCacheSlotOutcome {
    Reserved(CacheSlotRecord),
    Existing(CacheSlotRecord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitCacheSlotOutcome {
    Committed(CacheSlotRecord),
    Existing(CacheSlotRecord),
}

impl Database {
    pub fn register_cache_shard(&self, value: CacheShardSpec) -> Result<(), MirageError> {
        self.writer.register_cache_shard(value)
    }
    pub fn reserve_cache_slot(
        &self,
        hash: PageHash,
        logical_length: u32,
    ) -> Result<ReserveCacheSlotOutcome, MirageError> {
        self.writer.reserve_cache_slot(hash, logical_length)
    }
    pub fn reserve_cache_slots_batch(
        &self,
        requests: Vec<(PageHash, u32)>,
    ) -> Result<Vec<ReserveCacheSlotOutcome>, MirageError> {
        self.writer.reserve_cache_slots_batch(requests)
    }
    pub fn commit_cache_slot(
        &self,
        record: CacheSlotRecord,
    ) -> Result<CommitCacheSlotOutcome, MirageError> {
        self.writer.commit_cache_slot(record)
    }
    pub fn release_cache_reservation(&self, record: CacheSlotRecord) -> Result<(), MirageError> {
        self.writer.release_cache_reservation(record)
    }
    pub fn begin_cache_eviction(
        &self,
        record: CacheSlotRecord,
    ) -> Result<CacheSlotRecord, MirageError> {
        self.writer.begin_cache_eviction(record)
    }
    pub fn finish_cache_deallocation(&self, record: CacheSlotRecord) -> Result<(), MirageError> {
        self.writer.finish_cache_deallocation(record)
    }
    pub fn mark_cache_deallocation_retry(
        &self,
        record: CacheSlotRecord,
    ) -> Result<(), MirageError> {
        self.writer.mark_cache_deallocation_retry(record)
    }
    pub fn load_resident_cache_slots(&self) -> Result<Vec<CacheSlotRecord>, MirageError> {
        self.reads.with_connection(load_residents)
    }
    pub fn load_cache_slots(&self) -> Result<Vec<CacheSlotRecord>, MirageError> {
        self.reads.with_connection(load_all)
    }
    pub fn load_cache_shards(&self) -> Result<Vec<CacheShardSpec>, MirageError> {
        self.reads.with_connection(load_shards)
    }
}

pub(crate) fn reserve_batch(
    connection: &mut Connection,
    mut requests: Vec<(PageHash, u32)>,
) -> Result<Vec<ReserveCacheSlotOutcome>, MirageError> {
    if requests.is_empty() {
        return Err(MirageError::invalid_argument(
            "cache reservation batch is empty",
        ));
    }
    requests.sort_by_key(|request| request.0);
    for pair in requests.windows(2) {
        if pair[0].0 == pair[1].0 {
            if pair[0].1 != pair[1].1 {
                return Err(MirageError::invalid_argument(
                    "duplicate cache reservation has conflicting lengths",
                ));
            }
            return Err(MirageError::invalid_argument(
                "cache reservation batch contains duplicate pages",
            ));
        }
    }
    if requests.iter().any(|request| request.1 == 0) {
        return Err(MirageError::invalid_argument(
            "cache reservation batch contains a zero-length page",
        ));
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin cache batch reservation"))?;
    let mut outcomes = Vec::with_capacity(requests.len());
    let mut missing = Vec::new();
    for (hash, logical_length) in requests {
        if let Some(existing) = query_hash(&transaction, hash, CacheSlotState::Resident)? {
            if existing.logical_length != logical_length {
                return Err(MirageError::integrity_mismatch(
                    "resident page length differs from reservation request",
                ));
            }
            outcomes.push((hash, ReserveCacheSlotOutcome::Existing(existing)));
            continue;
        }
        let reserved = transaction
            .query_row(
                "SELECT shard_id, slot_index, generation, state, page_hash, logical_length
                 FROM cache_slots WHERE page_hash=?1 AND state=1
                 ORDER BY shard_id, slot_index LIMIT 1",
                [hash.as_bytes().as_slice()],
                decode_slot,
            )
            .optional()
            .map_err(|error| sqlite(error, "failed to query existing cache reservation"))?;
        if let Some(reserved) = reserved {
            if reserved.logical_length != logical_length {
                return Err(MirageError::integrity_mismatch(
                    "reserved page length differs from reservation request",
                ));
            }
            outcomes.push((hash, ReserveCacheSlotOutcome::Reserved(reserved)));
        } else {
            missing.push((hash, logical_length));
        }
    }
    let mut free = Vec::with_capacity(missing.len());
    {
        let mut statement = transaction
            .prepare(
                "SELECT shard_id, slot_index, generation FROM cache_slots
                 WHERE state=0 ORDER BY shard_id, slot_index LIMIT ?1",
            )
            .map_err(|error| sqlite(error, "failed to prepare free cache batch"))?;
        let rows = statement
            .query_map([i64::try_from(missing.len()).unwrap_or(i64::MAX)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|error| sqlite(error, "failed to select free cache batch"))?;
        for row in rows {
            free.push(row.map_err(|error| sqlite(error, "failed to decode free cache slot"))?);
        }
    }
    if free.len() != missing.len() {
        return Err(MirageError::cache_full(format!(
            "cache needs {} additional free slots but only {} are available",
            missing.len(),
            free.len()
        )));
    }
    for ((hash, logical_length), (shard_id, slot_index, old_generation)) in
        missing.into_iter().zip(free)
    {
        let generation = nonnegative(old_generation, "cache generation")?
            .checked_add(1)
            .ok_or_else(|| MirageError::internal_invariant("cache generation overflows"))?;
        let changed = transaction
            .execute(
                "UPDATE cache_slots SET generation=?3, state=1, page_hash=?4, logical_length=?5
                 WHERE shard_id=?1 AND slot_index=?2 AND state=0",
                params![
                    shard_id,
                    slot_index,
                    sqlite_integer(generation, "cache generation")?,
                    hash.as_bytes().as_slice(),
                    i64::from(logical_length),
                ],
            )
            .map_err(|error| sqlite(error, "failed to reserve cache batch slot"))?;
        if changed != 1 {
            return Err(conflict("free cache slot changed during batch reservation"));
        }
        outcomes.push((
            hash,
            ReserveCacheSlotOutcome::Reserved(CacheSlotRecord {
                shard_id,
                slot_index: u32::try_from(nonnegative(slot_index, "cache slot index")?)
                    .map_err(|_| MirageError::integrity_mismatch("cache slot index exceeds u32"))?,
                generation,
                state: CacheSlotState::Reserved,
                page_hash: Some(hash),
                logical_length,
            }),
        ));
    }
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit cache batch reservation"))?;
    outcomes.sort_by_key(|outcome| outcome.0);
    Ok(outcomes.into_iter().map(|outcome| outcome.1).collect())
}

pub(crate) fn register_shard(
    connection: &mut Connection,
    value: CacheShardSpec,
) -> Result<(), MirageError> {
    if value.shard_id < 0
        || value.relative_path.is_empty()
        || value.relative_path.len() > 255
        || value.relative_path.contains(['/', '\\'])
        || value.slot_count == 0
    {
        return Err(MirageError::invalid_argument(
            "cache shard specification is invalid",
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| sqlite(error, "failed to begin cache shard transaction"))?;
    transaction.execute("INSERT INTO cache_shards(shard_id, relative_path, page_size, slot_count, format_version) VALUES (?1, ?2, ?3, ?4, 1)", params![value.shard_id, value.relative_path, sqlite_integer(value.page_size.as_u64(), "cache page size")?, i64::from(value.slot_count)]).map_err(|error| sqlite(error, "failed to insert cache shard"))?;
    {
        let mut statement = transaction.prepare("INSERT INTO cache_slots(shard_id, slot_index, generation, state, page_hash, logical_length) VALUES (?1, ?2, 0, 0, NULL, 0)").map_err(|error| sqlite(error, "failed to prepare cache slots"))?;
        for slot in 0..value.slot_count {
            statement
                .execute(params![value.shard_id, i64::from(slot)])
                .map_err(|error| sqlite(error, "failed to initialize cache slot"))?;
        }
    }
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit cache shard"))
}

pub(crate) fn reserve(
    connection: &mut Connection,
    hash: PageHash,
    logical_length: u32,
) -> Result<ReserveCacheSlotOutcome, MirageError> {
    if logical_length == 0 {
        return Err(MirageError::invalid_argument(
            "cache page logical length is zero",
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| sqlite(error, "failed to begin cache reservation"))?;
    if let Some(existing) = query_hash(&transaction, hash, CacheSlotState::Resident)? {
        return Ok(ReserveCacheSlotOutcome::Existing(existing));
    }
    let free = transaction.query_row("SELECT shard_id, slot_index, generation FROM cache_slots WHERE state = 0 ORDER BY shard_id, slot_index LIMIT 1", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))).optional().map_err(|error| sqlite(error, "failed to select free cache slot"))?.ok_or_else(|| MirageError::cache_full("cache has no free slot"))?;
    let generation = u64::try_from(free.2)
        .map_err(|_| MirageError::integrity_mismatch("cache generation is negative"))?
        .checked_add(1)
        .ok_or_else(|| MirageError::internal_invariant("cache generation overflows"))?;
    transaction.execute("UPDATE cache_slots SET generation=?3, state=1, page_hash=?4, logical_length=?5 WHERE shard_id=?1 AND slot_index=?2 AND state=0", params![free.0, free.1, sqlite_integer(generation, "cache generation")?, hash.as_bytes().as_slice(), i64::from(logical_length)]).map_err(|error| sqlite(error, "failed to reserve cache slot"))?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit cache reservation"))?;
    Ok(ReserveCacheSlotOutcome::Reserved(CacheSlotRecord {
        shard_id: free.0,
        slot_index: u32::try_from(free.1)
            .map_err(|_| MirageError::integrity_mismatch("cache slot index invalid"))?,
        generation,
        state: CacheSlotState::Reserved,
        page_hash: Some(hash),
        logical_length,
    }))
}

pub(crate) fn commit_slot(
    connection: &mut Connection,
    record: CacheSlotRecord,
) -> Result<CommitCacheSlotOutcome, MirageError> {
    require(record, CacheSlotState::Reserved)?;
    let hash = record.page_hash.expect("validated");
    let transaction = connection
        .transaction()
        .map_err(|error| sqlite(error, "failed to begin cache slot commit"))?;
    if let Some(existing) = query_hash(&transaction, hash, CacheSlotState::Resident)? {
        transition_to_evicting(&transaction, record, CacheSlotState::Reserved)?;
        transaction
            .commit()
            .map_err(|error| sqlite(error, "failed to resolve duplicate cache slot"))?;
        return Ok(CommitCacheSlotOutcome::Existing(existing));
    }
    let changed = transaction.execute("UPDATE cache_slots SET state=2 WHERE shard_id=?1 AND slot_index=?2 AND generation=?3 AND state=1 AND page_hash=?4 AND logical_length=?5", params![record.shard_id, i64::from(record.slot_index), sqlite_integer(record.generation, "cache generation")?, hash.as_bytes().as_slice(), i64::from(record.logical_length)]).map_err(|error| sqlite(error, "failed to commit resident cache slot"))?;
    if changed != 1 {
        return Err(conflict("cache reservation changed before commit"));
    }
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit resident cache slot"))?;
    Ok(CommitCacheSlotOutcome::Committed(CacheSlotRecord {
        state: CacheSlotState::Resident,
        ..record
    }))
}

pub(crate) fn release(
    connection: &mut Connection,
    record: CacheSlotRecord,
) -> Result<(), MirageError> {
    require(record, CacheSlotState::Reserved)?;
    let transaction = connection
        .transaction()
        .map_err(|error| sqlite(error, "failed to begin reservation release"))?;
    transition_to_evicting(&transaction, record, CacheSlotState::Reserved)?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to release cache reservation"))
}

pub(crate) fn begin_eviction(
    connection: &mut Connection,
    record: CacheSlotRecord,
) -> Result<CacheSlotRecord, MirageError> {
    require(record, CacheSlotState::Resident)?;
    let changed = connection.execute("UPDATE cache_slots SET state=3 WHERE shard_id=?1 AND slot_index=?2 AND generation=?3 AND state=2 AND page_hash=?4", params![record.shard_id, i64::from(record.slot_index), sqlite_integer(record.generation, "cache generation")?, record.page_hash.expect("validated").as_bytes().as_slice()]).map_err(|error| sqlite(error, "failed to begin cache eviction"))?;
    if changed != 1 {
        return Err(conflict("resident cache slot changed before eviction"));
    }
    Ok(CacheSlotRecord {
        state: CacheSlotState::Evicting,
        ..record
    })
}
pub(crate) fn finish_deallocation(
    connection: &mut Connection,
    record: CacheSlotRecord,
) -> Result<(), MirageError> {
    if !matches!(
        record.state,
        CacheSlotState::Evicting | CacheSlotState::RetryDeallocate
    ) {
        return Err(MirageError::invalid_argument(
            "cache slot is not awaiting deallocation",
        ));
    }
    let changed = connection.execute("UPDATE cache_slots SET state=0, page_hash=NULL, logical_length=0 WHERE shard_id=?1 AND slot_index=?2 AND generation=?3 AND state IN (3,4)", params![record.shard_id, i64::from(record.slot_index), sqlite_integer(record.generation, "cache generation")?]).map_err(|error| sqlite(error, "failed to finish cache deallocation"))?;
    if changed != 1 {
        return Err(conflict("cache slot changed before deallocation completed"));
    }
    Ok(())
}
pub(crate) fn mark_retry(
    connection: &mut Connection,
    record: CacheSlotRecord,
) -> Result<(), MirageError> {
    let changed = connection.execute("UPDATE cache_slots SET state=4 WHERE shard_id=?1 AND slot_index=?2 AND generation=?3 AND state=3", params![record.shard_id, i64::from(record.slot_index), sqlite_integer(record.generation, "cache generation")?]).map_err(|error| sqlite(error, "failed to mark cache deallocation retry"))?;
    if changed != 1 {
        return Err(conflict("evicting cache slot changed before retry"));
    }
    Ok(())
}

fn transition_to_evicting(
    connection: &Connection,
    record: CacheSlotRecord,
    state: CacheSlotState,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE cache_slots SET state=3 WHERE shard_id=?1 AND slot_index=?2 AND generation=?3 AND state=?4",
            params![record.shard_id, i64::from(record.slot_index), sqlite_integer(record.generation, "cache generation")?, state as i64],
        )
        .map_err(|error| sqlite(error, "failed to quarantine cache slot"))?;
    if changed != 1 {
        return Err(conflict("cache slot changed before quarantine"));
    }
    Ok(())
}
fn require(record: CacheSlotRecord, state: CacheSlotState) -> Result<(), MirageError> {
    if record.state != state || record.page_hash.is_none() || record.logical_length == 0 {
        Err(MirageError::invalid_argument(
            "cache slot record is invalid for transition",
        ))
    } else {
        Ok(())
    }
}
fn query_hash(
    connection: &Connection,
    hash: PageHash,
    state: CacheSlotState,
) -> Result<Option<CacheSlotRecord>, MirageError> {
    connection.query_row("SELECT shard_id, slot_index, generation, state, page_hash, logical_length FROM cache_slots WHERE page_hash=?1 AND state=?2", params![hash.as_bytes().as_slice(), state as i64], decode_slot).optional().map_err(|error| sqlite(error, "failed to query cache hash"))
}
fn load_residents(connection: &Connection) -> Result<Vec<CacheSlotRecord>, MirageError> {
    let mut statement = connection.prepare("SELECT shard_id, slot_index, generation, state, page_hash, logical_length FROM cache_slots WHERE state=2 ORDER BY shard_id, slot_index").map_err(|error| sqlite(error, "failed to prepare resident cache load"))?;
    statement
        .query_map([], decode_slot)
        .map_err(|error| sqlite(error, "failed to load resident cache slots"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite(error, "failed to decode resident cache slot"))
}
fn load_all(connection: &Connection) -> Result<Vec<CacheSlotRecord>, MirageError> {
    let mut statement = connection.prepare("SELECT shard_id, slot_index, generation, state, page_hash, logical_length FROM cache_slots ORDER BY shard_id, slot_index").map_err(|error| sqlite(error, "failed to prepare cache slot load"))?;
    statement
        .query_map([], decode_slot)
        .map_err(|error| sqlite(error, "failed to load cache slots"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| sqlite(error, "failed to decode cache slot"))
}
fn load_shards(connection: &Connection) -> Result<Vec<CacheShardSpec>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT shard_id, relative_path, page_size, slot_count
             FROM cache_shards ORDER BY shard_id",
        )
        .map_err(|error| sqlite(error, "failed to prepare cache shard load"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| sqlite(error, "failed to load cache shards"))?;
    let mut shards = Vec::new();
    for row in rows {
        let (shard_id, relative_path, page_size, slot_count) =
            row.map_err(|error| sqlite(error, "failed to decode cache shard"))?;
        shards.push(CacheShardSpec {
            shard_id,
            relative_path,
            page_size: ByteCount::from_u64(nonnegative(page_size, "cache page size")?),
            slot_count: u32::try_from(nonnegative(slot_count, "cache slot count")?)
                .map_err(|_| MirageError::integrity_mismatch("cache slot count exceeds u32"))?,
        });
    }
    Ok(shards)
}
fn decode_slot(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheSlotRecord> {
    let state = match row.get::<_, i64>(3)? {
        0 => CacheSlotState::Free,
        1 => CacheSlotState::Reserved,
        2 => CacheSlotState::Resident,
        3 => CacheSlotState::Evicting,
        4 => CacheSlotState::RetryDeallocate,
        value => return Err(rusqlite::Error::IntegralValueOutOfRange(3, value)),
    };
    let hash = row
        .get::<_, Option<Vec<u8>>>(4)?
        .map(|bytes| fixed::<32>(bytes, "cache page hash").map(PageHash::from_bytes))
        .transpose()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(CacheSlotRecord {
        shard_id: row.get(0)?,
        slot_index: nonnegative(row.get::<_, i64>(1)?, "cache slot index")
            .and_then(|value| {
                u32::try_from(value)
                    .map_err(|_| MirageError::integrity_mismatch("cache slot index overflows"))
            })
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        generation: nonnegative(row.get::<_, i64>(2)?, "cache generation")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        state,
        page_hash: hash,
        logical_length: u32::try_from(
            nonnegative(row.get::<_, i64>(5)?, "cache logical length")
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}
