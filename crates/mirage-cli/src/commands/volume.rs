//! One-call managed Drive-backed volume creation, shared by `mirage volume
//! create` and the first-run wizard in `mirage-ui.exe`.
//!
//! Composition: empty local import → signed Drive publication (zero packs)
//! → repository registration → Drive origin → managed volume mode → mount
//! with a freshly minted Drive access token → per-disk free-space floor.
//! Explorer-visible mounts persist automatically via the service's
//! `active-mount.json` record; the per-user `mirage agent` logon task keeps
//! the Drive token supplied after reboot.

use std::path::{Path, PathBuf};

use mirage_manifest::{CommitSigner, InMemoryTestSigner};
use mirage_types::{GenerationId, MirageError, RepositoryId};

use super::{drive_live, repo_drive, repo_import_local, service};
use crate::client::ServiceTransport;

const GIB: u64 = 1 << 30;
const MAX_VOLUME_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSpec {
    pub name: String,
    pub drive_letter: String,
    pub budget_bytes: u64,
    pub floor_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct VolumeCreated {
    pub repository_id: RepositoryId,
    pub name: String,
    pub drive_letter: String,
    pub budget_bytes: u64,
    pub floor_bytes: Option<u64>,
    /// Set when the free-space floor could not be applied (for example the
    /// installed service predates interactive-user floor permission or the
    /// caller lacks rights) — the volume is still usable; set the floor
    /// later with `mirage disk set-floor`.
    pub floor_error: Option<String>,
    pub account_id: String,
    pub mount_point: PathBuf,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub volume_root: PathBuf,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// First free drive letter, scanning M: toward Z: like the original setup.
pub fn first_free_letter() -> Result<char, MirageError> {
    let used = used_drive_mask()?;
    for letter in 'M'..='Z' {
        let bit = (letter as u8) - b'A';
        if used & (1 << bit) == 0 {
            return Ok(letter);
        }
    }
    Err(MirageError::invalid_argument(
        "no free Explorer drive letter between M: and Z:",
    ))
}

#[cfg(windows)]
fn used_drive_mask() -> Result<u32, MirageError> {
    // GetLogicalDrives reports every DOS device letter, mounted or not.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    Ok(mask)
}

#[cfg(not(windows))]
fn used_drive_mask() -> Result<u32, MirageError> {
    Err(MirageError::provider_unavailable(
        "drive letters only exist on Windows",
    ))
}

/// Free/total bytes for each fixed drive letter (C: through Z:).
pub fn disks() -> Result<Vec<DiskInfo>, MirageError> {
    let used = used_drive_mask()?;
    let mut disks = Vec::new();
    for letter in 'C'..='Z' {
        let bit = (letter as u8) - b'A';
        if used & (1 << bit) == 0 {
            continue;
        }
        let root = PathBuf::from(format!("{letter}:\\"));
        let Some((total, free)) = disk_space(&root) else {
            continue;
        };
        if total == 0 {
            continue;
        }
        disks.push(DiskInfo {
            volume_root: root,
            total_bytes: total,
            free_bytes: free,
        });
    }
    Ok(disks)
}

#[cfg(windows)]
fn disk_space(root: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_available = 0_u64;
    let mut total = 0_u64;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_available,
            &mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some((total, free_available))
    }
}

#[cfg(not(windows))]
fn disk_space(_root: &Path) -> Option<(u64, u64)> {
    None
}

/// The volume that hosts the service state root (and therefore every
/// managed journal). Managed payload bytes physically live there, so the
/// free-space floor belongs to this disk regardless of where the volume's
/// data appears.
pub fn state_volume() -> Result<DiskInfo, MirageError> {
    let root = state_volume_root()?;
    let (total, free) = disk_space(&root).ok_or_else(|| {
        MirageError::invalid_argument("the service state disk could not be inspected")
    })?;
    Ok(DiskInfo {
        volume_root: root,
        total_bytes: total,
        free_bytes: free,
    })
}

/// Volume root of `%ProgramData%` (the service state root's parent) without
/// consulting the service: ProgramData always lives on the system volume.
fn state_volume_root() -> Result<PathBuf, MirageError> {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
    let root = program_data
        .components()
        .next()
        .map(|component| component.as_os_str().to_os_string())
        .ok_or_else(|| MirageError::invalid_argument("ProgramData path has no volume root"))?;
    Ok(PathBuf::from(format!("{}\\", root.to_string_lossy())))
}

/// Every free drive letter from D: through Z: (mounted letters excluded).
pub fn free_letters() -> Result<Vec<String>, MirageError> {
    let used = used_drive_mask()?;
    Ok(('D'..='Z')
        .filter(|letter| used & (1 << (*letter as u8 - b'A')) == 0)
        .map(|letter| letter.to_string())
        .collect())
}

/// Default local SSD budget: min(25% of free space on the disk with the
/// most free space, 64 GiB), as specified for the first-run wizard.
pub fn default_budget_bytes() -> Result<u64, MirageError> {
    let disk = disks()?
        .into_iter()
        .max_by_key(|disk| disk.free_bytes)
        .ok_or_else(|| MirageError::invalid_argument("no fixed disk has free space"))?;
    Ok((disk.free_bytes / 4).clamp(GIB, 64 * GIB))
}

/// Default free-space floor for the state disk: max(10% of total, 20 GiB).
pub fn default_floor_bytes(disk: &DiskInfo) -> u64 {
    (disk.total_bytes / 10).max(20 * GIB)
}

pub fn default_volume_name() -> &'static str {
    "MirageSSD"
}

