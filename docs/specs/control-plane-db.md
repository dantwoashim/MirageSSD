# Control-Plane SQLite Database Specification

## 1. Purpose and Architecture

The MirageSSD control plane provides durable, crash-consistent local persistence for:
- Repository configuration, verified generations, and optimistic generation activation.
- Backend provider accounts, immutable remote object references, and resumable upload sessions.
- Sealed session admissions, process tracking, page-level pin leases, and seal violation records.
- Copy-on-write update journals, overlay page allocations, native snapshots, and audit events.
- Versioned schema migrations with cryptographic BLAKE3 checksums.

### Hot-Path Isolation
In accordance with MirageSSD load-bearing invariants:
- **Zero SQLite on the hot read path**: The local virtual filesystem hit path (file resolution, page lookup, extent translation) reads directly from immutable memory-mapped `.midx` indices and the fixed-slot SSD cache arena.
- SQLite is accessed exclusively during control-plane lifecycle events: mounting/unmounting, repository creation, generation activation, session admission and teardown, upload staging, and update journal transitions.

---

## 2. Concurrency and Ownership Model

```
                     +---------------------------------------+
                     |         Mirage Control Plane          |
                     +---------------------------------------+
                                  |                 |
                    Typed Mutations                 Read Queries
                                  |                 |
                                  v                 v
                    +--------------------+   +-------------------+
                    |   DbWriter Actor   |   |     ReadPool      |
                    | (Bounded mpsc ch)  |   | (Read-Only Conns) |
                    +--------------------+   +-------------------+
                              |                        |
                   Exclusive Writable Conn       Read-Only Pool
                              |                        |
                              v                        v
                    +--------------------------------------------+
                    |             SQLite WAL Database            |
                    +--------------------------------------------+
```

1. **Single Writer Actor (`DbWriter`)**:
   - Exactly one background actor thread owns the exclusive writable SQLite connection.
   - External callers communicate with `DbWriter` exclusively via typed commands over a bounded sync channel (capacity 64).
   - No raw writable connection is ever exposed to other crates or modules.
2. **Read-Only Pool (`ReadPool`)**:
   - Read queries open connections with `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only = ON`.
   - Reads never block or serialize behind read pool mutexes and operate concurrently with WAL checkpoints.
3. **Connection Configuration**:
   - `PRAGMA journal_mode = WAL;`
   - `PRAGMA synchronous = FULL;`
   - `PRAGMA foreign_keys = ON;`
   - `PRAGMA trusted_schema = OFF;`
   - `PRAGMA busy_timeout = 5000;` (5 seconds)
   - `PRAGMA application_id = 0x4D495247;` ("MIRG" magic identifier)

---

## 3. Migration Discipline

Schema migrations are strictly versioned, contiguous, and checksummed.

- **Ledger Table**: `schema_migrations(version INTEGER PRIMARY KEY, name TEXT UNIQUE, checksum BLOB(32), applied_at_ns INTEGER)`
- **Validation**:
  - On startup, historical migrations are loaded and verified in ascending sequence `1..=N`.
  - Every migration's BLAKE3 checksum is computed against the embedded SQL migration artifact and matched against the ledger.
  - If a migration checksum mismatch, missing version gap, or unsupported future version is detected, startup halts with an `IntegrityMismatch` or `UnsupportedLayout` error.
- **Atomic Application**: Each migration file executes inside an isolated transaction; failures roll back completely without partial state.

### Migration Inventory (Week 5 Baseline)
1. `0001_core.sql`: Service metadata table.
2. `0002_repositories.sql`: Repositories and verified generations with deferred foreign-key generation activation.
3. `0003_backend.sql`: Backend accounts, immutable remote object mapping, and resumable upload sessions.
4. `0004_sessions.sql`: Sealed sessions, session process deduplication, session lease sets, and `sessions_sealed_ready_update_guard` trigger.
5. `0005_updates.sql`: Update journals, overlay page tracking, native snapshots, and audit journal events.

---

## 4. Domain Contracts and State Boundaries

### 4.1 Repositories and Generations
- **Generation Invariant**: An active generation must reference a verified generation (`verified = 1`) whose commit hash matches the recorded `active_commit_hash`.
- **Optimistic Activation**: `activate_generation` requires `expected_current: Option<(GenerationId, CommitHash)>`. If another process altered the active generation concurrently, the transaction fails with `RepositoryConflict`.
- **State Machine**: Repository state transitions (`Unmounted`, `Scanning`, `Mounting`, `Mounted`, `UpdateExclusive`, `Error`) are validated by the centralized state machine in `mirage_types`.

### 4.2 Backend and Remote Objects
- **Immutable Identity**: Remote objects are keyed by `(backend_id, object_key)`. Re-upserting an identical object is idempotent (`AlreadyPresent`); attempting to overwrite an object with conflicting hash, size, or provider ID is rejected with `RepositoryConflict`.
- **Monotonic Uploads**: Upload sessions enforce monotonic offset advance (`new_offset >= expected_offset`), optimistic lock checking on `committed_offset`, and upper-bound enforcement (`new_offset <= total_length`).

### 4.3 Sessions and Leases
- **Atomic Admission**: Session creation and lease insertion occur in a single `IMMEDIATE` transaction.
- **Sealing Guard Trigger**: The `sessions_sealed_ready_update_guard` trigger aborts any attempt to set state to `sealed_ready` if the count of rows in `session_leases` does not equal `expected_lease_count`.
- **Seal Violations**: When an unpinned or missing page read occurs in a sealed session, `mark_seal_violation` increments `seal_violation_count`, preserves the first summary string in `first_violation_summary`, and transitions state to `Violated`.
- **Deterministic Teardown**: `finish_session` verifies that the caller-supplied terminated process set matches the live `session_processes` records before releasing leases and transitioning to `Completed`.

### 4.4 Update Journals and Overlays
- **Single Active Update**: Partial indexes enforce that at most one non-terminal (`state NOT IN ('committed', 'rolled_back')`) update journal exists per repository.
- **Journal Event Ordering**: Every update transition appends a strictly sequenced event row to `journal_events` with bounded details (<= 4096 bytes).
- **Clean Teardown**: Foreign key cascades and central state transitions ensure rollback removes or marks overlay allocations without leaving orphan rows.

---

## 5. Recovery and Integrity Verification

- **Startup Recovery Check**: `Database::open` executes `PRAGMA quick_check` and `PRAGMA foreign_key_check` through a read-only connection before starting the writer actor.
- **CLI Database Check (`mirage db check`)**:
  - Invoked via CLI to inspect the database file without taking write locks.
  - Returns structured `DatabaseCheckReport`:
    ```json
    {
      "envelope_version": 1,
      "ok": true,
      "data": {
        "report_version": 1,
        "quick_check_ok": true,
        "integrity_check_ok": true,
        "foreign_key_violation_count": 0,
        "messages": ["ok"]
      }
    }
    ```
