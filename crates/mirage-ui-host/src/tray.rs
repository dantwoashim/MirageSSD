//! `--tray`: a notification-area companion for the UI host — hidden message
//! window, per-user tray icon, live menu, and balloon notifications.
//!
//! The app window is still the UI; the tray simply owns "always running".
//! Closing the window leaves MirageSSD running in the tray; Quit exits the host.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    HWND, LPARAM, LRESULT, POINT, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GetCursorPos, GetSystemMetrics, HWND_MESSAGE, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE,
    LR_LOADFROMFILE, LR_SHARED, LoadImageW, MF_SEPARATOR, MF_STRING, MSG,
    MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, PostQuitMessage, QS_ALLINPUT,
    RegisterClassW, SM_CXSMICON, SM_CYSMICON, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_RETURNCMD, TrackPopupMenu, WM_APP, WM_COMMAND, WM_DESTROY, WM_QUIT, WNDCLASSW,
};

const WM_TRAYICON: u32 = WM_APP + 1;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONUP: u32 = 0x0205;

const CMD_OPEN: u16 = 1;
const CMD_RECLAIM: u16 = 2;
const CMD_DIAGNOSTICS: u16 = 3;
const CMD_QUIT: u16 = 4;
const CMD_OPEN_DRIVE_BASE: u16 = 100;
const CMD_SIGNIN: u16 = 200;

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain([0]).collect()
}

/// The installed icon sits next to the exes; the stock application icon is
/// the fallback for unpackaged runs.
fn small_icon() -> windows_sys::Win32::Foundation::HANDLE {
    unsafe {
        if let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("mirage-drive.ico")))
        {
            let path = wide(&path.to_string_lossy());
            let icon = LoadImageW(
                ptr::null_mut(),
                path.as_ptr(),
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_LOADFROMFILE,
            );
            if !icon.is_null() {
                return icon;
            }
        }
        LoadImageW(
            GetModuleHandleW(ptr::null()),
            IDI_APPLICATION,
            IMAGE_ICON,
            16,
            16,
            LR_DEFAULTSIZE | LR_SHARED,
        )
    }
}

/// The settings view, addressed through the same page URL (`{origin}/#{token}`)
/// by inserting the view parameter ahead of the fragment.
fn settings_url(page_url: &str) -> String {
    match page_url.split_once('#') {
        Some((base, fragment)) => format!("{base}?view=settings#{fragment}"),
        None => format!("{page_url}?view=settings"),
    }
}

struct Tray {
    hwnd: HWND,
    nid: NOTIFYICONDATAW,
    url: String,
    mounted: Vec<String>,
    account: Option<String>,
    service_down_announced: bool,
    last_mount_count: usize,
}

impl Tray {
    fn notify_data(&self) -> NOTIFYICONDATAW {
        self.nid
    }

