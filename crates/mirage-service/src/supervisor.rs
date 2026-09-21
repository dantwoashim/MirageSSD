use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostId(String);
impl HostId {
    pub fn new(value: impl Into<String>) -> io::Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid host id",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub executable: PathBuf,
    pub mount_point: PathBuf,
    pub index: PathBuf,
    pub state_root: PathBuf,
    pub owner_sid: String,
    pub volume_total_bytes: u64,
    pub volume_free_bytes: u64,
    /// Immutable origin pack directory used for degraded read-through on a
    /// cache miss; `None` keeps strict offline semantics.
    pub origin_root: Option<PathBuf>,
    /// Writable managed volume (`--managed`); `false` launches a read-only
    /// cache mount (`--cache`).
    pub managed: bool,
    /// `drive-manifest.cbor` enabling the managed host's on-demand Drive
    /// provider; passed with `repository_key` or not at all.
    pub drive_manifest: Option<PathBuf>,
    /// `repository-key.dpapi` for the managed host's Drive provider.
    pub repository_key: Option<PathBuf>,
    /// Write-admission free-space floor for the journal volume (`--floor`).
    pub disk_floor: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostExit {
    pub success: bool,
    pub code: Option<i32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Running,
    Stopped,
    Crashed(HostExit),
}

pub trait ManagedChild: Send {
    fn wait_ready(&mut self, timeout: Duration) -> io::Result<bool>;
    fn try_exit(&mut self) -> io::Result<Option<HostExit>>;
    /// Writes one control line (`TOKEN <bearer>`, …) on the retained stdin.
    fn send_line(&mut self, line: &str) -> io::Result<()>;
    /// Bounded tail of the host's stderr for launch-failure diagnosis.
    fn stderr_tail(&self) -> String {
        String::new()
    }
    /// One post-readiness stdout line (e.g. `MIRAGE_EVICTED <bytes>`).
    fn read_stdout_line(&mut self, _timeout: Duration) -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "host stdout replies are not supported",
        ))
    }
    fn stop(&mut self) -> io::Result<HostExit>;
}
pub trait Launcher: Send + Sync {
    type Child: ManagedChild;
    fn launch(&self, spec: &HostSpec) -> io::Result<Self::Child>;
}

pub struct StdChild {
    child: Child,
    stdin: Option<ChildStdin>,
    ready: Receiver<io::Result<()>>,
    lines: Receiver<io::Result<String>>,
    stderr_tail: Arc<Mutex<String>>,
}
impl ManagedChild for StdChild {
    fn wait_ready(&mut self, timeout: Duration) -> io::Result<bool> {
        match self.ready.recv_timeout(timeout) {
            Ok(result) => result.map(|()| true),
            Err(RecvTimeoutError::Timeout) => Ok(false),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host readiness channel closed",
            )),
        }
    }
    fn try_exit(&mut self) -> io::Result<Option<HostExit>> {
        self.child.try_wait().map(|value| {
            value.map(|status| HostExit {
                success: status.success(),
                code: status.code(),
            })
        })
    }
    fn send_line(&mut self, line: &str) -> io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host control pipe is unavailable",
            )
        })?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }
    fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|tail| tail.clone())
            .unwrap_or_default()
    }
    fn read_stdout_line(&mut self, timeout: Duration) -> io::Result<String> {
        match self.lines.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "filesystem host did not answer the control command",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host stdout closed",
            )),
        }
    }
    fn stop(&mut self) -> io::Result<HostExit> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(HostExit {
                success: status.success(),
                code: status.code(),
            });
        }
        let mut stdin = self.stdin.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host control pipe is unavailable",
            )
        })?;
        stdin.write_all(b"STOP\n")?;
        stdin.flush()?;
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(12);
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                let status = self.child.wait()?;
                return Ok(HostExit {
                    success: false,
                    code: status.code(),
                });
            }
            thread::sleep(Duration::from_millis(25));
        };
        Ok(HostExit {
            success: status.success(),
            code: status.code(),
        })
    }
}
#[derive(Default)]
pub struct StdLauncher;
impl Launcher for StdLauncher {
    type Child = StdChild;
    fn launch(&self, spec: &HostSpec) -> io::Result<StdChild> {
        let mut command = Command::new(&spec.executable);
        command.args(host_args(spec));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        command.env("PATH", winfsp_runtime_search_path()?);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host stdout was not captured",
            )
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host stdin was not captured",
            )
        })?;
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            thread::spawn(move || {
                const CAP: usize = 8192;
                let mut reader = BufReader::new(stderr);
                let mut buf = [0_u8; 1024];
                while let Ok(read) = reader.read(&mut buf) {
                    if read == 0 {
                        break;
                    }
                    if let Ok(mut guard) = tail.lock() {
                        guard.push_str(&String::from_utf8_lossy(&buf[..read]));
                        let excess = guard.len().saturating_sub(CAP);
                        if excess > 0 {
                            guard.drain(..excess);
                        }
                    }
                }
            });
        }
        let (sender, ready) = mpsc::sync_channel(1);
        let (line_sender, lines) = mpsc::sync_channel::<io::Result<String>>(16);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let result = reader.read_line(&mut line).and_then(|read| {
                if read != 0 && line.trim() == "MIRAGE_READY" {
                    Ok(())
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "filesystem host did not emit the readiness marker",
                    ))
                }
            });
            let ready_ok = result.is_ok();
            let _ = sender.send(result);
            if !ready_ok {
                return;
            }
            // Post-readiness replies (MIRAGE_EVICTED, …) keep flowing to
            // whoever requested them; EOF ends the pump.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => return,
                    Ok(_) => {
                        if line_sender.send(Ok(line.trim().to_owned())).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = line_sender.send(Err(error));
                        return;
                    }
                }
            }
        });
        Ok(StdChild {
            child,
            stdin: Some(stdin),
            ready,
            lines,
            stderr_tail,
        })
    }
}

