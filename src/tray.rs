//! The notification-area icon and its menu: start at login, and exit.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::autostart;

/// Callback message for clicks on the icon.
pub const WM_TRAY: u32 = WM_APP + 8;
/// The icon resource `build.rs` embeds.
const ICON_ID: u16 = 1;
const ICON_UID: u32 = 1;

const MENU_START_AT_LOGIN: usize = 1;
const MENU_EXIT: usize = 2;

/// Explorer broadcasts this when it (re)starts; the icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

pub fn taskbar_created_message() -> u32 {
    TASKBAR_CREATED.load(Ordering::Relaxed)
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_UID, ..Default::default() }
}

pub fn add(hwnd: HWND) {
    unsafe {
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        let mut data = data(hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        let instance = GetModuleHandleW(None).ok().map(Into::into);
        let size = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        // MAKEINTRESOURCE(ICON_ID).
        let name = PCWSTR(ICON_ID as usize as *const u16);
        if let Ok(icon) = LoadImageW(instance, name, IMAGE_ICON, size.0, size.1, LR_DEFAULTCOLOR) {
            data.hIcon = HICON(icon.0);
        }
        let tip: Vec<u16> = "notch-woy".encode_utf16().collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

pub fn remove(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
    }
}

/// Handles a click on the icon (`lparam` is the mouse message): shows the
/// menu and acts on the choice.
pub fn on_click(hwnd: HWND, lparam: LPARAM) {
    let message = lparam.0 as u32;
    if message != WM_LBUTTONUP && message != WM_RBUTTONUP {
        return;
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let checked = if autostart::is_enabled() { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(menu, MF_STRING | checked, MENU_START_AT_LOGIN, w!("Start at login"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit"));

        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        // Without this the menu would not close when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let choice = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            cursor.x,
            cursor.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

        match choice.0 as usize {
            MENU_START_AT_LOGIN => {
                if let Err(e) = autostart::set_enabled(!autostart::is_enabled()) {
                    eprintln!("autostart change failed: {e}");
                }
            }
            MENU_EXIT => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}
