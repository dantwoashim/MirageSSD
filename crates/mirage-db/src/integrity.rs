use std::path::Path;

use mirage_types::MirageError;
use serde::{Deserialize, Serialize};

use crate::error::sqlite;
use crate::open::read_connection;

const MAX_CHECK_MESSAGES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseCheckReport {
    pub report_version: u32,
    pub quick_check_ok: bool,
    pub integrity_check_ok: bool,
    pub foreign_key_violation_count: u64,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CheckDepth {
    Quick,
    Full,
}

pub fn check_database(path: &Path) -> Result<DatabaseCheckReport, MirageError> {
    check(path, CheckDepth::Full)
}

pub(crate) fn quick_check_database(path: &Path) -> Result<DatabaseCheckReport, MirageError> {
    check(path, CheckDepth::Quick)
}

fn check(path: &Path, depth: CheckDepth) -> Result<DatabaseCheckReport, MirageError> {
    let connection = read_connection(path)?;
    let quick_messages = pragma_messages(&connection, "PRAGMA quick_check")?;
    let integrity_messages = match depth {
        CheckDepth::Quick => quick_messages.clone(),
        CheckDepth::Full => pragma_messages(&connection, "PRAGMA integrity_check")?,
    };
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|error| sqlite(error, "failed to prepare foreign-key check"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(format!(
                "foreign key violation in table {} row {} parent {} constraint {}",
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?
                    .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| sqlite(error, "failed to run foreign-key check"))?;
    let mut violations = Vec::new();
    let mut foreign_key_violation_count = 0_u64;
    for row in rows {
        let message = row.map_err(|error| sqlite(error, "failed to decode foreign-key check"))?;
        foreign_key_violation_count = foreign_key_violation_count.saturating_add(1);
        if violations.len() < MAX_CHECK_MESSAGES {
            violations.push(message);
        }
    }
    let quick_check_ok = is_ok(&quick_messages);
    let integrity_check_ok = is_ok(&integrity_messages);
    let mut messages = quick_messages;
    if matches!(depth, CheckDepth::Full) {
        messages.extend(integrity_messages);
    }
    messages.extend(violations);
    messages.truncate(MAX_CHECK_MESSAGES);
    Ok(DatabaseCheckReport {
        report_version: 1,
        quick_check_ok,
        integrity_check_ok,
        foreign_key_violation_count,
        messages,
    })
}

fn pragma_messages(
    connection: &rusqlite::Connection,
    pragma: &str,
) -> Result<Vec<String>, MirageError> {
    let mut statement = connection
        .prepare(pragma)
        .map_err(|error| sqlite(error, "failed to prepare database integrity check"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| sqlite(error, "failed to run database integrity check"))?;
    let mut messages = Vec::new();
    for row in rows {
        let message = row.map_err(|error| sqlite(error, "failed to decode integrity result"))?;
        if messages.len() < MAX_CHECK_MESSAGES {
            messages.push(message);
        }
    }
    Ok(messages)
}

fn is_ok(messages: &[String]) -> bool {
    messages.len() == 1 && messages[0].eq_ignore_ascii_case("ok")
}
