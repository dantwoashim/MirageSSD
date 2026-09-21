use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use mirage_backend_drive::{
    scope::DRIVE_FILE, session::default_token_path, token_store::TokenStore,
};
use mirage_types::MirageError;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::output;

const REMOTE_NAME: &str = "miragessd";
const READY_TIMEOUT: Duration = Duration::from_secs(120);

pub struct MountOptions<'a> {
    pub rclone: Option<&'a Path>,
    pub drive_letter: &'a str,
    pub remote_folder: &'a str,
    pub cache_dir: &'a Path,
    pub log_file: &'a Path,
    pub token_store: Option<&'a Path>,
    pub cache_max_size: &'a str,
    pub cache_min_free_space: &'a str,
    pub cache_max_age: &'a str,
    pub read_ahead: &'a str,
    pub read_chunk_size: &'a str,
    pub read_chunk_streams: u8,
    pub json: bool,
}

pub struct IngestOptions<'a> {
    pub rclone: Option<&'a Path>,
    pub source: &'a Path,
    pub remote_folder: &'a str,
    pub token_store: Option<&'a Path>,
    pub json: bool,
}

#[derive(Serialize)]
struct RcloneToken<'a> {
    access_token: &'static str,
    token_type: &'static str,
    refresh_token: &'a str,
    expiry: &'static str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoogleClientFile {
    installed: InstalledClient,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledClient {
    client_id: String,
    project_id: String,
    auth_uri: String,
    token_uri: String,
    auth_provider_x509_cert_url: String,
    client_secret: String,
    redirect_uris: Vec<String>,
}