fn validate_name(name: &str) -> Result<String, MirageError> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_VOLUME_NAME_CHARS
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
    {
        return Err(MirageError::invalid_argument(
            "volume name must be 1-64 characters without Windows filename symbols",
        ));
    }
    Ok(name.to_owned())
}

fn normalize_letter(letter: &str) -> Result<String, MirageError> {
    let trimmed = letter.trim().trim_end_matches(':').trim_end_matches('\\');
    let bytes = trimmed.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii_alphabetic() {
        return Err(MirageError::invalid_argument(
            "drive letter must be one ASCII letter, for example M",
        ));
    }
    Ok((bytes[0] as char).to_ascii_uppercase().to_string())
}

fn volumes_root() -> Result<PathBuf, MirageError> {
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| MirageError::invalid_argument("LOCALAPPDATA is unavailable"))?;
    Ok(PathBuf::from(local).join("MirageSSD").join("volumes"))
}

/// The Drive-facing half of `create`: account identity, a usable access
/// token, and generation publication. The live impl wraps
/// `drive_live::connect` + `repo_drive::publish_with_signer`; tests inject
/// a local stand-in so the whole orchestration runs without Drive.
pub trait VolumeBackend {
    fn account_id(&self) -> &str;
    fn access_token(&self) -> &str;
    fn publish(
        &self,
        import: &Path,
        repository_id: RepositoryId,
        signer: &dyn CommitSigner,
    ) -> Result<(), MirageError>;
}

struct LiveVolumeBackend {
    session: drive_live::LiveDriveSession,
}

impl VolumeBackend for LiveVolumeBackend {
    fn account_id(&self) -> &str {
        &self.session.account_id
    }
    fn access_token(&self) -> &str {
        self.session.access_token.as_str()
    }
    fn publish(
        &self,
        import: &Path,
        repository_id: RepositoryId,
        signer: &dyn CommitSigner,
    ) -> Result<(), MirageError> {
        repo_drive::publish_with_signer(import, repository_id, &self.session, signer).map(|_| ())
    }
}

/// Create and mount an empty managed Drive-backed volume.
///
/// `progress` receives short human-readable step labels (never secrets).
pub fn create(
    spec: &VolumeSpec,
    client_credentials: &Path,
    token_store: Option<&Path>,
    progress: &mut dyn FnMut(&str),
    transport: Option<&dyn ServiceTransport>,
) -> Result<VolumeCreated, MirageError> {
    progress("Connecting to Google Drive");
    let session = drive_live::connect(client_credentials, token_store)?;
    create_with_backend(spec, &LiveVolumeBackend { session }, progress, transport)
}

