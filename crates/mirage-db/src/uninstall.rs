use std::path::Path;

use mirage_types::{MirageError, RepositoryState, SessionState, UpdateState};

use crate::{error::sqlite, open, state_codec, value::nonnegative};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallSafetyReport {
    pub blocking_repositories: u64,
    pub active_sessions: u64,
    pub active_updates: u64,
    pub protected_cache_pins: u64,
}

impl UninstallSafetyReport {
    #[must_use]
    pub const fn is_safe(&self) -> bool {
        self.blocking_repositories == 0
            && self.active_sessions == 0
            && self.active_updates == 0
            && self.protected_cache_pins == 0
    }
}

/// Reads the durable control plane without running migrations or changing state.
///
/// A missing database means MirageSSD has no durable state to protect. Any unreadable,
/// malformed, or unknown state fails closed through the returned error.
pub fn check_uninstall_safety(path: &Path) -> Result<UninstallSafetyReport, MirageError> {
    if !path.exists() {
        return Ok(UninstallSafetyReport {
            blocking_repositories: 0,
            active_sessions: 0,
            active_updates: 0,
            protected_cache_pins: 0,
        });
    }

    let connection = open::read_connection(path)?;
    let mut blocking_repositories = 0_u64;
    {
        let mut statement = connection
            .prepare("SELECT state FROM repositories ORDER BY repository_id")
            .map_err(|error| sqlite(error, "failed to inspect repository state for uninstall"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| sqlite(error, "failed to query repository state for uninstall"))?;
        for row in rows {
            let state = row
                .map_err(|error| sqlite(error, "failed to read repository state for uninstall"))?;
            let state = state_codec::repository(&state)?;
            if !matches!(
                state,
                RepositoryState::Uninitialized | RepositoryState::ReadyUnmounted
            ) {
                blocking_repositories = blocking_repositories.saturating_add(1);
            }
        }
    }

    let active_sessions = count_nonterminal_states(
        &connection,
        "sessions",
        &[
            SessionState::Completed.as_str(),
            SessionState::Violated.as_str(),
            SessionState::Aborted.as_str(),
        ],
    )?;
    let active_updates = count_nonterminal_states(
        &connection,
        "update_journals",
        &[
            UpdateState::Committed.as_str(),
            UpdateState::RolledBack.as_str(),
        ],
    )?;
    let protected_cache_pins = count(
        &connection,
        "SELECT count(*) FROM cache_pins WHERE reason IN ('session', 'dirty', 'recovery')",
        "protected cache pins",
    )?;

    Ok(UninstallSafetyReport {
        blocking_repositories,
        active_sessions,
        active_updates,
        protected_cache_pins,
    })
}

fn count_nonterminal_states(
    connection: &rusqlite::Connection,
    table: &str,
    terminal: &[&str],
) -> Result<u64, MirageError> {
    let (query, values) = match terminal {
        [first, second] => (
            format!("SELECT count(*) FROM {table} WHERE state NOT IN (?1, ?2)"),
            vec![*first, *second],
        ),
        [first, second, third] => (
            format!("SELECT count(*) FROM {table} WHERE state NOT IN (?1, ?2, ?3)"),
            vec![*first, *second, *third],
        ),
        _ => {
            return Err(MirageError::internal_invariant(
                "uninstall terminal-state query is invalid",
            ));
        }
    };
    let count: i64 = connection
        .query_row(&query, rusqlite::params_from_iter(values), |row| row.get(0))
        .map_err(|error| sqlite(error, "failed to count active durable state for uninstall"))?;
    nonnegative(count, "active uninstall blocker count")
}

fn count(
    connection: &rusqlite::Connection,
    query: &str,
    label: &'static str,
) -> Result<u64, MirageError> {
    let count: i64 = connection
        .query_row(query, [], |row| row.get(0))
        .map_err(|error| sqlite(error, "failed to count uninstall blockers"))?;
    nonnegative(count, label)
}
