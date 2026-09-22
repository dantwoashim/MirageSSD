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
                drive_access_token: None,
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
                drive_access_token: None,
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
fn volume_mode_persists_unmounted_and_is_refused_while_mounted() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x91; 16]);
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "volume mode fixture".into(),
            local_root: directory.path().join("repository"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    let handler = ControlPlaneHandler::new(database.clone());

    assert_eq!(
        database.load_repository_volume_mode(repository_id).unwrap(),
        Some(mirage_db::VolumeMode::Legacy)
    );
    let managed = handler.handle(
        &principal(),
        request(
            1,
            Command::RepositorySetVolumeMode {
                repository_id,
                managed: true,
            },
        ),
    );
    assert!(matches!(managed, ResponseBody::Json(_)), "{managed:?}");
    assert_eq!(
        database.load_repository_volume_mode(repository_id).unwrap(),
        Some(mirage_db::VolumeMode::Managed)
    );

    database
        .set_repository_state(
            repository_id,
            RepositoryState::ReadyUnmounted,
            mirage_types::RepositoryEvent::MountRequested,
            2,
        )
        .expect("mount requested");
    database
        .set_repository_state(
            repository_id,
            RepositoryState::Mounting,
            mirage_types::RepositoryEvent::MountSucceeded,
            3,
        )
        .expect("mounted");
    let refused = handler.handle(
        &principal(),
        request(
            2,
            Command::RepositorySetVolumeMode {
                repository_id,
                managed: false,
            },
        ),
    );
    assert!(
        matches!(refused, ResponseBody::Error { ref code, .. } if code == "MIRAGE_REPOSITORY_CONFLICT"),
        "mounted switch must be refused: {refused:?}"
    );
    assert_eq!(
        database.load_repository_volume_mode(repository_id).unwrap(),
        Some(mirage_db::VolumeMode::Managed)
    );
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

#[test]
fn plan_rejects_a_stale_profile_bound_to_another_manifest() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x51; 16]);
    let generation = GenerationId::from_u64(3);
    let commit = CommitHash::from_bytes([7; 32]);
    let manifest = directory.path().join("objects/active.manifest");
    let index = directory.path().join("objects/active.midx");
    std::fs::create_dir_all(manifest.parent().expect("parent")).expect("objects");
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
            display_name: "plan fixture".into(),
            local_root: directory.path().join("repository"),
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
    let profile_root = runtime_root.join("profiles");
    std::fs::create_dir_all(&profile_root).expect("profile root");
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
            "drain_ms": 0
        }))
        .unwrap(),
    )
    .expect("runtime config");
    let stale = mirage_predictor::GameProfile {
        format_version: 1,
        repository_id,
        manifest_hash: ManifestHash::from_bytes([9; 32]),
        label: "1:test".into(),
        page_observations: vec![mirage_predictor::PageObservation {
            file_index: 0,
            page_ordinal: 0,
            first_touch_delta_us: 0,
            class: mirage_predictor::ObservationClass::Demand,
        }],
        processes: vec![],
        dropped_event_count: 0,
    };
    std::fs::write(
        profile_root.join("stale.profile.json"),
        serde_json::to_vec(&stale).unwrap(),
    )
    .expect("stale profile");
    let handler = ControlPlaneHandler::new(database);

    let planned = handler.handle(
        &principal(),
        request(
            1,
            Command::Plan {
                repository_id,
                full_volume: false,
            },
        ),
    );
    assert!(
        matches!(planned, ResponseBody::Error { ref code, ref message }
            if code == "MIRAGE_REPOSITORY_CONFLICT" && message.contains("wrong_manifest")),
        "stale profile must fail closed: {planned:?}"
    );
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
        _: Option<&Path>,
        _: (u64, u64),
        _: bool,
        _: Option<(&Path, &Path)>,
        _: Option<u64>,
        _: &str,
    ) -> Result<(), MirageError> {
        self.calls.lock().unwrap().push("mount");
        Ok(())
    }
    fn request_eviction(&mut self, _: RepositoryId, _: u64) -> Result<(u64, u64), MirageError> {
        self.calls.lock().unwrap().push("request_eviction");
        Ok((0, 0))
    }
    fn reload_pins(&mut self, _: RepositoryId) -> Result<(), MirageError> {
        self.calls.lock().unwrap().push("reload_pins");
        Ok(())
    }
    fn send_drive_token(&mut self, _: RepositoryId, _: &str) -> Result<(), MirageError> {
        self.calls.lock().unwrap().push("send_drive_token");
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

#[test]
fn drive_token_supply_requires_a_mounted_repository() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x62; 16]);
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "token fixture".into(),
            local_root: directory.path().join("repository"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = ControlPlaneHandler::with_mount_control(
        database.clone(),
        FakeMountControl {
            calls: Arc::clone(&calls),
        },
    );
    let response = handler.handle(
        &principal(),
        request(
            1,
            Command::DriveTokenSupply {
                repository_id,
                drive_access_token: mirage_ipc::SensitiveString::new("bearer".to_owned()).unwrap(),
            },
        ),
    );
    let ResponseBody::Error { message, .. } = response else {
        panic!("unmounted token supply must fail: {response:?}")
    };
    assert!(message.contains("not mounted"), "{message}");
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn namespace_pin_unpin_list_round_trip() {
    let directory = tempfile::tempdir().expect("directory");
    let database = Database::open(&directory.path().join("control.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([0x63; 16]);
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "pin fixture".into(),
            local_root: directory.path().join("repository"),
            owner_sid: "S-1-5-18".into(),
            content_encrypted: false,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    let root = database
        .namespace_create_volume(repository_id, 1)
        .expect("namespace volume");
    database
        .namespace_create(
            repository_id,
            root,
            "keep",
            mirage_db::NamespaceNodeKind::Directory,
            2,
        )
        .expect("dir");
    let handler = ControlPlaneHandler::new(database.clone());

    let pinned = handler.handle(
        &principal(),
        request(
            1,
            Command::NamespacePin {
                repository_id,
                path: "keep".into(),
            },
        ),
    );
    let ResponseBody::Json(pinned) = pinned else {
        panic!("pin response: {pinned:?}")
    };
    assert_eq!(pinned["pinned"], true);

    // Case-folded resolution matches the namespace lookup rules.
    let listed = handler.handle(
        &principal(),
        request(2, Command::NamespacePins { repository_id }),
    );
    let ResponseBody::Json(listed) = listed else {
        panic!("pins response: {listed:?}")
    };
    assert_eq!(listed["pins"].as_array().unwrap().len(), 1);
    assert_eq!(listed["pins"][0]["path"], "/keep");

    let missing = handler.handle(
        &principal(),
        request(
            3,
            Command::NamespacePin {
                repository_id,
                path: "gone".into(),
            },
        ),
    );
    assert!(
        matches!(missing, ResponseBody::Error { ref code, .. } if code == "MIRAGE_INVALID_ARGUMENT"),
        "{missing:?}"
    );

    let unpinned = handler.handle(
        &principal(),
        request(
            4,
            Command::NamespaceUnpin {
                repository_id,
                path: "/KEEP".into(),
            },
        ),
    );
    let ResponseBody::Json(unpinned) = unpinned else {
        panic!("unpin response: {unpinned:?}")
    };
    assert_eq!(unpinned["pinned"], false);
    assert!(database.namespace_pins(repository_id).unwrap().is_empty());

    // Unpinning an unpinned path is an error, not a silent success.
    let again = handler.handle(
        &principal(),
        request(
            5,
            Command::NamespaceUnpin {
                repository_id,
                path: "keep".into(),
            },
        ),
    );
    assert!(matches!(again, ResponseBody::Error { .. }), "{again:?}");
}
