use std::path::PathBuf;

use mirage_types::{
    ByteCount, ContentHash, GenerationId, MirageError, MirageErrorKind, PageHash, PageOrdinal,
    PageState, RepositoryId, SlotIndex, StableFileId, UpdateEvent, UpdateId, UpdateState,
    transition_update,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::Database;
use crate::error::{conflict, sqlite, transition};
use crate::state_codec;
use crate::value::{bounded_text, path_text, sqlite_integer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUpdateJournal {
    pub update_id: UpdateId,
    pub repository_id: RepositoryId,
    pub base_generation: GenerationId,
    pub target_generation: GenerationId,
    pub journal_path: PathBuf,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveUpdate {
    pub update_id: UpdateId,
    pub repository_id: RepositoryId,
    pub base_generation: GenerationId,
    pub target_generation: GenerationId,
    pub state: UpdateState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPage {
    pub update_id: UpdateId,
    pub file_id: StableFileId,
    pub page_index: PageOrdinal,
    pub state: PageState,
    pub arena_slot: Option<(u32, SlotIndex)>,
    pub page_hash: Option<PageHash>,
    pub staging_object_key: Option<ContentHash>,
    pub staging_offset: Option<u64>,
    pub encoded_length: Option<ByteCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSnapshot {
    pub update_id: UpdateId,
    pub relative_path: String,
    pub snapshot_path: PathBuf,
    pub byte_length: ByteCount,
    pub content_hash: ContentHash,
}

#[derive(Debug, Clone)]
pub(crate) struct UpdateTransition {
    pub update_id: UpdateId,
    pub expected: UpdateState,
    pub event: UpdateEvent,
    pub details: String,
    pub at_ns: i64,
}

impl Database {
    pub fn load_active_update(
        &self,
        repository_id: RepositoryId,
    ) -> Result<Option<ActiveUpdate>, MirageError> {
        self.reads.with_connection(|connection| {
            let row = connection
                .query_row(
                    "SELECT update_id, base_generation, target_generation, state
                     FROM update_journals
                     WHERE repository_id = ?1 AND state NOT IN ('committed', 'rolled_back')
                     ORDER BY created_at_ns DESC LIMIT 1",
                    [repository_id.as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load active update"))?;
            row.map(|(id, base, target, state)| {
                Ok(ActiveUpdate {
                    update_id: UpdateId::from_bytes(crate::value::fixed(id, "update ID")?),
                    repository_id,
                    base_generation: GenerationId::from_u64(crate::value::nonnegative(
                        base,
                        "base generation",
                    )?),
                    target_generation: GenerationId::from_u64(crate::value::nonnegative(
                        target,
                        "target generation",
                    )?),
                    state: state_codec::update(&state)?,
                })
            })
            .transpose()
        })
    }

    pub fn create_update_journal(&self, journal: NewUpdateJournal) -> Result<(), MirageError> {
        self.writer.create_update_journal(journal)
    }

    pub fn upsert_overlay_page(&self, page: OverlayPage) -> Result<(), MirageError> {
        self.writer.upsert_overlay_page(page)
    }

    pub fn upsert_native_snapshot(&self, snapshot: NativeSnapshot) -> Result<(), MirageError> {
        self.writer.upsert_native_snapshot(snapshot)
    }

    pub fn transition_update_state(
        &self,
        update_id: UpdateId,
        expected: UpdateState,
        event: UpdateEvent,
        details: String,
        at_ns: i64,
    ) -> Result<UpdateState, MirageError> {
        self.writer.transition_update_state(UpdateTransition {
            update_id,
            expected,
            event,
            details,
            at_ns,
        })
    }

    pub fn load_update_state(
        &self,
        update_id: UpdateId,
    ) -> Result<Option<UpdateState>, MirageError> {
        self.reads.with_connection(|connection| {
            let state: Option<String> = connection
                .query_row(
                    "SELECT state FROM update_journals WHERE update_id = ?1",
                    [update_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load update state"))?;
            state.map(|value| state_codec::update(&value)).transpose()
        })
    }
}

pub(crate) fn create_journal(
    connection: &mut Connection,
    journal: NewUpdateJournal,
) -> Result<(), MirageError> {
    let base = sqlite_integer(journal.base_generation.as_u64(), "base generation")?;
    let target = sqlite_integer(journal.target_generation.as_u64(), "target generation")?;
    if target <= base {
        return Err(MirageError::invalid_argument(
            "target generation must exceed base generation",
        ));
    }
    let journal_path = path_text(&journal.journal_path)?;
    let active: Option<Vec<u8>> = connection
        .query_row(
            "SELECT update_id FROM update_journals
             WHERE repository_id = ?1 AND state NOT IN ('committed', 'rolled_back')",
            [journal.repository_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to query active update journal"))?;
    if active.is_some() {
        return Err(MirageError::new(
            MirageErrorKind::UpdateActive,
            MirageErrorKind::UpdateActive.default_code(),
            "repository already has an active update journal",
        ));
    }
    let verified: Option<i64> = connection
        .query_row(
            "SELECT verified FROM generations WHERE repository_id = ?1 AND generation = ?2",
            params![journal.repository_id.as_bytes().as_slice(), base],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to validate update base generation"))?;
    if verified != Some(1) {
        return Err(conflict(
            "update journal requires a verified base generation",
        ));
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin update-journal transaction"))?;
    transaction
        .execute(
            "INSERT INTO update_journals(
                update_id, repository_id, base_generation, target_generation,
                state, journal_path, created_at_ns, updated_at_ns
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                journal.update_id.as_bytes().as_slice(),
                journal.repository_id.as_bytes().as_slice(),
                base,
                target,
                UpdateState::Created.as_str(),
                journal_path,
                journal.created_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to create update journal"))?;
    transaction
        .execute(
            "INSERT INTO journal_events(update_id, sequence, event_kind, details, created_at_ns)
             VALUES (?1, 0, 'created', '', ?2)",
            params![
                journal.update_id.as_bytes().as_slice(),
                journal.created_at_ns
            ],
        )
        .map_err(|error| sqlite(error, "failed to create initial journal event"))?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit update journal"))
}

pub(crate) fn upsert_overlay(
    connection: &mut Connection,
    page: OverlayPage,
) -> Result<(), MirageError> {
    let file_id = sqlite_integer(page.file_id.as_u64(), "overlay file ID")?;
    let page_index = i64::from(page.page_index.as_u32());
    let (arena_id, slot_index) = match page.arena_slot {
        Some((arena, slot)) => (Some(i64::from(arena)), Some(i64::from(slot.as_u32()))),
        None => (None, None),
    };
    let staging_offset = page
        .staging_offset
        .map(|value| sqlite_integer(value, "staging offset"))
        .transpose()?;
    let encoded_length = page
        .encoded_length
        .map(|value| sqlite_integer(value.as_u64(), "encoded length"))
        .transpose()?;
    let changed = connection
        .execute(
            "INSERT INTO overlay_pages(
                update_id, file_id, page_index, state, arena_id, slot_index,
                page_hash, staging_object_key, staging_offset, encoded_length
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(update_id, file_id, page_index) DO UPDATE SET
                state = excluded.state,
                arena_id = excluded.arena_id,
                slot_index = excluded.slot_index,
                page_hash = excluded.page_hash,
                staging_object_key = excluded.staging_object_key,
                staging_offset = excluded.staging_offset,
                encoded_length = excluded.encoded_length",
            params![
                page.update_id.as_bytes().as_slice(),
                file_id,
                page_index,
                page.state.as_str(),
                arena_id,
                slot_index,
                page.page_hash.map(|hash| *hash.as_bytes()),
                page.staging_object_key.map(|hash| *hash.as_bytes()),
                staging_offset,
                encoded_length,
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist overlay page"))?;
    if changed != 1 {
        return Err(MirageError::internal_invariant(
            "overlay upsert affected an unexpected row count",
        ));
    }
    Ok(())
}

pub(crate) fn upsert_snapshot(
    connection: &mut Connection,
    snapshot: NativeSnapshot,
) -> Result<(), MirageError> {
    bounded_text(&snapshot.relative_path, 1, 32_767, "snapshot relative path")?;
    let snapshot_path = path_text(&snapshot.snapshot_path)?;
    connection
        .execute(
            "INSERT INTO native_snapshots(
                update_id, relative_path, snapshot_path, byte_length, content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(update_id, relative_path) DO UPDATE SET
                snapshot_path = excluded.snapshot_path,
                byte_length = excluded.byte_length,
                content_hash = excluded.content_hash",
            params![
                snapshot.update_id.as_bytes().as_slice(),
                snapshot.relative_path,
                snapshot_path,
                sqlite_integer(snapshot.byte_length.as_u64(), "snapshot length")?,
                snapshot.content_hash.as_bytes().as_slice(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist native snapshot"))?;
    Ok(())
}

pub(crate) fn transition_state(
    connection: &mut Connection,
    change: UpdateTransition,
) -> Result<UpdateState, MirageError> {
    if change.details.len() > 4096 {
        return Err(MirageError::invalid_argument(
            "journal event details exceed 4096 bytes",
        ));
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin update transition"))?;
    let actual: String = transaction
        .query_row(
            "SELECT state FROM update_journals WHERE update_id = ?1",
            [change.update_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to load update transition state"))?
        .ok_or_else(|| MirageError::invalid_argument("update journal does not exist"))?;
    let actual = state_codec::update(&actual)?;
    if actual != change.expected {
        return Err(conflict("update journal state changed concurrently"));
    }
    let next = transition_update(actual, change.event).map_err(transition)?;
    let next_sequence: i64 = transaction
        .query_row(
            "SELECT COALESCE(max(sequence), -1) + 1 FROM journal_events WHERE update_id = ?1",
            [change.update_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|error| sqlite(error, "failed to allocate journal event sequence"))?;
    transaction
        .execute(
            "UPDATE update_journals SET state = ?1, updated_at_ns = ?2
             WHERE update_id = ?3 AND state = ?4",
            params![
                next.as_str(),
                change.at_ns,
                change.update_id.as_bytes().as_slice(),
                actual.as_str(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist update transition"))?;
    transaction
        .execute(
            "INSERT INTO journal_events(update_id, sequence, event_kind, details, created_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                change.update_id.as_bytes().as_slice(),
                next_sequence,
                change.event.as_str(),
                change.details,
                change.at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to append update journal event"))?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit update transition"))?;
    Ok(next)
}
