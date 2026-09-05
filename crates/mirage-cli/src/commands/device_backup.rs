use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use mirage_backend_drive::{
    scope::DRIVE_FILE, session::default_token_path, token_store::TokenStore,
};
use mirage_crypto::dpapi::{self, ProtectionScope};
use mirage_types::MirageError;
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

use crate::output;

use super::device_drive::{require_absolute, resolve_rclone, validate_remote_folder};

const REMOTE_NAME: &str = "miragessd";
const KEY_MAGIC: &[u8] = b"MIRAGESSD-RESTIC-KEY-V1\0";
const KEY_ENTROPY: &[u8] = b"miragessd/restic/pc-backup/v1";
const MAXIMUM_KEY_STORE_BYTES: u64 = 64 * 1024;

pub struct BackupOptions<'a> {
    pub restic: Option<&'a Path>,
    pub rclone: Option<&'a Path>,
    pub sources: &'a [PathBuf],
    pub exclusions: &'a [PathBuf],
    pub remote_folder: &'a str,
    pub token_store: Option<&'a Path>,
    pub key_store: Option<&'a Path>,
    pub state_file: Option<&'a Path>,
    pub log_file: Option<&'a Path>,
    pub json: bool,
}

pub struct RestoreOptions<'a> {
    pub restic: Option<&'a Path>,
    pub rclone: Option<&'a Path>,
    pub snapshot: &'a str,
    pub include: &'a Path,
    pub target: &'a Path,
    pub remote_folder: &'a str,
    pub token_store: Option<&'a Path>,
    pub key_store: Option<&'a Path>,
    pub log_file: Option<&'a Path>,
    pub check_repository: bool,
    pub apply: bool,
    pub json: bool,
}

#[derive(Serialize)]
struct RcloneToken<'a> {
    access_token: &'static str,
    token_type: &'static str,
    refresh_token: &'a str,
    expiry: &'static str,
}

#[derive(Serialize)]
struct BackupState<'a> {
    format_version: u16,
    status: &'a str,
    started_unix_seconds: u64,
    finished_unix_seconds: Option<u64>,
    process_id: u32,
    host: &'a str,
    sources: &'a [PathBuf],
    remote_folder: &'a str,
    exit_code: Option<i32>,
    detail: &'a str,
}

struct TemporaryPasswordFile(PathBuf);

