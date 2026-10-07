//! Shell integration: Explorer, browser, and app-window launches.

use super::*;

/// Opens Explorer on a mounted drive letter (validated `X:` shape only).
pub(super) fn open_explorer(letter: &str) -> Option<()> {
    let letter = letter.trim().trim_end_matches(':').trim_end_matches('\\');
    let bytes = letter.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let target = format!("{}:\\", (bytes[0] as char).to_ascii_uppercase());
    std::process::Command::new("explorer.exe")
        .arg(&target)
        .spawn()
        .ok()?;
    Some(())
}

/// Opens `url` in the default browser (kept for `file:///` drive opens).
pub(crate) fn open_browser_url(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    open_browser(url)
}

/// Opens a UI page as an app window — Edge `--app=` removes the browser
/// chrome so MirageSSD looks like a desktop app. Only loopback http URLs
/// qualify; anything else (or a missing Edge) falls back to the browser.
pub(crate) fn open_app_window(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let safe =
        url.starts_with("http://127.0.0.1:") && !url.chars().any(|c| c.is_whitespace() || c == '"');
    if safe {
        // ShellExecute resolves "msedge.exe" through App Paths.
        let operation = wide("open");
        let file = wide("msedge.exe");
        let parameters = wide(&format!(
            "--app=\"{url}\" --window-size=1240,860 --no-first-run"
        ));
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                parameters.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize > 32 {
            return Ok(());
        }
    }
    open_browser(url)
}

fn open_browser(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let operation = wide("open");
    let url = wide(url);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err("default browser could not be opened".into());
    }
    Ok(())
}

pub(super) fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain([0]).collect()
}
