use mirage_fault_inject::process_kill::kill_after_ready;
use std::{path::PathBuf, process::Command, time::Duration};

const CHILD_FLAG: &str = "MIRAGE_CRASH_DB_CHILD";
const DATABASE_PATH: &str = "MIRAGE_CRASH_DB_PATH";
const READY_PATH: &str = "MIRAGE_CRASH_READY_PATH";

#[test]
fn real_process_kill_reopens_a_flushed_control_database() {
    let directory = tempfile::tempdir().expect("directory");
    let database_path = directory.path().join("control.db");
    let ready_path = directory.path().join("ready");
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    child
        .arg("--exact")
        .arg("crash_database_child")
        .arg("--ignored")
        .arg("--nocapture")
        .env(CHILD_FLAG, "1")
        .env(DATABASE_PATH, &database_path)
        .env(READY_PATH, &ready_path);

    let killed = kill_after_ready(&mut child, &ready_path, Duration::from_secs(10))
        .expect("kill at durable boundary");
    assert!(!killed.status.success());

    let database = mirage_db::Database::open(&database_path).expect("automatic WAL recovery");
    let report = mirage_db::check_database(&database_path).expect("integrity report");
    assert!(report.quick_check_ok);
    assert_eq!(report.foreign_key_violation_count, 0);
    drop(database);
}

#[test]
#[ignore = "subprocess entry point"]
fn crash_database_child() {
    if std::env::var_os(CHILD_FLAG).is_none() {
        return;
    }
    let database_path = required_path(DATABASE_PATH);
    let ready_path = required_path(READY_PATH);
    let database = mirage_db::Database::open(&database_path).expect("create database");
    std::fs::write(&ready_path, b"durable").expect("publish readiness marker");
    loop {
        std::hint::black_box(database.reads());
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")))
}
