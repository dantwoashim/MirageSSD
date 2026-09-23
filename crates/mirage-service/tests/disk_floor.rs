use mirage_db::Database;
use mirage_ipc::{Command, PROTOCOL_VERSION, Principal, PrincipalRole, Request, ResponseBody};
use mirage_service::{ControlPlaneHandler, RequestHandler};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

fn principal() -> Principal {
    Principal {
        role: PrincipalRole::Administrator,
        windows_sid: "S-1-5-18".into(),
        authenticated: true,
    }
}

fn request(id: u64, command: Command) -> Request {
    Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: id,
        cancellation_id: None,
        command,
    }
}

fn handle(handler: &ControlPlaneHandler, id: u64, command: Command) -> ResponseBody {
    handler.handle(&principal(), request(id, command))
}

fn temp_volume_root(dir: &Path) -> String {
    let canonical = dir.canonicalize().expect("canonical");
    let prefix = canonical
        .components()
        .next()
        .and_then(|c| match c {
            std::path::Component::Prefix(p) => Some(p.as_os_str().to_string_lossy().into_owned()),
            _ => None,
        })
        .expect("drive-letter prefix");
    // Canonicalized paths carry the verbatim `\\?\` prefix; keep `D:` only.
    let cleaned = prefix.replace("\\\\?\\", "").replace('/', "\\");
    format!("{}\\", cleaned.trim_end_matches('\\'))
}

#[test]
fn floor_set_clear_status_round_trip() {
    let dir = tempfile::tempdir().expect("dir");
    let db = Database::open(&dir.path().join("control.db")).expect("db");
    let handler = ControlPlaneHandler::new(db.clone());
    let root = temp_volume_root(dir.path());

    let outcome = handle(
        &handler,
        1,
        Command::DiskFloorSet {
            volume_root: root.clone(),
            floor_bytes: 1 << 20,
            hysteresis_bytes: None,
        },
    );
    let ResponseBody::Json(set) = outcome else {
        panic!("set-floor rejected: {outcome:?}")
    };
    assert_eq!(set["floor_bytes"], 1 << 20);
    // default hysteresis = max(1 GiB, 5% of floor)
    assert_eq!(set["hysteresis_bytes"], 1 << 30);
    assert_eq!(
        db.disk_floor(&root).expect("read").unwrap().floor_bytes,
        1 << 20
    );

    let ResponseBody::Json(status) = handle(&handler, 2, Command::DiskStatus) else {
        panic!("status rejected")
    };
    let floor = &status["floors"][0];
    assert_eq!(floor["volume_root"], root);
    assert_eq!(floor["breached"], false);
    assert!(floor["total_bytes"].as_u64().unwrap() > 0);

    let ResponseBody::Json(clear) = handle(
        &handler,
        3,
        Command::DiskFloorClear {
            volume_root: root.clone(),
        },
    ) else {
        panic!("clear rejected")
    };
    assert_eq!(clear["cleared"], true);
    assert!(db.disk_floor(&root).expect("read").is_none());
}

#[test]
fn floor_at_or_above_total_is_rejected() {
    let dir = tempfile::tempdir().expect("dir");
    let db = Database::open(&dir.path().join("control.db")).expect("db");
    let handler = ControlPlaneHandler::new(db);
    let root = temp_volume_root(dir.path());
    let outcome = handle(
        &handler,
        1,
        Command::DiskFloorSet {
            volume_root: root,
            floor_bytes: u64::MAX / 2,
            hysteresis_bytes: None,
        },
    );
    assert!(matches!(outcome, ResponseBody::Error { .. }));
}

