use mirage_db::{Database, NewRepository, VerifiedGeneration};
use mirage_index::compile_to_bytes;
use mirage_ipc::{Command, PROTOCOL_VERSION, Principal, PrincipalRole, Request, ResponseBody};
use mirage_manifest::{DecodeLimits, decode_manifest_bounded};
use mirage_service::{ControlPlaneHandler, MountControl, RequestHandler};
use mirage_types::{
    CommitHash, GenerationId, ManifestHash, MirageError, RepositoryId, RepositoryState,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[test]
fn status_and_detail_come_from_durable_repository_state() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x31; 16]);
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "Durable fixture".into(),
            local_root: directory.path().join("repository"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    let handler = ControlPlaneHandler::new(database);

    let status = handler.handle(&principal(), request(1, Command::Status));
    let ResponseBody::Json(status) = status else {
        panic!("status response")
    };
    assert_eq!(status["configured"], true);
    assert_eq!(
        status["repositories"][0]["repository_id"],
        repository_id.to_string()
    );
    assert_eq!(status["repositories"][0]["state"], "ready_unmounted");

    let detail = handler.handle(
        &principal(),
        request(2, Command::RepositoryDetail { repository_id }),
    );
    let ResponseBody::Json(detail) = detail else {
        panic!("detail response")
    };
    assert_eq!(detail["display_name"], "Durable fixture");
    assert!(detail["active_generation"].is_null());
}

#[test]
fn missing_and_not_ready_commands_fail_truthfully() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let handler = ControlPlaneHandler::new(database.clone());
    let repository_id = RepositoryId::from_bytes([0x41; 16]);
    let missing = handler.handle(&principal(), request(1, Command::Repair { repository_id }));
    assert!(
        matches!(missing, ResponseBody::Error { ref code, .. } if code == "MIRAGE_INVALID_ARGUMENT")
    );

    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "Not mounted".into(),
            local_root: directory.path().join("repository"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    let unavailable = handler.handle(
        &principal(),
        request(
            2,
            Command::Mount {
                repository_id,
                generation: GenerationId::ZERO,
                drive_letter: None,
            },
        ),
    );
    assert!(
        matches!(unavailable, ResponseBody::Error { ref code, .. } if code == "MIRAGE_INVALID_ARGUMENT")
    );
}

#[test]
fn successful_mount_and_unmount_are_ordered_with_durable_state() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x61; 16]);
    let generation = GenerationId::from_u64(3);
    let commit = CommitHash::from_bytes([7; 32]);
    let root = directory.path().join("mount");
    let manifest = directory.path().join("objects/active.manifest");
    let index = directory.path().join("objects/active.midx");
    std::fs::create_dir_all(manifest.parent().expect("parent")).expect("objects");
    std::fs::create_dir_all(&root).expect("mount root");
    std::fs::write(&manifest, b"manifest fixture").expect("manifest");
    let fixture = decode_manifest_bounded(
        include_bytes!("../../mirage-manifest/tests/fixtures/manifest-v2-complex.cbor"),
        DecodeLimits::default(),
    )
    .expect("manifest fixture");
    std::fs::write(&index, compile_to_bytes(&fixture).expect("compile index")).expect("index");
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "mount fixture".into(),
            local_root: root.clone(),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    database
        .insert_verified_generation(VerifiedGeneration {
            repository_id,
            generation_id: generation,
            commit_hash: commit,
            manifest_hash: ManifestHash::from_bytes([8; 32]),
            manifest_local_path: manifest.clone(),
            mount_index_path: Some(index.clone()),
            created_at_ns: 2,
        })
        .expect("generation");
    database
        .activate_generation(repository_id, generation, commit, None, 3)
        .expect("activate");
    let runtime_root = directory
        .path()
        .join("repositories")
        .join(repository_id.to_string());
    std::fs::create_dir_all(&runtime_root).expect("runtime root");
    std::fs::write(
        runtime_root.join("runtime.json"),
        serde_json::to_vec(&serde_json::json!({
            "format_version": 1,
            "native_root": directory.path().canonicalize().unwrap(),
            "mount_subtree": "mount",
            "import_root": manifest.parent().unwrap(),
            "launcher_relative": "game.exe",
            "arguments": [],
            "version_label": "1",
            "configuration_label": "test",
            "cache_bytes": 1048576,
            "drain_ms": 0,
            "conversion": {
                "backup_root": directory.path().join("backup"),
                "inventory_blake3": "fixture",
                "file_count": 0,
                "total_bytes": 0
            }
        }))
        .unwrap(),
    )
    .expect("runtime config");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = ControlPlaneHandler::with_mount_control(
        database.clone(),
        FakeMountControl {
            calls: Arc::clone(&calls),
        },
    );

    let mounted = handler.handle(
        &principal(),
        request(
            1,
            Command::Mount {
                repository_id,
                generation,
                drive_letter: None,
            },
        ),
    );
    assert!(matches!(mounted, ResponseBody::Json(_)));
    assert_eq!(
        database.load_repository_state(repository_id).unwrap(),
        Some(RepositoryState::ReadyMounted)
    );
    let unmounted = handler.handle(&principal(), request(2, Command::Unmount { repository_id }));
    assert!(matches!(unmounted, ResponseBody::Json(_)));
    assert_eq!(
        database.load_repository_state(repository_id).unwrap(),
        Some(RepositoryState::ReadyUnmounted)
    );
    assert_eq!(*calls.lock().unwrap(), vec!["mount", "unmount"]);
}

