use mirage_types::MirageError;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionScope {
    CurrentUser,
    LocalMachine,
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };
    const UI_FORBIDDEN: u32 = 1;
    const LOCAL_MACHINE: u32 = 4;
    fn blob(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, MirageError> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(bytes.len())
                .map_err(|_| MirageError::invalid_argument("DPAPI input exceeds u32"))?,
            pbData: bytes.as_ptr().cast_mut(),
        })
    }
    fn copy_output(output: CRYPT_INTEGER_BLOB) -> Result<Zeroizing<Vec<u8>>, MirageError> {
        if output.cbData != 0 && output.pbData.is_null() {
            return Err(MirageError::internal_invariant(
                "DPAPI returned null output",
            ));
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe { LocalFree(output.pbData.cast()) };
        Ok(Zeroizing::new(bytes))
    }
    pub fn protect(
        plaintext: &[u8],
        entropy: &[u8],
        scope: ProtectionScope,
    ) -> Result<Vec<u8>, MirageError> {
        let input = blob(plaintext)?;
        let mut entropy_blob = blob(entropy)?;
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: ptr::null_mut(),
        };
        let flags = UI_FORBIDDEN
            | if scope == ProtectionScope::LocalMachine {
                LOCAL_MACHINE
            } else {
                0
            };
        let ok = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                if entropy.is_empty() {
                    ptr::null()
                } else {
                    &mut entropy_blob
                },
                ptr::null_mut(),
                ptr::null(),
                flags,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(MirageError::backend_permission_denied(
                "DPAPI protection failed",
            ));
        }
        Ok(copy_output(output)?.to_vec())
    }
    pub fn unprotect(ciphertext: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>, MirageError> {
        let input = blob(ciphertext)?;
        let mut entropy_blob = blob(entropy)?;
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                if entropy.is_empty() {
                    ptr::null()
                } else {
                    &mut entropy_blob
                },
                ptr::null_mut(),
                ptr::null(),
                UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(MirageError::backend_permission_denied(
                "DPAPI unprotect failed",
            ));
        }
        copy_output(output)
    }
}
#[cfg(windows)]
pub use platform::{protect, unprotect};

#[cfg(not(windows))]
pub fn protect(_: &[u8], _: &[u8], _: ProtectionScope) -> Result<Vec<u8>, MirageError> {
    Err(MirageError::unsupported_layout("DPAPI requires Windows"))
}
#[cfg(not(windows))]
pub fn unprotect(_: &[u8], _: &[u8]) -> Result<Zeroizing<Vec<u8>>, MirageError> {
    Err(MirageError::unsupported_layout("DPAPI requires Windows"))
}