#[test]
fn breached_floor_records_a_run_with_fake_probe() {
    let dir = tempfile::tempdir().expect("dir");
    let db = Database::open(&dir.path().join("control.db")).expect("db");
    let handler = ControlPlaneHandler::new(db.clone());
    let root = temp_volume_root(dir.path());
    db.set_disk_floor(mirage_db::DiskFloor {
        volume_root: root.clone(),
        floor_bytes: 1_000,
        hysteresis_bytes: 100,
        updated_ns: 1,
    })
    .expect("floor");

    // Fake probe reports 400 free → target = 1000+100−400 = 700; no
    // repositories → nothing evictable.
    let probes = AtomicU64::new(0);
    handler
        .enforce_disk_floors_with(|_| {
            probes.fetch_add(1, Ordering::SeqCst);
            Ok(400)
        })
        .expect("enforce");
    let run = db
        .latest_disk_floor_run(&root)
        .expect("run")
        .expect("recorded");
    assert_eq!(run.target_bytes, 700);
    assert_eq!(run.freed_bytes, 0);
    assert_eq!(run.outcome, "insufficient_evictable");

    // Not breached → no run recorded.
    handler
        .enforce_disk_floors_with(|_| Ok(1_000))
        .expect("enforce ok");
    let run2 = db
        .latest_disk_floor_run(&root)
        .expect("run")
        .expect("recorded");
    assert_eq!(run2.at_ns, run.at_ns);
}

#[test]
fn breached_floor_requests_eviction_from_mounted_host() {
    use mirage_service::MountControl;
    use mirage_types::{MirageError, RepositoryEvent, RepositoryId, RepositoryState};
    use std::sync::{Arc, Mutex};

    struct RecordingMounts {
        evictions: Arc<Mutex<Vec<u64>>>,
    }
    impl MountControl for RecordingMounts {
        fn mount(
            &mut self,
            _: RepositoryId,
            _: &Path,
            _: &Path,
            _: &Path,
            _: &str,
            _: Option<&Path>,
            _: (u64, u64),
            _: bool,
            _: Option<(&Path, &Path)>,
            _: Option<u64>,
            _: &str,
            _: Option<u64>,
            _: Option<&Path>,
        ) -> Result<(), MirageError> {
            Ok(())
        }
        fn request_eviction(
            &mut self,
            _: RepositoryId,
            bytes: u64,
        ) -> Result<(u64, u64), MirageError> {
            self.evictions.lock().unwrap().push(bytes);
            Ok((512, 0))
        }
        fn reload_pins(&mut self, _: RepositoryId) -> Result<(), MirageError> {
            Ok(())
        }
        fn send_drive_token(&mut self, _: RepositoryId, _: &str) -> Result<(), MirageError> {
            Ok(())
        }
        fn unmount(&mut self, _: RepositoryId) -> Result<(), MirageError> {
            Ok(())
        }
        fn is_running(&mut self, _: RepositoryId) -> Result<bool, MirageError> {
            Ok(true)
        }
    }

    let dir = tempfile::tempdir().expect("dir");
    let db = Database::open(&dir.path().join("control.db")).expect("db");
    let repository_id = RepositoryId::from_bytes([0x42; 16]);
    db.create_repository(mirage_db::NewRepository {
        repository_id,
        display_name: "mounted".into(),
        local_root: dir.path().join("native"),
        owner_sid: "S-1-5-18".into(),
        content_encrypted: false,
        initial_state: RepositoryState::ReadyUnmounted,
        created_at_ns: 1,
    })
    .expect("repo");
    db.set_repository_state(
        repository_id,
        RepositoryState::ReadyUnmounted,
        RepositoryEvent::MountRequested,
        2,
    )
    .unwrap();
    db.set_repository_state(
        repository_id,
        RepositoryState::Mounting,
        RepositoryEvent::MountSucceeded,
        3,
    )
    .unwrap();

    let evictions = Arc::new(Mutex::new(Vec::new()));
    let handler = ControlPlaneHandler::with_mount_control(
        db.clone(),
        RecordingMounts {
            evictions: Arc::clone(&evictions),
        },
    );
    let root = temp_volume_root(dir.path());
    db.set_disk_floor(mirage_db::DiskFloor {
        volume_root: root.clone(),
        floor_bytes: 1_000,
        hysteresis_bytes: 100,
        updated_ns: 1,
    })
    .expect("floor");

    handler
        .enforce_disk_floors_with(|_| Ok(400))
        .expect("enforce");
    assert_eq!(evictions.lock().unwrap().as_slice(), &[700]);
    let run = db
        .latest_disk_floor_run(&root)
        .expect("run")
        .expect("recorded");
    assert_eq!(run.freed_bytes, 512);
    assert_eq!(run.outcome, "insufficient_evictable");
}
