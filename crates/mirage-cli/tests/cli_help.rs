use std::fs;
use std::process::{Command, Output};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(arguments)
        .output()
        .expect("run mirage CLI")
}

#[test]
fn root_help_exposes_the_stable_command_surface() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    for command in [
        "version", "config", "db", "repo", "profile", "simulate", "mount", "unmount", "capsule",
        "launch", "update", "capacity",
    ] {
        assert!(help.contains(command), "root help is missing {command}");
    }
    assert!(help.contains("Usage: mirage"));
    assert!(help.contains("[OPTIONS] <COMMAND>"));
}

#[test]
fn every_nested_command_has_help() {
    for arguments in [
        &["config", "--help"][..],
        &["db", "--help"],
        &["repo", "--help"],
        &["profile", "--help"],
        &["capsule", "--help"],
        &["update", "--help"],
        &["capacity", "--help"],
        &["backend", "mount-device", "--help"],
        &["backend", "ingest-device", "--help"],
        &["backend", "backup-device", "--help"],
        &["backend", "restore-device", "--help"],
    ] {
        let output = run(arguments);
        assert!(output.status.success(), "help failed for {arguments:?}");
    }
}

#[test]
fn capacity_cli_rejects_invalid_space_promises_before_contacting_the_service() {
    let repository = "01010101010101010101010101010101";
    let zero = run(&["capacity", "plan", repository, "--bytes", "0"]);
    assert_eq!(zero.status.code(), Some(2));

    let too_short = run(&[
        "capacity",
        "acquire",
        repository,
        "--bytes",
        "1",
        "--lifetime-seconds",
        "29",
    ]);
    assert_eq!(too_short.status.code(), Some(2));
}

#[test]
fn version_json_is_a_versioned_success_envelope() {
    let output = run(&["version", "--json"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("version JSON");
    assert_eq!(value["envelope_version"], 1);
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["schemas"]["service_config"], 1);
    assert!(
        value["data"]["rust_version"]
            .as_str()
            .is_some_and(|version| version.starts_with("rustc "))
    );
    assert!(
        value["data"]["target"]
            .as_str()
            .is_some_and(|target| !target.is_empty())
    );
}

#[test]
fn config_validate_uses_the_typed_day_six_contract() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("mirage.toml");
    let root = if cfg!(windows) {
        r"C:\ProgramData\MirageSSD"
    } else {
        "/var/lib/miragessd"
    };
    fs::write(
        &path,
        format!("format_version = 1\nprogram_data_root = '{root}'\n"),
    )
    .expect("write config");
    let output = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["config", "validate"])
        .arg(&path)
        .arg("--json")
        .output()
        .expect("validate config");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("config JSON");
    assert_eq!(value["data"]["valid"], true);
    assert_eq!(value["data"]["format_version"], 1);
}

#[test]
fn service_owned_profile_command_never_uses_a_placeholder_or_mutates_cwd() {
    let directory = tempfile::tempdir().expect("temp directory");
    let before = fs::read_dir(directory.path()).expect("read before").count();
    let output = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .current_dir(directory.path())
        .args([
            "profile",
            "capture",
            "01010101010101010101010101010101",
            "--json",
        ])
        .output()
        .expect("run profile command");
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(value["ok"], false);
    assert_ne!(value["error"]["code"], "MIRAGE_NOT_IMPLEMENTED");
    let after = fs::read_dir(directory.path()).expect("read after").count();
    assert_eq!(before, after);
}

#[test]
fn repo_import_local_builds_verified_packs_without_touching_source() {
    let source = tempfile::tempdir().expect("source");
    let output_parent = tempfile::tempdir().expect("output parent");
    let asset = source.path().join("world.pak");
    let original = vec![0x4c; 1024 * 1024];
    fs::write(&asset, &original).expect("asset");
    let output = output_parent.path().join("import");
    let result = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["repo", "import", "--local-only", "--source"])
        .arg(source.path())
        .arg("--output")
        .arg(&output)
        .args([
            "--repository-id",
            "01010101010101010101010101010101",
            "--json",
        ])
        .output()
        .expect("local import");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(asset).expect("asset after"), original);
    assert!(output.join("base-manifest.cbor").exists());
    assert!(output.join("import-report.json").exists());
}

#[test]
fn invalid_config_returns_validation_exit_code_and_no_source_details() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("secret.toml");
    fs::write(
        &path,
        "format_version = 1\nprogram_data_root = '/tmp/mirage'\nrefresh_token = 'do-not-leak'\n",
    )
    .expect("write invalid config");
    let output = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["config", "validate"])
        .arg(&path)
        .arg("--json")
        .output()
        .expect("validate invalid config");
    assert_eq!(output.status.code(), Some(4));
    let error = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(error.contains("MIRAGE_INVALID_ARGUMENT"));
    assert!(!error.contains("do-not-leak"));
    assert!(!error.contains("refresh_token"));
}