/// `create` with an injectable Drive backend (the local test seam).
pub fn create_with_backend(
    spec: &VolumeSpec,
    backend: &dyn VolumeBackend,
    progress: &mut dyn FnMut(&str),
    transport: Option<&dyn ServiceTransport>,
) -> Result<VolumeCreated, MirageError> {
    create_in(&volumes_root()?, spec, backend, progress, transport)
}

fn create_in(
    volumes_root: &Path,
    spec: &VolumeSpec,
    backend: &dyn VolumeBackend,
    progress: &mut dyn FnMut(&str),
    transport: Option<&dyn ServiceTransport>,
) -> Result<VolumeCreated, MirageError> {
    let name = validate_name(&spec.name)?;
    let letter = normalize_letter(&spec.drive_letter)?;
    if spec.budget_bytes < GIB || spec.budget_bytes > (1 << 50) {
        return Err(MirageError::invalid_argument(
            "volume budget must be between 1 GiB and 1 PiB",
        ));
    }

    let repository_id = random_repository_id()?;
    let generation = GenerationId::ZERO;
    let root = volumes_root.join(repository_id.to_string());
    match create_staged(
        &root,
        repository_id,
        generation,
        &name,
        &letter,
        spec,
        backend,
        transport,
        progress,
    ) {
        Ok((mounted, applied_floor, floor_error)) => {
            // Sign-in happened at volume creation, so ensure the per-user
            // agent is registered to keep the volume mounted and
            // authenticated after reboot.
            let _ = super::agent::install_logon_registration();
            let mount_point = mounted["mount_point"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("{letter}:")));
            Ok(VolumeCreated {
                repository_id,
                name,
                drive_letter: letter,
                budget_bytes: spec.budget_bytes,
                floor_bytes: applied_floor,
                floor_error,
                account_id: backend.account_id().to_owned(),
                mount_point,
                state: mounted["state"]
                    .as_str()
                    .unwrap_or("ready_mounted")
                    .to_owned(),
            })
        }
        Err((step, published, onboarded, error)) => {
            if onboarded {
                // The repository stays registered (there is no unregister
                // command), but it must not stay mounted over deleted files.
                let _ = service_request(transport, mirage_ipc::Command::Unmount { repository_id });
            }
            // Only this run's directory — never anything else under volumes/.
            let _ = std::fs::remove_dir_all(&root);
            let remote_note = if published {
                " A Drive folder for this repository may have been left behind and can be deleted from Google Drive."
            } else {
                ""
            };
            let registered_note = if onboarded {
                " The repository is still registered with the service; its native/import files were removed."
            } else {
                ""
            };
            Err(MirageError::invalid_argument(format!(
                "volume create failed at \"{step}\" (repository {repository_id}): {error}.{remote_note}{registered_note}"
            )))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn create_staged(
    root: &Path,
    repository_id: RepositoryId,
    generation: GenerationId,
    name: &str,
    letter: &str,
    spec: &VolumeSpec,
    backend: &dyn VolumeBackend,
    transport: Option<&dyn ServiceTransport>,
    progress: &mut dyn FnMut(&str),
) -> Result<(serde_json::Value, Option<u64>, Option<String>), (String, bool, bool, MirageError)> {
    let import = root.join("import");
    let native = root.join("native");
    let empty = root.join("empty-source");
    let mut published = false;
    let staged = |step: &'static str, published: bool, result: Result<(), MirageError>| {
        result.map_err(|error| (step.to_owned(), published, false, error))
    };
    staged(
        "creating volume directories",
        published,
        (|| {
            for directory in [&import, &native, &empty] {
                std::fs::create_dir_all(directory).map_err(MirageError::from)?;
            }
            Ok(())
        })(),
    )?;
    // Registration requires a launcher file inside the native root; managed
    // volumes are Explorer-browsed rather than launched, so a marker suffices.
    let launcher = PathBuf::from("volume.mirage");
    staged(
        "creating the launcher marker",
        published,
        std::fs::write(
            native.join(&launcher),
            b"MirageSSD managed volume marker.\n",
        )
        .map_err(MirageError::from),
    )?;

    progress("Creating the volume index");
    staged(
        "creating the volume index",
        published,
        repo_import_local::run(
            true,
            &empty,
            &import,
            repository_id,
            generation,
            1_048_576,
            536_870_912,
            &[],
            1_048_576,
            false,
            false,
            true,
            false,
        ),
    )?;
    // A fresh volume publishes exactly one signed empty generation; the
    // random signer is not persisted because no later commit re-signs it.
    let mut key_id = [0_u8; 16];
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key_id)
        .and_then(|()| getrandom::fill(&mut key))
        .map_err(|_| {
            (
                "initializing the signer".to_owned(),
                published,
                false,
                MirageError::internal_invariant("operating-system randomness failed"),
            )
        })?;
    let signer = InMemoryTestSigner::new(key_id, key);

    progress("Publishing to Google Drive");
    backend
        .publish(&import, repository_id, &signer)
        .map_err(|error| {
            (
                "publishing to Google Drive".to_owned(),
                published,
                false,
                error,
            )
        })?;
    published = true;

    let token =
        mirage_ipc::SensitiveString::new(backend.access_token().to_owned()).map_err(|error| {
            (
                "preparing the mount token".to_owned(),
                published,
                false,
                error,
            )
        })?;
    let mounted = onboard(
        repository_id,
        name,
        &native,
        &import,
        &launcher,
        generation,
        letter,
        spec.budget_bytes,
        token,
        transport,
        progress,
    )
    .map_err(|error| {
        (
            "onboarding with the service".to_owned(),
            published,
            false,
            error,
        )
    })?;

    let mut applied_floor = None;
    let mut floor_error = None;
    if let Some(floor) = spec.floor_bytes {
        let disk = state_volume()
            .map_err(|error| ("reading the state disk".to_owned(), published, true, error))?;
        match service_request(
            transport,
            mirage_ipc::Command::DiskFloorSet {
                volume_root: disk.volume_root.to_string_lossy().into_owned(),
                floor_bytes: floor,
                hysteresis_bytes: None,
            },
        ) {
            Ok(_) => applied_floor = Some(floor),
            Err(error) if error.to_string().contains("PERMISSION_DENIED") => {
                // Older installed services only allow the floor for elevated
                // or service principals; do not undo the volume for that.
                progress("Free-space floor not applied (permission denied)");
                floor_error = Some(format!(
                    "the service declined the free-space floor: {error}"
                ));
            }
            Err(error) => {
                return Err((
                    "setting the free-space floor".to_owned(),
                    published,
                    true,
                    error,
                ));
            }
        }
    }
    Ok((mounted, applied_floor, floor_error))
}