pub fn mount(options: MountOptions<'_>) -> Result<(), MirageError> {
    let mount_point = validate_drive_letter(options.drive_letter)?;
    validate_remote_folder(options.remote_folder)?;
    require_absolute(options.cache_dir, "cache directory")?;
    require_absolute(options.log_file, "log file")?;
    require_size(options.cache_max_size, "cache maximum size")?;
    require_size(options.cache_min_free_space, "cache minimum free space")?;
    require_size(options.read_ahead, "read ahead")?;
    require_size(options.read_chunk_size, "read chunk size")?;

    if mount_point.exists() {
        return Err(MirageError::repository_conflict(format!(
            "{} is already mounted",
            mount_point.display()
        )));
    }
    std::fs::create_dir_all(options.cache_dir).map_err(MirageError::from)?;
    let log_parent = options
        .log_file
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("log file has no parent directory"))?;
    std::fs::create_dir_all(log_parent).map_err(MirageError::from)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(options.cache_dir)?;
    mirage_crypto::file_acl::restrict_to_current_user_system_admins(log_parent)?;

    let rclone = resolve_rclone(options.rclone)?;
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
            "device drive requires the exact drive.file OAuth scope",
        ));
    }
    let bound_client_id = metadata.client_id.as_deref().ok_or_else(|| {
        MirageError::backend_unauthenticated(
            "Drive login must be refresh-verified once before mounting the device drive",
        )
    })?;
    let client_id = bound_client_id.to_owned();
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

    let remote = format!("{REMOTE_NAME}:{}", options.remote_folder);
    let created = configured_command(&rclone, &client_id, client_secret_text, &token)
        .args(["mkdir", &remote, "--config", "NUL", "--log-file"])
        .arg(options.log_file)
        .args(["--log-level", "INFO"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(MirageError::from)?;
    if !created.success() {
        token.zeroize();
        refresh.zeroize();
        zeroize_optional(&mut client_secret);
        return Err(MirageError::backend_unavailable(format!(
            "Drive-backed device folder could not be opened ({created}); cause is unknown. Check the provider log before changing credentials"
        )));
    }

    let attribute_store = resolve_attribute_store(&metadata.account_id, options.remote_folder)?;
    let mut child = configured_command(&rclone, &client_id, client_secret_text, &token)
        .env("MIRAGESSD_ATTRIBUTES_FILE", &attribute_store.path)
        .env("MIRAGESSD_ATTRIBUTES_BINDING", &attribute_store.binding)
        .args(mount_arguments(&options, &mount_point, &remote))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(MirageError::from)?;
    token.zeroize();
    refresh.zeroize();
    zeroize_optional(&mut client_secret);

    wait_until_ready(&mount_point, &mut child)?;
    if options.json {
        output::emit_success(&serde_json::json!({
            "mounted": true,
            "mount_point": mount_point,
            "volume_name": "MirageSSD",
            "write_back": "5s-local-first",
            "cache_mode": "full",
            "cache_max_age": options.cache_max_age,
            "cold_read_streams": options.read_chunk_streams,
            "read_ahead": options.read_ahead,
            "read_chunk_size": options.read_chunk_size
        }))?;
    } else {
        println!(
            "MirageSSD writable device is ready at {}",
            mount_point.display()
        );
    }

    let status = child.wait().map_err(MirageError::from)?;
    if status.success() {
        Ok(())
    } else {
        Err(MirageError::provider_unavailable(format!(
            "MirageSSD device mount stopped with {status}"
        )))
    }
}

pub fn ingest(options: IngestOptions<'_>) -> Result<(), MirageError> {
    require_absolute(options.source, "ingest source")?;
    validate_remote_folder(options.remote_folder)?;
    let source_metadata = std::fs::metadata(options.source).map_err(|error| {
        MirageError::invalid_argument(format!("ingest source is unavailable: {error}"))
    })?;
    if !source_metadata.is_file() && !source_metadata.is_dir() {
        return Err(MirageError::invalid_argument(
            "ingest source must be a regular file or directory",
        ));
    }
    let source_name = options
        .source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MirageError::invalid_argument("ingest source has no valid file name"))?;
    validate_remote_folder(source_name)?;

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
            "device ingest requires the exact drive.file OAuth scope",
        ));
    }
    let client_id = metadata.client_id.ok_or_else(|| {
        MirageError::backend_unauthenticated(
            "Drive login must be refresh-verified once before device ingest",
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

    let rclone = resolve_rclone(options.rclone)?;
    let destination = format!("{REMOTE_NAME}:{}/{source_name}", options.remote_folder);
    let operation = if source_metadata.is_file() {
        "copyto"
    } else {
        "copy"
    };
    let mut command = configured_command(&rclone, &client_id, client_secret_text, &token);
    command.arg(operation).arg(options.source).arg(&destination);
    if source_metadata.is_dir() {
        command.arg("--create-empty-src-dirs");
    }
    let status = command
        .args([
            "--config",
            "NUL",
            "--fast-list",
            "--checksum",
            "--transfers",
            "16",
            "--checkers",
            "32",
            "--drive-chunk-size",
            "64Mi",
            "--drive-pacer-min-sleep",
            "10ms",
            "--drive-pacer-burst",
            "200",
            "--stats",
            "5s",
            "--stats-one-line",
            "--log-level",
            "NOTICE",
        ])
        .stdin(Stdio::null())
        .status()
        .map_err(MirageError::from)?;
    token.zeroize();
    refresh.zeroize();
    zeroize_optional(&mut client_secret);
    if !status.success() {
        return Err(MirageError::provider_unavailable(format!(
            "device ingest failed with {status}"
        )));
    }

    if options.json {
        output::emit_success(&serde_json::json!({
            "ingested": true,
            "provider_checksum_reconciled": true,
            "source": options.source,
            "destination": format!("{}/{}", options.remote_folder, source_name)
        }))
    } else {
        println!(
            "Imported {} into {}/{}",
            options.source.display(),
            options.remote_folder,
            source_name
        );
        Ok(())
    }
}

/// The complete provider argument list for the writable mount. This is the
/// authoritative record compared against observed child-process arguments in
/// tests and against `docs/capability-status.md`.
fn mount_arguments(options: &MountOptions<'_>, mount_point: &Path, remote: &str) -> Vec<OsString> {
    vec![
        OsString::from("mount"),
        OsString::from(remote),
        mount_point.as_os_str().to_owned(),
        OsString::from("--config"),
        OsString::from("NUL"),
        OsString::from("--volname"),
        OsString::from("MirageSSD"),
        OsString::from("--vfs-cache-mode"),
        OsString::from("full"),
        OsString::from("--cache-dir"),
        options.cache_dir.as_os_str().to_owned(),
        OsString::from("--vfs-cache-max-size"),
        OsString::from(options.cache_max_size),
        OsString::from("--vfs-cache-min-free-space"),
        OsString::from(options.cache_min_free_space),
        OsString::from("--vfs-cache-max-age"),
        OsString::from(options.cache_max_age),
        OsString::from("--vfs-write-back"),
        OsString::from("5s"),
        OsString::from("--vfs-cache-poll-interval"),
        OsString::from("15s"),
        OsString::from("--dir-cache-time"),
        OsString::from("72h"),
        OsString::from("--vfs-refresh"),
        OsString::from("--poll-interval"),
        OsString::from("1m"),
        OsString::from("--vfs-fast-fingerprint"),
        OsString::from("--vfs-links"),
        OsString::from("--buffer-size"),
        OsString::from("16Mi"),
        OsString::from("--vfs-read-ahead"),
        OsString::from(options.read_ahead),
        OsString::from("--vfs-read-chunk-size"),
        OsString::from(options.read_chunk_size),
        OsString::from("--vfs-read-chunk-streams"),
        OsString::from(options.read_chunk_streams.to_string()),
        OsString::from("--drive-chunk-size"),
        OsString::from("64Mi"),
        OsString::from("--drive-pacer-min-sleep"),
        OsString::from("10ms"),
        OsString::from("--drive-pacer-burst"),
        OsString::from("200"),
        OsString::from("--transfers"),
        OsString::from("8"),
        OsString::from("--checkers"),
        OsString::from("16"),
        OsString::from("--vfs-case-insensitive"),
        OsString::from("--attr-timeout"),
        OsString::from("1s"),
        OsString::from("--log-file"),
        options.log_file.as_os_str().to_owned(),
        OsString::from("--log-level"),
        OsString::from("NOTICE"),
    ]
}

struct ClientCredentials {
    client_id: String,
    client_secret: Zeroizing<Vec<u8>>,
}

pub fn authorize(
    client_credentials: &Path,
    token_store: Option<&Path>,
    json: bool,
) -> Result<(), MirageError> {
    require_absolute(client_credentials, "Drive client credential file")?;
    let token_path = match token_store {
        Some(path) if path.is_absolute() => path.to_owned(),
        Some(_) => {
            return Err(MirageError::invalid_argument(
                "Drive token store path must be absolute",
            ));
        }
        None => default_token_path()?,
    };
    let store = TokenStore::new(token_path);
    let metadata = store.metadata()?;
    let bound_client_id = metadata.client_id.as_deref().ok_or_else(|| {
        MirageError::backend_unauthenticated(
            "Drive login must be refresh-verified once before authorizing the device drive",
        )
    })?;
    let mut credentials = load_client_credentials(client_credentials)?;
    if credentials.client_id != bound_client_id {
        return Err(MirageError::backend_unauthenticated(
            "Drive credential file does not match the protected login",
        ));
    }
    store.bind_client_secret(&credentials.client_id, &mut credentials.client_secret)?;
    if json {
        output::emit_success(&serde_json::json!({
            "device_mount_authorized": true,
            "credential_protection": "current-user-dpapi"
        }))
    } else {
        println!("Persistent device mounting authorized with current-user DPAPI protection");
        Ok(())
    }
}

fn load_client_credentials(path: &Path) -> Result<ClientCredentials, MirageError> {
    let metadata = std::fs::metadata(path).map_err(MirageError::from)?;
    if metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        return Err(MirageError::invalid_argument(
            "Drive client credential file size is invalid",
        ));
    }
    let file: GoogleClientFile =
        serde_json::from_slice(&std::fs::read(path).map_err(MirageError::from)?).map_err(|_| {
            MirageError::invalid_argument("Drive client credential file is invalid")
        })?;
    let installed = file.installed;
    let _validated_non_secret_fields = (
        installed.project_id,
        installed.auth_uri,
        installed.token_uri,
        installed.auth_provider_x509_cert_url,
        installed.redirect_uris,
    );
    if !installed.client_id.ends_with(".apps.googleusercontent.com")
        || installed.client_secret.is_empty()
        || installed.client_secret.len() > 512
        || installed.client_secret.chars().any(char::is_control)
    {
        return Err(MirageError::invalid_argument(
            "Drive Desktop OAuth credential is invalid",
        ));
    }
    Ok(ClientCredentials {
        client_id: installed.client_id,
        client_secret: Zeroizing::new(installed.client_secret.into_bytes()),
    })
}

fn configured_command(
    rclone: &Path,
    client_id: &str,
    client_secret: Option<&str>,
    token: &str,
) -> Command {
    let mut command = Command::new(rclone);
    // Never inherit an RC listener, browser proxy, or unrelated account settings.
    // This process receives only the explicit MirageSSD provider configuration.
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_")
        {
            command.env_remove(key);
        }
    }
    command
        .env("RCLONE_CONFIG_MIRAGESSD_TYPE", "drive")
        .env("RCLONE_CONFIG_MIRAGESSD_CLIENT_ID", client_id);
    if let Some(client_secret) = client_secret {
        command.env("RCLONE_CONFIG_MIRAGESSD_CLIENT_SECRET", client_secret);
    }
    command
        .env("RCLONE_CONFIG_MIRAGESSD_SCOPE", "drive.file")
        .env("RCLONE_CONFIG_MIRAGESSD_TOKEN", token);
    command
}

