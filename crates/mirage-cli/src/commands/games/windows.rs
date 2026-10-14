//! Windows process handles and allocation measurements. Unsafe code stays in
//! these OS boundaries; no game memory is inspected.
use super::*;
use std::{
    os::windows::{ffi::OsStrExt, process::CommandExt},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE, SetLastError,
        WAIT_OBJECT_0,
    },
    Storage::FileSystem::GetDiskFreeSpaceExW,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            CREATE_NO_WINDOW, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
        },
    },
};

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
struct Process {
    handle: HANDLE,
    pid: u32,
    creation: u64,
    image: PathBuf,
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
impl Process {
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: owned kernel handle; output structures/buffers are writable.
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if handle.is_null() {
                return None;
            }
            let mut creation: FILETIME = std::mem::zeroed();
            let mut exit = creation;
            let mut kernel = creation;
            let mut user = creation;
            let mut image = [0u16; 32768];
            let mut length = image.len() as u32;
            if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0
                || QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut length) == 0
            {
                let error = if WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 {
                    87
                } else {
                    GetLastError()
                };
                CloseHandle(handle);
                SetLastError(error);
                return None;
            }
            Some(Self {
                handle,
                pid,
                creation: (u64::from(creation.dwHighDateTime) << 32)
                    | u64::from(creation.dwLowDateTime),
                image: PathBuf::from(String::from_utf16_lossy(&image[..length as usize])),
            })
        }
    }
    fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle, 0) != WAIT_OBJECT_0 }
    }
}
pub(super) fn pid_alive(pid: u32, creation: Option<u64>) -> bool {
    Process::open(pid).is_some_and(|process| {
        creation.is_none_or(|time| time == process.creation) && process.alive()
    })
}
pub(super) fn creation(pid: u32) -> Option<u64> {
    Process::open(pid).map(|p| p.creation)
}
pub(super) fn recover_orphan(root: &Path, state: &State) -> Result<(), MirageError> {
    let owner_path = root.join("cache/owner.lock");
    ordinary(&owner_path)?;
    if let Ok(file) = OpenOptions::new().read(true).write(true).open(owner_path)
        && file.try_lock().is_ok()
    {
        return Ok(());
    }
    let record = match read::<retention::OwnerRecord>(&root.join("cache/source-owner.json")) {
        Ok(record) => record,
        Err(_) if state.host_pid.is_none() => return Ok(()),
        Err(error) => return Err(error),
    };
    let Some(process) = Process::open(record.pid) else {
        return Ok(());
    };
    if !process.alive() || process.creation != record.creation_100ns {
        return Ok(());
    }
    let expected = std::env::current_exe()
        .map_err(MirageError::from)?
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("controller path"))?
        .join("mirage-fs.exe");
    if !same(&process.image, &record.executable)
        || !same(&process.image, &expected)
        || !record.config.starts_with(root.join("generations"))
        || record.config.file_name() != Some(std::ffi::OsStr::new("source.json"))
    {
        return Err(MirageError::repository_conflict(
            "unrecognized live source owner; automatic recovery refused",
        ));
    }
    // Worker lock is held and every game process is proven gone. Reopen the
    // exact PID instance with terminate permission, then recheck creation time
    // immediately before recovery-only termination. No unrelated host is used.
    use windows_sys::Win32::System::Threading::{PROCESS_TERMINATE, TerminateProcess};
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            record.pid,
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let recovery = Process {
        handle,
        pid: record.pid,
        creation: record.creation_100ns,
        image: process.image.clone(),
    };
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe {
        GetProcessTimes(
            recovery.handle,
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
        || ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
            != record.creation_100ns
    {
        return Err(MirageError::repository_conflict(
            "source owner identity changed during recovery",
        ));
    }
    if unsafe { TerminateProcess(recovery.handle, 1) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { WaitForSingleObject(recovery.handle, 10000) } != WAIT_OBJECT_0 {
        return Err(MirageError::repository_conflict(
            "orphan source did not exit",
        ));
    }
    Ok(())
}
fn processes() -> Result<Vec<Process>, MirageError> {
    // SAFETY: snapshot is owned and closed; PROCESSENTRY32W has required size.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut result = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more != 0 {
            if let Some(process) = Process::open(entry.th32ProcessID) {
                result.push(process);
            } else if std::io::Error::last_os_error().raw_os_error() != Some(87) {
                let length = entry
                    .szExeFile
                    .iter()
                    .position(|u| *u == 0)
                    .unwrap_or(entry.szExeFile.len());
                if String::from_utf16_lossy(&entry.szExeFile[..length])
                    .eq_ignore_ascii_case("GenshinImpact.exe")
                {
                    CloseHandle(snapshot);
                    let error = std::io::Error::last_os_error();
                    return Err(MirageError::repository_conflict(format!(
                        "game process identity is inaccessible; exit cannot be established ({error})"
                    )));
                }
            }
            more = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        Ok(result)
    }
}
fn same(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .trim_start_matches(r"\??\")
            .replace('/', "\\")
            .to_lowercase()
    };
    normalize(a) == normalize(b)
}
pub(super) fn game_running(root: &Path) -> Result<bool, MirageError> {
    let prefix = root.join("generations").to_string_lossy().to_lowercase();
    Ok(processes()?.iter().any(|p| {
        p.alive()
            && p.image
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&format!("{prefix}\\"))
            && p.image
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("GenshinImpact.exe"))
    }))
}
pub(super) fn available(root: &Path) -> Result<u64, MirageError> {
    let path = wide(root);
    let mut bytes = 0;
    // SAFETY: NUL-terminated path, writable output u64.
    let ok =
        unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut bytes, ptr::null_mut(), ptr::null_mut()) };
    if ok == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(bytes)
    }
}
pub(super) fn physical_tree(root: &Path) -> Result<u64, MirageError> {
    ordinary(root)?;
    let managed = root
        .ancestors()
        .find(|p| p.join("game.json").is_file())
        .map(Path::to_path_buf);
    let mut directories = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut count = 0;
    use std::os::windows::fs::MetadataExt;
    while let Some(directory) = directories.pop() {
        for item in std::fs::read_dir(directory).map_err(MirageError::from)? {
            count += 1;
            if count > 2_000_000 {
                return Err(MirageError::invalid_argument(
                    "game allocation inventory exceeds file-count bound",
                ));
            }
            let item = item.map_err(MirageError::from)?;
            let path = item.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(MirageError::from)?;
            if metadata.file_attributes() & 0x400 != 0 {
                let allowed = managed.as_ref().is_some_and(|managed| {
                    if path == managed.join("mount") {
                        return true;
                    }
                    install::ASSET_PREFIXES.iter().any(|prefix| {
                        path.ends_with(prefix.trim_end_matches('/'))
                            && std::fs::read_link(&path).ok().is_some_and(|target| {
                                same(
                                    &target,
                                    &managed.join("mount").join(prefix.trim_end_matches('/')),
                                )
                            })
                    })
                });
                if !allowed {
                    return Err(MirageError::invalid_argument(
                        "physical accounting found an unregistered reparse target",
                    ));
                }
                continue;
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() {
                let allocation = retention::allocated_bytes(&path).map_err(source_error)?;
                total = total
                    .checked_add(allocation)
                    .ok_or_else(|| MirageError::invalid_argument("physical accounting overflow"))?;
            }
        }
    }
    Ok(total)
}
fn junction(link: &Path, target: &Path) -> Result<(), MirageError> {
    ordinary(link)?;
    if link.exists() || std::fs::symlink_metadata(link).is_ok() {
        return Err(MirageError::repository_conflict(
            "managed asset presentation target is occupied",
        ));
    }
    std::fs::create_dir_all(
        link.parent()
            .ok_or_else(|| MirageError::invalid_argument("asset link parent"))?,
    )
    .map_err(MirageError::from)?;
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
    let script = format!(
        "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path {} -Value {} | Out-Null",
        quote(link),
        quote(target)
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(MirageError::from)?;
    if !status.success()
        || !same(
            &std::fs::read_link(link).map_err(MirageError::from)?,
            target,
        )
    {
        return Err(MirageError::repository_conflict(
            "asset junction creation failed",
        ));
    }
    Ok(())
}

fn winfsp_runtime_search_path() -> std::io::Result<std::ffi::OsString> {
    let program_files = std::env::var_os("ProgramFiles(x86)").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "ProgramFiles(x86) is unavailable",
        )
    })?;
    let runtime_bin = std::fs::read_dir(PathBuf::from(program_files).join("WinFsp/SxS"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.join("winfsp-x64.dll").is_file())
        .max()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "signed WinFsp x64 SxS runtime is unavailable",
            )
        })?;
    std::env::join_paths(std::iter::once(runtime_bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub(super) fn play(root: &Path) -> Result<(), MirageError> {
    let _owner = lock(root)?;
    let spec: Spec = read(&root.join("game.json"))?;
    spec.validate()?;
    if spec.root != root {
        return Err(MirageError::invalid_argument(
            "game registration root mismatch",
        ));
    }
    let mut state: State = read(&root.join("session.json"))?;
    ensure_stopped(root, &state)?;
    let active = generation(root, &state)?;
    let config_path = active.join("source.json");
    let mut config: Value = read(&config_path)?;
    config["retained_chunks"] = json!([]);
    write(&config_path, &config)?;
    phase(root, &mut state, "preparing")?;
    {
        let source = SophonSource::open(&config_path).map_err(source_error)?;
        state.prepared = source
            .prepare_learned(spec.startup_budget_bytes)
            .map_err(source_error)?;
    }
    config["retained_chunks"] = json!(state.prepared);
    write(&config_path, &config)?;
    let native = active.join("native");
    let mount = root.join("mount");
    let executable = std::env::current_exe().map_err(MirageError::from)?;
    let host_exe = executable
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("controller location"))?
        .join("mirage-fs.exe");
    let sid_output = Command::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(MirageError::from)?;
    let sid_text = String::from_utf8_lossy(&sid_output.stdout);
    let sid = sid_text
        .split(',')
        .nth(1)
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| s.starts_with("S-1-"))
        .ok_or_else(|| MirageError::invalid_argument("interactive owner identity unavailable"))?;
    let mut host = Command::new(&host_exe)
        .env(
            "PATH",
            winfsp_runtime_search_path().map_err(MirageError::from)?,
        )
        .arg(&mount)
        .arg(&config_path)
        .arg(root)
        .arg(sid)
        .args([
            "--sophon",
            "1099511627776",
            "0",
            "--label",
            "Genshin Assets",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(MirageError::from)?;
    let host_identity = Process::open(host.id())
        .ok_or_else(|| MirageError::repository_conflict("source host identity unavailable"))?;
    state.host_pid = Some(host_identity.pid);
    state.host_creation_100ns = Some(host_identity.creation);
    save(root, &state)?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let output = host
        .stdout
        .take()
        .ok_or_else(|| MirageError::internal_invariant("source stdout"))?;
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(output)
            .lines()
            .map_while(Result::ok)
        {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut input = host
        .stdin
        .take()
        .ok_or_else(|| MirageError::internal_invariant("source control pipe"))?;
    let mut tracked: Vec<Process> = Vec::new();
    let result = (|| -> Result<(), MirageError> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if receiver
                .recv_timeout(Duration::from_millis(100))
                .ok()
                .as_deref()
                == Some("MIRAGE_READY")
            {
                break;
            }
            if host.try_wait().map_err(MirageError::from)?.is_some() || Instant::now() >= deadline {
                return Err(MirageError::backend_unavailable(
                    "source mount did not become ready",
                ));
            }
        }
        for prefix in install::ASSET_PREFIXES {
            let link = native.join(prefix.trim_end_matches('/'));
            let target = mount.join(prefix.trim_end_matches('/'));
            match std::fs::symlink_metadata(&link) {
                Ok(_)
                    if std::fs::read_link(&link)
                        .ok()
                        .is_some_and(|p| same(&p, &target)) => {}
                Ok(_) => {
                    return Err(MirageError::repository_conflict(
                        "asset junction changed; repair before playing",
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => junction(&link, &target)?,
                Err(e) => return Err(e.into()),
            }
        }
        phase(root, &mut state, "waiting_for_game")?;
        let game_exe = native.join("GenshinImpact.exe");
        let launcher = spec.launcher.as_ref().unwrap_or(&game_exe);
        let _launcher = Command::new(launcher)
            .current_dir(&native)
            .spawn()
            .map_err(MirageError::from)?;
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut observed = false;
        let mut quiet_since = None;
        loop {
            for process in processes()? {
                if process.alive()
                    && same(&process.image, &game_exe)
                    && !tracked
                        .iter()
                        .any(|p| p.pid == process.pid && p.creation == process.creation)
                {
                    state.game_pid = Some(process.pid);
                    // Retain the exact handle before any fallible pipe/journal
                    // operation. Error unwinding must still protect the game.
                    let pid = process.pid;
                    tracked.push(process);
                    #[cfg(debug_assertions)]
                    if std::env::var_os("MIRAGE_TEST_GAME_MONITOR_FAILURE").is_some() {
                        return Err(MirageError::repository_conflict(
                            "injected fixture monitoring failure",
                        ));
                    }
                    writeln!(input, "GAMEPLAY {}", pid)
                        .and_then(|()| input.flush())
                        .map_err(MirageError::from)?;
                    phase(root, &mut state, "playing")?;
                    observed = true;
                }
            }
            tracked.retain(Process::alive);
            if observed && tracked.is_empty() {
                let since = quiet_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(10) {
                    break;
                }
            } else {
                quiet_since = None;
            }
            if !observed && Instant::now() >= deadline {
                return Err(MirageError::repository_conflict(
                    "launcher did not start this managed game within ten minutes; select its native folder in HoYoPlay",
                ));
            }
            // A source failure never establishes that the game exited. Continue
            // holding worker ownership until every observed game handle exits.
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    })();
    if result.is_err() {
        state.phase = "monitoring_failed".into();
        state.error = result.as_ref().err().map(|error| error.message.clone());
        let _ = save(root, &state);
        // Keep the host and worker alive through known process instances even
        // when monitoring/journaling/control fails. Recheck detached siblings.
        loop {
            let any_known = tracked.iter().any(Process::alive);
            match game_running(root) {
                Ok(false) if !any_known => break,
                Ok(_) => std::thread::sleep(Duration::from_millis(200)),
                Err(_) => return result, // leave source for exclusive recovery
            }
        }
    }
    // Prove game exit before the first stop/unmount request, including the
    // successful handoff path's quiet interval.
    if game_running(root)? {
        phase(root, &mut state, "cleanup_pending")?;
        return Err(MirageError::repository_conflict(
            "a game process remains active",
        ));
    }
    let _ = writeln!(input, "GAMEPLAY 0");
    let _ = writeln!(input, "STOP");
    let _ = input.flush();
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.try_wait().map_err(MirageError::from)?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if host.try_wait().map_err(MirageError::from)?.is_none() {
        state.phase = "cleanup_pending".into();
        state.error = Some("source did not drain; payload cleanup is deferred".into());
        save(root, &state)?;
        return Err(MirageError::repository_conflict(
            "source did not drain; cleanup remains pending",
        ));
    }
    state.host_pid = None;
    state.host_creation_100ns = None;
    state.game_pid = None;
    if game_running(root)? {
        phase(root, &mut state, "cleanup_pending")?;
        return Err(MirageError::repository_conflict(
            "a game process remains active",
        ));
    }
    do_cleanup(&spec, &mut state)?;
    if let Err(error) = &result {
        state.phase = "launch_failed".into();
        state.error = Some(error.message.clone());
        save(root, &state)?;
    }
    result
}
