//! Pins a drive letter to Explorer's Quick Access via the shell's
//! `pintohome` verb — no PowerShell, no elevation. Hand-declared vtables for
//! `IShellFolder`/`IContextMenu` (windows-sys doesn't ship the interfaces).

#![allow(non_camel_case_types, non_snake_case, unsafe_code)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, S_OK};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::Shell::{SHBindToParent, SHParseDisplayName};

type HRESULT = i32;
type IUnknownPtr = *mut core::ffi::c_void;

#[repr(C)]
struct IShellFolder {
    vtable: *const IShellFolderVtbl,
}

#[repr(C)]
#[allow(clippy::upper_case_acronyms)]
struct IShellFolderVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut IShellFolder) -> u32,
    parse_display_name: usize,
    enum_objects: usize,
    bind_to_object: usize,
    bind_to_storage: usize,
    compare_ids: usize,
    create_view_object: usize,
    get_attributes_of: usize,
    get_ui_object_of: unsafe extern "system" fn(
        *mut IShellFolder,
        HWND,
        u32,
        *const *const ITEMIDLIST,
        *const windows_sys::core::GUID,
        *mut u32,
        *mut IUnknownPtr,
    ) -> HRESULT,
    get_display_name_of: usize,
    set_name_of: usize,
}

#[repr(C)]
struct IContextMenu {
    vtable: *const IContextMenuVtbl,
}

#[repr(C)]
struct CMINVOKECOMMANDINFO {
    cb_size: u32,
    mask: u32,
    hwnd: HWND,
    verb: *const i8,
    parameters: *const i8,
    directory: *const u16,
    show: i32,
    hot_key: u32,
    icon: *mut core::ffi::c_void,
}

#[repr(C)]
#[allow(clippy::upper_case_acronyms)]
struct IContextMenuVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut IContextMenu) -> u32,
    query_context_menu: usize,
    invoke_command:
        unsafe extern "system" fn(*mut IContextMenu, *const CMINVOKECOMMANDINFO) -> HRESULT,
    get_command_string:
        unsafe extern "system" fn(*mut IContextMenu, usize, u32, *mut u32, *mut i8, u32) -> HRESULT,
    handle_menu_msg: usize,
}

// IID_IContextMenu {000214E4-0000-0000-C000-000000000046}
const IID_ICONTEXTMENU: windows_sys::core::GUID = windows_sys::core::GUID {
    data1: 0x0002_14E4,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};

const GCS_VERBA: u32 = 0;

/// Invokes the `pintohome` shell verb on `X:\`. Errors are surfaced to the
/// caller so the UI can explain a refusal.
pub fn pin_to_quick_access(letter: char) -> Result<(), String> {
    unsafe {
        // STA for shell verbs.
        let _ = CoInitializeEx(ptr::null(), 0x2 /* COINIT_APARTMENTTHREADED */);
        let path: Vec<u16> = OsStr::new(&format!("{letter}:\\"))
            .encode_wide()
            .chain([0])
            .collect();
        let mut pidl: *mut ITEMIDLIST = ptr::null_mut();
        let hr = SHParseDisplayName(
            path.as_ptr(),
            ptr::null_mut(),
            &mut pidl,
            0,
            ptr::null_mut(),
        );
        if hr != S_OK || pidl.is_null() {
            CoUninitialize();
            return Err(format!("SHParseDisplayName failed 0x{hr:08x}"));
        }
        let result = pin_pidl(pidl);
        CoTaskMemFree(pidl.cast());
        CoUninitialize();
        result
    }
}

fn pin_pidl(pidl: *mut ITEMIDLIST) -> Result<(), String> {
    unsafe { pin_pidl_inner(pidl) }
}

unsafe fn pin_pidl_inner(pidl: *mut ITEMIDLIST) -> Result<(), String> {
    #![allow(unsafe_op_in_unsafe_fn)]
    // IShellFolder IID for SHBindToParent.
    const IID_ISHELLFOLDER: windows_sys::core::GUID = windows_sys::core::GUID {
        data1: 0x0002_14E6,
        data2: 0,
        data3: 0,
        data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
    };
    let mut folder: IUnknownPtr = ptr::null_mut();
    let mut child: *mut ITEMIDLIST = ptr::null_mut();
    let hr = SHBindToParent(
        pidl,
        &IID_ISHELLFOLDER,
        &mut folder,
        &mut child as *mut *mut ITEMIDLIST,
    );
    if hr != S_OK || folder.is_null() || child.is_null() {
        return Err(format!("SHBindToParent failed 0x{hr:08x}"));
    }
    let folder = folder.cast::<IShellFolder>();
    let mut menu: IUnknownPtr = ptr::null_mut();
    let children = [child as *const ITEMIDLIST];
    let mut eaten = 0_u32;
    let hr = ((*(*folder).vtable).get_ui_object_of)(
        folder,
        ptr::null_mut(),
        1,
        children.as_ptr(),
        &IID_ICONTEXTMENU,
        &mut eaten,
        &mut menu,
    );
    ((*(*folder).vtable).release)(folder);
    if hr != S_OK || menu.is_null() {
        return Err(format!("GetUIObjectOf failed 0x{hr:08x}"));
    }
    let menu = menu.cast::<IContextMenu>();

    // Find the pintohome verb's canonical name index.
    let mut invoked = false;
    for index in 0..512_usize {
        let mut buffer = [0_i8; 64];
        let mut eaten = 0_u32;
        let hr = ((*(*menu).vtable).get_command_string)(
            menu,
            index,
            GCS_VERBA,
            &mut eaten,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        );
        if hr != S_OK {
            break;
        }
        let name = buffer
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8 as char)
            .collect::<String>();
        if name == "pintohome" {
            let info = CMINVOKECOMMANDINFO {
                cb_size: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                mask: 0,
                hwnd: ptr::null_mut(),
                verb: index as *const i8,
                parameters: ptr::null(),
                directory: ptr::null(),
                show: 1, // SW_SHOWNORMAL
                hot_key: 0,
                icon: ptr::null_mut(),
            };
            let hr = ((*(*menu).vtable).invoke_command)(menu, &info);
            invoked = hr == S_OK;
            break;
        }
    }
    ((*(*menu).vtable).release)(menu);
    if invoked {
        Ok(())
    } else {
        Err("the pintohome verb was not available for this drive".to_owned())
    }
}
