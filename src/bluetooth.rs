//! Tracks connected Bluetooth devices (classic and Low Energy) on a
//! background thread and reports connects and disconnects.
//!
//! Polls instead of using a device watcher: a watcher filtered on
//! "connected" does not reliably report a device that stops matching.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::core::{Result, HSTRING};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window when devices connect or disconnect.
pub const WM_BLUETOOTH: u32 = WM_APP + 6;

const POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DeviceKind {
    Headphones,
    Keyboard,
    Mouse,
    Phone,
    Other,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Device {
    pub name: String,
    pub kind: DeviceKind,
}

#[derive(Default)]
struct State {
    connected: Vec<Device>,
    /// (device, connected?) since the UI last looked.
    events: VecDeque<(Device, bool)>,
}

pub struct Bluetooth {
    state: Arc<Mutex<State>>,
}

impl Bluetooth {
    /// Starts watching; posts `WM_BLUETOOTH` to `hwnd` on every change.
    pub fn spawn(hwnd: HWND) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, shared) {
                eprintln!("bluetooth watcher stopped: {e}");
            }
        });
        Self { state }
    }

    /// The device the notch's indicator stands for: an audio device if one
    /// is connected (what people care about), else any.
    pub fn primary(&self) -> Option<Device> {
        let state = self.state.lock().unwrap();
        let audio = state.connected.iter().find(|d| d.kind == DeviceKind::Headphones);
        audio.or(state.connected.first()).cloned()
    }

    /// The newest connect or disconnect; older ones are dropped, as the
    /// notch shows one alert at a time.
    pub fn take_latest_event(&self) -> Option<(Device, bool)> {
        let mut state = self.state.lock().unwrap();
        let latest = state.events.pop_back();
        state.events.clear();
        latest
    }
}

fn run(hwnd: isize, state: Arc<Mutex<State>>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let classic = BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)?;
    let le = BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)?;
    // Device kind costs a lookup; it never changes, so it is cached by id.
    let mut kinds: HashMap<String, DeviceKind> = HashMap::new();

    // Whatever is connected at start-up shows in the indicator, without alerts.
    let mut previous = connected(&classic, &le, &mut kinds);
    state.lock().unwrap().connected = previous.iter().map(|(_, d)| d.clone()).collect();
    post(hwnd);

    loop {
        thread::sleep(POLL);
        let now = connected(&classic, &le, &mut kinds);
        let mut events = Vec::new();
        for (id, device) in &now {
            if !previous.iter().any(|(old, _)| old == id) {
                events.push((device.clone(), true));
            }
        }
        for (id, device) in &previous {
            if !now.iter().any(|(new, _)| new == id) {
                events.push((device.clone(), false));
            }
        }
        if events.is_empty() {
            continue;
        }
        let mut state = state.lock().unwrap();
        state.connected = now.iter().map(|(_, d)| d.clone()).collect();
        state.events.extend(events);
        drop(state);
        previous = now;
        post(hwnd);
    }
}

fn post(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_BLUETOOTH, WPARAM(0), LPARAM(0));
    }
}

/// Connected devices as (id, device). A device that is both classic and
/// LE (common for earbuds) is listed once, by name.
fn connected(classic: &HSTRING, le: &HSTRING, kinds: &mut HashMap<String, DeviceKind>) -> Vec<(String, Device)> {
    let mut devices: Vec<(String, Device)> = Vec::new();
    for (selector, is_le) in [(classic, false), (le, true)] {
        let Ok(found) = DeviceInformation::FindAllAsyncAqsFilter(selector).and_then(|op| op.get()) else {
            continue;
        };
        for info in found {
            let (Ok(id), Ok(name)) = (info.Id(), info.Name()) else { continue };
            let (id, name) = (id.to_string(), name.to_string());
            if name.is_empty() || devices.iter().any(|(_, d)| d.name == name) {
                continue;
            }
            let kind = *kinds.entry(id.clone()).or_insert_with(|| lookup_kind(&id, is_le));
            devices.push((id, Device { name, kind }));
        }
    }
    devices
}

fn lookup_kind(id: &str, is_le: bool) -> DeviceKind {
    let id = HSTRING::from(id);
    let kind = if is_le {
        BluetoothLEDevice::FromIdAsync(&id)
            .and_then(|op| op.get())
            .and_then(|d| d.Appearance())
            .and_then(|a| a.RawValue())
            .map(kind_from_appearance)
    } else {
        BluetoothDevice::FromIdAsync(&id)
            .and_then(|op| op.get())
            .and_then(|d| d.ClassOfDevice())
            .and_then(|c| c.RawValue())
            .map(kind_from_class_of_device)
    };
    kind.unwrap_or(DeviceKind::Other)
}

/// Classic Bluetooth "Class of Device": major class in bits 8-12, minor in
/// bits 2-7.
fn kind_from_class_of_device(raw: u32) -> DeviceKind {
    let major = (raw >> 8) & 0x1F;
    let minor = (raw >> 2) & 0x3F;
    match major {
        0x02 => DeviceKind::Phone,
        0x04 => DeviceKind::Headphones,
        // Peripheral: keyboard and pointing-device flags in the minor class.
        0x05 if minor & 0x10 != 0 => DeviceKind::Keyboard,
        0x05 if minor & 0x20 != 0 => DeviceKind::Mouse,
        _ => DeviceKind::Other,
    }
}

/// Bluetooth LE "Appearance": category in the top 10 bits, subcategory in
/// the low 6.
fn kind_from_appearance(raw: u16) -> DeviceKind {
    match (raw >> 6, raw & 0x3F) {
        (0x001, _) => DeviceKind::Phone,
        (0x00F, 0x01) => DeviceKind::Keyboard,
        (0x00F, 0x02) => DeviceKind::Mouse,
        // Audio sink, wearable audio device (earbuds, headsets).
        (0x021 | 0x025, _) => DeviceKind::Headphones,
        _ => DeviceKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_classic_devices() {
        // Wearable headset: major 0x04, minor 0x01.
        assert_eq!(kind_from_class_of_device(0x240404), DeviceKind::Headphones);
        // Keyboard: major 0x05, minor 0x10.
        assert_eq!(kind_from_class_of_device(0x002540), DeviceKind::Keyboard);
        // Mouse: major 0x05, minor 0x20.
        assert_eq!(kind_from_class_of_device(0x002580), DeviceKind::Mouse);
        // Smartphone: major 0x02.
        assert_eq!(kind_from_class_of_device(0x5a020c), DeviceKind::Phone);
        assert_eq!(kind_from_class_of_device(0x000100), DeviceKind::Other);
    }

    #[test]
    fn classifies_le_devices() {
        assert_eq!(kind_from_appearance(0x03C1), DeviceKind::Keyboard);
        assert_eq!(kind_from_appearance(0x03C2), DeviceKind::Mouse);
        assert_eq!(kind_from_appearance(0x0941), DeviceKind::Headphones);
        assert_eq!(kind_from_appearance(0x0040), DeviceKind::Phone);
        assert_eq!(kind_from_appearance(0x0000), DeviceKind::Other);
    }

    /// Lists what is connected now: `cargo test lists_connected -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_connected() {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        let classic = BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected).unwrap();
        let le = BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected).unwrap();
        let devices = connected(&classic, &le, &mut HashMap::new());
        println!("{} connected", devices.len());
        for (_, d) in devices {
            println!("  {:?} ({:?})", d.name, d.kind);
        }
    }
}
