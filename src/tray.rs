//! The notification-area icon and its menu: settings, start at login, and
//! exit.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::autostart;
use crate::settings::{AlertDuration, AlertKind, NotchWidth, Settings, BLUETOOTH_QUIET_MINUTES};

/// Callback message for clicks on the icon.
pub const WM_TRAY: u32 = WM_APP + 8;
/// The icon resource `build.rs` embeds.
const ICON_ID: u16 = 1;
const ICON_UID: u32 = 1;

const MENU_START_AT_LOGIN: usize = 1;
const MENU_EXIT: usize = 2;
/// Each settings group's items are numbered from its base, in the order
/// of its `ALL` list.
const MENU_ALERT: usize = 100;
const MENU_DURATION: usize = 200;
const MENU_WIDTH: usize = 300;
const MENU_BLUETOOTH_QUIET: usize = 400;

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
/// menu and acts on the choice. Returns the new settings when one of them
/// was changed; saving and applying them is up to the caller.
pub fn on_click(hwnd: HWND, lparam: LPARAM, settings: Settings) -> Option<Settings> {
    let message = lparam.0 as u32;
    if message != WM_LBUTTONUP && message != WM_RBUTTONUP {
        return None;
    }
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let alerts = CreatePopupMenu().ok();
        if let Some(sub) = alerts {
            for (i, kind) in AlertKind::ALL.into_iter().enumerate() {
                let checked = if settings.alert_enabled(kind) { MF_CHECKED } else { MF_UNCHECKED };
                let _ = AppendMenuW(sub, MF_STRING | checked, MENU_ALERT + i, alert_label(kind));
            }
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Alerts"));
        }
        let durations = AlertDuration::ALL.map(|d| match d {
            AlertDuration::Short => w!("Short"),
            AlertDuration::Normal => w!("Normal"),
            AlertDuration::Long => w!("Long"),
        });
        let current = AlertDuration::ALL.iter().position(|&d| d == settings.duration);
        radio_submenu(menu, w!("Alert duration"), MENU_DURATION, &durations, current);
        let widths = NotchWidth::ALL.map(|width| match width {
            NotchWidth::Normal => w!("Normal"),
            NotchWidth::Wide => w!("Wide"),
            NotchWidth::Wider => w!("Wider"),
        });
        let current = NotchWidth::ALL.iter().position(|&width| width == settings.width);
        radio_submenu(menu, w!("Notch width"), MENU_WIDTH, &widths, current);
        let quiet = BLUETOOTH_QUIET_MINUTES.map(|minutes| match minutes {
            0 => w!("Alert every time"),
            1 => w!("Quiet for 1 minute"),
            5 => w!("Quiet for 5 minutes"),
            15 => w!("Quiet for 15 minutes"),
            _ => w!("Quiet for 1 hour"),
        });
        let current = BLUETOOTH_QUIET_MINUTES.iter().position(|&m| m == settings.bluetooth_quiet_minutes);
        radio_submenu(menu, w!("Bluetooth reconnects"), MENU_BLUETOOTH_QUIET, &quiet, current);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
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
        // Takes the submenus with it.
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

        let choice = choice.0 as usize;
        match choice {
            MENU_START_AT_LOGIN => {
                if let Err(e) = autostart::set_enabled(!autostart::is_enabled()) {
                    eprintln!("autostart change failed: {e}");
                }
                None
            }
            MENU_EXIT => {
                let _ = DestroyWindow(hwnd);
                None
            }
            _ => apply_choice(settings, choice),
        }
    }
}

fn alert_label(kind: AlertKind) -> PCWSTR {
    match kind {
        AlertKind::Battery => w!("Battery"),
        AlertKind::Volume => w!("Volume (replaces the Windows pop-up)"),
        AlertKind::Bluetooth => w!("Bluetooth devices"),
        AlertKind::Notifications => w!("App notifications"),
        AlertKind::NowPlaying => w!("New track"),
    }
}

/// Adds a submenu of mutually exclusive `items`, numbered from `base`, with
/// `current` marked.
unsafe fn radio_submenu(menu: HMENU, title: PCWSTR, base: usize, items: &[PCWSTR], current: Option<usize>) {
    let Ok(sub) = (unsafe { CreatePopupMenu() }) else { return };
    unsafe {
        for (i, label) in items.iter().enumerate() {
            let _ = AppendMenuW(sub, MF_STRING, base + i, *label);
        }
        if let Some(i) = current {
            let last = base + items.len() - 1;
            let _ = CheckMenuRadioItem(sub, base as u32, last as u32, (base + i) as u32, MF_BYCOMMAND.0);
        }
        let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, title);
    }
}

/// `settings` with menu item `id` applied, if it is a settings item that
/// changes something.
fn apply_choice(settings: Settings, id: usize) -> Option<Settings> {
    let mut next = settings;
    let index = |base: usize, len: usize| (base..base + len).contains(&id).then(|| id - base);
    if let Some(i) = index(MENU_ALERT, AlertKind::ALL.len()) {
        let kind = AlertKind::ALL[i];
        next.set_alert(kind, !settings.alert_enabled(kind));
    } else if let Some(i) = index(MENU_DURATION, AlertDuration::ALL.len()) {
        next.duration = AlertDuration::ALL[i];
    } else if let Some(i) = index(MENU_WIDTH, NotchWidth::ALL.len()) {
        next.width = NotchWidth::ALL[i];
    } else if let Some(i) = index(MENU_BLUETOOTH_QUIET, BLUETOOTH_QUIET_MINUTES.len()) {
        next.bluetooth_quiet_minutes = BLUETOOTH_QUIET_MINUTES[i];
    }
    (next != settings).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_items_change_settings() {
        let base = Settings::default();
        let volume = AlertKind::ALL.iter().position(|&k| k == AlertKind::Volume).unwrap();
        let off = apply_choice(base, MENU_ALERT + volume).unwrap();
        assert!(!off.alert_enabled(AlertKind::Volume));
        assert_eq!(apply_choice(off, MENU_ALERT + volume), Some(base));
        assert_eq!(apply_choice(base, MENU_DURATION + 2).unwrap().duration, AlertDuration::Long);
        assert_eq!(apply_choice(base, MENU_WIDTH + 1).unwrap().width, NotchWidth::Wide);
        assert_eq!(apply_choice(base, MENU_BLUETOOTH_QUIET).unwrap().bluetooth_quiet_minutes, 0);
        // Picking what is already chosen, or nothing, changes nothing.
        assert_eq!(apply_choice(base, MENU_DURATION + 1), None);
        assert_eq!(apply_choice(base, 0), None);
        assert_eq!(apply_choice(base, MENU_WIDTH + 3), None);
    }
}