impl Drop for TemporaryPasswordFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn backup(options: BackupOptions<'_>) -> Result<(), MirageError> {
    if options.sources.is_empty() {
        return Err(MirageError::invalid_argument(
            "at least one backup source is required",
        ));
    }
    validate_remote_folder(options.remote_folder)?;
    for source in options.sources {
        require_absolute(source, "backup source")?;
        if !source.exists() {
            return Err(MirageError::invalid_argument(format!(
                "backup source is unavailable: {}",
                source.display()
            )));
        }
    }
    for excluded in options.exclusions {
        require_absolute(excluded, "backup exclusion")?;
    }

    let paths = BackupPaths::resolve(options.key_store, options.state_file, options.log_file)?;
    secure_parent(&paths.key_store)?;
    secure_parent(&paths.state_file)?;
    secure_parent(&paths.log_file)?;
    secure_parent(&paths.password_directory.join("placeholder"))?;
    cleanup_stale_password_files(&paths.password_directory)?;

    let token_path = match options.token_store {
        Some(path) if path.is_absolute() => path.to_owned(),
        Some(_) => {
            return Err(MirageError::invalid_argument(
                "Drive token store path must be absolute",
            ));
        }
        None => default_token_path()?,
    };
    let store = TokenStore::new(token_path.clone());
    let (metadata, mut refresh) = store.load()?;
    if metadata.scopes != [DRIVE_FILE] {
        return Err(MirageError::backend_permission_denied(
            "PC backup requires the exact drive.file OAuth scope",
        ));
    }
    let client_id = metadata.client_id.ok_or_else(|| {
        MirageError::backend_unauthenticated(
            "Drive login must be refresh-verified before PC backup",
        )
    })?;
    let mut client_secret = store.load_client_secret_optional()?;
    let client_secret_text = client_secret
        .as_deref()
        .map(|secret| std::str::from_utf8(secret))
        .transpose()
        .map_err(|_| {
            MirageError::backend_unauthenticated("protected Drive client credential is invalid")
        })?;
    let refresh_text = std::str::from_utf8(&refresh).map_err(|_| {
        MirageError::backend_unauthenticated("stored Drive refresh token is invalid")
    })?;
    let mut token = Zeroizing::new(
        serde_json::to_string(&RcloneToken {
            access_token: "",
            token_type: "Bearer",
            refresh_token: refresh_text,
            expiry: "1970-01-01T00:00:00Z",
        })
        .map_err(|_| MirageError::internal_invariant("Drive token serialization failed"))?,
    );

    let restic = resolve_restic(options.restic)?;
    let rclone = resolve_rclone(options.rclone)?;
    let rclone_directory = rclone.parent().ok_or_else(|| {
        MirageError::invalid_argument("rclone executable has no parent directory")
    })?;
    let mut search_path = vec![rclone_directory.to_owned()];
    if let Some(existing) = std::env::var_os("PATH") {
        search_path.extend(std::env::split_paths(&existing));
    }
    let search_path = std::env::join_paths(search_path)
        .map_err(|_| MirageError::invalid_argument("rclone search path is invalid"))?;
    let mut password = load_or_create_key(&paths.key_store)?;
    let password_file = create_password_file(&paths.password_directory, &password)?;
    let host = backup_host();
    let remote_folder = format!("{}/{host}", options.remote_folder.trim_end_matches('/'));
    validate_remote_folder(&remote_folder)?;
    let repository = format!("rclone:{REMOTE_NAME}:{remote_folder}");
    let started = unix_seconds()?;
    write_state(
        &paths.state_file,
        BackupState {
            format_version: 1,
            status: "running",
            started_unix_seconds: started,
            finished_unix_seconds: None,
            process_id: std::process::id(),
            host: &host,
            sources: options.sources,
            remote_folder: &remote_folder,
            exit_code: None,
            detail: "encrypted Drive snapshot is running",
        },
    )?;

    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log_file)
        .map_err(MirageError::from)?;
    writeln!(
        log,
        "\nMirageSSD PC backup started at unix={started} host={host}"
    )
    .map_err(MirageError::from)?;
    log.flush().map_err(MirageError::from)?;

    let configured = |command: &mut Command| {
        command
            .env("RCLONE_CONFIG_MIRAGESSD_TYPE", "drive")
            .env("RCLONE_CONFIG_MIRAGESSD_CLIENT_ID", &client_id);
        if let Some(client_secret) = client_secret_text {
            command.env("RCLONE_CONFIG_MIRAGESSD_CLIENT_SECRET", client_secret);
        }
        command
            .env("RCLONE_CONFIG_MIRAGESSD_SCOPE", "drive.file")
            .env("RCLONE_CONFIG_MIRAGESSD_TOKEN", token.as_str())
            .env("RCLONE_DRIVE_CHUNK_SIZE", "64Mi")
            .env("RCLONE_DRIVE_PACER_MIN_SLEEP", "10ms")
            .env("RCLONE_DRIVE_PACER_BURST", "200")
            .env("RESTIC_PASSWORD_FILE", &password_file.0)
            .env("PATH", &search_path)
            .args([
                OsString::from("--repo"),
                OsString::from(&repository),
                OsString::from("--option"),
                OsString::from("rclone.program=rclone.exe"),
                OsString::from("--option"),
                OsString::from("rclone.connections=8"),
                OsString::from("--cache-dir"),
                paths.restic_cache.as_os_str().to_owned(),
                OsString::from("--pack-size"),
                OsString::from("64"),
                OsString::from("--retry-lock"),
                OsString::from("2h"),
            ]);
    };

    // Restic only removes locks whose owning process is no longer alive. This
    // makes recovery from a forced shutdown or terminated scheduled task
    // automatic without breaking a live writer's lock.
    let mut unlock = Command::new(&restic);
    configured(&mut unlock);
    let unlock_log = log.try_clone().map_err(MirageError::from)?;
    let unlock_status = unlock
        .arg("unlock")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            unlock_log.try_clone().map_err(MirageError::from)?,
        ))
        .stderr(Stdio::from(unlock_log))
        .status()
        .map_err(MirageError::from)?;
    if !unlock_status.success() && unlock_status.code() != Some(10) {
        scrub(&mut token, &mut refresh, &mut client_secret, &mut password);
        return finish_failure(
            &paths.state_file,
            started,
            &host,
            options.sources,
            &remote_folder,
            unlock_status.code(),
            "stale backup repository locks could not be cleaned",
        );
    }

    let mut probe = Command::new(&restic);
    configured(&mut probe);
    let probe_status = probe
        .args(["snapshots", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(MirageError::from)?;
    if !probe_status.success() {
        // An absent repository reported through rclone can surface as exit 1
        // instead of Restic's native exit 10, so initialization is the
        // authoritative second probe.
        let mut initialize = Command::new(&restic);
        configured(&mut initialize);
        let init_log = log.try_clone().map_err(MirageError::from)?;
        let init_status = initialize
            .args(["init", "--repository-version", "2"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                init_log.try_clone().map_err(MirageError::from)?,
            ))
            .stderr(Stdio::from(init_log))
            .status()
            .map_err(MirageError::from)?;
        if !init_status.success() {
            scrub(&mut token, &mut refresh, &mut client_secret, &mut password);
            return finish_failure(
                &paths.state_file,
                started,
                &host,
                options.sources,
                &remote_folder,
                init_status.code(),
                "backup repository initialization failed",
            );
        }
    }

    let mut command = Command::new(&restic);
    configured(&mut command);
    command.arg("backup");
    #[cfg(windows)]
    if options
        .sources
        .iter()
        .any(|path| is_windows_volume_root(path))
    {
        command.arg("--use-fs-snapshot");
    }
    #[cfg(not(windows))]
    command.arg("--one-file-system");
    command.args([
        "--read-concurrency",
        "16",
        "--no-scan",
        "--skip-if-unchanged",
        "--exclude-caches",
        "--exclude-cloud-files",
        "--compression",
        "fastest",
        "--tag",
        "miragessd-pc-backup",
        "--host",
        &host,
        "--json",
    ]);
    for excluded in default_exclusions(options.sources, options.exclusions, &paths, &token_path) {
        command.arg("--iexclude").arg(excluded);
    }
    command.args(options.sources);
    let backup_log = log.try_clone().map_err(MirageError::from)?;
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            backup_log.try_clone().map_err(MirageError::from)?,
        ))
        .stderr(Stdio::from(backup_log))
        .status()
        .map_err(MirageError::from)?;
    scrub(&mut token, &mut refresh, &mut client_secret, &mut password);

    let exit_code = status.code();
    let (state, detail) = if status.success() {
        ("succeeded", "encrypted Drive snapshot completed")
    } else if exit_code == Some(3) {
        (
            "partial",
            "snapshot completed but some source files were unreadable",
        )
    } else {
        ("failed", "snapshot did not complete")
    };
    write_state(
        &paths.state_file,
        BackupState {
            format_version: 1,
            status: state,
            started_unix_seconds: started,
            finished_unix_seconds: Some(unix_seconds()?),
            process_id: std::process::id(),
            host: &host,
            sources: options.sources,
            remote_folder: &remote_folder,
            exit_code,
            detail,
        },
    )?;
    if !status.success() {
        return Err(MirageError::provider_unavailable(format!(
            "PC backup ended with {status}; inspect {}",
            paths.log_file.display()
        )));
    }
    if options.json {
        output::emit_success(&serde_json::json!({
            "backup_complete": true,
            "host": host,
            "sources": options.sources,
            "remote_folder": remote_folder,
            "encryption": "restic-aead",
            "key_protection": "current-user-dpapi",
            "read_concurrency": 16,
            "single_pass": true,
            "pack_size_mib": 64
        }))
    } else {
        println!("Encrypted whole-PC snapshot completed in {remote_folder}");
        Ok(())
    }
}

