use std::time::{SystemTime, UNIX_EPOCH};

use mirage_types::{MirageError, MirageErrorKind};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::sqlite;

#[derive(Debug, Clone, Copy)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_core.sql",
        sql: include_str!("../../../migrations/0001_core.sql"),
    },
    Migration {
        version: 2,
        name: "0002_repositories.sql",
        sql: include_str!("../../../migrations/0002_repositories.sql"),
    },
    Migration {
        version: 3,
        name: "0003_backend.sql",
        sql: include_str!("../../../migrations/0003_backend.sql"),
    },
    Migration {
        version: 4,
        name: "0004_sessions.sql",
        sql: include_str!("../../../migrations/0004_sessions.sql"),
    },
    Migration {
        version: 5,
        name: "0005_updates.sql",
        sql: include_str!("../../../migrations/0005_updates.sql"),
    },
    Migration {
        version: 6,
        name: "0006_cache.sql",
        sql: include_str!("../../../migrations/0006_cache.sql"),
    },
    Migration {
        version: 7,
        name: "0007_cache_pins.sql",
        sql: include_str!("../../../migrations/0007_cache_pins.sql"),
    },
    Migration {
        version: 8,
        name: "0008_repository_owners.sql",
        sql: include_str!("../../../migrations/0008_repository_owners.sql"),
    },
    Migration {
        version: 9,
        name: "0009_repository_encryption.sql",
        sql: include_str!("../../../migrations/0009_repository_encryption.sql"),
    },
    Migration {
        version: 10,
        name: "0010_space_leases.sql",
        sql: include_str!("../../../migrations/0010_space_leases.sql"),
    },
];

pub(crate) fn apply_all(connection: &mut Connection) -> Result<(), MirageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version       INTEGER PRIMARY KEY NOT NULL,
                name          TEXT NOT NULL UNIQUE,
                checksum      BLOB NOT NULL CHECK(length(checksum) = 32),
                applied_at_ns INTEGER NOT NULL
             ) STRICT;",
        )
        .map_err(|error| sqlite(error, "failed to create migration ledger"))?;
    validate_history(connection)?;
    for migration in MIGRATIONS {
        let exists: bool = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| sqlite(error, "failed to query migration ledger"))?
            .is_some();
        if exists {
            continue;
        }
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite(error, "failed to begin migration transaction"))?;
        transaction
            .execute_batch(migration.sql)
            .map_err(|error| sqlite(error, "database migration failed"))?;
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, name, checksum, applied_at_ns)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    migration.version,
                    migration.name,
                    checksum(migration.sql).as_slice(),
                    now_ns()?,
                ],
            )
            .map_err(|error| sqlite(error, "failed to record database migration"))?;
        transaction
            .commit()
            .map_err(|error| sqlite(error, "failed to commit database migration"))?;
    }
    validate_history(connection)
}

fn validate_history(connection: &Connection) -> Result<(), MirageError> {
    let mut statement = connection
        .prepare("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
        .map_err(|error| sqlite(error, "failed to prepare migration history check"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|error| sqlite(error, "failed to read migration history"))?;
    let mut expected_next = 1_i64;
    for row in rows {
        let (version, name, observed_checksum) =
            row.map_err(|error| sqlite(error, "failed to decode migration history"))?;
        if version != expected_next {
            return Err(MirageError::new(
                MirageErrorKind::IntegrityMismatch,
                MirageErrorKind::IntegrityMismatch.default_code(),
                "database migration history is non-contiguous",
            ));
        }
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == version)
            .ok_or_else(|| {
                MirageError::new(
                    MirageErrorKind::UnsupportedLayout,
                    MirageErrorKind::UnsupportedLayout.default_code(),
                    "database was migrated by a newer unsupported build",
                )
            })?;
        if name != migration.name || observed_checksum != checksum(migration.sql) {
            return Err(MirageError::integrity_mismatch(
                "historical database migration checksum or name changed",
            ));
        }
        expected_next += 1;
    }
    Ok(())
}

fn checksum(sql: &str) -> [u8; 32] {
    *blake3::hash(sql.as_bytes()).as_bytes()
}

fn now_ns() -> Result<i64, MirageError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            MirageError::internal_invariant("system clock predates Unix epoch").with_source(error)
        })?;
    i64::try_from(duration.as_nanos()).map_err(|_| {
        MirageError::internal_invariant("current timestamp does not fit SQLite integer")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_migration_versions_and_names_are_contiguous() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                migration.version,
                i64::try_from(index + 1).expect("version")
            );
            assert!(migration.name.starts_with(&format!("{:04}_", index + 1)));
            assert!(!migration.sql.trim().is_empty());
        }
    }
}
