#![cfg(windows)]
#![allow(unsafe_code)]
use mirage_service::authenticated_named_pipe_client_sid;
use std::{
    os::windows::io::{BorrowedHandle, RawHandle},
    ptr, thread,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    },
};

#[test]
fn connected_pipe_identity_comes_from_impersonated_kernel_token() {
    let name: Vec<u16> = format!(r"\\.\pipe\miragessd-test-{}", std::process::id())
        .encode_utf16()
        .chain([0])
        .collect();
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            1024,
            1024,
            0,
            ptr::null(),
        )
    };
    assert_ne!(server, INVALID_HANDLE_VALUE);
    let client_name = name.clone();
    let client = thread::spawn(move || unsafe {
        let handle = CreateFileW(
            client_name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION,
            ptr::null_mut(),
        );
        if handle != INVALID_HANDLE_VALUE {
            let mut written = 0;
            WriteFile(
                handle,
                [1_u8].as_ptr().cast(),
                1,
                &mut written,
                ptr::null_mut(),
            );
        }
        handle as isize
    });
    unsafe {
        ConnectNamedPipe(server, ptr::null_mut());
    }
    let client = client.join().expect("client") as *mut core::ffi::c_void;
    assert_ne!(client, INVALID_HANDLE_VALUE);
    let borrowed = unsafe { BorrowedHandle::borrow_raw(server as RawHandle) };
    let sid = authenticated_named_pipe_client_sid(borrowed).expect("SID");
    assert!(sid.starts_with("S-1-"));
    unsafe {
        CloseHandle(client);
        CloseHandle(server);
    }
}