pub fn restore(options: RestoreOptions<'_>) -> Result<(), MirageError> {
    validate_snapshot(options.snapshot)?;
    validate_remote_folder(options.remote_folder)?;
    require_absolute(options.target, "restore target")?;
    require_absolute(options.include, "restore include path")?;
    let snapshot_path = restic_snapshot_path(options.include)?;
    let snapshot_selector = format!("{}:{snapshot_path}", options.snapshot);

    let paths = BackupPaths::resolve(options.key_store, None, options.log_file)?;
    if !paths.key_store.is_file() {
        return Err(MirageError::backend_unauthenticated(
            "the protected PC backup key is unavailable",
        ));
    }
    secure_parent(&paths.log_file)?;
    secure_parent(&paths.password_directory.join("placeholder"))?;
    cleanup_stale_password_files(&paths.password_directory)?;

    let token_path = match options.token_store {
        Some(path) if path.is_absolute() => path.to_owned(),
        Some(_) => {
            return Err(MirageError::invalid_argument(
                "Drive token store path must be absolute",
            ));
        }
        None => default_token_path()?,
    };
    let store = TokenStore::new(token_path);
    let (metadata, mut refresh) = store.load()?;
    if metadata.scopes != [DRIVE_FILE] {
        return Err(MirageError::backend_permission_denied(
            "PC restore requires the exact drive.file OAuth scope",
        ));
    }
    let client_id = metadata.client_id.ok_or_else(|| {
        MirageError::backend_unauthenticated(
            "Drive login must be refresh-verified before PC restore",
        )
    })?;
    let mut client_secret = store.load_client_secret_optional()?;
    let client_secret_text = client_secret
        .as_deref()
        .map(|secret| std::str::from_utf8(secret))
        .transpose()
        .map_err(|_| {
            MirageError::backend_unauthenticated("protected Drive client credential is invalid")
        })?;
    let refresh_text = std::str::from_utf8(&refresh).map_err(|_| {
        MirageError::backend_unauthenticated("stored Drive refresh token is invalid")
    })?;
    let mut token = Zeroizing::new(
        serde_json::to_string(&RcloneToken {
            access_token: "",
            token_type: "Bearer",
            refresh_token: refresh_text,
            expiry: "1970-01-01T00:00:00Z",
        })
        .map_err(|_| MirageError::internal_invariant("Drive token serialization failed"))?,
    );

    let restic = resolve_restic(options.restic)?;
    let rclone = resolve_rclone(options.rclone)?;
    let rclone_directory = rclone.parent().ok_or_else(|| {
        MirageError::invalid_argument("rclone executable has no parent directory")
    })?;
    let mut search_path = vec![rclone_directory.to_owned()];
    if let Some(existing) = std::env::var_os("PATH") {
        search_path.extend(std::env::split_paths(&existing));
    }
    let search_path = std::env::join_paths(search_path)
        .map_err(|_| MirageError::invalid_argument("rclone search path is invalid"))?;
    let mut password = load_or_create_key(&paths.key_store)?;
    let password_file = create_password_file(&paths.password_directory, &password)?;
    let host = backup_host();
    let remote_folder = format!("{}/{host}", options.remote_folder.trim_end_matches('/'));
    validate_remote_folder(&remote_folder)?;
    let repository = format!("rclone:{REMOTE_NAME}:{remote_folder}");

    let configured = |command: &mut Command| {
        command
            .env("RCLONE_CONFIG_MIRAGESSD_TYPE", "drive")
            .env("RCLONE_CONFIG_MIRAGESSD_CLIENT_ID", &client_id);
        if let Some(client_secret) = client_secret_text {
            command.env("RCLONE_CONFIG_MIRAGESSD_CLIENT_SECRET", client_secret);
        }
        command
            .env("RCLONE_CONFIG_MIRAGESSD_SCOPE", "drive.file")
            .env("RCLONE_CONFIG_MIRAGESSD_TOKEN", token.as_str())
            .env("RCLONE_DRIVE_CHUNK_SIZE", "64Mi")
            .env("RCLONE_DRIVE_PACER_MIN_SLEEP", "10ms")
            .env("RCLONE_DRIVE_PACER_BURST", "200")
            .env("RESTIC_PASSWORD_FILE", &password_file.0)
            .env("PATH", &search_path)
            .args([
                OsString::from("--repo"),
                OsString::from(&repository),
                OsString::from("--option"),
                OsString::from("rclone.program=rclone.exe"),
                OsString::from("--option"),
                OsString::from("rclone.connections=8"),
                OsString::from("--cache-dir"),
                paths.restic_cache.as_os_str().to_owned(),
                OsString::from("--retry-lock"),
                OsString::from("2h"),
            ]);
    };

    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log_file)
        .map_err(MirageError::from)?;
    writeln!(
        log,
        "\nMirageSSD restore {} at unix={} target={}",
        if options.apply { "started" } else { "planned" },
        unix_seconds()?,
        options.target.display()
    )
    .map_err(MirageError::from)?;

    if options.check_repository {
        let mut check = Command::new(&restic);
        configured(&mut check);
        let check_log = log.try_clone().map_err(MirageError::from)?;
        let check_status = check
            .arg("check")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                check_log.try_clone().map_err(MirageError::from)?,
            ))
            .stderr(Stdio::from(check_log))
            .status()
            .map_err(MirageError::from)?;
        if !check_status.success() {
            scrub(&mut token, &mut refresh, &mut client_secret, &mut password);
            return Err(MirageError::provider_unavailable(format!(
                "PC backup repository check ended with {check_status}; inspect {}",
                paths.log_file.display()
            )));
        }
    }

    let mut command = Command::new(&restic);
    configured(&mut command);
    command.args([
        "restore",
        &snapshot_selector,
        "--target",
        options
            .target
            .to_str()
            .ok_or_else(|| MirageError::invalid_argument("restore target is not valid Unicode"))?,
        "--host",
        &host,
        "--tag",
        "miragessd-pc-backup",
        "--overwrite",
        "if-changed",
    ]);
    if options.snapshot == "latest" {
        command.arg("--path").arg(options.include);
    }
    if options.apply {
        std::fs::create_dir_all(options.target).map_err(MirageError::from)?;
        command.arg("--verify");
    } else {
        command.args(["--dry-run", "--verbose=2"]);
    }
    let restore_log = log.try_clone().map_err(MirageError::from)?;
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            restore_log.try_clone().map_err(MirageError::from)?,
        ))
        .stderr(Stdio::from(restore_log))
        .status()
        .map_err(MirageError::from)?;
    scrub(&mut token, &mut refresh, &mut client_secret, &mut password);
    if !status.success() {
        return Err(MirageError::provider_unavailable(format!(
            "PC restore ended with {status}; inspect {}",
            paths.log_file.display()
        )));
    }
    if options.json {
        output::emit_success(&serde_json::json!({
            "restore_complete": options.apply,
            "dry_run_complete": !options.apply,
            "snapshot": options.snapshot,
            "include": options.include,
            "target": options.target,
            "verified": options.apply,
            "repository_checked": options.check_repository,
            "remote_folder": remote_folder
        }))
    } else {
        println!(
            "MirageSSD restore {} for {}",
            if options.apply {
                "completed and verified"
            } else {
                "plan completed"
            },
            options.target.display()
        );
        Ok(())
    }
}

