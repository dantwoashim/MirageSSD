use crate::authenticated_named_pipe_client_principal;
use mirage_ipc::{
    Authorization, MAX_FRAME_BYTES, PROTOCOL_VERSION, Principal, Request, Response, ResponseBody,
    decode_frame, encode_frame,
};
use mirage_types::{MirageError, MirageErrorKind};
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::io::{AsHandle, FromRawHandle, RawHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{GetLastError, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    },
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    },
};

/// Connects briefly so a service blocked in `ConnectNamedPipe` can observe shutdown.
pub fn wake_server() {
    let name: Vec<u16> = r"\\.\pipe\MirageSSD.v1".encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
    }
}

pub trait RequestHandler {
    fn handle(&self, principal: &Principal, request: Request) -> ResponseBody;
}

pub fn serve_one(handler: &impl RequestHandler) -> Result<(), MirageError> {
    serve_one_named(handler, r"\\.\pipe\MirageSSD.v1")
}

/// Serves one request on an explicitly named local pipe.
///
/// Production uses [`serve_one`]. The named entry point exists so integration
/// tests never connect to an already-running MirageSSD service.
pub fn serve_one_named(handler: &impl RequestHandler, pipe_name: &str) -> Result<(), MirageError> {
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)"
        .encode_utf16()
        .chain([0])
        .collect();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(MirageError::internal_invariant(
            "service pipe ACL construction failed",
        ));
    }
    let descriptor = SecurityDescriptor(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name: Vec<u16> = pipe_name.encode_utf16().chain([0]).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            16,
            (MAX_FRAME_BYTES + 4) as u32,
            (MAX_FRAME_BYTES + 4) as u32,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io(std::io::Error::last_os_error()));
    }
    let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
    const ERROR_PIPE_CONNECTED: u32 = 535;
    if connected == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return Err(io(std::io::Error::last_os_error()));
    }
    let mut pipe = unsafe { File::from_raw_handle(handle as RawHandle) };
    let frame = read_frame(&mut pipe)?;
    let request: Request = decode_frame(&frame)?;
    request.validate()?;
    let principal = authenticated_named_pipe_client_principal(pipe.as_handle())?;
    Authorization::authenticate(&principal)?;
    let request_id = request.request_id;
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        body: handler.handle(&principal, request),
    };
    pipe.write_all(&encode_frame(&response)?).map_err(io)?;
    pipe.flush().map_err(io)
}

fn read_frame(pipe: &mut File) -> Result<Vec<u8>, MirageError> {
    let mut prefix = [0_u8; 4];
    pipe.read_exact(&mut prefix).map_err(io)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(MirageError::invalid_argument("IPC request exceeds maximum"));
    }
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + length, 0);
    pipe.read_exact(&mut frame[4..]).map_err(io)?;
    Ok(frame)
}
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}
fn io(error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        "service pipe I/O failed",
    )
    .with_source(error)
}
