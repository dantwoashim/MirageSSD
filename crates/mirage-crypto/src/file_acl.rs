use mirage_types::MirageError;
use std::path::Path;

#[cfg(windows)]
pub fn restrict_to_current_user_system_admins(path: &Path) -> Result<(), MirageError> {
    platform::restrict(path)
}

#[cfg(not(windows))]
pub fn restrict_to_current_user_system_admins(_: &Path) -> Result<(), MirageError> {
    Err(MirageError::unsupported_layout(
        "repository key ACLs require Windows",
    ))
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, GetTokenInformation, PSECURITY_DESCRIPTOR, SetFileSecurityW,
            TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub fn restrict(path: &Path) -> Result<(), MirageError> {
        let sid = current_user_sid()?;
        let sddl = format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{sid})");
        let sddl = sddl.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
            || descriptor.is_null()
        {
            return Err(permission("repository key ACL construction failed"));
        }
        let path = path
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>();
        let applied =
            unsafe { SetFileSecurityW(path.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
        unsafe { LocalFree(descriptor) };
        if applied == 0 {
            return Err(permission("repository key ACL application failed"));
        }
        Ok(())
    }

    fn current_user_sid() -> Result<String, MirageError> {
        let mut token: HANDLE = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(permission("current process token is unavailable"));
        }
        let token = Handle(token);
        let mut needed = 0_u32;
        unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed) };
        if needed == 0 || needed > 64 * 1024 {
            return Err(permission("current process token has invalid size"));
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
            return Err(permission("current process SID query failed"));
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut text = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 || text.is_null() {
            return Err(permission("current process SID conversion failed"));
        }
        let mut length = 0_usize;
        while length < 256 && unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        if length == 0 || length == 256 {
            unsafe { LocalFree(text.cast()) };
            return Err(permission("current process SID is malformed"));
        }
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| permission("current process SID is malformed"));
        unsafe { LocalFree(text.cast()) };
        sid
    }

    fn permission(message: &'static str) -> MirageError {
        MirageError::backend_permission_denied(message)
    }
}