struct BackupPaths {
    key_store: PathBuf,
    state_file: PathBuf,
    log_file: PathBuf,
    password_directory: PathBuf,
    restic_cache: PathBuf,
}

impl BackupPaths {
    fn resolve(
        key_store: Option<&Path>,
        state_file: Option<&Path>,
        log_file: Option<&Path>,
    ) -> Result<Self, MirageError> {
        let explicit_root = key_store
            .or(state_file)
            .or(log_file)
            .and_then(Path::parent)
            .map(Path::to_owned);
        let root = match explicit_root {
            Some(path) if path.is_absolute() => path,
            Some(_) => {
                return Err(MirageError::invalid_argument(
                    "backup state paths must be absolute",
                ));
            }
            None => {
                let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
                    MirageError::provider_unavailable("LOCALAPPDATA is unavailable")
                })?;
                PathBuf::from(local).join("MirageSSD/backup")
            }
        };
        let choose = |explicit: Option<&Path>,
                      fallback: PathBuf,
                      label: &str|
         -> Result<PathBuf, MirageError> {
            if let Some(path) = explicit {
                require_absolute(path, label)?;
                Ok(path.to_owned())
            } else {
                Ok(fallback)
            }
        };
        Ok(Self {
            key_store: choose(
                key_store,
                root.join("pc-backup-key.dpapi"),
                "backup key store",
            )?,
            state_file: choose(state_file, root.join("state.json"), "backup state file")?,
            log_file: choose(log_file, root.join("backup.log"), "backup log")?,
            password_directory: root.join("runtime"),
            restic_cache: root.join("cache"),
        })
    }
}

