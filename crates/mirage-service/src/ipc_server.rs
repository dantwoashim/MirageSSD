use crate::{RequestHandler, authenticated_named_pipe_client_principal};
use mirage_ipc::{
    Authorization, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response, decode_frame, encode_frame,
};
use mirage_types::{MirageError, MirageErrorKind};
use std::sync::atomic::Ordering;
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, RawHandle},
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
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
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

pub fn serve_one(handler: &impl RequestHandler) -> Result<(), MirageError> {
    serve_one_named(handler, r"\\.\pipe\MirageSSD.v1")
}

fn pipe_security_attributes() -> Result<SecurityDescriptorGuard, MirageError> {
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
    Ok(SecurityDescriptorGuard(descriptor))
}

fn create_pipe_instance(
    pipe_name: &str,
    descriptor: &SecurityDescriptorGuard,
) -> Result<File, MirageError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name: Vec<u16> = pipe_name.encode_utf16().chain([0]).collect();
    // PIPE_UNLIMITED_INSTANCES lets the serve loop keep a pending instance
    // listening while a request is being handled, so concurrent clients see
    // a waitable pipe instead of ERROR_PIPE_BUSY.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            (MAX_FRAME_BYTES + 4) as u32,
            (MAX_FRAME_BYTES + 4) as u32,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

/// Serves one request on an explicitly named local pipe.
///
/// Production uses [`serve`]. The named entry point exists so integration
/// tests never connect to an already-running MirageSSD service.
pub fn serve_one_named(handler: &impl RequestHandler, pipe_name: &str) -> Result<(), MirageError> {
    let descriptor = pipe_security_attributes()?;
    let mut pipe = create_pipe_instance(pipe_name, &descriptor)?;
    wait_connected(&mut pipe)?;
    handle_connection(handler, pipe)
}

/// The production accept loop: always keeps one pending pipe instance
/// listening while a request is handled inline, so parallel clients queue
/// inside the pipe instead of receiving ERROR_PIPE_BUSY.
pub fn serve(
    handler: &impl RequestHandler,
    pipe_name: &str,
    stopping: &std::sync::atomic::AtomicBool,
) -> Result<(), MirageError> {
    let descriptor = pipe_security_attributes()?;
    let mut listener = create_pipe_instance(pipe_name, &descriptor)?;
    while !stopping.load(Ordering::Acquire) {
        if let Err(error) = wait_connected(&mut listener) {
            eprintln!("MirageSSD pipe accept failed: {error}");
            listener = create_pipe_instance(pipe_name, &descriptor)?;
            continue;
        }
        // Keep a fresh instance listening before serving this client so
        // further connections find a pipe to wait on.
        let next = create_pipe_instance(pipe_name, &descriptor)?;
        if let Err(error) = handle_connection(handler, listener) {
            eprintln!("MirageSSD pipe request failed: {error}");
        }
        listener = next;
    }
    Ok(())
}

fn wait_connected(pipe: &mut File) -> Result<(), MirageError> {
    let connected = unsafe { ConnectNamedPipe(pipe.as_raw_handle() as _, ptr::null_mut()) };
    const ERROR_PIPE_CONNECTED: u32 = 535;
    if connected == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn handle_connection(handler: &impl RequestHandler, mut pipe: File) -> Result<(), MirageError> {
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
struct SecurityDescriptorGuard(PSECURITY_DESCRIPTOR);
impl Drop for SecurityDescriptorGuard {
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