fn zeroize_optional(secret: &mut Option<Zeroizing<Vec<u8>>>) {
    if let Some(secret) = secret.as_mut() {
        secret.zeroize();
    }
}

fn wait_until_ready(mount_point: &Path, child: &mut Child) -> Result<(), MirageError> {
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if mount_point.is_dir() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(MirageError::from)? {
            return Err(MirageError::provider_unavailable(format!(
                "device mount exited before becoming ready ({status})"
            )));
        }
        thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    Err(MirageError::deadline_exceeded(
        "device mount did not become ready within 120 seconds",
    ))
}

fn validate_drive_letter(value: &str) -> Result<PathBuf, MirageError> {
    let trimmed = value.trim_end_matches(':');
    if trimmed.len() != 1
        || !trimmed
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
    {
        return Err(MirageError::invalid_argument(
            "drive letter must be one ASCII letter",
        ));
    }
    let letter = trimmed.to_ascii_uppercase();
    if matches!(letter.as_str(), "A" | "B" | "C") {
        return Err(MirageError::invalid_argument(
            "drive letter A, B, or C is not allowed",
        ));
    }
    Ok(PathBuf::from(format!("{letter}:\\")))
}

pub(super) fn validate_remote_folder(value: &str) -> Result<(), MirageError> {
    if value.is_empty()
        || value.len() > 180
        || value.starts_with(['/', '\\'])
        || value.contains(':')
        || value.chars().any(char::is_control)
        || value
            .split(['/', '\\'])
            .any(|part| part == "." || part == "..")
    {
        return Err(MirageError::invalid_argument(
            "remote folder must be a safe relative Drive path",
        ));
    }
    Ok(())
}