fn host_args(spec: &HostSpec) -> Vec<std::ffi::OsString> {
    let mut args = vec![
        spec.mount_point.clone().into_os_string(),
        spec.index.clone().into_os_string(),
        spec.state_root.clone().into_os_string(),
        spec.owner_sid.clone().into(),
        std::ffi::OsString::from(if spec.managed { "--managed" } else { "--cache" }),
        spec.volume_total_bytes.to_string().into(),
        spec.volume_free_bytes.to_string().into(),
    ];
    if let Some(origin_root) = &spec.origin_root {
        args.push("--origin".into());
        args.push(origin_root.clone().into_os_string());
    }
    if let Some(floor) = spec.disk_floor {
        args.push("--floor".into());
        args.push(floor.to_string().into());
    }
    if let (Some(manifest), Some(key)) = (&spec.drive_manifest, &spec.repository_key) {
        args.push("--drive-manifest".into());
        args.push(manifest.clone().into_os_string());
        args.push("--repository-key".into());
        args.push(key.clone().into_os_string());
    }
    args
}

#[cfg(windows)]
fn winfsp_runtime_search_path() -> io::Result<std::ffi::OsString> {
    let program_files = std::env::var_os("ProgramFiles(x86)").ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "ProgramFiles(x86) is unavailable")
    })?;
    let runtime_bin = std::fs::read_dir(PathBuf::from(program_files).join("WinFsp/SxS"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.join("winfsp-x64.dll").is_file())
        .max()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "signed WinFsp x64 SxS runtime is unavailable",
            )
        })?;
    std::env::join_paths(std::iter::once(runtime_bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

struct Host<C> {
    child: Option<C>,
    state: HostState,
}
pub struct Supervisor<L: Launcher> {
    launcher: L,
    hosts: BTreeMap<HostId, Host<L::Child>>,
}
impl<L: Launcher> Supervisor<L> {
    pub fn new(launcher: L) -> Self {
        Self {
            launcher,
            hosts: BTreeMap::new(),
        }
    }
    pub fn start(&mut self, id: HostId, spec: &HostSpec) -> io::Result<()> {
        if matches!(
            self.hosts.get(&id).map(|host| host.state),
            Some(HostState::Running)
        ) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "host already running",
            ));
        }
        let child = self.launcher.launch(spec)?;
        self.hosts.insert(
            id,
            Host {
                child: Some(child),
                state: HostState::Running,
            },
        );
        Ok(())
    }
    pub fn poll(&mut self) -> io::Result<Vec<(HostId, HostState)>> {
        let mut changed = Vec::new();
        for (id, host) in &mut self.hosts {
            if host.state != HostState::Running {
                continue;
            }
            if let Some(exit) = host
                .child
                .as_mut()
                .expect("running host has child")
                .try_exit()?
            {
                host.child = None;
                host.state = if exit.success {
                    HostState::Stopped
                } else {
                    HostState::Crashed(exit)
                };
                changed.push((id.clone(), host.state));
            }
        }
        Ok(changed)
    }
    pub fn wait_ready(&mut self, id: &HostId, timeout: Duration) -> io::Result<bool> {
        let host = self
            .hosts
            .get_mut(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown host"))?;
        if host.state != HostState::Running {
            return Ok(false);
        }
        host.child
            .as_mut()
            .expect("running host has child")
            .wait_ready(timeout)
    }
    /// Writes one control line to a running host's stdin.
    /// Bounded stderr tail of a live host, for launch-failure diagnosis.
    pub fn stderr_tail(&self, id: &HostId) -> String {
        self.hosts
            .get(id)
            .map(|host| {
                host.child
                    .as_ref()
                    .map(|child| child.stderr_tail())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Sends `EVICT <bytes>` and waits for `MIRAGE_EVICTED <freed>
    /// [<blocked>]` on the host's stdout; other lines are skipped until the
    /// reply or timeout. `blocked` counts bytes skipped solely for pins.
    pub fn request_eviction(
        &mut self,
        id: &HostId,
        bytes: u64,
        timeout: Duration,
    ) -> io::Result<(u64, u64)> {
        self.send_line(id, &format!("EVICT {bytes}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "EVICT reply timed out"))?;
            let line = self
                .hosts
                .get_mut(id)
                .and_then(|host| host.child.as_mut())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown host"))?
                .read_stdout_line(remaining)?;
            if let Some(rest) = line.strip_prefix("MIRAGE_EVICTED ") {
                let mut parts = rest.split_whitespace();
                let freed = parts.next().and_then(|v| v.parse::<u64>().ok());
                let blocked = parts
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                return freed.map(|freed| (freed, blocked)).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "malformed MIRAGE_EVICTED reply")
                });
            }
        }
    }

    /// Asks a running host to reload its pinned-inode set after a pin change.
    pub fn reload_pins(&mut self, id: &HostId) -> io::Result<()> {
        self.send_line(id, "PINS-RELOAD")
    }

    pub fn send_line(&mut self, id: &HostId, line: &str) -> io::Result<()> {
        let host = self
            .hosts
            .get_mut(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown host"))?;
        if host.state != HostState::Running {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "filesystem host is not running",
            ));
        }
        host.child
            .as_mut()
            .expect("running host has child")
            .send_line(line)
    }
    pub fn stop(&mut self, id: &HostId) -> io::Result<HostState> {
        let host = self
            .hosts
            .get_mut(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown host"))?;
        if host.state == HostState::Running {
            let exit = host
                .child
                .as_mut()
                .expect("running host has child")
                .stop()?;
            host.child = None;
            host.state = if exit.success {
                HostState::Stopped
            } else {
                HostState::Crashed(exit)
            };
        }
        Ok(host.state)
    }
    pub fn stop_all(&mut self) -> io::Result<()> {
        let ids = self.hosts.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let _ = self.stop(&id)?;
        }
        Ok(())
    }
    pub fn state(&self, id: &HostId) -> Option<HostState> {
        self.hosts.get(id).map(|host| host.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(managed: bool) -> HostSpec {
        HostSpec {
            executable: PathBuf::from("mirage-fs.exe"),
            mount_point: PathBuf::from("M:"),
            index: PathBuf::from("state/mount.idx"),
            state_root: PathBuf::from("state"),
            owner_sid: "S-1-1-0".into(),
            volume_total_bytes: 1024,
            volume_free_bytes: 512,
            origin_root: Some(PathBuf::from("objects")),
            managed,
            disk_floor: None,
            drive_manifest: None,
            repository_key: None,
        }
    }

    #[test]
    fn managed_host_args_carry_drive_provider_paths() {
        let mut spec = spec(true);
        spec.drive_manifest = Some(PathBuf::from("import/drive-manifest.cbor"));
        spec.repository_key = Some(PathBuf::from("import/repository-key.dpapi"));
        let args = host_args(&spec);
        assert_eq!(args[9], "--drive-manifest");
        assert_eq!(args[10], PathBuf::from("import/drive-manifest.cbor"));
        assert_eq!(args[11], "--repository-key");
        assert_eq!(args[12], PathBuf::from("import/repository-key.dpapi"));
    }

    #[test]
    fn launcher_argv_selects_cache_or_managed_mode() {
        let cache = host_args(&spec(false));
        assert_eq!(cache[4], "--cache");
        let managed = host_args(&spec(true));
        assert_eq!(managed[4], "--managed");
        assert_eq!(managed[7], "--origin");
    }

    #[cfg(windows)]
    #[test]
    fn installed_winfsp_runtime_is_added_to_host_search_path() {
        let search_path = winfsp_runtime_search_path().expect("installed WinFsp runtime");
        assert!(
            std::env::split_paths(&search_path)
                .any(|path| { path.join("winfsp-x64.dll").is_file() })
        );
    }
}