#[test]
fn update_begin_status_and_empty_rollback_are_durable() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x71; 16]);
    let generation = GenerationId::from_u64(4);
    let commit = CommitHash::from_bytes([9; 32]);
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "update fixture".into(),
            local_root: directory.path().join("root"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    database
        .insert_verified_generation(VerifiedGeneration {
            repository_id,
            generation_id: generation,
            commit_hash: commit,
            manifest_hash: ManifestHash::from_bytes([10; 32]),
            manifest_local_path: directory.path().join("active.manifest"),
            mount_index_path: None,
            created_at_ns: 2,
        })
        .expect("generation");
    database
        .activate_generation(repository_id, generation, commit, None, 3)
        .expect("activate");
    let handler = ControlPlaneHandler::new(database.clone());

    let begun = handler.handle(
        &principal(),
        request(1, Command::UpdateBegin { repository_id }),
    );
    let ResponseBody::Json(begun) = begun else {
        panic!("begin response")
    };
    assert_eq!(begun["base_generation"], 4);
    assert_eq!(begun["target_generation"], 5);
    assert_eq!(
        database.load_repository_state(repository_id).unwrap(),
        Some(RepositoryState::Updating)
    );

    let status = handler.handle(
        &principal(),
        request(2, Command::UpdateStatus { repository_id }),
    );
    let ResponseBody::Json(status) = status else {
        panic!("status response")
    };
    assert_eq!(status["active"], true);
    assert_eq!(status["state"], "created");

    let rollback = handler.handle(
        &principal(),
        request(3, Command::UpdateRollback { repository_id }),
    );
    let ResponseBody::Json(rollback) = rollback else {
        panic!("rollback response")
    };
    assert_eq!(rollback["state"], "rolled_back");
    assert_eq!(
        database.load_repository_state(repository_id).unwrap(),
        Some(RepositoryState::ReadyUnmounted)
    );
    assert!(
        database
            .load_active_update(repository_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn normal_users_only_see_and_target_their_owned_repositories() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let owned = RepositoryId::from_bytes([0x81; 16]);
    let foreign = RepositoryId::from_bytes([0x82; 16]);
    for (repository_id, owner_sid, name) in [
        (owned, "S-1-5-21-111", "owned"),
        (foreign, "S-1-5-21-222", "foreign"),
    ] {
        database
            .create_repository(NewRepository {
                repository_id,
                display_name: name.into(),
                local_root: directory.path().join(name),
                owner_sid: owner_sid.into(),
                content_encrypted: true,
                initial_state: RepositoryState::ReadyUnmounted,
                created_at_ns: 1,
            })
            .expect("repository");
    }
    let handler = ControlPlaneHandler::new(database);
    let owner = Principal {
        windows_sid: "S-1-5-21-111".into(),
        role: PrincipalRole::ReadOnly,
        authenticated: true,
    };
    let ResponseBody::Json(status) = handler.handle(&owner, request(1, Command::Status)) else {
        panic!("status response")
    };
    assert_eq!(status["repositories"].as_array().unwrap().len(), 1);
    assert_eq!(
        status["repositories"][0]["repository_id"],
        owned.to_string()
    );

    let foreign_detail = handler.handle(
        &owner,
        request(
            2,
            Command::RepositoryDetail {
                repository_id: foreign,
            },
        ),
    );
    assert!(matches!(
        foreign_detail,
        ResponseBody::Error { ref code, .. } if code == "MIRAGE_BACKEND_PERMISSION_DENIED"
    ));

    let owned_cancel = handler.handle(
        &owner,
        request(
            3,
            Command::Cancel {
                repository_id: owned,
                cancellation_id: 7,
            },
        ),
    );
    assert!(!matches!(
        owned_cancel,
        ResponseBody::Error { ref code, .. } if code == "MIRAGE_BACKEND_PERMISSION_DENIED"
    ));
}

struct FakeMountControl {
    calls: Arc<Mutex<Vec<&'static str>>>,
}
impl MountControl for FakeMountControl {
    fn mount(
        &mut self,
        _: RepositoryId,
        _: &Path,
        _: &Path,
        _: &Path,
        _: &str,
        _: (u64, u64),
    ) -> Result<(), MirageError> {
        self.calls.lock().unwrap().push("mount");
        Ok(())
    }
    fn unmount(&mut self, _: RepositoryId) -> Result<(), MirageError> {
        self.calls.lock().unwrap().push("unmount");
        Ok(())
    }
    fn is_running(&mut self, _: RepositoryId) -> Result<bool, MirageError> {
        Ok(false)
    }
}

fn request(request_id: u64, command: Command) -> Request {
    Request {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        cancellation_id: None,
        command,
    }
}

fn principal() -> Principal {
    Principal {
        windows_sid: "S-1-5-18".into(),
        role: PrincipalRole::Service,
        authenticated: true,
    }
}
