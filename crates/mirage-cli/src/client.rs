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

#[cfg(windows)]
fn open_pipe() -> Result<std::fs::File, MirageError> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
        },
    };
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