    fn set_tip(&mut self, tip: &str) {
        let tip_wide: Vec<u16> = tip.encode_utf16().take(127).chain([0]).collect();
        self.nid.uFlags = NIF_TIP | NIF_MESSAGE | NIF_ICON | NIF_SHOWTIP;
        self.nid.szTip = [0; 128];
        for (i, c) in tip_wide.iter().enumerate().take(127) {
            self.nid.szTip[i] = *c;
        }
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &self.nid);
        }
    }

    fn balloon(&mut self, title: &str, text: &str) {
        self.nid.uFlags = NIF_INFO;
        let t: Vec<u16> = title.encode_utf16().take(63).chain([0]).collect();
        let x: Vec<u16> = text.encode_utf16().take(255).chain([0]).collect();
        self.nid.szInfoTitle = [0; 64];
        self.nid.szInfo = [0; 256];
        for (i, c) in t.iter().enumerate().take(63) {
            self.nid.szInfoTitle[i] = *c;
        }
        for (i, c) in x.iter().enumerate().take(255) {
            self.nid.szInfo[i] = *c;
        }
        self.nid.dwInfoFlags = 0x1; // NIIF_INFO
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &self.nid);
        }
        self.nid.uFlags = NIF_TIP | NIF_MESSAGE | NIF_ICON | NIF_SHOWTIP;
    }

    fn refresh_state(&mut self) {
        match mirage_cli::commands::service::request_json(mirage_ipc::Command::Status) {
            Ok(status) => {
                self.service_down_announced = false;
                let repos = status["repositories"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                self.mounted = repos
                    .iter()
                    .filter(|r| r["state"].as_str() == Some("ready_mounted"))
                    .filter_map(|r| {
                        // mounted letters come back in the mount record — use
                        // display_name fallback when the letter is absent.
                        r["mount_path"]
                            .as_str()
                            .map(|s| s.trim_end_matches(['\\', '/']).to_owned())
                            .filter(|s| !s.is_empty())
                            .or_else(|| r["display_name"].as_str().map(|s| s.to_owned()))
                    })
                    .collect();
                if self.last_mount_count > 0 && self.mounted.len() > self.last_mount_count {
                    self.balloon("Drive mounted", "A MirageSSD drive is ready in Explorer.");
                }
                self.last_mount_count = self.mounted.len();
                let account = self
                    .account
                    .clone()
                    .unwrap_or_else(|| "not signed in".to_owned());
                self.set_tip(&format!(
                    "MirageSSD — {account} — {} drive{} mounted",
                    self.mounted.len(),
                    if self.mounted.len() == 1 { "" } else { "s" }
                ));
            }
            Err(_) => {
                if !self.service_down_announced {
                    self.service_down_announced = true;
                    self.balloon(
                        "MirageSSD service unavailable",
                        "The background service isn't answering; drives may disconnect.",
                    );
                }
                self.set_tip("MirageSSD — service unavailable");
            }
        }
    }

    fn menu(&mut self) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let open = wide("Open MirageSSD");
            AppendMenuW(menu, MF_STRING, CMD_OPEN as usize, open.as_ptr());
            for (i, letter) in self.mounted.iter().enumerate() {
                let text = wide(&format!("Open {letter}"));
                AppendMenuW(
                    menu,
                    MF_STRING,
                    (CMD_OPEN_DRIVE_BASE + i as u16) as usize,
                    text.as_ptr(),
                );
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            let reclaim = wide("Free up space now");
            AppendMenuW(menu, MF_STRING, CMD_RECLAIM as usize, reclaim.as_ptr());
            let sign = wide("Account…");
            AppendMenuW(menu, MF_STRING, CMD_SIGNIN as usize, sign.as_ptr());
            let diag = wide("Collect diagnostics");
            AppendMenuW(menu, MF_STRING, CMD_DIAGNOSTICS as usize, diag.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            let quit = wide("Quit");
            AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, quit.as_ptr());
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            SetForegroundWindow(self.hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                point.x,
                point.y,
                0,
                self.hwnd,
                ptr::null(),
            );
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyMenu(menu);
            self.command(command as u16);
        }
    }

    fn command(&mut self, command: u16) {
        match command {
            CMD_OPEN => {
                let _ = super::windows_host::open_app_window(&self.url);
            }
            CMD_RECLAIM => {
                match mirage_cli::commands::service::request_json(
                    mirage_ipc::Command::DiskReclaimNow,
                ) {
                    Ok(result) => {
                        let freed = result["reclaimed_bytes"].as_u64().unwrap_or(0);
                        if freed == 0 {
                            self.balloon(
                                "Nothing to free",
                                "Nothing to free right now — everything local is still in use or waiting to upload.",
                            );
                        } else {
                            self.balloon(
                                "Space freed",
                                &format!(
                                    "Freed {} of files that are already in Google Drive.",
                                    mirage_cli::output::format_bytes(freed)
                                ),
                            );
                        }
                    }
                    Err(error) => {
                        self.balloon("Couldn't free up space", &error.to_string());
                    }
                }
            }
            CMD_DIAGNOSTICS => match mirage_cli::commands::diagnostics::collect_zip(None) {
                Ok(path) => self.balloon("Diagnostics saved", &path.to_string_lossy()),
                Err(error) => self.balloon("Couldn't collect diagnostics", &error.to_string()),
            },
            CMD_SIGNIN => {
                // Account actions live on the Settings page — open it directly.
                let _ = super::windows_host::open_app_window(&settings_url(&self.url));
            }
            CMD_QUIT => unsafe {
                Shell_NotifyIconW(NIM_DELETE, &self.notify_data());
                PostQuitMessage(0);
            },
            c if (CMD_OPEN_DRIVE_BASE..CMD_OPEN_DRIVE_BASE + 64).contains(&c) => {
                if let Some(letter) = self.mounted.get((c - CMD_OPEN_DRIVE_BASE) as usize)
                    && let Some(drive) = letter.chars().next()
                {
                    let _ = super::windows_host::open_browser_url(&format!("file:///{drive}:/"));
                }
            }
            _ => {}
        }
    }
}

