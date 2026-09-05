use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
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
        command
            .arg(&spec.mount_point)
            .arg(&spec.index)
            .arg(&spec.state_root)
            .arg(&spec.owner_sid)
            .arg("--cache")
            .arg(spec.volume_total_bytes.to_string())
            .arg(spec.volume_free_bytes.to_string())
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
        let (sender, ready) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout)
                .read_line(&mut line)
                .and_then(|read| {
                    if read != 0 && line.trim() == "MIRAGE_READY" {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "filesystem host did not emit the readiness marker",
                        ))
                    }
                });
            let _ = sender.send(result);
        });
        Ok(StdChild {
            child,
            stdin: Some(stdin),
            ready,
        })
    }
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn installed_winfsp_runtime_is_added_to_host_search_path() {
        let search_path = winfsp_runtime_search_path().expect("installed WinFsp runtime");
        assert!(
            std::env::split_paths(&search_path)
                .any(|path| { path.join("winfsp-x64.dll").is_file() })
        );
    }
}
