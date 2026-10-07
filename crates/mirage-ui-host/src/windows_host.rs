//! Windows host: serves the management UI over loopback and bridges to the service.

use mirage_ipc::{
    Command, DriveQuotaSnapshot, MAX_FRAME_BYTES, Request, Response, ResponseBody, SensitiveString,
    decode_frame, encode_frame,
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

pub(crate) mod http;
pub(crate) mod ipc;
pub(crate) mod jobs;
pub(crate) mod shell;

use http::authorize;
use http::constant_time_eq;
use http::random_token;
use http::read_http_request;
use http::write_file;
use http::write_response;
use ipc::exchange;
use ipc::inject_drive_access;
use jobs::drive_status;
use jobs::start_drive_login;
use jobs::start_volume_create;
use jobs::start_volume_offload;
use jobs::start_volume_set_cache;
use shell::open_explorer;
use shell::wide;
pub(crate) use shell::{open_app_window, open_browser_url};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let default_root = executable
        .parent()
        .ok_or("UI executable has no parent directory")?
        .join("ui");
    let mut ui_root = default_root;
    let mut no_open = false;
    let mut print_url = false;
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
            "--print-url" => print_url = true,
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
            windows_sys::Win32::System::Threading::CreateMutexW(std::ptr::null(), 0, name.as_ptr())
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
    if print_url {
        // Diagnostics: lets a script drive this host's page URL (the
        // token is per launch and only valid for this process).
        println!("{page_url}");
    }
    if !no_open {
        open_app_window(&page_url)?;
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
    offload: serde_json::Value,
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
                .and_then(|path| mirage_backend_drive::token_store::TokenStore::new(path).delete());
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
        ("POST", "/api/volume/offload") => {
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
                .map(|payload| start_volume_offload(shared, &payload))
            {
                Some(Ok(body)) => body,
                Some(Err(error)) => serde_json::json!({"error": error.to_string()}),
                None => serde_json::json!({"error": "offload could not be started"}),
            };
            write_response(
                stream,
                200,
                "application/json; charset=utf-8",
                &serde_json::to_vec(&body)?,
                false,
            )?;
        }
        ("GET", "/api/volume/offload-status") => {
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
                .map(|state| state.offload.clone())
                .unwrap_or_default();
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