fn load_or_create_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, MirageError> {
    if path.exists() {
        let metadata = std::fs::metadata(path).map_err(MirageError::from)?;
        if metadata.len() <= KEY_MAGIC.len() as u64 || metadata.len() > MAXIMUM_KEY_STORE_BYTES {
            return Err(MirageError::manifest_invalid(
                "backup key store has an invalid size",
            ));
        }
        let bytes = std::fs::read(path).map_err(MirageError::from)?;
        if !bytes.starts_with(KEY_MAGIC) {
            return Err(MirageError::manifest_invalid(
                "backup key store format is invalid",
            ));
        }
        return dpapi::unprotect(&bytes[KEY_MAGIC.len()..], KEY_ENTROPY);
    }
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| MirageError::internal_invariant("backup key generation failed"))?;
    let mut key = Zeroizing::new(Vec::with_capacity(64));
    for byte in random {
        key.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    random.zeroize();
    let encrypted = dpapi::protect(&key, KEY_ENTROPY, ProtectionScope::CurrentUser)?;
    let mut record = Vec::with_capacity(KEY_MAGIC.len() + encrypted.len());
    record.extend_from_slice(KEY_MAGIC);
    record.extend_from_slice(&encrypted);
    mirage_crypto::durable_file::write_atomic(path, &record)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(path)?;
    Ok(key)
}

