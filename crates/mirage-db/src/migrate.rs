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
    Migration {
        version: 11,
        name: "0011_namespace.sql",
        sql: include_str!("../../../migrations/0011_namespace.sql"),
    },
    Migration {
        version: 12,
        name: "0012_namespace_history.sql",
        sql: include_str!("../../../migrations/0012_namespace_history.sql"),
    },
    Migration {
        version: 13,
        name: "0013_allocation.sql",
        sql: include_str!("../../../migrations/0013_allocation.sql"),
    },
    Migration {
        version: 14,
        name: "0014_local_mutations.sql",
        sql: include_str!("../../../migrations/0014_local_mutations.sql"),
    },
    Migration {
        version: 15,
        name: "0015_byte_extents.sql",
        sql: include_str!("../../../migrations/0015_byte_extents.sql"),
    },
    Migration {
        version: 16,
        name: "0016_remote_publication.sql",
        sql: include_str!("../../../migrations/0016_remote_publication.sql"),
    },
    Migration {
        version: 17,
        name: "0017_remote_observation.sql",
        sql: include_str!("../../../migrations/0017_remote_observation.sql"),
    },
    Migration {
        version: 18,
        name: "0018_workspace_lease.sql",
        sql: include_str!("../../../migrations/0018_workspace_lease.sql"),
    },
    Migration {
        version: 19,
        name: "0019_gc.sql",
        sql: include_str!("../../../migrations/0019_gc.sql"),
    },
    Migration {
        version: 20,
        name: "0020_extent_heads.sql",
        sql: include_str!("../../../migrations/0020_extent_heads.sql"),
    },
    Migration {
        version: 21,
        name: "0021_repository_volume_mode.sql",
        sql: include_str!("../../../migrations/0021_repository_volume_mode.sql"),
    },
    Migration {
        version: 22,
        name: "0022_journal_physical_file.sql",
        sql: include_str!("../../../migrations/0022_journal_physical_file.sql"),
    },
    Migration {
        version: 23,
        name: "0023_managed_namespace_seeds.sql",
        sql: include_str!("../../../migrations/0023_managed_namespace_seeds.sql"),
    },
    Migration {
        version: 24,
        name: "0024_payload_remote_objects.sql",
        sql: include_str!("../../../migrations/0024_payload_remote_objects.sql"),
    },
    Migration {
        version: 25,
        name: "0025_disk_floors.sql",
        sql: include_str!("../../../migrations/0025_disk_floors.sql"),
    },
    Migration {
        version: 26,
        name: "0026_namespace_pins.sql",
        sql: include_str!("../../../migrations/0026_namespace_pins.sql"),
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
