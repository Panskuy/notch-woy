//! Start at login, via the per-user Run key. On by default the first time
//! the app runs; after that it follows the tray menu's choice.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_ANY,
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("notch-woy");
/// Remembers that the default has been applied, so turning autostart off
/// is not undone on the next launch.
const SETTINGS_KEY: PCWSTR = w!(r"Software\notch-woy");
const CHOSEN_VALUE: PCWSTR = w!("AutostartChosen");

pub fn is_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_ANY, None, None, None) }.is_ok()
}

pub fn set_enabled(on: bool) -> windows::core::Result<()> {
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_win32())?;
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * size_of::<u16>()) as u32,
            )
            .ok()?;
        } else if is_enabled() {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE).ok()?;
        }
        let chosen = 1u32;
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            CHOSEN_VALUE,
            REG_DWORD.0,
            Some((&chosen as *const u32).cast()),
            size_of::<u32>() as u32,
        )
        .ok()
    }
}

/// On first run, turns autostart on. Later runs keep the user's choice,
/// rewriting the entry if it is on so it follows the exe if it moved.
// Called by release builds only; see `main`.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub fn apply_default() -> windows::core::Result<()> {
    let chosen =
        unsafe { RegGetValueW(HKEY_CURRENT_USER, SETTINGS_KEY, CHOSEN_VALUE, RRF_RT_ANY, None, None, None) }.is_ok();
    if !chosen || is_enabled() { set_enabled(true) } else { Ok(()) }
}