fn create_password_file(
    directory: &Path,
    password: &[u8],
) -> Result<TemporaryPasswordFile, MirageError> {
    std::fs::create_dir_all(directory).map_err(MirageError::from)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(directory)?;
    for _ in 0..16 {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce)
            .map_err(|_| MirageError::internal_invariant("backup runtime nonce failed"))?;
        let suffix = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = directory.join(format!("password-{suffix}.tmp"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut file) => {
                file.write_all(password).map_err(MirageError::from)?;
                file.write_all(b"\n").map_err(MirageError::from)?;
                file.sync_all().map_err(MirageError::from)?;
                drop(file);
                mirage_crypto::file_acl::restrict_to_current_user_system_admins(&path)?;
                return Ok(TemporaryPasswordFile(path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(MirageError::from(error)),
        }
    }
    Err(MirageError::repository_conflict(
        "could not allocate protected backup runtime file",
    ))
}

fn resolve_restic(explicit: Option<&Path>) -> Result<PathBuf, MirageError> {
    if let Some(path) = explicit {
        require_absolute(path, "restic executable")?;
        if path.is_file() {
            return Ok(path.to_owned());
        }
        return Err(MirageError::provider_unavailable(
            "configured restic executable does not exist",
        ));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("restic.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::provider_unavailable("LOCALAPPDATA is unavailable"))?;
    let packages = PathBuf::from(local).join("Microsoft/WinGet/Packages");
    for root in std::fs::read_dir(&packages)
        .map_err(MirageError::from)?
        .flatten()
    {
        if !root
            .file_name()
            .to_string_lossy()
            .starts_with("restic.restic_")
        {
            continue;
        }
        for entry in std::fs::read_dir(root.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("restic_"))
                && path.extension().is_some_and(|extension| extension == "exe")
            {
                return Ok(path);
            }
        }
    }
    Err(MirageError::provider_unavailable(
        "restic executable was not found; install Restic or pass --restic",
    ))
}

fn secure_parent(path: &Path) -> Result<(), MirageError> {
    let parent = path
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("protected path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(MirageError::from)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(parent)
}

fn backup_host() -> String {
    let original = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "windows-pc".to_owned());
    let sanitized = original
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "windows-pc".to_owned()
    } else {
        sanitized
    }
}

fn validate_snapshot(value: &str) -> Result<(), MirageError> {
    if value == "latest"
        || (value.len() >= 4
            && value.len() <= 64
            && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        Err(MirageError::invalid_argument(
            "snapshot must be latest or a hexadecimal Restic snapshot ID",
        ))
    }
}

fn restic_snapshot_path(path: &Path) -> Result<String, MirageError> {
    let value = path.to_str().ok_or_else(|| {
        MirageError::invalid_argument("restore include path is not valid Unicode")
    })?;
    let bytes = value.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return Err(MirageError::invalid_argument(
            "restore include path must be an absolute Windows drive path",
        ));
    }
    let drive = char::from(bytes[0]).to_ascii_uppercase();
    let tail = value[2..].replace('\\', "/");
    let tail = tail.trim_matches('/');
    if tail.is_empty() {
        Ok(format!("/{drive}"))
    } else {
        Ok(format!("/{drive}/{tail}"))
    }
}

#[cfg(windows)]
fn is_windows_volume_root(path: &Path) -> bool {
    let value = path.to_string_lossy();
    let bytes = value.as_bytes();
    bytes.len() == 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn default_exclusions(
    sources: &[PathBuf],
    requested: &[PathBuf],
    paths: &BackupPaths,
    token_store: &Path,
) -> Vec<PathBuf> {
    let mut exclusions = Vec::new();
    for source in sources {
        let root = source
            .components()
            .next()
            .map(|component| component.as_os_str());
        if let Some(root) = root {
            let root = PathBuf::from(format!("{}\\", root.to_string_lossy()));
            exclusions.extend([
                root.join("$Recycle.Bin"),
                root.join("System Volume Information"),
                root.join("pagefile.sys"),
                root.join("hiberfil.sys"),
                root.join("swapfile.sys"),
            ]);
        }
    }
    exclusions.extend([
        PathBuf::from(r"D:\MirageSSD-Cache"),
        paths.password_directory.clone(),
        paths.restic_cache.clone(),
        paths.log_file.clone(),
        paths.state_file.clone(),
        paths.key_store.clone(),
        token_store.to_owned(),
    ]);
    if sources.iter().any(|path| is_whole_volume_source(path)) {
        // These trees are package-manager or compiler outputs, not source
        // data. Apply this profile only to whole-volume PC snapshots; a
        // selected game directory must remain byte-complete even when it has
        // a legitimately required directory named `build` or `node_modules`.
        exclusions.extend([
            PathBuf::from("**/node_modules/**"),
            PathBuf::from("**/.venv/**"),
            PathBuf::from("**/venv/**"),
            PathBuf::from("**/__pycache__/**"),
            PathBuf::from("**/.pytest_cache/**"),
            PathBuf::from("**/.mypy_cache/**"),
            PathBuf::from("**/.ruff_cache/**"),
            PathBuf::from("**/.npm/**"),
            PathBuf::from("**/.pnpm-store/**"),
            PathBuf::from("**/.cargo/registry/**"),
            PathBuf::from("**/.rustup/**"),
            PathBuf::from("**/.next/**"),
            PathBuf::from("**/.turbo/**"),
            PathBuf::from("**/.zephyr-workspace/**"),
            PathBuf::from("**/build/**"),
            PathBuf::from("**/target/debug/**"),
            PathBuf::from("**/target/release/**"),
            PathBuf::from("**/AppData/Local/Temp/**"),
            PathBuf::from("**/Windows/Temp/**"),
        ]);
    }
    exclusions.extend_from_slice(requested);
    exclusions
}

#[cfg(windows)]
fn is_whole_volume_source(path: &Path) -> bool {
    is_windows_volume_root(path)
}

#[cfg(not(windows))]
fn is_whole_volume_source(path: &Path) -> bool {
    path.parent().is_none()
}

fn cleanup_stale_password_files(directory: &Path) -> Result<(), MirageError> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory).map_err(MirageError::from)? {
        let entry = entry.map_err(MirageError::from)?;
        let file_type = entry.file_type().map_err(MirageError::from)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_file() && name.starts_with("password-") && name.ends_with(".tmp") {
            std::fs::remove_file(entry.path()).map_err(MirageError::from)?;
        }
    }
    Ok(())
}

