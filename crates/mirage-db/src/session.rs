use std::collections::{BTreeSet, HashMap};

use mirage_types::{
    CapsuleId, GenerationId, MirageError, RepositoryId, SessionEvent, SessionId, SessionState,
    transition_session,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::Database;
use crate::error::{conflict, sqlite, transition};
use crate::lease::LeaseSpec;
use crate::state_codec;
use crate::value::{bounded_text, nonnegative, sqlite_integer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSealedSession {
    pub session_id: SessionId,
    pub repository_id: RepositoryId,
    pub generation_id: GenerationId,
    pub capsule_id: Option<CapsuleId>,
    pub expected_lease_count: u32,
    pub leases: Vec<LeaseSpec>,
    pub started_at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionProcess {
    pub session_id: SessionId,
    pub process_id: u32,
    pub started_at_ns: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionTransition {
    pub session_id: SessionId,
    pub expected: SessionState,
    pub event: SessionEvent,
    pub at_ns: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct SealViolation {
    pub session_id: SessionId,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub(crate) struct FinishSession {
    pub session_id: SessionId,
    pub verified_terminated_processes: Vec<u32>,
    pub ended_at_ns: i64,
}

impl Database {
    pub fn create_session_with_leases(&self, session: NewSealedSession) -> Result<(), MirageError> {
        self.writer.create_session_with_leases(session)
    }

    pub fn record_session_process(&self, process: SessionProcess) -> Result<(), MirageError> {
        self.writer.record_session_process(process)
    }

    pub fn transition_session_state(
        &self,
        session_id: SessionId,
        expected: SessionState,
        event: SessionEvent,
        at_ns: i64,
    ) -> Result<SessionState, MirageError> {
        self.writer.transition_session_state(SessionTransition {
            session_id,
            expected,
            event,
            at_ns,
        })
    }

    pub fn mark_seal_violation(
        &self,
        session_id: SessionId,
        summary: String,
    ) -> Result<u64, MirageError> {
        self.writer.mark_seal_violation(SealViolation {
            session_id,
            summary,
        })
    }

    pub fn finish_session(
        &self,
        session_id: SessionId,
        verified_terminated_processes: Vec<u32>,
        ended_at_ns: i64,
    ) -> Result<(), MirageError> {
        self.writer.finish_session(FinishSession {
            session_id,
            verified_terminated_processes,
            ended_at_ns,
        })
    }

    pub fn load_session_state(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionState>, MirageError> {
        self.reads.with_connection(|connection| {
            let state: Option<String> = connection
                .query_row(
                    "SELECT state FROM sessions WHERE session_id = ?1",
                    [session_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load session state"))?;
            state.map(|value| state_codec::session(&value)).transpose()
        })
    }

    pub fn session_lease_count(&self, session_id: SessionId) -> Result<u64, MirageError> {
        self.reads.with_connection(|connection| {
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM session_leases WHERE session_id = ?1",
                    [session_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .map_err(|error| sqlite(error, "failed to count session leases"))?;
            nonnegative(count, "session lease count")
        })
    }
}

pub(crate) fn create_with_leases(
    connection: &mut Connection,
    session: NewSealedSession,
) -> Result<(), MirageError> {
    let mut unique = HashMap::new();
    for lease in session.leases {
        bounded_text(&lease.reason, 1, 64, "lease reason")?;
        if let Some(existing) = unique.insert(lease.page_hash, lease.reason.clone())
            && existing != lease.reason
        {
            return Err(MirageError::invalid_argument(
                "duplicate page lease has conflicting reasons",
            ));
        }
    }
    if unique.len() != session.expected_lease_count as usize {
        return Err(MirageError::invalid_argument(
            "sealed session lease set does not match expected count",
        ));
    }
    let generation = sqlite_integer(session.generation_id.as_u64(), "session generation")?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin sealed-session transaction"))?;
    let verified: Option<i64> = transaction
        .query_row(
            "SELECT verified FROM generations WHERE repository_id = ?1 AND generation = ?2",
            params![session.repository_id.as_bytes().as_slice(), generation],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to validate session generation"))?;
    if verified != Some(1) {
        return Err(conflict(
            "sealed session requires an existing verified generation",
        ));
    }
    transaction
        .execute(
            "INSERT INTO sessions(
                session_id, repository_id, generation, mode, capsule_id, state,
                expected_lease_count, started_at_ns
             ) VALUES (?1, ?2, ?3, 'sealed', ?4, ?5, ?6, ?7)",
            params![
                session.session_id.as_bytes().as_slice(),
                session.repository_id.as_bytes().as_slice(),
                generation,
                session.capsule_id.map(|value| *value.as_bytes()),
                SessionState::Verifying.as_str(),
                i64::from(session.expected_lease_count),
                session.started_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to create sealed session"))?;
    for (page_hash, reason) in unique {
        transaction
            .execute(
                "INSERT INTO session_leases(session_id, page_hash, reason) VALUES (?1, ?2, ?3)",
                params![
                    session.session_id.as_bytes().as_slice(),
                    page_hash.as_bytes().as_slice(),
                    reason,
                ],
            )
            .map_err(|error| sqlite(error, "failed to create session lease"))?;
    }
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit verifying session"))
}

pub(crate) fn record_process(
    connection: &mut Connection,
    process: SessionProcess,
) -> Result<(), MirageError> {
    if process.process_id == 0 {
        return Err(MirageError::invalid_argument(
            "session process ID must be nonzero",
        ));
    }
    let existing: Option<i64> = connection
        .query_row(
            "SELECT started_at_ns FROM session_processes
             WHERE session_id = ?1 AND process_id = ?2",
            params![
                process.session_id.as_bytes().as_slice(),
                i64::from(process.process_id),
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to query session process"))?;
    if let Some(started) = existing {
        return if started == process.started_at_ns {
            Ok(())
        } else {
            Err(conflict("session process identity changed"))
        };
    }
    connection
        .execute(
            "INSERT INTO session_processes(session_id, process_id, started_at_ns)
             VALUES (?1, ?2, ?3)",
            params![
                process.session_id.as_bytes().as_slice(),
                i64::from(process.process_id),
                process.started_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist session process"))?;
    Ok(())
}

pub(crate) fn transition_state(
    connection: &mut Connection,
    change: SessionTransition,
) -> Result<SessionState, MirageError> {
    let actual = load_state(connection, change.session_id)?;
    if actual != change.expected {
        return Err(conflict("session state changed concurrently"));
    }
    let next = transition_session(actual, change.event).map_err(transition)?;
    let changed = connection
        .execute(
            "UPDATE sessions SET state = ?1
             WHERE session_id = ?2 AND state = ?3",
            params![
                next.as_str(),
                change.session_id.as_bytes().as_slice(),
                actual.as_str(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist session transition"))?;
    if changed != 1 {
        return Err(conflict("session state changed concurrently"));
    }
    let _ = change.at_ns;
    Ok(next)
}

pub(crate) fn mark_violation(
    connection: &mut Connection,
    violation: SealViolation,
) -> Result<u64, MirageError> {
    bounded_text(&violation.summary, 1, 1024, "seal violation summary")?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin seal-violation transaction"))?;
    let actual = load_state(&transaction, violation.session_id)?;
    let next = if actual == SessionState::Violated {
        SessionState::Violated
    } else {
        transition_session(actual, SessionEvent::SealViolated).map_err(transition)?
    };
    transaction
        .execute(
            "UPDATE sessions
             SET state = ?1,
                 seal_violation_count = seal_violation_count + 1,
                 first_violation_summary = COALESCE(first_violation_summary, ?2)
             WHERE session_id = ?3",
            params![
                next.as_str(),
                violation.summary,
                violation.session_id.as_bytes().as_slice(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to persist seal violation"))?;
    let count: i64 = transaction
        .query_row(
            "SELECT seal_violation_count FROM sessions WHERE session_id = ?1",
            [violation.session_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|error| sqlite(error, "failed to load seal violation count"))?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit seal violation"))?;
    nonnegative(count, "seal violation count")
}

pub(crate) fn finish(
    connection: &mut Connection,
    finish: FinishSession,
) -> Result<(), MirageError> {
    let supplied: BTreeSet<u32> = finish.verified_terminated_processes.into_iter().collect();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin session finish transaction"))?;
    let actual = load_state(&transaction, finish.session_id)?;
    if actual == SessionState::Completed {
        return Ok(());
    }
    let mut statement = transaction
        .prepare(
            "SELECT process_id FROM session_processes
             WHERE session_id = ?1 AND ended_at_ns IS NULL ORDER BY process_id",
        )
        .map_err(|error| sqlite(error, "failed to query live session processes"))?;
    let rows = statement
        .query_map([finish.session_id.as_bytes().as_slice()], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| sqlite(error, "failed to read live session processes"))?;
    let mut recorded = BTreeSet::new();
    for row in rows {
        let process_id = row.map_err(|error| sqlite(error, "failed to decode process ID"))?;
        recorded.insert(
            u32::try_from(nonnegative(process_id, "process ID")?)
                .map_err(|_| MirageError::integrity_mismatch("stored process ID exceeds u32"))?,
        );
    }
    drop(statement);
    if recorded != supplied {
        return Err(conflict(
            "verified terminated process set does not match live session ownership",
        ));
    }
    let next = transition_session(actual, SessionEvent::ProcessesExited).map_err(transition)?;
    transaction
        .execute(
            "UPDATE session_processes SET ended_at_ns = ?1
             WHERE session_id = ?2 AND ended_at_ns IS NULL",
            params![finish.ended_at_ns, finish.session_id.as_bytes().as_slice()],
        )
        .map_err(|error| sqlite(error, "failed to close session processes"))?;
    transaction
        .execute(
            "DELETE FROM session_leases WHERE session_id = ?1",
            [finish.session_id.as_bytes().as_slice()],
        )
        .map_err(|error| sqlite(error, "failed to release session leases"))?;
    transaction
        .execute(
            "UPDATE sessions SET state = ?1, ended_at_ns = ?2 WHERE session_id = ?3",
            params![
                next.as_str(),
                finish.ended_at_ns,
                finish.session_id.as_bytes().as_slice(),
            ],
        )
        .map_err(|error| sqlite(error, "failed to finish session"))?;
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit session finish"))
}

fn load_state(connection: &Connection, session_id: SessionId) -> Result<SessionState, MirageError> {
    let state: String = connection
        .query_row(
            "SELECT state FROM sessions WHERE session_id = ?1",
            [session_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to load session state"))?
        .ok_or_else(|| MirageError::invalid_argument("session does not exist"))?;
    state_codec::session(&state)
}
