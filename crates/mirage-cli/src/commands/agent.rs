//! Per-user logon agent: keeps this user's managed Drive-backed volumes
//! mounted and their filesystem hosts supplied with fresh Drive access
//! tokens (which expire after about an hour). Registered under
//! `HKCU\...\Run` on first successful sign-in; runs hidden, single-instance,
//! and never logs tokens.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mirage_types::{GenerationId, MirageError, RepositoryId};

use super::{backend_login, drive_live, service, volume};

const REFRESH_SECONDS: u64 = 45 * 60;
const RETRY_SECONDS: u64 = 60;
const LOG_MAX_BYTES: u64 = 512 * 1024;

pub fn run(once: bool) -> Result<(), MirageError> {
    let _guard = match AgentMutex::acquire()? {
        Some(guard) => guard,
        None => return Ok(()),
    };
    let log = AgentLog::open();
    loop {
        if let Err(error) = cycle(&log, once) {
            log.line(&format!("cycle failed: {error}"));
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(REFRESH_SECONDS));
    }
}

fn cycle(log: &AgentLog, once: bool) -> Result<(), MirageError> {
    let credentials = match backend_login::oauth_client_credentials(None) {
        Ok(credentials) => credentials,
        Err(_) => {
            log.line("oauth-desktop.json not found; waiting for sign-in");
            if !once {
                std::thread::sleep(Duration::from_secs(RETRY_SECONDS));
            }
            return Ok(());
        }
    };
    let session = match drive_live::connect(&credentials, None) {
        Ok(session) => session,
        Err(error) => {
            log.line(&format!("Drive session refresh failed: {error}"));
            if !once {
                std::thread::sleep(Duration::from_secs(RETRY_SECONDS));
            }
            return Ok(());
        }
    };
    let status = match service::request_json(mirage_ipc::Command::Status) {
        Ok(status) => status,
        Err(error) => {
            log.line(&format!("service unreachable: {error}"));
            if !once {
                std::thread::sleep(Duration::from_secs(RETRY_SECONDS));
            }
            return Ok(());
        }
    };
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
        let detail =
            match service::request_json(mirage_ipc::Command::RepositoryDetail { repository_id }) {
                Ok(detail) => detail,
                Err(error) => {
                    log.line(&format!("{id_text}: detail failed: {error}"));
                    continue;
                }
            };
        if detail["origin"].as_str() != Some("drive")
            || detail["volume_mode"].as_str() != Some("managed")
        {
            continue;
        }
        match repository["state"].as_str() {
            Some("ready_mounted") => {
                let token =
                    mirage_ipc::SensitiveString::new(session.access_token.as_str().to_owned())?;
                match service::request_json(mirage_ipc::Command::DriveTokenSupply {
                    repository_id,
                    drive_access_token: token,
                }) {
                    Ok(_) => log.line(&format!("{id_text}: supplied Drive token")),
                    Err(error) => log.line(&format!("{id_text}: token supply failed: {error}")),
                }
            }
            Some("ready_unmounted") => {
                let Some(generation) = detail["active_generation"].as_u64() else {
                    log.line(&format!("{id_text}: no active generation; skipping mount"));
                    continue;
                };
                let letter = mount_letter(repository_id, &detail);
                let token =
                    mirage_ipc::SensitiveString::new(session.access_token.as_str().to_owned())?;
                match service::request_json(mirage_ipc::Command::Mount {
                    repository_id,
                    generation: GenerationId::from_u64(generation),
                    drive_letter: Some(letter.clone()),
                    drive_access_token: Some(token),
                }) {
                    Ok(_) => log.line(&format!("{id_text}: mounted on {letter}")),
                    Err(error) => log.line(&format!("{id_text}: mount failed: {error}")),
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Prefer the letter recorded in the volume's mount record; otherwise the
/// first free letter from M.
fn mount_letter(repository_id: RepositoryId, detail: &serde_json::Value) -> String {
    if let Some(letter) = detail["mount_point"]
        .as_str()
        .and_then(|point| point.chars().next())
        .filter(|letter| letter.is_ascii_alphabetic())
    {
        return letter.to_ascii_uppercase().to_string();
    }
    let _ = repository_id;
    volume::first_free_letter()
        .map(|letter| letter.to_string())
        .unwrap_or_else(|_| "M".to_owned())
}

/// Registers `mirage.exe agent` under the per-user Run key so volumes
/// reconnect at every Windows sign-in. No elevation required.
/// The Run-key command line for the agent. When called in-process from a
/// sibling binary (mirage-ui.exe), resolve `mirage.exe` next to it rather
/// than registering the UI executable — it ignores arguments.
#[cfg(windows)]
fn agent_command_line(current_exe: &Path) -> Result<String, MirageError> {
    let executable = if current_exe.file_stem().is_some_and(|stem| stem == "mirage") {
        current_exe.to_path_buf()
    } else {
        current_exe
            .parent()
            .map(|dir| dir.join("mirage.exe"))
            .unwrap_or_else(|| PathBuf::from("mirage.exe"))
    };
    if !executable.is_file() {
        return Err(MirageError::invalid_argument(format!(
            "agent executable not found beside {}: {}",
            current_exe.display(),
            executable.display()
        )));
    }
    Ok(format!("\"{}\" agent", executable.display()))
}

pub fn install_logon_registration() -> Result<(), MirageError> {
    #[cfg(windows)]
    {
        install_logon_registration_windows()
    }
    #[cfg(not(windows))]
    {
        Err(MirageError::provider_unavailable(
            "logon registration requires Windows",
        ))
    }
}

#[cfg(windows)]
fn install_logon_registration_windows() -> Result<(), MirageError> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegSetValueExW,
    };

    let executable = std::env::current_exe().map_err(MirageError::from)?;
    let command = agent_command_line(&executable)?;
    let subkey: Vec<u16> = OsStr::new(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let value_name: Vec<u16> = OsStr::new("MirageSSD")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut key = std::ptr::null_mut();
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Err(MirageError::from(std::io::Error::from_raw_os_error(
            result as i32,
        )));
    }
    let mut data: Vec<u8> = command
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let set = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr(),
            data.len() as u32,
        )
    };
    unsafe { RegCloseKey(key) };
    data.fill(0);
    if set != 0 {
        return Err(MirageError::from(std::io::Error::from_raw_os_error(
            set as i32,
        )));
    }
    Ok(())
}

