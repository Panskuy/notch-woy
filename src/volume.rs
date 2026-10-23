//! Watches the default output device's volume and mute, from any source
//! (keyboard keys, the taskbar slider, apps), follows the default device
//! when it changes (headphones plugged in...), and sets them for the
//! Control Center page.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use windows::core::{implement, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window on every volume or mute change. `wparam` is
/// the volume in percent; `lparam` is 1 when muted.
pub const WM_VOLUME: u32 = WM_APP + 4;

/// What the watcher thread waits for.
enum Message {
    DefaultDeviceChanged,
    SetLevel(f32),
    SetMute(bool),
}

/// The current output level (0..1) and mute.
type Level = Arc<Mutex<(f32, bool)>>;

pub struct Volume {
    messages: Sender<Message>,
    level: Level,
}

impl Volume {
    /// Starts watching on a background thread for the life of the app.
    pub fn spawn(hwnd: HWND) -> Self {
        let (messages, rx) = mpsc::channel();
        let level = Arc::new(Mutex::new((0.0, false)));
        let shared = level.clone();
        let device_changed = messages.clone();
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, rx, device_changed, shared) {
                eprintln!("volume watcher stopped: {e}");
            }
        });
        Self { messages, level }
    }

    /// Level 0..1 and mute, as last read or reported.
    pub fn level(&self) -> (f32, bool) {
        *self.level.lock().unwrap()
    }

    pub fn set_level(&self, level: f32) {
        // Shown right away; the device's own report follows.
        self.level.lock().unwrap().0 = level;
        let _ = self.messages.send(Message::SetLevel(level));
    }

    pub fn set_mute(&self, muted: bool) {
        self.level.lock().unwrap().1 = muted;
        let _ = self.messages.send(Message::SetMute(muted));
    }
}

fn run(hwnd: isize, rx: mpsc::Receiver<Message>, device_changed: Sender<Message>, level: Level) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        let client: IMMNotificationClient = DeviceWatcher { device_changed }.into();
        devices.RegisterEndpointNotificationCallback(&client)?;

        let callback: IAudioEndpointVolumeCallback = VolumeCallback { hwnd, level: level.clone() }.into();
        loop {
            // Without an output device there is nothing to watch until one
            // appears.
            let endpoint = devices
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|device| device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None));
            if let Ok(endpoint) = &endpoint {
                endpoint.RegisterControlChangeNotify(&callback)?;
                let current = (
                    endpoint.GetMasterVolumeLevelScalar().unwrap_or(0.0),
                    endpoint.GetMute().is_ok_and(|m| m.as_bool()),
                );
                *level.lock().unwrap() = current;
            }
            // Serve slider changes until the default device changes, then
            // move over to the new one.
            loop {
                let Ok(message) = rx.recv() else { return Ok(()) };
                let Ok(endpoint) = &endpoint else {
                    if matches!(message, Message::DefaultDeviceChanged) {
                        break;
                    }
                    continue;
                };
                match message {
                    Message::DefaultDeviceChanged => break,
                    Message::SetLevel(v) => {
                        // A drag sends a burst; only the latest value matters.
                        let latest = rx.try_iter().fold(Some(v), |v, m| match m {
                            Message::SetLevel(v) => Some(v),
                            _ => v,
                        });
                        if let Some(v) = latest {
                            let _ = endpoint.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null());
                        }
                    }
                    Message::SetMute(muted) => {
                        let _ = endpoint.SetMute(muted, std::ptr::null());
                    }
                }
            }
            if let Ok(endpoint) = &endpoint {
                let _ = endpoint.UnregisterControlChangeNotify(&callback);
            }
        }
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    hwnd: isize,
    level: Level,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        let Some(data) = (unsafe { data.as_ref() }) else { return Ok(()) };
        *self.level.lock().unwrap() = (data.fMasterVolume, data.bMuted.as_bool());
        let percent = (data.fMasterVolume * 100.0).round() as usize;
        let muted = data.bMuted.as_bool() as isize;
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_VOLUME, WPARAM(percent), LPARAM(muted));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::*;

    use super::*;

    /// Registers the callback with the real default device, nudges the
    /// volume by 1% and puts it back right away (setting the same value
    /// does not notify), and waits for `WM_VOLUME`:
    /// `cargo test notifies_volume -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn notifies_volume() {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
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
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let endpoint: IAudioEndpointVolume = devices
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .unwrap()
                .Activate(CLSCTX_ALL, None)
                .unwrap();
            let level = Arc::new(Mutex::new((0.0, false)));
            let callback: IAudioEndpointVolumeCallback = VolumeCallback { hwnd: hwnd.0 as isize, level }.into();
            endpoint.RegisterControlChangeNotify(&callback).unwrap();

            let original = endpoint.GetMasterVolumeLevelScalar().unwrap();
            let level = if original <= 0.5 { original + 0.01 } else { original - 0.01 };
            endpoint.SetMasterVolumeLevelScalar(level, std::ptr::null()).unwrap();

            let mut msg = MSG::default();
            let start = Instant::now();
            let mut received = None;
            while start.elapsed() < Duration::from_secs(2) {
                if PeekMessageW(&mut msg, Some(hwnd), WM_VOLUME, WM_VOLUME, PM_REMOVE).as_bool() {
                    received = Some((msg.wParam.0, msg.lParam.0));
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            endpoint.SetMasterVolumeLevelScalar(original, std::ptr::null()).unwrap();
            endpoint.UnregisterControlChangeNotify(&callback).unwrap();
            let _ = DestroyWindow(hwnd);

            let restored = endpoint.GetMasterVolumeLevelScalar().unwrap();
            println!(
                "set {:.0}%, received {received:?}, restored to {:.0}%",
                level * 100.0,
                restored * 100.0
            );
            let (percent, _) = received.expect("no WM_VOLUME within 2 s");
            assert_eq!(percent, (level * 100.0).round() as usize);
        }
    }
}

#[implement(IMMNotificationClient)]
struct DeviceWatcher {
    device_changed: Sender<Message>,
}

impl IMMNotificationClient_Impl for DeviceWatcher_Impl {
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            let _ = self.device_changed.send(Message::DefaultDeviceChanged);
        }
        Ok(())
    }

    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}
