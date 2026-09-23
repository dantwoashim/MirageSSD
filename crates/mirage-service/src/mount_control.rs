use crate::{HostId, HostSpec, HostState, StdLauncher, Supervisor};
use mirage_types::{MirageError, RepositoryId};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub trait MountControl: Send {
    #[allow(clippy::too_many_arguments)]
    fn mount(
        &mut self,
        repository_id: RepositoryId,
        mount_point: &Path,
        index: &Path,
        state_root: &Path,
        owner_sid: &str,
        origin_root: Option<&Path>,
        volume_capacity: (u64, u64),
        managed: bool,
        drive_provider: Option<(&Path, &Path)>,
        disk_floor: Option<u64>,
        volume_label: &str,
        dirty_budget: Option<u64>,
        journal_root: Option<&Path>,
    ) -> Result<(), MirageError>;
    /// Pushes a bearer token to the live host of a mounted repository.
    fn send_drive_token(
        &mut self,
        repository_id: RepositoryId,
        token: &str,
    ) -> Result<(), MirageError>;
    /// Asks a mounted managed host to evict published payloads, returning the
    /// `(freed, pinned_blocked)` journal bytes reported by `MIRAGE_EVICTED`
    /// within 30 s.
    fn request_eviction(
        &mut self,
        repository_id: RepositoryId,
        bytes: u64,
    ) -> Result<(u64, u64), MirageError>;
    /// Tells a mounted host to refresh its pinned-inode set.
    fn reload_pins(&mut self, repository_id: RepositoryId) -> Result<(), MirageError>;
    fn unmount(&mut self, repository_id: RepositoryId) -> Result<(), MirageError>;
    fn is_running(&mut self, repository_id: RepositoryId) -> Result<bool, MirageError>;
}

pub struct NativeMountControl {
    executable: PathBuf,
    supervisor: Supervisor<StdLauncher>,
}

impl NativeMountControl {
    #[must_use]
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            supervisor: Supervisor::new(StdLauncher),
        }
    }
}

impl Drop for NativeMountControl {
    fn drop(&mut self) {
        let _ = self.supervisor.stop_all();
    }
}

impl MountControl for NativeMountControl {
    #[allow(clippy::too_many_arguments)]
    fn mount(
        &mut self,
        repository_id: RepositoryId,
        mount_point: &Path,
        index: &Path,
        state_root: &Path,
        owner_sid: &str,
        origin_root: Option<&Path>,
        volume_capacity: (u64, u64),
        managed: bool,
        drive_provider: Option<(&Path, &Path)>,
        disk_floor: Option<u64>,
        volume_label: &str,
        dirty_budget: Option<u64>,
        journal_root: Option<&Path>,
    ) -> Result<(), MirageError> {
        let (volume_total_bytes, volume_free_bytes) = volume_capacity;
        if !self.executable.is_file() {
            return Err(MirageError::provider_unavailable(
                "filesystem host executable is unavailable",
            ));
        }
        // A fresh install has no cache directory yet (shards are optional for
        // managed volumes); create it rather than refuse the first mount.
        let _ = fs::create_dir_all(state_root.join("cache"));
        if !index.is_file()
            || !state_root.join("control.db").is_file()
            || !state_root.join("cache").is_dir()
        {
            return Err(MirageError::integrity_mismatch(
                "verified mount index or local cache state is unavailable",
            ));
        }
        if volume_total_bytes == 0 || volume_free_bytes > volume_total_bytes {
            return Err(MirageError::integrity_mismatch(
                "filesystem volume capacity is invalid",
            ));
        }
        let id = host_id(repository_id)?;
        self.supervisor
            .start(
                id.clone(),
                &HostSpec {
                    executable: self.executable.clone(),
                    mount_point: winfsp_mount_point(mount_point),
                    index: index.to_path_buf(),
                    state_root: state_root.to_path_buf(),
                    owner_sid: owner_sid.to_owned(),
                    volume_total_bytes,
                    volume_free_bytes,
                    origin_root: origin_root.map(Path::to_path_buf),
                    managed,
                    drive_manifest: drive_provider.map(|(manifest, _)| manifest.to_path_buf()),
                    repository_key: drive_provider.map(|(_, key)| key.to_path_buf()),
                    disk_floor,
                    dirty_budget,
                    journal_root: journal_root.map(Path::to_path_buf),
                    label: {
                        let trimmed = volume_label.trim();
                        (!trimmed.is_empty()).then(|| trimmed.chars().take(32).collect::<String>())
                    },
                },
            )
            .map_err(io_error)?;
        let ready = self.supervisor.wait_ready(&id, Duration::from_secs(15));
        if let Err(error) = ready {
            let tail = self.supervisor.stderr_tail(&id);
            let _ = self.supervisor.stop(&id);
            return Err(MirageError::provider_unavailable(format!(
                "filesystem host operation failed: {error}{}",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("; host stderr: {}", tail.trim())
                }
            )));
        }
        if !ready.unwrap_or(false) {
            let tail = self.supervisor.stderr_tail(&id);
            let _ = self.supervisor.stop(&id);
            return Err(MirageError::deadline_exceeded(format!(
                "filesystem host did not become ready before the startup deadline{}",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("; host stderr: {}", tail.trim())
                }
            )));
        }
        if let Err(error) = wait_for_mount_response(&mut self.supervisor, &id, mount_point) {
            let tail = self.supervisor.stderr_tail(&id);
            let _ = self.supervisor.stop(&id);
            return Err(MirageError::provider_unavailable(format!(
                "{error}{}",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("; host stderr: {}", tail.trim())
                }
            )));
        }
        if self.supervisor.state(&id) != Some(HostState::Running) {
            return Err(MirageError::provider_unavailable(
                "filesystem host exited during mount startup",
            ));
        }
        Ok(())
    }

    fn send_drive_token(
        &mut self,
        repository_id: RepositoryId,
        token: &str,
    ) -> Result<(), MirageError> {
        self.supervisor
            .send_line(&host_id(repository_id)?, &format!("TOKEN {token}"))
            .map_err(io_error)
    }

    fn request_eviction(
        &mut self,
        repository_id: RepositoryId,
        bytes: u64,
    ) -> Result<(u64, u64), MirageError> {
        self.supervisor
            .request_eviction(&host_id(repository_id)?, bytes, Duration::from_secs(30))
            .map_err(io_error)
    }

    fn reload_pins(&mut self, repository_id: RepositoryId) -> Result<(), MirageError> {
        self.supervisor
            .reload_pins(&host_id(repository_id)?)
            .map_err(io_error)
    }

    fn unmount(&mut self, repository_id: RepositoryId) -> Result<(), MirageError> {
        let state = self
            .supervisor
            .stop(&host_id(repository_id)?)
            .map_err(io_error)?;
        if matches!(state, HostState::Crashed(_)) {
            return Err(MirageError::provider_unavailable(
                "filesystem host crashed while unmounting",
            ));
        }
        Ok(())
    }

    fn is_running(&mut self, repository_id: RepositoryId) -> Result<bool, MirageError> {
        let _ = self.supervisor.poll().map_err(io_error)?;
        Ok(self.supervisor.state(&host_id(repository_id)?) == Some(HostState::Running))
    }
}

