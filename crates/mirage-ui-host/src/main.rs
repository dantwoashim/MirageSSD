//! Hardened loopback bridge for the MirageSSD management UI.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(windows, allow(unsafe_code))]

#[cfg(windows)]
mod pin_quick_access;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod update_check;

#[cfg(windows)]
mod windows_host;

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_host::run() {
        eprintln!("MirageSSD UI failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mirage-ui requires Windows");
}