fn write_state(path: &Path, state: BackupState<'_>) -> Result<(), MirageError> {
    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|_| MirageError::internal_invariant("backup state serialization failed"))?;
    mirage_crypto::durable_file::write_atomic(path, &bytes)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(path)
}

fn finish_failure(
    path: &Path,
    started: u64,
    host: &str,
    sources: &[PathBuf],
    remote_folder: &str,
    exit_code: Option<i32>,
    detail: &str,
) -> Result<(), MirageError> {
    write_state(
        path,
        BackupState {
            format_version: 1,
            status: "failed",
            started_unix_seconds: started,
            finished_unix_seconds: Some(unix_seconds()?),
            process_id: std::process::id(),
            host,
            sources,
            remote_folder,
            exit_code,
            detail,
        },
    )?;
    Err(MirageError::provider_unavailable(detail))
}

fn scrub(
    token: &mut Zeroizing<String>,
    refresh: &mut Zeroizing<Vec<u8>>,
    client_secret: &mut Option<Zeroizing<Vec<u8>>>,
    password: &mut Zeroizing<Vec<u8>>,
) {
    token.zeroize();
    refresh.zeroize();
    if let Some(client_secret) = client_secret.as_mut() {
        client_secret.zeroize();
    }
    password.zeroize();
}