fn wait_for_mount_response(
    supervisor: &mut Supervisor<StdLauncher>,
    id: &HostId,
    mount_point: &Path,
) -> Result<(), MirageError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let _ = supervisor.poll().map_err(io_error)?;
        if supervisor.state(id) != Some(HostState::Running) {
            return Err(MirageError::provider_unavailable(
                "filesystem host exited before its mount point answered",
            ));
        }
        let readiness_root = drive_root(mount_point).unwrap_or_else(|| mount_point.to_path_buf());
        if let Ok(mut entries) = fs::read_dir(&readiness_root) {
            match entries.next().transpose() {
                Ok(_) => return Ok(()),
                Err(_) if Instant::now() < deadline => {}
                Err(error) => {
                    return Err(MirageError::provider_unavailable(
                        "mounted filesystem root did not answer directory I/O",
                    )
                    .with_source(error));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(MirageError::deadline_exceeded(
                "mounted filesystem root did not answer before the readiness deadline",
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Converts a canonical (`\\?\`-prefixed) directory path into the plain form WinFsp accepts.
///
/// Registration stores canonical mount roots, but `FspFileSystemSetMountPoint` rejects verbatim
/// paths with `STATUS_OBJECT_NAME_INVALID` when the host runs without elevation. `\\?\C:\dir`
/// becomes `C:\dir` and `\\?\UNC\server\share` becomes `\\server\share`; anything else is returned
/// unchanged.
fn winfsp_mount_point(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.len() >= 3 && rest.as_bytes()[1] == b':' => PathBuf::from(rest),
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => PathBuf::from(format!(r"\\{unc}")),
            None => path.to_path_buf(),
        },
        None => path.to_path_buf(),
    }
}

fn drive_root(path: &Path) -> Option<PathBuf> {
    let value = path.to_str()?;
    let bytes = value.as_bytes();
    (bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| PathBuf::from(format!("{}:\\", bytes[0] as char)))
}

fn host_id(repository_id: RepositoryId) -> Result<HostId, MirageError> {
    HostId::new(repository_id.to_string()).map_err(io_error)
}

fn io_error(error: std::io::Error) -> MirageError {
    MirageError::provider_unavailable("filesystem host operation failed").with_source(error)
}

pub(crate) struct UnavailableMountControl;
impl MountControl for UnavailableMountControl {
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
        Err(MirageError::provider_unavailable(
            "filesystem host is not configured",
        ))
    }
    fn send_drive_token(&mut self, _: RepositoryId, _: &str) -> Result<(), MirageError> {
        Err(MirageError::provider_unavailable(
            "filesystem host is not configured",
        ))
    }
    fn request_eviction(&mut self, _: RepositoryId, _: u64) -> Result<(u64, u64), MirageError> {
        Err(MirageError::provider_unavailable(
            "filesystem host is not configured",
        ))
    }
    fn reload_pins(&mut self, _: RepositoryId) -> Result<(), MirageError> {
        Err(MirageError::provider_unavailable(
            "filesystem host is not configured",
        ))
    }
    fn unmount(&mut self, _: RepositoryId) -> Result<(), MirageError> {
        Err(MirageError::provider_unavailable(
            "filesystem host is not configured",
        ))
    }
    fn is_running(&mut self, _: RepositoryId) -> Result<bool, MirageError> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::winfsp_mount_point;
    use std::path::{Path, PathBuf};

    #[test]
    fn verbatim_prefixes_are_stripped_for_winfsp() {
        assert_eq!(
            winfsp_mount_point(Path::new(r"\\?\D:\Games\Example\assets")),
            PathBuf::from(r"D:\Games\Example\assets")
        );
        assert_eq!(
            winfsp_mount_point(Path::new(r"\\?\UNC\server\share\assets")),
            PathBuf::from(r"\\server\share\assets")
        );
        assert_eq!(
            winfsp_mount_point(Path::new(r"D:\Games\Example\assets")),
            PathBuf::from(r"D:\Games\Example\assets")
        );
        assert_eq!(winfsp_mount_point(Path::new("M:")), PathBuf::from("M:"));
    }
}
