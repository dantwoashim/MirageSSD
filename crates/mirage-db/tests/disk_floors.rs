use mirage_db::{Database, DiskFloor, DiskFloorRun};
use tempfile::tempdir;

#[test]
fn floor_crud_round_trip() {
    let dir = tempdir().expect("temp directory");
    let db = Database::open(&dir.path().join("control.db")).expect("open");

    assert!(db.disk_floors().expect("list").is_empty());
    assert!(!db.clear_disk_floor("D:\\").expect("clear"));

    db.set_disk_floor(DiskFloor {
        volume_root: "D:\\".into(),
        floor_bytes: 150 << 30,
        hysteresis_bytes: 5 << 30,
        updated_ns: 100,
    })
    .expect("set");
    let floor = db.disk_floor("D:\\").expect("get").expect("present");
    assert_eq!(floor.floor_bytes, 150 << 30);
    assert_eq!(floor.hysteresis_bytes, 5 << 30);

    // upsert replaces
    db.set_disk_floor(DiskFloor {
        volume_root: "D:\\".into(),
        floor_bytes: 200 << 30,
        hysteresis_bytes: 1 << 30,
        updated_ns: 200,
    })
    .expect("update");
    assert_eq!(db.disk_floors().expect("list").len(), 1);
    assert_eq!(
        db.disk_floor("D:\\").expect("get").unwrap().floor_bytes,
        200 << 30
    );

    assert!(db.clear_disk_floor("D:\\").expect("clear"));
    assert!(db.disk_floor("D:\\").expect("get").is_none());
}

#[test]
fn floor_rejects_zero() {
    let dir = tempdir().expect("temp directory");
    let db = Database::open(&dir.path().join("control.db")).expect("open");
    assert!(
        db.set_disk_floor(DiskFloor {
            volume_root: "D:\\".into(),
            floor_bytes: 0,
            hysteresis_bytes: 0,
            updated_ns: 1,
        })
        .is_err()
    );
}

#[test]
fn runs_keep_newest_hundred_per_volume() {
    let dir = tempdir().expect("temp directory");
    let db = Database::open(&dir.path().join("control.db")).expect("open");
    for i in 0..130_i64 {
        db.record_disk_floor_run(DiskFloorRun {
            volume_root: "D:\\".into(),
            at_ns: i,
            target_bytes: 10,
            freed_bytes: i as u64,
            outcome: "ok".into(),
        })
        .expect("record");
        // also record on another volume to confirm per-volume retention
        db.record_disk_floor_run(DiskFloorRun {
            volume_root: "E:\\".into(),
            at_ns: i,
            target_bytes: 1,
            freed_bytes: 0,
            outcome: "insufficient_evictable".into(),
        })
        .expect("record E");
    }
    let raw = rusqlite::Connection::open(dir.path().join("control.db")).expect("raw");
    let count_d: i64 = raw
        .query_row(
            "SELECT count(*) FROM disk_floor_runs WHERE volume_root='D:\\'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    let count_e: i64 = raw
        .query_row(
            "SELECT count(*) FROM disk_floor_runs WHERE volume_root='E:\\'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count_d, 100);
    assert_eq!(count_e, 100);
    let latest = db
        .latest_disk_floor_run("D:\\")
        .expect("latest")
        .expect("present");
    assert_eq!(latest.at_ns, 129);
    assert_eq!(latest.freed_bytes, 129);
    assert_eq!(latest.outcome, "ok");
}
