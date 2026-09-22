//! `--tray`: a notification-area companion for the UI host — hidden message
//! window, per-user tray icon, live menu, and balloon notifications.
//!
//! The browser tab is still the UI; the tray simply owns "always running".
//! Closing the tab leaves MirageSSD running in the tray; Quit exits the host.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GetCursorPos, HWND_MESSAGE, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, LoadImageW,
    MF_SEPARATOR, MF_STRING, MSG, PostQuitMessage, RegisterClassW, SetForegroundWindow,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu, WM_APP, WM_COMMAND, WM_DESTROY,
    WNDCLASSW,
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

fn small_icon() -> windows_sys::Win32::Foundation::HANDLE {
    unsafe {
        LoadImageW(
            GetModuleHandleW(ptr::null()),
            IDI_APPLICATION as *const u16,
            IMAGE_ICON,
            16,
            16,
            LR_DEFAULTSIZE | LR_SHARED,
        )
    }
}

struct Tray {
    hwnd: HWND,
    nid: NOTIFYICONDATAW,
    url: String,
    signed_in: bool,
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
        self.signed_in = self.account.is_some();
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
            let sign = wide(if self.signed_in {
                "Sign out…"
            } else {
                "Sign in…"
            });
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
                let _ = super::windows_host::open_browser_url(&self.url);
            }
            CMD_RECLAIM => {
                if let Ok(result) =
                    mirage_cli::commands::service::request_json(mirage_ipc::Command::DiskReclaimNow)
                {
                    let freed = result["reclaimed_bytes"].as_u64().unwrap_or(0);
                    self.balloon(
                        "Space freed",
                        &format!("Moved {freed} bytes of already-uploaded content to Drive."),
                    );
                }
            }
            CMD_DIAGNOSTICS => {
                let _ = mirage_cli::commands::diagnostics::collect_zip(None);
            }
            CMD_SIGNIN => {
                // Signing in/out lives on the account chip — open the window.
                let _ = super::windows_host::open_browser_url(&self.url);
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
            signed_in: account.is_some(),
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
        // Periodic refresh of tooltip + state; messages also pump the icon.
        let mut ticks = 0_u32;
        let mut message = MSG::default();
        loop {
            while windows_sys::Win32::UI::WindowsAndMessaging::PeekMessageW(
                &mut message,
                ptr::null_mut(),
                0,
                0,
                1, // PM_REMOVE
            ) != 0
            {
                if message.message == 0x0012 {
                    // WM_QUIT
                    Shell_NotifyIconW(NIM_DELETE, &(*tray).nid);
                    std::process::exit(0);
                }
                DispatchMessageW(&message);
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
            if !show_event.is_null()
                && windows_sys::Win32::System::Threading::WaitForSingleObject(show_event, 0) == 0
            {
                let url = (*tray).url.clone();
                let _ = super::windows_host::open_browser_url(&url);
            }
            ticks += 5;
            if ticks >= 30 {
                ticks = 0;
                (*tray).refresh_state();
            }
        }
    }
}
