//! Hardened loopback bridge for the MirageSSD management UI.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(windows, allow(unsafe_code))]

#[cfg(windows)]
#[cfg(windows)]
mod pin_quick_access;
#[cfg(windows)]
mod tray;
mod update_check;

#[cfg(windows)]
mod windows_host {
    use mirage_ipc::{
        Command, DriveQuotaSnapshot, MAX_FRAME_BYTES, Request, Response, ResponseBody,
        SensitiveString, decode_frame, encode_frame,
    };
    use mirage_types::{MirageError, MirageErrorKind};
    use std::{
        collections::BTreeMap,
        ffi::OsStr,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
        time::Duration,
    };
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

    const MAX_HTTP_HEADER_BYTES: usize = 32 * 1024;
    const MAX_HTTP_BODY_BYTES: usize = MAX_FRAME_BYTES;

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let executable = std::env::current_exe()?;
        let default_root = executable
            .parent()
            .ok_or("UI executable has no parent directory")?
            .join("ui");
        let mut ui_root = default_root;
        let mut no_open = false;
        let mut tray = false;
        let mut health_check = false;
        let mut drive_token_store = None;
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_string_lossy().as_ref() {
                "--ui-root" => {
                    ui_root = PathBuf::from(arguments.next().ok_or("--ui-root needs a path")?);
                }
                "--no-open" => no_open = true,
                "--tray" => tray = true,
                "--health-check" => health_check = true,
                "--drive-token-store" => {
                    let path =
                        PathBuf::from(arguments.next().ok_or("--drive-token-store needs a path")?);
                    if !path.is_absolute() {
                        return Err("--drive-token-store must be absolute".into());
                    }
                    drive_token_store = Some(path);
                }
                _ => return Err("unsupported MirageSSD UI argument".into()),
            }
        }
        validate_ui_root(&ui_root)?;
        if health_check {
            return Ok(());
        }

        // One UI per user: a second launch exits quietly; the first instance
        // already owns the browser tab and the listener.
        let _single_instance = {
            let name: Vec<u16> = r"Local\MirageSSD.UI".encode_utf16().chain([0]).collect();
            let handle = unsafe {
                windows_sys::Win32::System::Threading::CreateMutexW(
                    std::ptr::null(),
                    0,
                    name.as_ptr(),
                )
            };
            if handle.is_null() {
                return Err("UI single-instance mutex failed".into());
            }
            const ERROR_ALREADY_EXISTS: u32 = 183;
            if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
                // A tray instance owns the UI — ask it to surface the window.
                let name: Vec<u16> = r"Local\MirageSSD.UI.Show"
                    .encode_utf16()
                    .chain([0])
                    .collect();
                let event = unsafe {
                    windows_sys::Win32::System::Threading::OpenEventW(
                        windows_sys::Win32::System::Threading::EVENT_MODIFY_STATE,
                        0,
                        name.as_ptr(),
                    )
                };
                if !event.is_null() {
                    unsafe {
                        windows_sys::Win32::System::Threading::SetEvent(event);
                        windows_sys::Win32::Foundation::CloseHandle(event);
                    }
                }
                return Ok(());
            }
            handle
        };

        log_event(
            "ui.started",
            if tray {
                "tray companion"
            } else {
                "window launch"
            },
        );
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let origin = format!("http://127.0.0.1:{}", address.port());
        let token = random_token()?;
        let shared = std::sync::Arc::new(std::sync::Mutex::new(SharedState::default()));
        // If this user has a stored Drive session, (re)register the logon
        // agent idempotently — overwrites entries written by older builds
        // that pointed at mirage-ui.exe instead of mirage.exe.
        let token_store_path = drive_token_store
            .clone()
            .or_else(|| mirage_cli::commands::backend_login::default_token_store_path().ok());
        if let Some(store) = token_store_path.as_deref()
            && store.is_file()
        {
            if let Err(error) = mirage_cli::commands::agent::install_logon_registration() {
                log_event("agent.registration_failed", &error.to_string());
            }
            // An upgrade or a service restart leaves mounted drives without a
            // Drive token until the agent runs; start it now if it is not
            // already running (a second instance exits on the agent mutex).
            if let Err(error) = mirage_cli::commands::agent::spawn_detached() {
                log_event("agent.spawn_failed", &error.to_string());
            }
        }
        // The tray companion keeps the host alive after the browser tab
        // closes; re-register on every start so updates repair the path.
        if let Err(error) = mirage_cli::commands::agent::install_tray_registration() {
            log_event("tray.registration_failed", &error.to_string());
        }
        let page_url = format!("{origin}/#{token}");
        if tray {
            // The tray owns the foreground: the accept loop moves to a
            // worker and the hidden icon lives on this thread.
            let url = page_url.clone();
            let worker_root = ui_root.clone();
            let worker_origin = origin.clone();
            let worker_token = token.clone();
            let worker_store = drive_token_store.clone();
            let worker_shared = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || {
                let _ = serve(
                    listener,
                    &worker_root,
                    &worker_origin,
                    &worker_token,
                    worker_store.as_deref(),
                    &worker_shared,
                );
            });
            crate::tray::run(url);
        }
        if !no_open {
            open_browser(&page_url)?;
        }
        serve(
            listener,
            &ui_root,
            &origin,
            &token,
            drive_token_store.as_deref(),
            &shared,
        )
    }

    fn serve(
        listener: TcpListener,
        ui_root: &Path,
        origin: &str,
        token: &str,
        drive_token_store: Option<&Path>,
        shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for connection in listener.incoming() {
            match connection {
                Ok(mut stream) => {
                    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
                    if let Err(error) = handle_connection(
                        &mut stream,
                        ui_root,
                        origin,
                        token,
                        drive_token_store,
                        shared,
                    ) {
                        let _ = write_response(
                            &mut stream,
                            500,
                            "application/json; charset=utf-8",
                            br#"{"error":"local UI bridge failed"}"#,
                            false,
                        );
                        log_event("request.failed", &error.to_string());
                    }
                }
                Err(error) => log_event("accept.failed", &error.to_string()),
            }
        }
        Ok(())
    }

    fn validate_ui_root(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        for relative in ["index.html", "assets/mirage-ui.js", "assets/mirage-ui.css"] {
            if !root.join(relative).is_file() {
                return Err(format!("management UI asset is missing: {relative}").into());
            }
        }
        Ok(())
    }

    /// Long-running Drive operations are reported through shared state so
    /// the HTTP handler returns promptly and the UI polls for progress.
    #[derive(Default)]
    struct SharedState {
        login: LoginState,
        login_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        create: serde_json::Value,
        set_cache: serde_json::Value,
    }

    #[derive(Default)]
    enum LoginState {
        #[default]
        Idle,
        InFlight,
        Done {
            account_id: String,
        },
        Failed {
            message: String,
        },
    }

    /// `%LOCALAPPDATA%\MirageSSD\logs\ui.log` — persistent UI bridge log
    /// (8 MiB, keep 5). Never log tokens or credentials.
    fn ui_log() -> std::sync::Arc<mirage_observability::RotatingLog> {
        use std::sync::Arc;
        static LOG: std::sync::OnceLock<Arc<mirage_observability::RotatingLog>> =
            std::sync::OnceLock::new();
        Arc::clone(LOG.get_or_init(|| {
            let path = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join("MirageSSD")
                .join("logs")
                .join("ui.log");
            Arc::new(
                mirage_observability::RotatingLog::new(path, 8 * 1024 * 1024, 5)
                    .expect("ui log bounds are nonzero"),
            )
        }))
    }

    fn log_event(event: &str, detail: &str) {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let detail = detail.replace(['\\', '"', '\n', '\r'], "_");
        ui_log().write_line(&format!(
            "{{\"ts\":{seconds},\"event\":\"{event}\",\"detail\":\"{detail}\"}}"
        ));
        eprintln!("MirageSSD UI {event}: {detail}");
    }

    fn authorize(request: &HttpRequest, origin: &str, token: &str) -> Result<(), Vec<u8>> {
        let supplied_origin = request.headers.get("origin").map(String::as_str);
        let supplied_token = request.headers.get("x-mirage-token").map(String::as_str);
        if supplied_origin != Some(origin)
            || !supplied_token.is_some_and(|value| constant_time_eq(value, token))
        {
            return Err(br#"{"error":"forbidden"}"#.to_vec());
        }
        Ok(())
    }

    fn drive_status(shared: &std::sync::Arc<std::sync::Mutex<SharedState>>) -> serde_json::Value {
        let login = shared
            .lock()
            .map(|state| match &state.login {
                LoginState::Idle => serde_json::json!("idle"),
                LoginState::InFlight => serde_json::json!("in_flight"),
                LoginState::Done { account_id } => {
                    serde_json::json!({"done": account_id})
                }
                LoginState::Failed { message } => serde_json::json!({"failed": message}),
            })
            .unwrap_or(serde_json::json!("idle"));
        let store = mirage_cli::commands::backend_login::default_token_store_path()
            .map(mirage_backend_drive::token_store::TokenStore::new);
        let metadata = store.ok().and_then(|store| store.metadata().ok());
        serde_json::json!({
            "authenticated": metadata.is_some(),
            "account_id": metadata.as_ref().map(|m| m.account_id.clone()),
            "issued_unix_seconds": metadata.as_ref().map(|m| m.issued_unix_seconds),
            "login": login,
        })
    }

    fn start_drive_login(
        shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
    ) -> serde_json::Value {
        {
            let mut state = match shared.lock() {
                Ok(state) => state,
                Err(_) => return serde_json::json!({"error": "state lock poisoned"}),
            };
            if matches!(state.login, LoginState::InFlight) {
                return serde_json::json!({"started": false, "in_flight": true});
            }
            state.login = LoginState::InFlight;
            state.login_cancel = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )));
        }
        let cancel = shared.lock().ok().and_then(|s| s.login_cancel.clone());
        std::thread::spawn({
            // The handler thread is parked on a socket; spawn a worker for
            // the blocking OAuth exchange.
            let shared = shared.clone();
            move || {
                let flag = cancel.clone();
                let result = mirage_cli::commands::backend_login::authenticate_cancellable(
                    None, None, None, 600, cancel,
                );
                if let Ok(mut state) = shared.lock() {
                    let cancelled =
                        flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
                    state.login_cancel = None;
                    state.login = match result {
                        _ if cancelled => LoginState::Idle,
                        Ok(outcome) => LoginState::Done {
                            account_id: outcome.account_id,
                        },
                        Err(error) => {
                            let message = if error.to_string().contains("access_denied")
                                || error.to_string().contains("permission_denied")
                            {
                                "Google declined access for this account. Try again or use a different account."
                                    .to_owned()
                            } else {
                                error.to_string()
                            };
                            LoginState::Failed { message }
                        }
                    };
                }
            }
        });
        serde_json::json!({"started": true})
    }

    /// Moves a volume's local cache to another disk on a worker thread;
    /// progress and the outcome are polled through set_cache.
    fn start_volume_set_cache(
        shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
        payload: &serde_json::Value,
        drive_token_store: Option<&Path>,
    ) -> Result<serde_json::Value, MirageError> {
        let repository_id = payload["repository_id"]
            .as_str()
            .ok_or_else(|| MirageError::invalid_argument("repository_id is required"))?
            .parse::<mirage_types::RepositoryId>()?;
        let cache_disk = payload["cache_disk"]
            .as_str()
            .map(str::trim)
            .filter(|disk| !disk.is_empty())
            .map(str::to_owned);
        {
            let mut state = shared
                .lock()
                .map_err(|_| MirageError::internal_invariant("state lock poisoned"))?;
            if state.set_cache["in_flight"].as_bool() == Some(true) {
                return Ok(serde_json::json!({"started": false, "in_flight": true}));
            }
            state.set_cache = serde_json::json!({
                "in_flight": true,
                "step": "starting",
                "repository_id": repository_id.to_string(),
            });
        }
        let token_store = drive_token_store.map(Path::to_path_buf);
        std::thread::spawn({
            let shared = shared.clone();
            move || {
                let progress_shared = shared.clone();
                let mut progress = move |step: &str| {
                    if let Ok(mut state) = progress_shared.lock() {
                        state.set_cache["step"] = serde_json::json!(step);
                    }
                };
                // A remount needs a Drive token; without one the move still
                // completes and the agent remounts when it next signs in.
                let token = futures_executor::block_on(
                    mirage_backend_drive::refresh_stored_session(token_store.as_deref()),
                )
                .ok()
                .and_then(|session| {
                    mirage_ipc::SensitiveString::new(session.access_token.as_str().to_owned()).ok()
                });
                let result = mirage_cli::commands::volume::set_cache(
                    repository_id,
                    cache_disk.as_deref(),
                    token,
                    None,
                    &mut progress,
                );
                if let Ok(mut state) = shared.lock() {
                    state.set_cache = match result {
                        Ok(mut value) => {
                            value["in_flight"] = serde_json::json!(false);
                            value["done"] = serde_json::json!(true);
                            value
                        }
                        Err(error) => serde_json::json!({
                            "in_flight": false,
                            "done": false,
                            "repository_id": repository_id.to_string(),
                            "error": error.to_string(),
                        }),
                    };
                }
            }
        });
        Ok(serde_json::json!({"started": true}))
    }

    fn start_volume_create(
        shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, MirageError> {
        {
            let mut state = shared
                .lock()
                .map_err(|_| MirageError::internal_invariant("state lock poisoned"))?;
            if state.create["in_flight"].as_bool() == Some(true) {
                return Ok(serde_json::json!({"started": false, "in_flight": true}));
            }
            state.create = serde_json::json!({"in_flight": true, "step": "starting"});
        }
        let spec = mirage_cli::commands::volume::VolumeSpec {
            name: payload["name"]
                .as_str()
                .unwrap_or(mirage_cli::commands::volume::default_volume_name())
                .to_owned(),
            drive_letter: payload["letter"].as_str().unwrap_or("M").to_owned(),
            budget_bytes: payload["budget_bytes"]
                .as_u64()
                .map(Ok)
                .unwrap_or_else(mirage_cli::commands::volume::default_budget_bytes)
                .unwrap_or(8 * (1 << 30)),
            cache_disk: match payload["cache_disk"].as_str() {
                Some(disk) if !disk.trim().is_empty() => Some(disk.trim().to_owned()),
                _ => mirage_cli::commands::volume::default_cache_disk().unwrap_or(None),
            },
            floor_bytes: payload["floor_bytes"].as_u64(),
        };
        // The floor guards the disk that holds the cache.
        let spec = mirage_cli::commands::volume::VolumeSpec {
            floor_bytes: spec.floor_bytes.or_else(|| {
                mirage_cli::commands::volume::cache_disk_info(spec.cache_disk.as_deref())
                    .ok()
                    .map(|disk| mirage_cli::commands::volume::default_floor_bytes(&disk))
            }),
            ..spec
        };
        std::thread::spawn({
            let shared = shared.clone();
            move || {
                let progress_shared = shared.clone();
                let mut progress = move |step: &str| {
                    if let Ok(mut state) = progress_shared.lock() {
                        state.create["step"] = serde_json::json!(step);
                    }
                };
                let credentials =
                    mirage_cli::commands::backend_login::oauth_client_credentials(None);
                let result = credentials.and_then(|credentials| {
                    mirage_cli::commands::volume::create(
                        &spec,
                        &credentials,
                        None,
                        &mut progress,
                        None,
                    )
                });
                if let Ok(mut state) = shared.lock() {
                    state.create = match result {
                        Ok(created) => serde_json::json!({
                            "in_flight": false,
                            "done": true,
                            "repository_id": created.repository_id.to_string(),
                            "drive_letter": created.drive_letter,
                            "name": created.name,
                            "budget_bytes": created.budget_bytes,
                            "cache_root": created.cache_root,
                            "account_id": created.account_id,
                        }),
                        Err(error) => serde_json::json!({
                            "in_flight": false,
                            "done": false,
                            "error": error.to_string(),
                        }),
                    };
                }
            }
        });
        Ok(serde_json::json!({"started": true}))
    }

    /// Opens Explorer on a mounted drive letter (validated `X:` shape only).
    fn open_explorer(letter: &str) -> Option<()> {
        let letter = letter.trim().trim_end_matches(':').trim_end_matches('\\');
        let bytes = letter.as_bytes();
        if bytes.len() != 1 || !bytes[0].is_ascii_alphabetic() {
            return None;
        }
        let target = format!("{}:\\", (bytes[0] as char).to_ascii_uppercase());
        std::process::Command::new("explorer.exe")
            .arg(&target)
            .spawn()
            .ok()?;
        Some(())
    }

    fn handle_connection(
        stream: &mut TcpStream,
        ui_root: &Path,
        origin: &str,
        token: &str,
        drive_token_store: Option<&Path>,
        shared: &std::sync::Arc<std::sync::Mutex<SharedState>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let request = read_http_request(stream)?;
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => write_file(
                stream,
                &ui_root.join("index.html"),
                "text/html; charset=utf-8",
                false,
            )?,
            ("GET", "/assets/mirage-ui.js") => write_file(
                stream,
                &ui_root.join("assets/mirage-ui.js"),
                "text/javascript; charset=utf-8",
                true,
            )?,
            ("GET", "/assets/mirage-ui.css") => write_file(
                stream,
                &ui_root.join("assets/mirage-ui.css"),
                "text/css; charset=utf-8",
                true,
            )?,
            ("GET", "/api/disks") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = serde_json::json!({
                    "disks": mirage_cli::commands::volume::disks()
                        .unwrap_or_default()
                        .iter()
                        .map(|disk| serde_json::json!({
                            "volume_root": disk.volume_root,
                            "total_bytes": disk.total_bytes,
                            "free_bytes": disk.free_bytes,
                        }))
                        .collect::<Vec<_>>(),
                    "state_volume": mirage_cli::commands::volume::state_volume()
                        .map(|disk| serde_json::json!({
                            "volume_root": disk.volume_root,
                            "total_bytes": disk.total_bytes,
                            "free_bytes": disk.free_bytes,
                        }))
                        .unwrap_or(serde_json::Value::Null),
                    "default_letter": mirage_cli::commands::volume::first_free_letter()
                        .map(|letter| letter.to_string())
                        .unwrap_or_default(),
                    "free_letters": mirage_cli::commands::volume::free_letters()
                        .unwrap_or_default(),
                    "default_budget_bytes": mirage_cli::commands::volume::default_budget_bytes()
                        .unwrap_or(0),
                    // The disk that will hold the local cache unless the user
                    // picks another: freest fixed disk, or the state disk.
                    "default_cache_disk": mirage_cli::commands::volume::default_cache_disk()
                        .ok()
                        .flatten()
                        .or_else(|| mirage_cli::commands::volume::state_volume()
                            .ok()
                            .map(|disk| disk.volume_root.to_string_lossy().into_owned())),
                });
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("GET", "/api/drive/status") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = drive_status(shared);
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/drive/login") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = start_drive_login(shared);
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/drive/login/cancel") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let cancelled = shared
                    .lock()
                    .map(|mut state| {
                        if let Some(flag) = &state.login_cancel {
                            flag.store(true, std::sync::atomic::Ordering::Relaxed);
                            state.login = LoginState::Idle;
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&serde_json::json!({"cancelled": cancelled}))?,
                    false,
                )?;
            }
            ("POST", "/api/pin-quick-access") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let payload: serde_json::Value =
                    serde_json::from_slice(&request.body).unwrap_or_default();
                let letter = payload["letter"]
                    .as_str()
                    .and_then(|s| s.chars().next())
                    .filter(|c| c.is_ascii_alphabetic());
                let body = match letter {
                    Some(letter) => match crate::pin_quick_access::pin_to_quick_access(letter) {
                        Ok(()) => serde_json::json!({"ok": true}),
                        Err(error) => serde_json::json!({"ok": false, "error": error}),
                    },
                    None => serde_json::json!({"ok": false, "error": "a drive letter is required"}),
                };
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/service/start") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                // `sc start` needs elevation — ShellExecute "runas" surfaces
                // the UAC prompt instead of failing silently.
                let exe = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join("mirage.exe")));
                let started = exe.is_some_and(|exe| {
                    let operation = wide("runas");
                    let file = wide(&exe.to_string_lossy());
                    let params = wide("service start");
                    unsafe {
                        ShellExecuteW(
                            std::ptr::null_mut(),
                            operation.as_ptr(),
                            file.as_ptr(),
                            params.as_ptr(),
                            std::ptr::null(),
                            SW_SHOWNORMAL,
                        ) as isize
                            > 32
                    }
                });
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&serde_json::json!({"ok": started}))?,
                    false,
                )?;
            }
            ("GET", "/api/update/check") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&crate::update_check::check())?,
                    false,
                )?;
            }
            ("POST", "/api/diagnostics/collect") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = match mirage_cli::commands::diagnostics::collect_zip(None) {
                    Ok(path) => serde_json::json!({"ok": true, "path": path}),
                    Err(error) => serde_json::json!({"ok": false, "error": error.to_string()}),
                };
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/drive/logout") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let result = mirage_cli::commands::backend_login::default_token_store_path()
                    .and_then(|path| {
                        mirage_backend_drive::token_store::TokenStore::new(path).delete()
                    });
                let body = serde_json::json!({"signed_out": result.unwrap_or(false)});
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/volume/create") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = match serde_json::from_slice::<serde_json::Value>(&request.body)
                    .ok()
                    .and_then(|payload| start_volume_create(shared, &payload).ok())
                {
                    Some(body) => body,
                    None => serde_json::json!({"error": "volume creation could not be started"}),
                };
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/volume/set-cache") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = match serde_json::from_slice::<serde_json::Value>(&request.body)
                    .ok()
                    .and_then(|payload| {
                        start_volume_set_cache(shared, &payload, drive_token_store).ok()
                    }) {
                    Some(body) => body,
                    None => serde_json::json!({"error": "cache move could not be started"}),
                };
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("GET", "/api/volume/set-cache-status") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = shared
                    .lock()
                    .map(|state| state.set_cache.clone())
                    .unwrap_or_default();
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("GET", "/api/volume/create-status") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let body = shared
                    .lock()
                    .map(|state| state.create.clone())
                    .unwrap_or_default();
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/open-explorer") => {
                if let Err(response) = authorize(&request, origin, token) {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        &response,
                        false,
                    )?;
                    return Ok(());
                }
                let payload: serde_json::Value = serde_json::from_slice(&request.body)?;
                let body = match payload["letter"].as_str().and_then(open_explorer) {
                    Some(()) => serde_json::json!({"opened": true}),
                    None => serde_json::json!({"error": "drive letter is invalid"}),
                };
                write_response(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    &serde_json::to_vec(&body)?,
                    false,
                )?;
            }
            ("POST", "/api/invoke") | ("POST", "/api/invoke-drive") => {
                let supplied_origin = request.headers.get("origin").map(String::as_str);
                let supplied_token = request.headers.get("x-mirage-token").map(String::as_str);
                if supplied_origin != Some(origin)
                    || !supplied_token.is_some_and(|value| constant_time_eq(value, token))
                {
                    write_response(
                        stream,
                        403,
                        "application/json; charset=utf-8",
                        br#"{"error":"forbidden"}"#,
                        false,
                    )?;
                    return Ok(());
                }
                if !request
                    .headers
                    .get("content-type")
                    .is_some_and(|value| value.starts_with("application/json"))
                {
                    write_response(
                        stream,
                        415,
                        "application/json; charset=utf-8",
                        br#"{"error":"application/json required"}"#,
                        false,
                    )?;
                    return Ok(());
                }
                let mut control_request: Request = serde_json::from_slice(&request.body)?;
                control_request.validate()?;
                if request.path == "/api/invoke-drive"
                    && let Err(error) = inject_drive_access(&mut control_request, drive_token_store)
                {
                    let response = Response {
                        protocol_version: mirage_ipc::PROTOCOL_VERSION,
                        request_id: control_request.request_id,
                        body: ResponseBody::Error {
                            code: error.code.into(),
                            message: error.message,
                        },
                    };
                    let body = serde_json::to_vec(&response)?;
                    write_response(stream, 200, "application/json; charset=utf-8", &body, false)?;
                    return Ok(());
                }
                let response = exchange(&control_request)?;
                let body = serde_json::to_vec(&response)?;
                write_response(stream, 200, "application/json; charset=utf-8", &body, false)?;
            }
            _ => write_response(
                stream,
                404,
                "application/json; charset=utf-8",
                br#"{"error":"not found"}"#,
                false,
            )?,
        }
        Ok(())
    }

    fn inject_drive_access(
        request: &mut Request,
        token_store: Option<&Path>,
    ) -> Result<(), MirageError> {
        match &request.command {
            Command::CapacityPlan {
                drive_access_token, ..
            }
            | Command::CapacityAcquire {
                drive_access_token, ..
            }
            | Command::NativeActivate {
                drive_access_token, ..
            }
            | Command::Mount {
                drive_access_token, ..
            } if drive_access_token.is_none() => {}
            Command::Materialize {
                drive_access_token,
                drive_quota,
                ..
            } if drive_access_token.is_none() && drive_quota.is_none() => {}
            _ => {
                return Err(MirageError::invalid_argument(
                    "the Drive credential bridge only accepts an unauthenticated capacity, materialization, or native activation request",
                ));
            }
        }
        let session =
            futures_executor::block_on(mirage_backend_drive::refresh_stored_session(token_store))?;
        let quota = if matches!(&request.command, Command::Materialize { .. }) {
            Some(
                futures_executor::block_on(mirage_backend_drive::quota::storage_quota(
                    session.transport.as_ref(),
                    session.access_token.as_str(),
                ))
                .map_err(MirageError::from)?,
            )
        } else {
            None
        };
        let token = SensitiveString::new(session.access_token.as_str().to_owned())?;
        match &mut request.command {
            Command::CapacityPlan {
                drive_access_token, ..
            }
            | Command::CapacityAcquire {
                drive_access_token, ..
            }
            | Command::NativeActivate {
                drive_access_token, ..
            }
            | Command::Mount {
                drive_access_token, ..
            } => *drive_access_token = Some(token),
            Command::Materialize {
                drive_access_token,
                drive_quota,
                ..
            } => {
                *drive_access_token = Some(token);
                let quota = quota.expect("materialization quota was requested");
                *drive_quota = Some(DriveQuotaSnapshot {
                    limit_bytes: quota.limit,
                    usage_bytes: quota.usage,
                });
            }
            _ => unreachable!("validated Drive bridge command changed"),
        }
        Ok(())
    }

    struct HttpRequest {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    fn read_http_request(
        stream: &mut TcpStream,
    ) -> Result<HttpRequest, Box<dyn std::error::Error>> {
        let mut bytes = Vec::with_capacity(4096);
        let header_end = loop {
            if bytes.len() >= MAX_HTTP_HEADER_BYTES {
                return Err("HTTP request headers exceed the bridge limit".into());
            }
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err("HTTP request ended before its headers".into());
            }
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let (method, path, headers, content_length) = {
            let header_text = std::str::from_utf8(&bytes[..header_end])?;
            let mut lines = header_text.split("\r\n");
            let request_line = lines.next().ok_or("missing HTTP request line")?;
            let mut request_parts = request_line.split_ascii_whitespace();
            let method = request_parts
                .next()
                .ok_or("missing HTTP method")?
                .to_owned();
            let path = request_parts
                .next()
                .ok_or("missing HTTP path")?
                .split('?')
                .next()
                .ok_or("missing HTTP path")?
                .to_owned();
            let version = request_parts.next().ok_or("missing HTTP version")?;
            if request_parts.next().is_some() || version != "HTTP/1.1" {
                return Err("unsupported HTTP request line".into());
            }
            let mut headers = BTreeMap::new();
            for line in lines.filter(|line| !line.is_empty()) {
                let (name, value) = line.split_once(':').ok_or("malformed HTTP header")?;
                let name = name.trim().to_ascii_lowercase();
                if headers.insert(name, value.trim().to_owned()).is_some() {
                    return Err("duplicate HTTP header".into());
                }
            }
            let content_length = headers
                .get("content-length")
                .map(|value| value.parse::<usize>())
                .transpose()?
                .unwrap_or(0);
            (method, path, headers, content_length)
        };
        if content_length > MAX_HTTP_BODY_BYTES {
            return Err("HTTP request body exceeds the bridge limit".into());
        }
        let total = header_end
            .checked_add(content_length)
            .ok_or("HTTP request length overflow")?;
        while bytes.len() < total {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err("HTTP request body ended early".into());
            }
            bytes.extend_from_slice(&chunk[..read]);
            if bytes.len() > total {
                return Err("HTTP pipelining is not supported".into());
            }
        }
        Ok(HttpRequest {
            method,
            path,
            headers,
            body: bytes[header_end..total].to_vec(),
        })
    }

    fn write_file(
        stream: &mut TcpStream,
        path: &Path,
        content_type: &str,
        immutable: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let body = std::fs::read(path)?;
        write_response(stream, 200, content_type, &body, immutable)?;
        Ok(())
    }

    fn write_response(
        stream: &mut TcpStream,
        status: u16,
        content_type: &str,
        body: &[u8],
        immutable: bool,
    ) -> std::io::Result<()> {
        let reason = match status {
            200 => "OK",
            403 => "Forbidden",
            404 => "Not Found",
            415 => "Unsupported Media Type",
            _ => "Internal Server Error",
        };
        let cache = if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-store"
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: {cache}\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'\r\nCross-Origin-Resource-Policy: same-origin\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
        stream.flush()
    }

    fn exchange(request: &Request) -> Result<Response, MirageError> {
        let mut pipe = open_pipe()?;
        let frame = encode_frame(request)?;
        pipe.write_all(&frame).map_err(io)?;
        pipe.flush().map_err(io)?;
        let mut prefix = [0_u8; 4];
        pipe.read_exact(&mut prefix).map_err(io)?;
        let length = u32::from_le_bytes(prefix) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(MirageError::integrity_mismatch(
                "service response exceeds IPC bound",
            ));
        }
        let mut frame = Vec::with_capacity(4 + length);
        frame.extend_from_slice(&prefix);
        frame.resize(4 + length, 0);
        pipe.read_exact(&mut frame[4..]).map_err(io)?;
        let response: Response = decode_frame(&frame)?;
        if response.protocol_version != mirage_ipc::PROTOCOL_VERSION
            || response.request_id != request.request_id
        {
            return Err(MirageError::integrity_mismatch(
                "service response correlation failed",
            ));
        }
        Ok(response)
    }

    fn open_pipe() -> Result<std::fs::File, MirageError> {
        mirage_cli::client::open_service_pipe_with_retry(
            mirage_cli::client::SERVICE_PIPE_NAME,
            15_000,
        )
    }

    fn random_token() -> Result<String, MirageError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| MirageError::internal_invariant("UI bridge token generation failed"))?;
        let mut token = String::with_capacity(64);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut token, "{byte:02x}")
                .map_err(|_| MirageError::internal_invariant("UI token encoding failed"))?;
        }
        Ok(token)
    }

    fn constant_time_eq(left: &str, right: &str) -> bool {
        if left.len() != right.len() {
            return false;
        }
        left.as_bytes()
            .iter()
            .zip(right.as_bytes())
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
    }

    /// Opens `url` in the default browser (used by run, the tray, and
    /// second-instance handoff).
    pub(crate) fn open_browser_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
        open_browser(url)
    }

    fn open_browser(url: &str) -> Result<(), Box<dyn std::error::Error>> {
        let operation = wide("open");
        let url = wide(url);
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize <= 32 {
            return Err("default browser could not be opened".into());
        }
        Ok(())
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain([0]).collect()
    }

    fn io(error: std::io::Error) -> MirageError {
        MirageError::new(
            MirageErrorKind::Io,
            MirageErrorKind::Io.default_code(),
            "service IPC failed",
        )
        .with_source(error)
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_host::run() {
        eprintln!("MirageSSD UI failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mirage-ui requires Windows");
}