/// Hidden single-instance guard: the agent exits quietly if another copy is
/// already running for this session.
#[cfg(windows)]
struct AgentMutex(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl AgentMutex {
    fn acquire() -> Result<Option<Self>, MirageError> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = OsStr::new(r"Local\MirageSSD.Agent")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(MirageError::from(std::io::Error::last_os_error()));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

#[cfg(windows)]
impl Drop for AgentMutex {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
struct AgentMutex;

#[cfg(not(windows))]
impl AgentMutex {
    fn acquire() -> Result<Option<Self>, MirageError> {
        Ok(Self)
    }
}

struct AgentLog {
    path: PathBuf,
}

impl AgentLog {
    fn open() -> Self {
        let directory = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("MirageSSD")
            .join("logs");
        let _ = std::fs::create_dir_all(&directory);
        Self {
            path: directory.join("agent.log"),
        }
    }

    /// Append one line; rotates the log to agent.log.1 at 512 KiB. Never
    /// called with token material.
    fn line(&self, message: &str) {
        if self
            .path
            .metadata()
            .map(|metadata| metadata.len() > LOG_MAX_BYTES)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{stamp} {message}");
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn agent_command_line_prefers_mirage_exe_beside_the_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = dir.path().join("mirage.exe");
        std::fs::write(&cli, b"x").expect("cli stub");
        let ui = dir.path().join("mirage-ui.exe");
        let command = agent_command_line(&ui).expect("command");
        assert_eq!(command, format!("\"{}\" agent", cli.display()));
    }

    #[test]
    fn agent_command_line_uses_mirage_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cli = dir.path().join("mirage.exe");
        std::fs::write(&cli, b"x").expect("cli stub");
        let command = agent_command_line(&cli).expect("command");
        assert_eq!(command, format!("\"{}\" agent", cli.display()));
    }

    #[test]
    fn agent_command_line_fails_when_mirage_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ui = dir.path().join("mirage-ui.exe");
        let error = agent_command_line(&ui).expect_err("must fail without mirage.exe");
        assert!(format!("{error}").contains("agent executable not found"));
    }
}
