//! Explorer drive-icon registration for mounted repositories.

/// Per-user Explorer drive icon for letter mounts. The service runs as
/// SYSTEM, so it writes under `HKEY_USERS\<owner-sid>`; a missing hive or
/// denied write is cosmetic and never fails the mount.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) mod imp {

    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_USERS, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
        RegSetValueExW,
    };

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain([0]).collect()
    }

    fn icon_path() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("mirage-drive.ico")))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| r"C:\Program Files\MirageSSD\mirage-drive.ico".to_owned())
    }

    fn letter_of(mount_point: &Path) -> Option<char> {
        mount_point
            .to_string_lossy()
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphabetic())
    }

    fn key_path(owner_sid: &str, letter: char) -> String {
        format!(
            r"{owner_sid}\Software\Classes\Applications\Explorer.exe\Drives\{letter}\DefaultIcon"
        )
    }

    pub fn register(owner_sid: &str, mount_point: &Path) {
        let Some(letter) = letter_of(mount_point) else {
            return;
        };
        let path = wide(&key_path(owner_sid, letter));
        let mut key: HKEY = std::ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_USERS,
                path.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS || key.is_null() {
            return;
        }
        let icon = wide(&icon_path());
        unsafe {
            RegSetValueExW(
                key,
                std::ptr::null(),
                0,
                REG_SZ,
                icon.as_ptr().cast(),
                (icon.len() * 2) as u32,
            );
            RegCloseKey(key);
        }
    }

    pub fn unregister(owner_sid: &str, mount_point: &Path) {
        let Some(letter) = letter_of(mount_point) else {
            return;
        };
        let path = wide(&key_path(owner_sid, letter));
        unsafe {
            RegDeleteTreeW(HKEY_USERS, path.as_ptr());
        }
    }

    fn mirage_cli_path() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("mirage.exe")))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| r"C:\Program Files\MirageSSD\mirage.exe".to_owned())
    }

    fn verb_key(owner_sid: &str, verb: &str, letter: char) -> String {
        format!(r"{owner_sid}\Software\Classes\Directory\shell\MirageSSD.{verb}.{letter}")
    }

    /// "Keep on this device" / "Free up space" verbs scoped by `AppliesTo`
    /// to this volume's letter; the command also re-checks the prefix so a
    /// stale registration is a no-op.
    pub fn register_verbs(owner_sid: &str, mount_point: &Path, repository_id: &str) {
        let Some(letter) = letter_of(mount_point) else {
            return;
        };
        let cli = mirage_cli_path();
        for (verb, text, args) in [
            (
                "Pin",
                "Keep on this device",
                format!("shell-verb pin {repository_id} {letter} \"%1\""),
            ),
            (
                "Free",
                "Free up space",
                format!("shell-verb free {repository_id} {letter} \"%1\""),
            ),
        ] {
            let key = verb_key(owner_sid, verb, letter);
            let applies_to = format!("System.ItemPathDisplay:~\"{letter}:\\\"");
            set_verb(&key, text, &applies_to, &format!("\"{cli}\" {args}"));
        }
    }

    pub fn unregister_verbs(owner_sid: &str, mount_point: &Path) {
        let Some(letter) = letter_of(mount_point) else {
            return;
        };
        for verb in ["Pin", "Free"] {
            let path = wide(&verb_key(owner_sid, verb, letter));
            unsafe {
                RegDeleteTreeW(HKEY_USERS, path.as_ptr());
            }
        }
    }

    fn set_verb(key: &str, text: &str, applies_to: &str, command: &str) {
        let mut handle: HKEY = std::ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_USERS,
                wide(key).as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut handle,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS || handle.is_null() {
            return;
        }
        unsafe {
            let set = |name: &str, value: &str| {
                let name = wide(name);
                let data: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
                RegSetValueExW(
                    handle,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr(),
                    data.len() as u32,
                )
            };
            // Default value = menu caption.
            set("", text);
            set("AppliesTo", applies_to);
            RegCloseKey(handle);
            // <verb>\command default value = the command line.
            let mut command_key: HKEY = std::ptr::null_mut();
            if RegCreateKeyExW(
                HKEY_USERS,
                wide(&format!(r"{key}\command")).as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut command_key,
                std::ptr::null_mut(),
            ) == ERROR_SUCCESS
                && !command_key.is_null()
            {
                let data: Vec<u8> = wide(command).iter().flat_map(|c| c.to_le_bytes()).collect();
                RegSetValueExW(
                    command_key,
                    std::ptr::null(),
                    0,
                    REG_SZ,
                    data.as_ptr(),
                    data.len() as u32,
                );
                RegCloseKey(command_key);
            }
        }
    }
}

#[cfg(not(windows))]
pub(super) mod imp {

    use std::path::Path;
    pub fn register(_: &str, _: &Path) {}
    pub fn unregister(_: &str, _: &Path) {}
    pub fn register_verbs(_: &str, _: &Path, _: &str) {}
    pub fn unregister_verbs(_: &str, _: &Path) {}
}

pub(super) use imp::*;
