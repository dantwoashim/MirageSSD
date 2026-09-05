use mirage_ipc::{Principal, PrincipalRole};
use mirage_types::MirageError;
use std::{
    os::windows::io::{AsRawHandle, BorrowedHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW},
        CheckTokenMembership, GetTokenInformation, RevertToSelf, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::{
        Pipes::ImpersonateNamedPipeClient,
        Threading::{GetCurrentThread, OpenThreadToken},
    },
};

struct RevertGuard;
impl Drop for RevertGuard {
    fn drop(&mut self) {
        unsafe {
            RevertToSelf();
        }
    }
}
struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Binds an already connected named-pipe request to the kernel-authenticated client SID.
pub fn authenticated_named_pipe_client_sid(
    pipe: BorrowedHandle<'_>,
) -> Result<String, MirageError> {
    let pipe = pipe.as_raw_handle().cast();
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client impersonation failed",
        ));
    }
    let _revert = RevertGuard;
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client token is unavailable",
        ));
    }
    let token = HandleGuard(token);
    let mut needed = 0_u32;
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 64 * 1024 {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client token has invalid size",
        ));
    }
    let mut buffer = vec![0_u8; needed as usize];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client identity query failed",
        ));
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid_text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) } == 0 || sid_text.is_null() {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client SID conversion failed",
        ));
    }
    let mut length = 0_usize;
    while length < 256 && unsafe { *sid_text.add(length) } != 0 {
        length += 1;
    }
    if length == 0 || length == 256 {
        unsafe {
            LocalFree(sid_text.cast());
        }
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client SID is malformed",
        ));
    }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, length) })
        .map_err(|_| MirageError::backend_unauthenticated("named-pipe client SID is malformed"));
    unsafe {
        LocalFree(sid_text.cast());
    }
    sid
}

/// Resolves a kernel-authenticated pipe client into the least-privileged IPC role.
pub fn authenticated_named_pipe_client_principal(
    pipe: BorrowedHandle<'_>,
) -> Result<Principal, MirageError> {
    let pipe = pipe.as_raw_handle().cast();
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client impersonation failed",
        ));
    }
    let _revert = RevertGuard;
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client token is unavailable",
        ));
    }
    let token = HandleGuard(token);
    let sid = token_user_sid(token.0)?;
    let role = if sid == "S-1-5-18" {
        PrincipalRole::Service
    } else if token_is_administrator(token.0)? {
        PrincipalRole::Administrator
    } else {
        PrincipalRole::ReadOnly
    };
    Ok(Principal {
        windows_sid: sid,
        role,
        authenticated: true,
    })
}

fn token_user_sid(token: HANDLE) -> Result<String, MirageError> {
    let mut needed = 0_u32;
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 64 * 1024 {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client token has invalid size",
        ));
    }
    let mut buffer = vec![0_u8; needed as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client identity query failed",
        ));
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_to_string(user.User.Sid)
}

fn sid_to_string(sid: *mut core::ffi::c_void) -> Result<String, MirageError> {
    let mut sid_text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 || sid_text.is_null() {
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client SID conversion failed",
        ));
    }
    let mut length = 0_usize;
    while length < 256 && unsafe { *sid_text.add(length) } != 0 {
        length += 1;
    }
    if length == 0 || length == 256 {
        unsafe { LocalFree(sid_text.cast()) };
        return Err(MirageError::backend_unauthenticated(
            "named-pipe client SID is malformed",
        ));
    }
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, length) })
        .map_err(|_| MirageError::backend_unauthenticated("named-pipe client SID is malformed"));
    unsafe { LocalFree(sid_text.cast()) };
    result
}

fn token_is_administrator(token: HANDLE) -> Result<bool, MirageError> {
    let text: Vec<u16> = "S-1-5-32-544".encode_utf16().chain([0]).collect();
    let mut administrators = ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut administrators) } == 0 {
        return Err(MirageError::backend_unauthenticated(
            "administrator SID construction failed",
        ));
    }
    let mut member = 0_i32;
    let checked = unsafe { CheckTokenMembership(token, administrators, &mut member) };
    unsafe { LocalFree(administrators) };
    if checked == 0 {
        return Err(MirageError::backend_unauthenticated(
            "administrator membership check failed",
        ));
    }
    Ok(member != 0)
}
