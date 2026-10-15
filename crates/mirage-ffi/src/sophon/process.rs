//! Bounded request-process classification. Handles bind cached identities to a
//! particular process lifetime; inaccessible identities merely disable learning.
use std::collections::VecDeque;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{FILETIME, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};

const MAX_IDENTITIES: usize = 64;

struct Identity {
    pid: u32,
    handle: OwnedHandle,
    creation: u64,
    matches: bool,
}

pub(super) struct Classifier {
    expected: Option<PathBuf>,
    identities: VecDeque<Identity>,
}

impl Classifier {
    pub(super) fn new(expected: Option<&Path>) -> Self {
        Self {
            expected: expected
                .filter(|path| path.is_absolute())
                .filter(|path| super::cache::ordinary_path(path).is_ok())
                .and_then(|path| path.canonicalize().ok())
                .filter(|path| path.is_file()),
            identities: VecDeque::new(),
        }
    }

    pub(super) fn is_game(&mut self, pid: u32) -> bool {
        let Some(expected) = &self.expected else {
            return false;
        };
        if pid == 0 {
            return false;
        }
        if let Some(index) = self.identities.iter().position(|entry| entry.pid == pid) {
            let identity = &self.identities[index];
            // SAFETY: the owned process handle remains valid while borrowed.
            let alive =
                unsafe { WaitForSingleObject(identity.handle.as_raw_handle(), 0) } == WAIT_TIMEOUT;
            if alive && creation_time(&identity.handle) == Some(identity.creation) {
                return identity.matches;
            }
            self.identities.remove(index);
        }
        // SAFETY: opening a query/synchronization-only handle has no process side effects.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            return false;
        }
        // SAFETY: OpenProcess returned a new owned handle, closed exactly once by OwnedHandle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: this owned handle is valid; an already exited process is ineligible.
        if unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != WAIT_TIMEOUT {
            return false;
        }
        let Some(creation) = creation_time(&handle) else {
            return false;
        };
        let mut image = vec![0u16; 32768];
        let mut length = image.len() as u32;
        // SAFETY: image is writable for length UTF-16 units and the handle is valid.
        let queried = unsafe {
            QueryFullProcessImageNameW(handle.as_raw_handle(), 0, image.as_mut_ptr(), &mut length)
        };
        let matches = queried != 0
            && PathBuf::from(String::from_utf16_lossy(&image[..length as usize]))
                .canonicalize()
                .is_ok_and(|image| image == *expected);
        if self.identities.len() == MAX_IDENTITIES {
            self.identities.pop_front();
        }
        self.identities.push_back(Identity {
            pid,
            handle,
            creation,
            matches,
        });
        matches
    }
}

fn creation_time(handle: &OwnedHandle) -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all outputs are valid FILETIME pointers; handle is owned by the caller.
    (unsafe {
        GetProcessTimes(
            handle.as_raw_handle(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } != 0)
        .then_some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_executable_identity_is_required_and_cached_per_process() {
        let executable = std::env::current_exe().unwrap();
        let mut classifier = Classifier::new(Some(&executable));
        assert!(classifier.is_game(std::process::id()));
        assert!(classifier.is_game(std::process::id()));
        assert_eq!(classifier.identities.len(), 1);
        assert!(!classifier.is_game(0));
        let different = std::env::current_dir().unwrap().join("Cargo.toml");
        assert!(!Classifier::new(Some(&different)).is_game(std::process::id()));
        assert!(!Classifier::new(None).is_game(std::process::id()));
    }
}
