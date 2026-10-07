use std::time::{Duration, Instant};

use mirage_db::{Database, Durability};
use mirage_types::RepositoryId;
use tempfile::tempdir;

/// The barrier bumps `durability_barrier.seq` once per call and the writer's
/// synchronous pragma matches the requested mode (NORMAL=1, FULL=2).
#[test]
fn barrier_increments_seq_and_modes_report_sync_levels() {
    let dir = tempdir().expect("temp directory");
    let path = dir.path().join("control.db");
    let group =
        Database::open_with_durability(&path, Durability::Group).expect("open group database");
    assert_eq!(group.writer().writer_synchronous().unwrap(), 1);
    assert_eq!(group.durability_barrier_seq().unwrap(), 0);
    group.durability_barrier().unwrap();
    group.durability_barrier().unwrap();
    assert_eq!(group.durability_barrier_seq().unwrap(), 2);
    drop(group);

    let strict = Database::open(&path).expect("open strict database");
    assert_eq!(strict.writer().writer_synchronous().unwrap(), 2);
    assert_eq!(strict.durability_barrier_seq().unwrap(), 2);
    strict.durability_barrier().unwrap();
    assert_eq!(strict.durability_barrier_seq().unwrap(), 3);
}

/// Group mode: a committed write ages past the barrier window and the writer
/// thread barriers it without any explicit call.
#[test]
fn group_writer_barriers_unbarriered_commits_on_idle() {
    let dir = tempdir().expect("temp directory");
    let db = Database::open_with_durability(&dir.path().join("control.db"), Durability::Group)
        .expect("open group database");
    let volume = RepositoryId::from_bytes([7; 16]);
    db.writer()
        .create_namespace_volume(volume, 1)
        .expect("committed write");
    let deadline = Instant::now() + Duration::from_secs(10);
    while db.durability_barrier_seq().unwrap() == 0 {
        assert!(Instant::now() < deadline, "writer never ran the barrier");
        std::thread::sleep(Duration::from_millis(25));
    }
}