fn service_request(
    transport: Option<&dyn ServiceTransport>,
    command: mirage_ipc::Command,
) -> Result<serde_json::Value, MirageError> {
    match transport {
        Some(transport) => service::request_with(transport, command),
        None => service::request_json(command),
    }
}

/// Register → Drive origin → managed mode → mount with a fresh token.
/// Separated from `create` so the command sequence is testable with a fake
/// service transport.
#[allow(clippy::too_many_arguments)]
fn onboard(
    repository_id: RepositoryId,
    name: &str,
    native: &Path,
    import: &Path,
    launcher: &Path,
    generation: GenerationId,
    letter: &str,
    budget_bytes: u64,
    token: mirage_ipc::SensitiveString,
    transport: Option<&dyn ServiceTransport>,
    progress: &mut dyn FnMut(&str),
) -> Result<serde_json::Value, MirageError> {
    progress("Registering the volume");
    service_request(
        transport,
        mirage_ipc::Command::RepositoryRegister {
            repository_id,
            display_name: name.to_owned(),
            native_root: native.to_path_buf(),
            mount_subtree: PathBuf::from("."),
            import_root: import.to_path_buf(),
            launcher_relative: launcher.to_path_buf(),
            arguments: Vec::new(),
            version_label: "0".to_owned(),
            configuration_label: "managed".to_owned(),
            cache_bytes: budget_bytes,
        },
    )?;
    service_request(
        transport,
        mirage_ipc::Command::RepositorySetDriveOrigin {
            repository_id,
            drive: true,
        },
    )?;
    service_request(
        transport,
        mirage_ipc::Command::RepositorySetVolumeMode {
            repository_id,
            managed: true,
        },
    )?;
    progress(&format!("Mounting {letter}:"));
    service_request(
        transport,
        mirage_ipc::Command::Mount {
            repository_id,
            generation,
            drive_letter: Some(letter.to_owned()),
            drive_access_token: Some(token),
        },
    )
}

