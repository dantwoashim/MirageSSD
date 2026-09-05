//! Hardened loopback bridge for the MirageSSD management UI.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(windows, allow(unsafe_code))]

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
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        path::{Path, PathBuf},
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };

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
        let mut health_check = false;
        let mut drive_token_store = None;
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_string_lossy().as_ref() {
                "--ui-root" => {
                    ui_root = PathBuf::from(arguments.next().ok_or("--ui-root needs a path")?);
                }
                "--no-open" => no_open = true,
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

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let origin = format!("http://127.0.0.1:{}", address.port());
        let token = random_token()?;
        if !no_open {
            open_browser(&format!("{origin}/#{token}"))?;
        }
        for connection in listener.incoming() {
            match connection {
                Ok(mut stream) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    if let Err(error) = handle_connection(
                        &mut stream,
                        &ui_root,
                        &origin,
                        &token,
                        drive_token_store.as_deref(),
                    ) {
                        let _ = write_response(
                            &mut stream,
                            500,
                            "application/json; charset=utf-8",
                            br#"{"error":"local UI bridge failed"}"#,
                            false,
                        );
                        eprintln!("MirageSSD UI request failed: {error}");
                    }
                }
                Err(error) => eprintln!("MirageSSD UI accept failed: {error}"),
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

    fn handle_connection(
        stream: &mut TcpStream,
        ui_root: &Path,
        origin: &str,
        token: &str,
        drive_token_store: Option<&Path>,
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
        use std::os::windows::io::RawHandle;
        let name: Vec<u16> = r"\\.\pipe\MirageSSD.v1".encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(MirageError::provider_unavailable(
                "MirageSSD service pipe is unavailable",
            ));
        }
        Ok(unsafe { std::fs::File::from_raw_handle(handle as RawHandle) })
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
