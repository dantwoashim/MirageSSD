use crate::{HostId, HostSpec, HostState, StdLauncher, Supervisor};
use mirage_types::{MirageError, RepositoryId};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub trait MountControl: Send {
    fn mount(
        &mut self,
        repository_id: RepositoryId,
        mount_point: &Path,
        index: &Path,
        state_root: &Path,
        owner_sid: &str,
        volume_capacity: (u64, u64),
    ) -> Result<(), MirageError>;
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
    fn mount(
        &mut self,
        repository_id: RepositoryId,
        mount_point: &Path,
        index: &Path,
        state_root: &Path,
        owner_sid: &str,
        volume_capacity: (u64, u64),
    ) -> Result<(), MirageError> {
        let (volume_total_bytes, volume_free_bytes) = volume_capacity;
        if !self.executable.is_file() {
            return Err(MirageError::provider_unavailable(
                "filesystem host executable is unavailable",
            ));
        }
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
                    mount_point: mount_point.to_path_buf(),
                    index: index.to_path_buf(),
                    state_root: state_root.to_path_buf(),
                    owner_sid: owner_sid.to_owned(),
                    volume_total_bytes,
                    volume_free_bytes,
                },
            )
            .map_err(io_error)?;
        if !self
            .supervisor
            .wait_ready(&id, Duration::from_secs(15))
            .map_err(io_error)?
        {
            let _ = self.supervisor.stop(&id);
            return Err(MirageError::deadline_exceeded(
                "filesystem host did not become ready before the startup deadline",
            ));
        }
        if let Err(error) = wait_for_mount_response(&mut self.supervisor, &id, mount_point) {
            let _ = self.supervisor.stop(&id);
            return Err(error);
        }
        if self.supervisor.state(&id) != Some(HostState::Running) {
            return Err(MirageError::provider_unavailable(
                "filesystem host exited during mount startup",
            ));
        }
        Ok(())
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
        _: (u64, u64),
    ) -> Result<(), MirageError> {
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