pub(super) fn require_absolute(path: &Path, name: &str) -> Result<(), MirageError> {
    if !path.is_absolute() {
        return Err(MirageError::invalid_argument(format!(
            "{name} must be absolute"
        )));
    }
    Ok(())
}

fn require_size(value: &str, name: &str) -> Result<(), MirageError> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    {
        return Err(MirageError::invalid_argument(format!(
            "{name} has an invalid size"
        )));
    }
    Ok(())
}

pub(super) fn resolve_rclone(explicit: Option<&Path>) -> Result<PathBuf, MirageError> {
    if let Some(path) = explicit {
        require_absolute(path, "rclone executable")?;
        if path.is_file() {
            return Ok(path.to_owned());
        }
        return Err(MirageError::provider_unavailable(
            "configured rclone executable does not exist",
        ));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("rclone.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::provider_unavailable("LOCALAPPDATA is unavailable"))?;
    let packages = PathBuf::from(local).join("Microsoft/WinGet/Packages");
    let roots = std::fs::read_dir(&packages).map_err(MirageError::from)?;
    for root in roots.flatten() {
        if !root
            .file_name()
            .to_string_lossy()
            .starts_with("Rclone.Rclone_")
        {
            continue;
        }
        for version in std::fs::read_dir(root.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let candidate = version.path().join("rclone.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(MirageError::provider_unavailable(
        "rclone is not installed; install Rclone.Rclone with Winget",
    ))
}

/// A mounted attribute journal whose on-disk name and embedded binding are
/// derived from the Drive account plus the remote root, so a different account
/// mounting the same folder name can never inherit its attribute state.
pub struct AttributeStoreSelection {
    pub path: PathBuf,
    pub binding: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AttributeBindingRegistry {
    #[serde(default)]
    format_version: u32,
    #[serde(default)]
    stores: BTreeMap<String, AttributeBindingRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttributeBindingRecord {
    account_id: String,
    remote_folder: String,
}

fn attribute_store_root() -> Result<PathBuf, MirageError> {
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::provider_unavailable("LOCALAPPDATA is unavailable"))?;
    Ok(PathBuf::from(local).join("MirageSSD/device"))
}

fn attribute_binding(account_id: &str, remote_folder: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"miragessd/device/attribute-store/v2\0");
    hasher.update(account_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(remote_folder.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn bound_store_name(binding: &str) -> String {
    format!("windows-attributes-{}.json", &binding[..16])
}

fn legacy_store_name(remote_folder: &str) -> String {
    let remote_id = blake3::hash(remote_folder.as_bytes()).to_hex();
    format!("windows-attributes-{}.json", &remote_id[..16])
}

fn load_binding_registry(root: &Path) -> AttributeBindingRegistry {
    let path = root.join("attribute-bindings.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return AttributeBindingRegistry::default();
    };
    // An unreadable registry disables legacy migration but must not block the
    // mount: bound store names already encode their account and root.
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_binding_registry(
    root: &Path,
    registry: &AttributeBindingRegistry,
) -> Result<(), MirageError> {
    std::fs::create_dir_all(root).map_err(MirageError::from)?;
    let bytes = serde_json::to_vec(registry)
        .map_err(|_| MirageError::internal_invariant("binding registry serialization failed"))?;
    mirage_crypto::durable_file::write_atomic(&root.join("attribute-bindings.json"), &bytes)
}

/// Resolve the attribute journal for this mount. Stores written before
/// account binding existed carry no verifiable owner; they migrate only when
/// the binding registry already records them for this exact account and root.
/// Unverifiable legacy files are left untouched and a fresh bound store is
/// started instead of attaching another identity's attribute history.
fn resolve_attribute_store_in(
    root: &Path,
    account_id: &str,
    remote_folder: &str,
) -> Result<AttributeStoreSelection, MirageError> {
    let binding = attribute_binding(account_id, remote_folder);
    let bound = root.join(bound_store_name(&binding));
    if bound.exists() {
        let mut registry = load_binding_registry(root);
        record_binding(&mut registry, &bound, account_id, remote_folder);
        save_binding_registry(root, &registry)?;
        return Ok(AttributeStoreSelection {
            path: bound,
            binding,
        });
    }
    let legacy = root.join(legacy_store_name(remote_folder));
    let mut registry = load_binding_registry(root);
    if legacy.exists() {
        let verified = registry
            .stores
            .get(
                legacy
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
            )
            .is_some_and(|record| {
                record.account_id == account_id && record.remote_folder == remote_folder
            });
        if verified {
            std::fs::rename(&legacy, &bound).map_err(MirageError::from)?;
        }
    }
    record_binding(&mut registry, &bound, account_id, remote_folder);
    save_binding_registry(root, &registry)?;
    Ok(AttributeStoreSelection {
        path: bound,
        binding,
    })
}

fn record_binding(
    registry: &mut AttributeBindingRegistry,
    store: &Path,
    account_id: &str,
    remote_folder: &str,
) {
    let Some(name) = store.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    registry.format_version = 1;
    registry.stores.insert(
        name.to_owned(),
        AttributeBindingRecord {
            account_id: account_id.to_owned(),
            remote_folder: remote_folder.to_owned(),
        },
    );
}

fn resolve_attribute_store(
    account_id: &str,
    remote_folder: &str,
) -> Result<AttributeStoreSelection, MirageError> {
    resolve_attribute_store_in(&attribute_store_root()?, account_id, remote_folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letter_and_remote_path_are_strict() {
        assert_eq!(validate_drive_letter("n").unwrap(), PathBuf::from("N:\\"));
        assert!(validate_drive_letter("C:").is_err());
        assert!(validate_drive_letter("NN").is_err());
        assert!(validate_remote_folder("MirageSSD Storage").is_ok());
        assert!(validate_remote_folder("../escape").is_err());
        assert!(validate_remote_folder("bad:remote").is_err());
    }

    #[test]
    fn attribute_store_is_bound_to_account_and_root() {
        let root = tempfile::tempdir().unwrap();
        let first =
            resolve_attribute_store_in(root.path(), "account-a", "MirageSSD Storage").unwrap();
        let same =
            resolve_attribute_store_in(root.path(), "account-a", "MirageSSD Storage").unwrap();
        assert_eq!(first.path, same.path);
        assert_eq!(first.binding.len(), 64);
        let other_account =
            resolve_attribute_store_in(root.path(), "account-b", "MirageSSD Storage").unwrap();
        let other_root =
            resolve_attribute_store_in(root.path(), "account-a", "Other Folder").unwrap();
        assert_ne!(first.path, other_account.path);
        assert_ne!(first.path, other_root.path);
        assert_ne!(first.binding, other_account.binding);
    }

    #[test]
    fn legacy_store_migrates_only_with_matching_binding() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join(legacy_store_name("MirageSSD Storage"));
        std::fs::write(&legacy, b"{\"format_version\":1,\"entries\":{}}").unwrap();

        // No recorded binding: the legacy store is preserved but not adopted.
        let selected =
            resolve_attribute_store_in(root.path(), "account-a", "MirageSSD Storage").unwrap();
        assert!(legacy.exists());
        assert_ne!(selected.path, legacy);

        // A binding for a different account must not migrate it either.
        let mut registry = load_binding_registry(root.path());
        record_binding(&mut registry, &legacy, "account-b", "MirageSSD Storage");
        save_binding_registry(root.path(), &registry).unwrap();
        let other =
            resolve_attribute_store_in(root.path(), "account-c", "MirageSSD Storage").unwrap();
        assert!(legacy.exists());
        assert_ne!(other.path, legacy);

        // A binding for the exact account and root migrates the file.
        let mut registry = load_binding_registry(root.path());
        record_binding(&mut registry, &legacy, "account-a", "MirageSSD Storage");
        save_binding_registry(root.path(), &registry).unwrap();
        let migrated =
            resolve_attribute_store_in(root.path(), "account-a", "MirageSSD Storage").unwrap();
        assert!(!legacy.exists());
        assert_eq!(
            migrated.path,
            root.path().join(bound_store_name(&migrated.binding))
        );
        assert_eq!(
            std::fs::read(&migrated.path).unwrap(),
            b"{\"format_version\":1,\"entries\":{}}"
        );
    }

    #[test]
    fn mount_arguments_are_the_recorded_provider_configuration() {
        let options = MountOptions {
            rclone: None,
            drive_letter: "M",
            remote_folder: "MirageSSD Storage",
            cache_dir: Path::new("D:\\cache"),
            log_file: Path::new("D:\\logs\\mount.log"),
            token_store: None,
            cache_max_size: "50G",
            cache_min_free_space: "5G",
            cache_max_age: "720h",
            read_ahead: "128Mi",
            read_chunk_size: "16Mi",
            read_chunk_streams: 4,
            json: false,
        };
        let args = mount_arguments(&options, Path::new("M:\\"), "miragessd:MirageSSD Storage");
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            [
                "mount",
                "miragessd:MirageSSD Storage",
                "M:\\",
                "--config",
                "NUL",
                "--volname",
                "MirageSSD",
                "--vfs-cache-mode",
                "full",
                "--cache-dir",
                "D:\\cache",
                "--vfs-cache-max-size",
                "50G",
                "--vfs-cache-min-free-space",
                "5G",
                "--vfs-cache-max-age",
                "720h",
                "--vfs-write-back",
                "5s",
                "--vfs-cache-poll-interval",
                "15s",
                "--dir-cache-time",
                "72h",
                "--vfs-refresh",
                "--poll-interval",
                "1m",
                "--vfs-fast-fingerprint",
                "--vfs-links",
                "--buffer-size",
                "16Mi",
                "--vfs-read-ahead",
                "128Mi",
                "--vfs-read-chunk-size",
                "16Mi",
                "--vfs-read-chunk-streams",
                "4",
                "--drive-chunk-size",
                "64Mi",
                "--drive-pacer-min-sleep",
                "10ms",
                "--drive-pacer-burst",
                "200",
                "--transfers",
                "8",
                "--checkers",
                "16",
                "--vfs-case-insensitive",
                "--attr-timeout",
                "1s",
                "--log-file",
                "D:\\logs\\mount.log",
                "--log-level",
                "NOTICE",
            ]
        );
    }
}
