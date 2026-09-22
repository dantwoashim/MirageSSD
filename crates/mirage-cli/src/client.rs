use mirage_ipc::{MAX_FRAME_BYTES, Request, Response, decode_frame, encode_frame};
use mirage_types::{MirageError, MirageErrorKind};
use std::io::{Read, Write};

pub trait ServiceTransport {
    fn exchange(&self, request: &Request) -> Result<Response, MirageError>;
}
pub struct NamedPipeTransport;

impl ServiceTransport for NamedPipeTransport {
    fn exchange(&self, request: &Request) -> Result<Response, MirageError> {
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
}

/// Pipe name shared by every MirageSSD IPC client.
#[cfg(windows)]
pub const SERVICE_PIPE_NAME: &str = r"\\.\pipe\MirageSSD.v1";

/// How a failed `CreateFileW` on the service pipe is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipeOpenDisposition {
    /// `ERROR_PIPE_BUSY`: an instance exists but is held — wait and retry.
    WaitAndRetry,
    /// `ERROR_FILE_NOT_FOUND`: no pipe object at all — the service is down.
    ServiceNotRunning,
    /// Anything else: surface the raw I/O error.
    Fail,
}

pub(crate) fn pipe_open_disposition(raw_os_error: Option<i32>) -> PipeOpenDisposition {
    const ERROR_PIPE_BUSY: i32 = 231;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    match raw_os_error {
        Some(ERROR_PIPE_BUSY) => PipeOpenDisposition::WaitAndRetry,
        Some(ERROR_FILE_NOT_FOUND) => PipeOpenDisposition::ServiceNotRunning,
        _ => PipeOpenDisposition::Fail,
    }
}

/// Opens the service pipe, waiting through `ERROR_PIPE_BUSY` contention for
/// up to `total_timeout_ms`. The service keeps a pending instance listening
/// between requests, but a brief busy window remains while one request is
/// being served; `WaitNamedPipeW` + retry covers it.
#[cfg(windows)]
pub fn open_service_pipe_with_retry(
    pipe_name: &str,
    total_timeout_ms: u32,
) -> Result<std::fs::File, MirageError> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
        },
        System::Pipes::WaitNamedPipeW,
    };
    let name: Vec<u16> = pipe_name.encode_utf16().chain([0]).collect();
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(total_timeout_ms as u64);
    // Older service builds drop the listening instance briefly between
    // requests, so a missing pipe object is also transient contention — give
    // it a short grace window before declaring the service down.
    let absent_deadline = std::time::Instant::now() + std::time::Duration::from_millis(2_000);
    loop {
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
        if handle != INVALID_HANDLE_VALUE {
            return Ok(unsafe { std::fs::File::from_raw_handle(handle as RawHandle) });
        }
        match pipe_open_disposition(std::io::Error::last_os_error().raw_os_error()) {
            PipeOpenDisposition::WaitAndRetry if std::time::Instant::now() < deadline => {
                if unsafe { WaitNamedPipeW(name.as_ptr(), 5_000) } == 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(231)
                {
                    // Timeout or pipe vanished between the failure and the
                    // wait — retry once more within the overall deadline.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            PipeOpenDisposition::WaitAndRetry => {
                return Err(MirageError::provider_unavailable(
                    "MirageSSD service pipe stayed busy; the service may be stuck serving another client",
                ));
            }
            PipeOpenDisposition::ServiceNotRunning
                if std::time::Instant::now() < absent_deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            PipeOpenDisposition::ServiceNotRunning => {
                return Err(MirageError::provider_unavailable(
                    "MirageSSD service pipe is unavailable; the MirageSSD service is not running",
                ));
            }
            PipeOpenDisposition::Fail => {
                return Err(MirageError::provider_unavailable(
                    "MirageSSD service pipe is unavailable",
                ));
            }
        }
    }
}

#[cfg(windows)]
fn open_pipe() -> Result<std::fs::File, MirageError> {
    open_service_pipe_with_retry(SERVICE_PIPE_NAME, 15_000)
}
#[cfg(not(windows))]
fn open_pipe() -> Result<std::fs::File, MirageError> {
    Err(MirageError::unsupported_layout(
        "MirageSSD service IPC requires Windows",
    ))
}
fn io(error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "service IPC failed",
    )
    .with_source(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_busy_retries_while_absent_fails_fast() {
        assert_eq!(
            pipe_open_disposition(Some(231)),
            PipeOpenDisposition::WaitAndRetry
        );
        assert_eq!(
            pipe_open_disposition(Some(2)),
            PipeOpenDisposition::ServiceNotRunning
        );
        assert_eq!(pipe_open_disposition(Some(5)), PipeOpenDisposition::Fail);
        assert_eq!(pipe_open_disposition(None), PipeOpenDisposition::Fail);
    }
}
