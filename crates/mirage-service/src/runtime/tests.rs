use super::*;
use mirage_backend::{BackendId, ImmutableRevision, ProviderObjectId};
use mirage_pack::{ImportPlan, PlannedFile, import_local};

use super::config::ConversionRecord;
use super::conversion::ConversionIntent;
use super::conversion::ConversionIntentOperation;
use super::conversion::NativeBackupResidency;
use super::conversion::conversion_backup_path;
use super::conversion::conversion_intent_path;
use super::drive::active_generation;
use super::drive::validate_drive_manifest;
fn plan_fixture(cache_bytes: u64) -> (tempfile::TempDir, Database, RepositoryId) {
    let directory = tempfile::tempdir().unwrap();
    let native_root = directory.path().join("game");
    let source = native_root.join("assets");
    let import_root = directory.path().join("import");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(native_root.join("game.exe"), b"native launcher").unwrap();
    let mut bytes = Vec::new();
    for page in 0..84 {
        bytes.extend(std::iter::repeat_n((page % 42) as u8, 65536));
    }
    bytes.extend([0x79; 137]);
    std::fs::write(source.join("content.pak"), bytes).unwrap();
    let repository_id = RepositoryId::from_bytes([0x72; 16]);
    import_local(&ImportPlan {
        repository_id,
        generation_id: GenerationId::ZERO,
        source_root: source,
        files: vec![PlannedFile {
            relative_path: "content.pak".into(),
            class: mirage_manifest::FileClass::VirtualContainer,
        }],
        page_size: 65536,
        pack_target: 8 * 1024 * 1024,
        output_staging_directory: import_root.clone(),
        encryption: None,
    })
    .unwrap();
    let database = Database::open(&directory.path().join("control.db")).unwrap();
    register(
        &database,
        "S-1-5-21-1111111111-2222222222-3333333333-1001",
        RegisterSpec {
            repository_id,
            display_name: "batch materialization".into(),
            native_root,
            mount_subtree: "assets".into(),
            import_root,
            launcher_relative: "game.exe".into(),
            arguments: Vec::new(),
            version_label: "1".into(),
            configuration_label: "default".into(),
            cache_bytes,
        },
    )
    .unwrap();
    let profiles = repository_state_root(&database, repository_id)
        .unwrap()
        .join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    write_json_atomic(&profiles.join("synthetic.profile.json"), &json!({
        "format_version": 1,
        "repository_id": repository_id.to_string(),
        "manifest_hash": active_generation(&database, repository_id).unwrap().manifest_hash.to_string(),
        "label": "1:default",
        "page_observations": [{"file_index": 0, "page_ordinal": 0, "first_touch_delta_us": 0, "class": "demand"}],
        "processes": [{"stable_index": 1, "role": "game", "redacted_image_path": "game.exe"}],
        "dropped_event_count": 0,
    })).unwrap();
    (directory, database, repository_id)
}

fn materialization_fixture() -> (
    tempfile::TempDir,
    Database,
    RepositoryId,
    mirage_types::CapsuleId,
) {
    let (directory, database, repository_id) = plan_fixture(8 * 1024 * 1024);
    let planned = plan(&database, repository_id, true).unwrap();
    let capsule = serde_json::from_value(planned["capsule_id"].clone()).unwrap();
    (directory, database, repository_id, capsule)
}

