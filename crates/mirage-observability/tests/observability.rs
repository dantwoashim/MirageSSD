use mirage_observability::{BoundedJsonLog, Event, Metrics, RegisteredRoots, Secret};
use std::path::PathBuf;

#[test]
fn secrets_and_external_paths_are_redacted() {
    assert_eq!(
        format!("{} {:?}", Secret("canary"), Secret("canary")),
        "[REDACTED] [REDACTED]"
    );
    let roots = RegisteredRoots::new([PathBuf::from(r"C:\Games\Safe")]);
    assert_eq!(
        roots.display(PathBuf::from(r"C:\Users\private.txt").as_path()),
        "[OUTSIDE_REGISTERED_ROOT]"
    );
}

#[test]
fn logging_rotates_and_metrics_are_atomic() {
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("events.log");
    let log = BoundedJsonLog::new(path.clone(), 1).expect("log");
    log.append(&Event {
        name: "read_miss",
        repository: "r1",
        detail: "bounded",
    })
    .expect("first");
    log.append(&Event {
        name: "seal_violation",
        repository: "r1",
        detail: "bounded",
    })
    .expect("second");
    assert!(path.with_extension("previous.log").exists());
    let metrics = Metrics::default();
    metrics.read_miss();
    metrics.seal_violation();
    metrics.backend_retry();
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.read_misses, 1);
    assert_eq!(snapshot.seal_violations, 1);
    assert_eq!(snapshot.backend_retries, 1);
}