fn random_repository_id() -> Result<RepositoryId, MirageError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("operating-system randomness failed"))?;
    Ok(RepositoryId::from_bytes(bytes))
}

/// List this user's managed Drive-backed volumes for the UI drive card.
pub fn list(
    transport: Option<&dyn ServiceTransport>,
) -> Result<Vec<serde_json::Value>, MirageError> {
    let status = service_request(transport, mirage_ipc::Command::Status)?;
    let mut volumes = Vec::new();
    for repository in status["repositories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let Some(id_text) = repository["repository_id"].as_str() else {
            continue;
        };
        let Ok(repository_id) = id_text.parse::<RepositoryId>() else {
            continue;
        };
        let detail = service_request(
            transport,
            mirage_ipc::Command::RepositoryDetail { repository_id },
        )?;
        if detail["origin"].as_str() != Some("drive")
            || detail["volume_mode"].as_str() != Some("managed")
        {
            continue;
        }
        let mut detail = detail;
        detail["repository_id"] = serde_json::json!(id_text);
        detail["state"] = repository["state"].clone();
        volumes.push(detail);
    }
    Ok(volumes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ServiceTransport;
    use mirage_ipc::{PROTOCOL_VERSION, Request, Response, ResponseBody};
    use std::sync::Mutex;

    struct RecordingTransport {
        requests: Mutex<Vec<mirage_ipc::Command>>,
    }

    impl ServiceTransport for RecordingTransport {
        fn exchange(&self, request: &Request) -> Result<Response, MirageError> {
            self.requests.lock().unwrap().push(request.command.clone());
            let body = if matches!(request.command, mirage_ipc::Command::Mount { .. }) {
                ResponseBody::Json(serde_json::json!({
                    "state": "ready_mounted",
                    "mount_point": "Q:",
                }))
            } else {
                ResponseBody::Json(serde_json::json!({"ok": true}))
            };
            Ok(Response {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                body,
            })
        }
    }

    #[test]
    fn onboard_issues_register_origin_mode_mount_in_order() {
        let transport = RecordingTransport {
            requests: Mutex::new(Vec::new()),
        };
        let repository_id = RepositoryId::from_bytes([7; 16]);
        let token = mirage_ipc::SensitiveString::new("token".to_owned()).unwrap();
        let mounted = onboard(
            repository_id,
            "Test Volume",
            Path::new("D:/v/native"),
            Path::new("D:/v/import"),
            Path::new("volume.mirage"),
            GenerationId::ZERO,
            "Q",
            8 << 30,
            token,
            Some(&transport),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(mounted["mount_point"].as_str(), Some("Q:"));
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        match &requests[0] {
            mirage_ipc::Command::RepositoryRegister {
                display_name,
                cache_bytes,
                mount_subtree,
                ..
            } => {
                assert_eq!(display_name, "Test Volume");
                assert_eq!(*cache_bytes, 8 << 30);
                assert_eq!(mount_subtree, &PathBuf::from("."));
            }
            other => panic!("expected register, got {other:?}"),
        }
        assert!(matches!(
            requests[1],
            mirage_ipc::Command::RepositorySetDriveOrigin { drive: true, .. }
        ));
        assert!(matches!(
            requests[2],
            mirage_ipc::Command::RepositorySetVolumeMode { managed: true, .. }
        ));
        match &requests[3] {
            mirage_ipc::Command::Mount {
                drive_letter,
                drive_access_token,
                ..
            } => {
                assert_eq!(drive_letter.as_deref(), Some("Q"));
                assert!(drive_access_token.is_some());
            }
            other => panic!("expected mount, got {other:?}"),
        }
    }

    #[test]
    fn list_filters_to_managed_drive_volumes() {
        struct Detail {
            requests: Mutex<u32>,
        }
        impl ServiceTransport for Detail {
            fn exchange(&self, request: &Request) -> Result<Response, MirageError> {
                let first = RepositoryId::from_bytes([1; 16]);
                let body = match &request.command {
                    mirage_ipc::Command::Status => ResponseBody::Json(serde_json::json!({
                        "repositories": [
                            {"repository_id": first.to_string(), "state": "ready_mounted"},
                            {"repository_id": RepositoryId::from_bytes([2;16]).to_string(), "state": "ready_unmounted"}
                        ]
                    })),
                    mirage_ipc::Command::RepositoryDetail { repository_id } => {
                        *self.requests.lock().unwrap() += 1;
                        if *repository_id == first {
                            ResponseBody::Json(serde_json::json!({
                                "origin": "drive", "volume_mode": "managed"
                            }))
                        } else {
                            ResponseBody::Json(serde_json::json!({
                                "origin": "local", "volume_mode": "readonly"
                            }))
                        }
                    }
                    _ => ResponseBody::Json(serde_json::json!({})),
                };
                Ok(Response {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: request.request_id,
                    body,
                })
            }
        }
        let transport = Detail {
            requests: Mutex::new(0),
        };
        let volumes = list(Some(&transport)).unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0]["origin"], "drive");
        assert_eq!(volumes[0]["state"], "ready_mounted");
    }

    #[test]
    fn create_end_to_end_against_the_local_backend_seam() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct LocalBackend {
            publishes: AtomicUsize,
        }
        impl VolumeBackend for LocalBackend {
            fn account_id(&self) -> &str {
                "test@example.com"
            }
            fn access_token(&self) -> &str {
                "local-seam-token"
            }
            fn publish(
                &self,
                import: &Path,
                _repository_id: RepositoryId,
                _signer: &dyn CommitSigner,
            ) -> Result<(), MirageError> {
                self.publishes.fetch_add(1, Ordering::Relaxed);
                // Mirror the real publisher's output shape without Drive.
                std::fs::write(import.join("drive-manifest.cbor"), b"seam")
                    .map_err(MirageError::from)
            }
        }

        let transport = RecordingTransport {
            requests: Mutex::new(Vec::new()),
        };
        let backend = LocalBackend {
            publishes: AtomicUsize::new(0),
        };
        let root = tempfile::tempdir().expect("volumes root");
        let mut steps = Vec::new();
        let created = create_in(
            root.path(),
            &VolumeSpec {
                name: "Test Drive".to_owned(),
                drive_letter: "n:".to_owned(),
                budget_bytes: 8 * GIB,
                floor_bytes: None,
            },
            &backend,
            &mut |step| steps.push(step.to_owned()),
            Some(&transport),
        )
        .expect("create");

        assert_eq!(backend.publishes.load(Ordering::Relaxed), 1);
        let import = root
            .path()
            .join(created.repository_id.to_string())
            .join("import");
        assert!(import.join("base-manifest.cbor").is_file());
        assert!(import.join("repository-key.dpapi").is_file());
        assert!(import.join("drive-manifest.cbor").is_file());
        assert_eq!(created.drive_letter, "N");
        assert_eq!(created.account_id, "test@example.com");
        assert_eq!(created.mount_point, PathBuf::from("Q:"));
        assert_eq!(
            steps,
            [
                "Creating the volume index",
                "Publishing to Google Drive",
                "Registering the volume",
                "Mounting N:",
            ]
        );
        // The empty index imported and published without seeding files.
        let manifest_bytes = std::fs::read(import.join("base-manifest.cbor")).unwrap();
        assert!(!manifest_bytes.is_empty());
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(matches!(requests[3], mirage_ipc::Command::Mount { .. }));
    }

    #[test]
    fn failed_create_rolls_back_the_local_dirs_and_reports_the_step() {
        struct FailingTransport;
        impl ServiceTransport for FailingTransport {
            fn exchange(&self, request: &Request) -> Result<Response, MirageError> {
                if matches!(
                    request.command,
                    mirage_ipc::Command::RepositoryRegister { .. }
                ) {
                    return Err(MirageError::provider_unavailable(
                        "service pipe is unavailable",
                    ));
                }
                Ok(Response {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: request.request_id,
                    body: ResponseBody::Json(serde_json::json!({"ok": true})),
                })
            }
        }
        struct PublishOk;
        impl VolumeBackend for PublishOk {
            fn account_id(&self) -> &str {
                "a@b.c"
            }
            fn access_token(&self) -> &str {
                "t"
            }
            fn publish(
                &self,
                _import: &Path,
                _repository_id: RepositoryId,
                _signer: &dyn CommitSigner,
            ) -> Result<(), MirageError> {
                Ok(())
            }
        }

        let root = tempfile::tempdir().expect("volumes root");
        let error = create_in(
            root.path(),
            &VolumeSpec {
                name: "Test".to_owned(),
                drive_letter: "N".to_owned(),
                budget_bytes: 8 * GIB,
                floor_bytes: None,
            },
            &PublishOk,
            &mut |_| {},
            Some(&FailingTransport),
        )
        .expect_err("register failure must surface");
        let message = error.to_string();
        assert!(message.contains("onboarding with the service"), "{message}");
        assert!(message.contains("Drive folder"), "{message}");
        // The only directory under volumes/ was ours — it is gone now.
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn floor_permission_denial_keeps_the_volume_and_warns() {
        struct FloorFails {
            requests: Mutex<Vec<mirage_ipc::Command>>,
        }
        impl ServiceTransport for FloorFails {
            fn exchange(&self, request: &Request) -> Result<Response, MirageError> {
                self.requests.lock().unwrap().push(request.command.clone());
                if matches!(request.command, mirage_ipc::Command::DiskFloorSet { .. }) {
                    return Err(MirageError::backend_permission_denied("denied"));
                }
                let body = if matches!(request.command, mirage_ipc::Command::Mount { .. }) {
                    ResponseBody::Json(
                        serde_json::json!({"state": "ready_mounted", "mount_point": "N:"}),
                    )
                } else {
                    ResponseBody::Json(serde_json::json!({"ok": true}))
                };
                Ok(Response {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: request.request_id,
                    body,
                })
            }
        }
        struct PublishOk;
        impl VolumeBackend for PublishOk {
            fn account_id(&self) -> &str {
                "a@b.c"
            }
            fn access_token(&self) -> &str {
                "t"
            }
            fn publish(
                &self,
                _import: &Path,
                _repository_id: RepositoryId,
                _signer: &dyn CommitSigner,
            ) -> Result<(), MirageError> {
                Ok(())
            }
        }

        let root = tempfile::tempdir().expect("volumes root");
        let transport = FloorFails {
            requests: Mutex::new(Vec::new()),
        };
        let created = create_in(
            root.path(),
            &VolumeSpec {
                name: "Test".to_owned(),
                drive_letter: "N".to_owned(),
                budget_bytes: 8 * GIB,
                floor_bytes: Some(20 * GIB),
            },
            &PublishOk,
            &mut |_| {},
            Some(&transport),
        )
        .expect("a permission-denied floor is a warning, not a rollback");
        assert!(created.floor_bytes.is_none());
        assert!(created.floor_error.unwrap().contains("denied"));
        let requests = transport.requests.lock().unwrap();
        assert!(
            !requests
                .iter()
                .any(|command| matches!(command, mirage_ipc::Command::Unmount { .. })),
            "the volume stays mounted when only the floor was denied: {requests:?}"
        );
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_some());
    }

    #[test]
    fn letter_and_name_validation() {
        assert_eq!(normalize_letter("m").unwrap(), "M");
        assert_eq!(normalize_letter("Q:").unwrap(), "Q");
        assert!(normalize_letter("AB").is_err());
        assert!(normalize_letter("1").is_err());
        assert_eq!(validate_name("  My Drive ").unwrap(), "My Drive");
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn default_floor_is_ten_percent_or_twenty_gib() {
        let disk = DiskInfo {
            volume_root: PathBuf::from("C:\\"),
            total_bytes: 500 * GIB,
            free_bytes: 100 * GIB,
        };
        assert_eq!(default_floor_bytes(&disk), 50 * GIB);
        let small = DiskInfo {
            volume_root: PathBuf::from("C:\\"),
            total_bytes: 100 * GIB,
            free_bytes: 30 * GIB,
        };
        assert_eq!(default_floor_bytes(&small), 20 * GIB);
    }
}