#[test]
fn plan_reports_verified_scope_for_full_volume_and_profiled_adaptive_for_profiles() {
    let (_directory, database, repository_id) = plan_fixture(8 * 1024 * 1024);
    let full = plan(&database, repository_id, true).unwrap();
    assert_eq!(full["readiness"]["mode"], "verified_scope");
    assert_eq!(full["readiness"]["scope_completeness"], "complete");
    assert_eq!(full["readiness"]["presentation"], "win_fsp_projection");
    let capsule_id = full["capsule_id"].as_str().unwrap();
    let record_path = repository_state_root(&database, repository_id)
        .unwrap()
        .join("capsules")
        .join(format!("{capsule_id}.readiness.json"));
    let record: mirage_types::ReadinessRecord =
        serde_json::from_str(&std::fs::read_to_string(record_path).unwrap()).unwrap();
    record.validate().unwrap();

    let adaptive = plan(&database, repository_id, false).unwrap();
    assert_eq!(adaptive["readiness"]["mode"], "profiled_adaptive");
    assert_eq!(adaptive["readiness"]["scope_completeness"], "empirical");
    assert!(
        adaptive["readiness"]["invalidation_conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|condition| condition == "origin_unavailable")
    );
}

#[test]
fn plan_refuses_a_scope_that_exceeds_the_budget_as_unsupported() {
    let (_directory, database, repository_id) = plan_fixture(1024 * 1024);
    let error = plan(&database, repository_id, true).unwrap_err();
    assert_eq!(error.kind, mirage_types::MirageErrorKind::CacheFull);
    assert!(error.message.contains("no qualified readiness plan"));
}

#[test]
fn batched_materialization_deduplicates_and_resumes_verified_pages() {
    let (_directory, database, repository_id, capsule) = materialization_fixture();
    let first = materialize(&database, repository_id, capsule, None, None).unwrap();
    assert_eq!(first["complete"], true);
    assert_eq!(first["failed_pages"], 0);
    assert_eq!(first["downloaded_pages"], 43);
    assert_eq!(first["downloaded_bytes"], 42 * 65536 + 137);
    assert_eq!(database.load_resident_cache_slots().unwrap().len(), 43);
    let resumed = materialize(&database, repository_id, capsule, None, None).unwrap();
    assert_eq!(resumed["complete"], true);
    assert_eq!(resumed["downloaded_pages"], 0);
    assert_eq!(resumed["already_resident"], resumed["total_pages"]);
}

#[test]
fn batched_materialization_repairs_a_corrupt_resident_on_resume() {
    let (_directory, database, repository_id, capsule) = materialization_fixture();
    materialize(&database, repository_id, capsule, None, None).unwrap();
    let record = database.load_resident_cache_slots().unwrap()[0];
    let (shard, _) = open_cache(&database, 65536, 8 * 1024 * 1024).unwrap();
    shard.write_slot(record.slot_index, &[0xfe]).unwrap();
    shard.flush().unwrap();
    let repaired = materialize(&database, repository_id, capsule, None, None).unwrap();
    assert_eq!(repaired["complete"], true);
    assert_eq!(repaired["downloaded_pages"], 1);
    let (_, resident) = open_cache(&database, 65536, 8 * 1024 * 1024).unwrap();
    assert_eq!(
        verify_page(
            &resident,
            &database,
            record.page_hash.unwrap(),
            IntegrityClass::Clean
        )
        .unwrap(),
        VerifyOutcome::Verified
    );
}

#[test]
fn subtree_conversion_is_dry_run_first_and_byte_reversible() {
    let directory = tempfile::tempdir().unwrap();
    let native_root = directory.path().join("game");
    let subtree = native_root.join("assets");
    let import_root = directory.path().join("import");
    std::fs::create_dir_all(&subtree).unwrap();
    std::fs::write(native_root.join("game.exe"), b"native executable").unwrap();
    std::fs::write(subtree.join("content.pak"), b"immutable asset bytes").unwrap();
    let repository_id = RepositoryId::from_bytes([0x42; 16]);
    import_local(&ImportPlan {
        repository_id,
        generation_id: GenerationId::ZERO,
        source_root: subtree.clone(),
        files: vec![PlannedFile {
            relative_path: "content.pak".into(),
            class: mirage_manifest::FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 256 * 1024,
        output_staging_directory: import_root.clone(),
        encryption: None,
    })
    .unwrap();
    let database = Database::open(&directory.path().join("control.db")).unwrap();
    register(
        &database,
        "S-1-5-21-1111111111-2222222222-3333333333-1001",
        RegisterSpec {
            repository_id,
            display_name: "conversion test".into(),
            native_root: native_root.clone(),
            mount_subtree: "assets".into(),
            import_root,
            launcher_relative: "game.exe".into(),
            arguments: Vec::new(),
            version_label: "1".into(),
            configuration_label: "default".into(),
            cache_bytes: 1024 * 1024,
        },
    )
    .unwrap();

    assert_eq!(
        convert(&database, repository_id, false).unwrap()["dry_run"],
        true
    );
    assert_eq!(
        std::fs::read(subtree.join("content.pak")).unwrap(),
        b"immutable asset bytes"
    );

    let config = load_config(&database, repository_id).unwrap();
    let conversion_source = config.native_root.join(&config.mount_subtree);
    let backup = conversion_backup_path(&config, repository_id).unwrap();
    let inventory = directory_inventory(&conversion_source).unwrap();
    let record = ConversionRecord {
        backup_root: backup.clone(),
        inventory_blake3: inventory.blake3,
        file_count: inventory.file_count,
        total_bytes: inventory.total_bytes,
        backup_residency: NativeBackupResidency::Resident,
    };
    write_json_atomic(
        &conversion_intent_path(&database, repository_id).unwrap(),
        &ConversionIntent {
            format_version: 1,
            repository_id,
            operation: ConversionIntentOperation::Convert,
            source: conversion_source.clone(),
            backup: backup.clone(),
            record,
        },
    )
    .unwrap();
    std::fs::rename(&conversion_source, &backup).unwrap();
    std::fs::create_dir(&conversion_source).unwrap();
    assert_eq!(
        convert(&database, repository_id, false).unwrap()["dry_run"],
        true
    );
    assert_eq!(
        std::fs::read(subtree.join("content.pak")).unwrap(),
        b"immutable asset bytes"
    );

    assert_eq!(
        convert(&database, repository_id, true).unwrap()["converted"],
        true
    );
    assert!(!subtree.exists());
    validate_mount_ready(&database, repository_id, &subtree).unwrap();
    assert!(!subtree.exists());
    assert_eq!(
        restore_native(&database, repository_id, false).unwrap()["dry_run"],
        true
    );
    assert_eq!(
        restore_native(&database, repository_id, true).unwrap()["restored_native"],
        true
    );
    assert_eq!(
        std::fs::read(subtree.join("content.pak")).unwrap(),
        b"immutable asset bytes"
    );
    assert_eq!(
        std::fs::read(native_root.join("game.exe")).unwrap(),
        b"native executable"
    );
}

#[test]
fn drive_manifest_can_only_replace_remote_object_identity() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let import = directory.path().join("import");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("content.pak"), b"immutable asset bytes").unwrap();
    let repository_id = RepositoryId::from_bytes([0x51; 16]);
    import_local(&ImportPlan {
        repository_id,
        generation_id: GenerationId::ZERO,
        source_root: source,
        files: vec![PlannedFile {
            relative_path: "content.pak".into(),
            class: mirage_manifest::FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 256 * 1024,
        output_staging_directory: import.clone(),
        encryption: None,
    })
    .unwrap();
    let local = decode_manifest_bounded(
        &std::fs::read(import.join("base-manifest.cbor")).unwrap(),
        DecodeLimits::default(),
    )
    .unwrap();
    let mut drive = local.clone();
    for (index, location) in drive.remote_locations.iter_mut().enumerate() {
        location.object.backend_id = BackendId::new("drive").unwrap();
        location.object.provider_object_id =
            ProviderObjectId::new(format!("drive-file-{index}")).unwrap();
        location.object.immutable_revision =
            Some(ImmutableRevision::new(format!("revision-{index}")).unwrap());
    }
    validate_drive_manifest(&local, &drive).unwrap();
    drive.remote_locations[0].offset += 1;
    assert!(validate_drive_manifest(&local, &drive).is_err());
}