#[test]
fn repo_scan_writes_a_report_outside_source_without_mutating_source() {
    let directory = tempfile::tempdir().expect("temp directory");
    let source = directory.path().join("Source");
    fs::create_dir(&source).expect("source directory");
    let payload = source.join("world.pak");
    let original = vec![0x5A; 1024 * 1024];
    fs::write(&payload, &original).expect("source payload");
    let report = directory.path().join("inventory.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["repo", "scan"])
        .arg(&source)
        .arg("--report")
        .arg(&report)
        .arg("--json")
        .output()
        .expect("scan repository");
    assert!(
        output.status.success(),
        "repo scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&payload).expect("source payload"), original);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(report).expect("report")).expect("report JSON");
    assert_eq!(report["report_version"], 1);
    assert_eq!(
        report["classifications"][0]["verdict"]["eligible_for_virtualization"],
        true
    );
}

#[test]
fn repo_scan_refuses_to_write_its_report_inside_source() {
    let directory = tempfile::tempdir().expect("temp directory");
    let source = directory.path().join("Source");
    fs::create_dir(&source).expect("source directory");
    let output = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["repo", "scan"])
        .arg(&source)
        .arg("--report")
        .arg(source.join("inventory.json"))
        .arg("--json")
        .output()
        .expect("scan repository");
    assert!(!output.status.success());
    assert!(!source.join("inventory.json").exists());
}

#[test]
fn db_check_reports_valid_database_in_json_and_text_modes() {
    let directory = tempfile::tempdir().expect("temp directory");
    let db_path = directory.path().join("control-plane.db");
    let _db = mirage_db::Database::open(&db_path).expect("open db");

    // JSON mode
    let output_json = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["db", "check"])
        .arg(&db_path)
        .arg("--json")
        .output()
        .expect("run db check json");
    assert!(output_json.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&output_json.stdout).expect("db check JSON");
    assert_eq!(value["envelope_version"], 1);
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["report_version"], 1);
    assert_eq!(value["data"]["quick_check_ok"], true);
    assert_eq!(value["data"]["integrity_check_ok"], true);
    assert_eq!(value["data"]["foreign_key_violation_count"], 0);

    // Text mode
    let output_text = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["db", "check"])
        .arg(&db_path)
        .output()
        .expect("run db check text");
    assert!(output_text.status.success());
    let stdout = String::from_utf8(output_text.stdout).expect("UTF-8 text");
    assert!(stdout.contains("database integrity and foreign keys are valid"));
}

#[test]
fn db_check_returns_failure_on_corrupted_database() {
    let directory = tempfile::tempdir().expect("temp directory");
    let db_path = directory.path().join("corrupted.db");
    let _db = mirage_db::Database::open(&db_path).expect("open db");
    drop(_db);

    // Corrupt database
    let mut bytes = fs::read(&db_path).expect("read db");
    if bytes.len() > 200 {
        for b in &mut bytes[100..200] {
            *b = 0xFF;
        }
    }
    fs::write(&db_path, bytes).expect("write corrupt db");

    let output = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["db", "check"])
        .arg(&db_path)
        .arg("--json")
        .output()
        .expect("run db check corrupt");
    assert!(!output.status.success());
}

#[test]
fn local_repository_cli_round_trips_and_refuses_extract_overwrite() {
    let directory = tempfile::tempdir().expect("temp directory");
    let source = directory.path().join("source");
    let import = directory.path().join("import");
    let backend = directory.path().join("backend");
    let extract = directory.path().join("extract");
    fs::create_dir(&source).expect("source directory");
    let original = (0_u8..=254)
        .cycle()
        .take(2 * 1024 * 1024)
        .collect::<Vec<_>>();
    fs::write(source.join("asset.pak"), &original).expect("source asset");
    let repository = "35353535353535353535353535353535";
    let key_id = "35353535353535353535353535353535";
    let key = "53".repeat(32);

    let imported = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["repo", "import", "--local-only", "--source"])
        .arg(&source)
        .arg("--output")
        .arg(&import)
        .args([
            "--repository-id",
            repository,
            "--page-size",
            "65536",
            "--pack-target",
            "131072",
        ])
        .output()
        .expect("import command");
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );

    let committed = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["repo", "commit-local", "--import"])
        .arg(&import)
        .arg("--backend-root")
        .arg(&backend)
        .args([
            "--repository-id",
            repository,
            "--key-id-hex",
            key_id,
            "--test-key-hex",
            &key,
        ])
        .output()
        .expect("commit command");
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );

    let verify = Command::new(env!("CARGO_BIN_EXE_mirage"))
        .args(["repo", "verify", "--backend-root"])
        .arg(&backend)
        .args([
            "--repository-id",
            repository,
            "--key-id-hex",
            key_id,
            "--test-key-hex",
            &key,
            "--level",
            "metadata",
        ])
        .output()
        .expect("verify command");
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );

    let extract_once = || {
        Command::new(env!("CARGO_BIN_EXE_mirage"))
            .args(["repo", "extract", "--backend-root"])
            .arg(&backend)
            .arg("--destination")
            .arg(&extract)
            .args([
                "--repository-id",
                repository,
                "--key-id-hex",
                key_id,
                "--test-key-hex",
                &key,
            ])
            .arg("--repository-key")
            .arg(import.join("repository-key.dpapi"))
            .output()
            .expect("extract command")
    };
    assert!(extract_once().status.success());
    assert_eq!(
        fs::read(extract.join("asset.pak")).expect("extract"),
        original
    );
    assert!(
        !extract_once().status.success(),
        "second extract must not overwrite"
    );
}