static TRAY: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_TRAYICON {
        let tray = TRAY.load(Ordering::SeqCst) as *mut Tray;
        if !tray.is_null() {
            unsafe {
                match lparam as u32 {
                    WM_LBUTTONUP => (*tray).command(CMD_OPEN),
                    WM_RBUTTONUP => (*tray).menu(),
                    _ => {}
                }
            }
        }
        return 0;
    }
    if message == WM_COMMAND {
        let tray = TRAY.load(Ordering::SeqCst) as *mut Tray;
        if !tray.is_null() {
            unsafe {
                (*tray).command((wparam & 0xffff) as u16);
            }
        }
        return 0;
    }
    if message == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Runs the tray loop on this thread (the host's accept loop runs elsewhere).
/// Never returns except via Quit.
pub fn run(url: String) -> ! {
    unsafe {
        let class_name = wide("MirageSSD.Tray");
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: GetModuleHandleW(ptr::null()),
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("MirageSSD").as_ptr(),
            0,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            HWND_MESSAGE,
            ptr::null_mut(),
            class.hInstance,
            ptr::null(),
        );
        let icon = small_icon();
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = icon;
        let tip = wide("MirageSSD");
        for (i, c) in tip.iter().enumerate().take(127) {
            nid.szTip[i] = *c;
        }
        Shell_NotifyIconW(NIM_ADD, &nid);

        let account = mirage_cli::commands::service::request_json(mirage_ipc::Command::Status)
            .ok()
            .and_then(|s| s["account_id"].as_str().map(str::to_owned));
        let tray = Box::new(Tray {
            hwnd,
            nid,
            url,
            mounted: Vec::new(),
            account,
            service_down_announced: false,
            last_mount_count: 0,
        });
        TRAY.store(Box::into_raw(tray) as usize, Ordering::SeqCst);

        // Named event a second (non-tray) launch signals to surface the UI.
        let show_event = {
            let name = wide(r"Local\MirageSSD.UI.Show");
            windows_sys::Win32::System::Threading::CreateEventW(ptr::null(), 0, 0, name.as_ptr())
        };

        let tray = TRAY.load(Ordering::SeqCst) as *mut Tray;
        (*tray).refresh_state();
        // Clicks, the second-launch show event, and the 30 s state refresh all
        // wake this wait — no sleep means the menu feels instant.
        let mut next_refresh = Instant::now() + Duration::from_secs(30);
        let handles = [show_event];
        let mut message = MSG::default();
        loop {
            let count = u32::from(!show_event.is_null());
            let wait_ms = next_refresh
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u32::MAX as u128) as u32;
            let signaled = MsgWaitForMultipleObjects(
                count,
                if count == 0 {
                    ptr::null()
                } else {
                    handles.as_ptr()
                },
                0,
                wait_ms,
                QS_ALLINPUT,
            );
            if count == 1 && signaled == WAIT_OBJECT_0 {
                // A second launch asked for the window.
                let url = (*tray).url.clone();
                let _ = super::windows_host::open_app_window(&url);
                continue;
            }
            if signaled == WAIT_OBJECT_0 + count {
                while PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if message.message == WM_QUIT {
                        Shell_NotifyIconW(NIM_DELETE, &(*tray).nid);
                        std::process::exit(0);
                    }
                    DispatchMessageW(&message);
                }
                continue;
            }
            if signaled == WAIT_FAILED {
                // A dead handle would spin until the refresh deadline; don't.
                std::thread::sleep(Duration::from_secs(1));
            }
            // Timeout (or a failed wait): refresh on schedule.
            if Instant::now() >= next_refresh {
                (*tray).refresh_state();
                next_refresh = Instant::now() + Duration::from_secs(30);
            }
        }
    }
}
