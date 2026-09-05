#![cfg(windows)]
#![allow(unsafe_code)]
use mirage_ipc::{
    Command, PROTOCOL_VERSION, Principal, Request, Response, ResponseBody, decode_frame,
    encode_frame,
};
use mirage_service::{RequestHandler, serve_one_named};
use std::{
    io::{Read, Write},
    os::windows::io::{FromRawHandle, RawHandle},
    ptr, thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
        SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT,
    },
};

struct Handler;
impl RequestHandler for Handler {
    fn handle(&self, principal: &Principal, request: Request) -> ResponseBody {
        assert!(principal.windows_sid.starts_with("S-1-"));
        assert_eq!(request.command, Command::Status);
        ResponseBody::Json(serde_json::json!({"healthy":true}))
    }
}

#[test]
fn acl_pipe_round_trips_bounded_correlated_request() {
    let pipe_name = format!(r"\\.\pipe\MirageSSD.test.{}", std::process::id());
    let server_name = pipe_name.clone();
    let server = thread::spawn(move || serve_one_named(&Handler, &server_name));
    let name: Vec<u16> = pipe_name.encode_utf16().chain([0]).collect();
    let mut connected = None;
    for _ in 0..200 {
        let value = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION,
                ptr::null_mut(),
            )
        };
        if value != INVALID_HANDLE_VALUE {
            connected = Some(value);
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let handle = connected.expect("service pipe became available");
    let mut pipe = unsafe { std::fs::File::from_raw_handle(handle as RawHandle) };
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 44,
        cancellation_id: None,
        command: Command::Status,
    };
    pipe.write_all(&encode_frame(&request).expect("encode"))
        .expect("write");
    let mut prefix = [0_u8; 4];
    pipe.read_exact(&mut prefix).expect("prefix");
    let length = u32::from_le_bytes(prefix) as usize;
    let mut frame = Vec::from(prefix);
    frame.resize(4 + length, 0);
    pipe.read_exact(&mut frame[4..]).expect("body");
    let response: Response = decode_frame(&frame).expect("decode");
    assert_eq!(response.request_id, 44);
    assert_eq!(
        response.body,
        ResponseBody::Json(serde_json::json!({"healthy":true}))
    );
    server.join().expect("thread").expect("serve");
}
