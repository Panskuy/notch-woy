//! Registers the notch as a shell appbar without reserving any screen space,
//! only so the shell tells us when a fullscreen app opens or closes.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// Message the shell uses to send appbar notifications (ABN_*) to the window.
pub const WM_APPBAR: u32 = WM_APP + 1;

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    }
}

pub fn register(hwnd: HWND) {
    let mut abd = data(hwnd);
    abd.uCallbackMessage = WM_APPBAR;
    unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
}

pub fn unregister(hwnd: HWND) {
    let mut abd = data(hwnd);
    unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
}