fn unix_seconds() -> Result<u64, MirageError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| MirageError::internal_invariant("system clock predates Unix epoch"))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn whole_volume_exclusions_keep_their_drive_prefixes() {
        let paths = BackupPaths {
            key_store: PathBuf::from(r"C:\backup\key.dpapi"),
            state_file: PathBuf::from(r"C:\backup\state.json"),
            log_file: PathBuf::from(r"C:\backup\backup.log"),
            password_directory: PathBuf::from(r"C:\backup\runtime"),
            restic_cache: PathBuf::from(r"C:\backup\cache"),
        };
        let exclusions = default_exclusions(
            &[PathBuf::from(r"C:\"), PathBuf::from(r"D:\")],
            &[PathBuf::from(r"D:\custom-cache")],
            &paths,
            Path::new(r"C:\backup\drive-token.json"),
        );
        for expected in [
            r"C:\pagefile.sys",
            r"C:\System Volume Information",
            r"D:\$Recycle.Bin",
            r"D:\hiberfil.sys",
            r"D:\custom-cache",
            r"C:\backup\key.dpapi",
            r"C:\backup\drive-token.json",
            r"**/node_modules/**",
            r"**/.zephyr-workspace/**",
            r"**/build/**",
            r"**/target/release/**",
        ] {
            assert!(
                exclusions.contains(&PathBuf::from(expected)),
                "missing exclusion {expected}"
            );
        }
    }

    #[test]
    fn selected_directory_backup_never_drops_game_named_build_trees() {
        let paths = BackupPaths {
            key_store: PathBuf::from(r"C:\backup\key.dpapi"),
            state_file: PathBuf::from(r"C:\backup\state.json"),
            log_file: PathBuf::from(r"C:\backup\backup.log"),
            password_directory: PathBuf::from(r"C:\backup\runtime"),
            restic_cache: PathBuf::from(r"C:\backup\cache"),
        };
        let exclusions = default_exclusions(
            &[PathBuf::from(r"D:\Games\Example")],
            &[],
            &paths,
            Path::new(r"C:\backup\drive-token.json"),
        );
        assert!(!exclusions.contains(&PathBuf::from("**/build/**")));
        assert!(!exclusions.contains(&PathBuf::from("**/node_modules/**")));
    }

    #[test]
    fn restore_snapshot_identifier_is_strict() {
        assert!(validate_snapshot("latest").is_ok());
        assert!(validate_snapshot("7ea1b20f").is_ok());
        assert!(validate_snapshot("../../latest").is_err());
        assert!(validate_snapshot("latest:D").is_err());
    }

    #[test]
    fn native_restore_path_maps_to_restic_tree() {
        assert_eq!(
            restic_snapshot_path(Path::new(r"D:\Games\Example")).unwrap(),
            "/D/Games/Example"
        );
        assert_eq!(restic_snapshot_path(Path::new(r"C:\")).unwrap(), "/C");
        assert!(restic_snapshot_path(Path::new(r"relative\path")).is_err());
    }

    #[test]
    fn vss_is_reserved_for_whole_volume_sources() {
        assert!(is_windows_volume_root(Path::new(r"C:\")));
        assert!(is_windows_volume_root(Path::new("d:/")));
        assert!(!is_windows_volume_root(Path::new(
            r"D:\Games\Genshin Impact game"
        )));
    }
}
