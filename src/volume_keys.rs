//! Takes over the keyboard's volume keys so Windows' own volume pop-up does
//! not show: the keys never reach the shell, the notch changes the volume
//! itself, and its own alert appears instead. The Quick Settings volume
//! slider is not a key and keeps working as usual.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Posted to the notch window for each volume key press (and repeat while
/// held). `wparam` is a `VolumeKey`.
pub const WM_VOLUME_KEY: u32 = WM_APP + 9;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VolumeKey {
    Up,
    Down,
    Mute,
}

impl VolumeKey {
    pub fn from_wparam(wparam: WPARAM) -> Option<Self> {
        [Self::Up, Self::Down, Self::Mute].get(wparam.0).copied()
    }
}

/// The notch window keys are posted to, and the hook, as raw handles: the
/// hook procedure is a plain function with no other way to reach them.
static NOTCH: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
/// Mute toggles once per press, not on every auto-repeat while held.
static MUTE_HELD: AtomicBool = AtomicBool::new(false);

/// Takes the keys over; does nothing if they already are.
pub fn install(hwnd: HWND) {
    NOTCH.store(hwnd.0 as isize, Ordering::Relaxed);
    if HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    let module = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), module, 0) } {
        Ok(hook) => HOOK.store(hook.0 as isize, Ordering::Relaxed),
        Err(e) => eprintln!("volume keys stay with Windows: {e}"),
    }
}

pub fn uninstall() {
    let hook = HOOK.swap(0, Ordering::Relaxed);
    if hook != 0 {
        let _ = unsafe { UnhookWindowsHookEx(HHOOK(hook as _)) };
    }
}

/// Runs for every key on the system, so it only posts and returns: Windows
/// drops hooks that keep input waiting.
unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let key = match VIRTUAL_KEY(info.vkCode as u16) {
            VK_VOLUME_UP => Some(VolumeKey::Up),
            VK_VOLUME_DOWN => Some(VolumeKey::Down),
            VK_VOLUME_MUTE => Some(VolumeKey::Mute),
            _ => None,
        };
        if let Some(key) = key {
            let pressed = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let repeat = key == VolumeKey::Mute && MUTE_HELD.swap(pressed, Ordering::Relaxed) && pressed;
            if pressed && !repeat {
                let notch = HWND(NOTCH.load(Ordering::Relaxed) as _);
                let _ = unsafe { PostMessageW(Some(notch), WM_VOLUME_KEY, WPARAM(key as usize), LPARAM(0)) };
            }
            // Swallowed, press and release: the shell never sees the key,
            // so it shows no volume pop-up.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The volume after a key press, stepping 2% on the even grid Windows uses.
pub fn step(level: f32, key: VolumeKey) -> f32 {
    let percent = (level * 100.0 / 2.0).round() * 2.0;
    let next = match key {
        VolumeKey::Up => percent + 2.0,
        VolumeKey::Down => percent - 2.0,
        VolumeKey::Mute => percent,
    };
    next.clamp(0.0, 100.0) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_like_windows() {
        assert_eq!(step(0.44, VolumeKey::Up), 0.46);
        assert_eq!(step(0.44, VolumeKey::Down), 0.42);
        // Off-grid levels snap to it first.
        assert_eq!(step(0.437, VolumeKey::Up), 0.46);
        assert_eq!(step(1.0, VolumeKey::Up), 1.0);
        assert_eq!(step(0.0, VolumeKey::Down), 0.0);
    }

    /// Presses Volume Up (simulated) with the hook in place and checks the
    /// key arrived here while the system volume stayed put, i.e. Windows
    /// never saw it: `cargo test swallows_volume_keys -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn swallows_volume_keys() {
        use std::time::{Duration, Instant};
        use windows::core::w;
        use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
        use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYBD_EVENT_FLAGS,
        };
        use windows::Win32::UI::WindowsAndMessaging::*;

        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let endpoint: IAudioEndpointVolume =
                devices.GetDefaultAudioEndpoint(eRender, eConsole).unwrap().Activate(CLSCTX_ALL, None).unwrap();
            let before = endpoint.GetMasterVolumeLevelScalar().unwrap();

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("Static"),
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            install(hwnd);

            let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_VOLUME_UP, dwFlags: flags, ..Default::default() } },
            };
            SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);

            // The hook runs on this thread's message loop.
            let mut received = None;
            let start = Instant::now();
            let mut msg = MSG::default();
            while start.elapsed() < Duration::from_millis(800) {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_VOLUME_KEY {
                        received = VolumeKey::from_wparam(msg.wParam);
                    }
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            uninstall();
            let _ = DestroyWindow(hwnd);

            let after = endpoint.GetMasterVolumeLevelScalar().unwrap();
            println!("key received: {received:?}; volume {:.0}% -> {:.0}%", before * 100.0, after * 100.0);
            assert_eq!(received, Some(VolumeKey::Up));
            assert_eq!(before, after, "Windows handled the key");
        }
    }

    #[test]
    fn keys_round_trip_through_wparam() {
        for key in [VolumeKey::Up, VolumeKey::Down, VolumeKey::Mute] {
            assert_eq!(VolumeKey::from_wparam(WPARAM(key as usize)), Some(key));
        }
        assert_eq!(VolumeKey::from_wparam(WPARAM(7)), None);
    }
}
