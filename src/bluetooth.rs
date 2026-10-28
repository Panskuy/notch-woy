//! Tracks connected Bluetooth devices (classic and Low Energy) on a
//! background thread and reports connects and disconnects.
//!
//! Polls instead of using a device watcher: a watcher filtered on
//! "connected" does not reliably report a device that stops matching.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{Result, HSTRING};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window when devices connect or disconnect.
pub const WM_BLUETOOTH: u32 = WM_APP + 6;

const POLL: Duration = Duration::from_secs(2);
/// A device must stay gone this long to count as disconnected.
const DISCONNECT_GRACE: Duration = Duration::from_secs(8);

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
    /// After a device connects (or disconnects), the same change again
    /// within this many seconds is not alerted; the indicator still
    /// follows it. Read by the watcher on every poll.
    repeat_quiet_secs: Arc<AtomicU64>,
}

impl Bluetooth {
    /// Starts watching; posts `WM_BLUETOOTH` to `hwnd` on every change.
    pub fn spawn(hwnd: HWND, repeat_quiet: Duration) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let repeat_quiet_secs = Arc::new(AtomicU64::new(repeat_quiet.as_secs()));
        let (shared, quiet) = (state.clone(), repeat_quiet_secs.clone());
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, shared, quiet) {
                eprintln!("bluetooth watcher stopped: {e}");
            }
        });
        Self { state, repeat_quiet_secs }
    }

    pub fn set_repeat_quiet(&self, quiet: Duration) {
        self.repeat_quiet_secs.store(quiet.as_secs(), Ordering::Relaxed);
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

fn run(hwnd: isize, state: Arc<Mutex<State>>, repeat_quiet_secs: Arc<AtomicU64>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let classic = BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)?;
    let le = BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)?;
    // Device kind costs a lookup; it never changes, so it is cached by id.
    let mut kinds: HashMap<String, DeviceKind> = HashMap::new();

    // Whatever is connected at start-up shows in the indicator, without alerts.
    let mut tracker = Tracker::default();
    loop {
        if let Some(now) = connected(&classic, &le, &mut kinds) {
            tracker.connected = now;
            break;
        }
        thread::sleep(POLL);
    }
    state.lock().unwrap().connected = tracker.connected.clone();
    post(hwnd);

    loop {
        thread::sleep(POLL);
        // A failed lookup says nothing about the devices; skip it rather
        // than read it as everything disconnecting.
        let Some(now) = connected(&classic, &le, &mut kinds) else { continue };
        let quiet = Duration::from_secs(repeat_quiet_secs.load(Ordering::Relaxed));
        let (changed, events) = tracker.update(now, Instant::now(), quiet);
        if !changed {
            continue;
        }
        let mut state = state.lock().unwrap();
        state.connected = tracker.connected.clone();
        state.events.extend(events);
        drop(state);
        post(hwnd);
    }
}

/// Turns polled device lists into connects and disconnects worth an alert.
#[derive(Default)]
struct Tracker {
    /// Devices taken as connected (what the indicator shows).
    connected: Vec<Device>,
    /// Devices gone from the poll but still inside `DISCONNECT_GRACE`, by name.
    missing_since: HashMap<String, Instant>,
    /// Last connect or disconnect per (name, connected?), alerted or not.
    last_change: HashMap<(String, bool), Instant>,
}

impl Tracker {
    /// Takes the devices connected `now`. Returns whether the connected
    /// list changed, and the changes to alert; a repeat of a device's change
    /// within `quiet` is not alerted.
    fn update(&mut self, polled: Vec<Device>, now: Instant, quiet: Duration) -> (bool, Vec<(Device, bool)>) {
        let mut changed = false;
        let mut events = Vec::new();
        for device in &polled {
            self.missing_since.remove(&device.name);
            if !self.connected.iter().any(|d| d.name == device.name) {
                self.connected.push(device.clone());
                changed = true;
                if self.note_change(&device.name, true, now, quiet) {
                    events.push((device.clone(), true));
                }
            }
        }
        let mut kept = Vec::new();
        for device in std::mem::take(&mut self.connected) {
            if polled.iter().any(|d| d.name == device.name) {
                kept.push(device);
                continue;
            }
            // A device that drops for a moment (sleeping keyboards and
            // mice, earbuds switching links) and comes back is no change.
            let since = *self.missing_since.entry(device.name.clone()).or_insert(now);
            if now.duration_since(since) < DISCONNECT_GRACE {
                kept.push(device);
                continue;
            }
            self.missing_since.remove(&device.name);
            changed = true;
            if self.note_change(&device.name, false, now, quiet) {
                events.push((device, false));
            }
        }
        self.connected = kept;
        (changed, events)
    }

    /// Records a connect or disconnect; whether to alert it. A device that
    /// keeps reconnecting alerts once, then stays quiet until it has
    /// settled for `quiet`.
    fn note_change(&mut self, name: &str, connected: bool, now: Instant, quiet: Duration) -> bool {
        let last = self.last_change.insert((name.to_string(), connected), now);
        !last.is_some_and(|t| now.duration_since(t) < quiet)
    }
}

fn post(hwnd: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_BLUETOOTH, WPARAM(0), LPARAM(0));
    }
}

/// Connected devices, or `None` if a lookup failed. A device that is both
/// classic and LE (common for earbuds) is listed once, by name: its id
/// differs between the two, so ids cannot tell devices apart.
fn connected(classic: &HSTRING, le: &HSTRING, kinds: &mut HashMap<String, DeviceKind>) -> Option<Vec<Device>> {
    let mut devices: Vec<Device> = Vec::new();
    for (selector, is_le) in [(classic, false), (le, true)] {
        let found = DeviceInformation::FindAllAsyncAqsFilter(selector).and_then(|op| op.get()).ok()?;
        for info in found {
            let (Ok(id), Ok(name)) = (info.Id(), info.Name()) else { continue };
            let (id, name) = (id.to_string(), name.to_string());
            if name.is_empty() || devices.iter().any(|d| d.name == name) {
                continue;
            }
            let kind = *kinds.entry(id.clone()).or_insert_with(|| match lookup_kind(&id, is_le) {
                // Some devices report no class at all; their name may still tell.
                DeviceKind::Other => kind_from_name(&name),
                kind => kind,
            });
            devices.push(Device { name, kind });
        }
    }
    Some(devices)
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

/// A guess from the name, for devices that report no class.
fn kind_from_name(name: &str) -> DeviceKind {
    let name = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["keyboard", "keychron", "k380", "k780"]) {
        DeviceKind::Keyboard
    } else if has(&["mouse", "mx master", "mx anywhere", "trackpad"]) {
        DeviceKind::Mouse
    } else if has(&["buds", "pods", "headphone", "headset", "earphone", "tws", "wh-", "wf-", "jbl", "soundcore"]) {
        DeviceKind::Headphones
    } else {
        DeviceKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_from_names() {
        assert_eq!(kind_from_name("Galaxy Buds2 Pro"), DeviceKind::Headphones);
        assert_eq!(kind_from_name("Sony WH-1000XM5"), DeviceKind::Headphones);
        assert_eq!(kind_from_name("Logitech MX Master 3S"), DeviceKind::Mouse);
        assert_eq!(kind_from_name("Keychron K2"), DeviceKind::Keyboard);
        assert_eq!(kind_from_name("AULA-S75Pro-1"), DeviceKind::Other);
    }

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

    const QUIET: Duration = Duration::from_secs(5 * 60);

    fn buds() -> Device {
        Device { name: "Galaxy Buds2 Pro".into(), kind: DeviceKind::Headphones }
    }

    #[test]
    fn brief_drops_are_no_change() {
        let t0 = Instant::now();
        let mut tracker = Tracker { connected: vec![buds()], ..Default::default() };
        // Gone for two polls, then back: nothing to report.
        assert_eq!(tracker.update(vec![], t0, QUIET), (false, vec![]));
        assert_eq!(tracker.update(vec![], t0 + POLL, QUIET), (false, vec![]));
        assert_eq!(tracker.update(vec![buds()], t0 + POLL * 2, QUIET), (false, vec![]));
        assert_eq!(tracker.connected, vec![buds()]);
        // Gone past the grace period: one disconnect.
        let t1 = t0 + Duration::from_secs(60);
        assert_eq!(tracker.update(vec![], t1, QUIET), (false, vec![]));
        assert_eq!(tracker.update(vec![], t1 + DISCONNECT_GRACE, QUIET), (true, vec![(buds(), false)]));
        assert!(tracker.connected.is_empty());
    }

    #[test]
    fn repeated_reconnects_alert_once() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.update(vec![buds()], t0, QUIET), (true, vec![(buds(), true)]));
        // Drops and comes back a few times: the first drop is alerted, the
        // rest only move the indicator.
        let mut t = t0;
        for round in 0..3 {
            t += Duration::from_secs(30);
            tracker.update(vec![], t, QUIET);
            t += DISCONNECT_GRACE;
            let alerted = if round == 0 { vec![(buds(), false)] } else { vec![] };
            assert_eq!(tracker.update(vec![], t, QUIET), (true, alerted));
            t += Duration::from_secs(30);
            assert_eq!(tracker.update(vec![buds()], t, QUIET), (true, vec![]));
        }
        // Settled long enough: alerts again.
        t += QUIET;
        tracker.update(vec![], t, QUIET);
        t += DISCONNECT_GRACE;
        assert_eq!(tracker.update(vec![], t, QUIET), (true, vec![(buds(), false)]));
        t += QUIET;
        assert_eq!(tracker.update(vec![buds()], t, QUIET), (true, vec![(buds(), true)]));
    }

    #[test]
    fn zero_quiet_alerts_every_change() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let mut t = t0;
        for _ in 0..3 {
            assert_eq!(tracker.update(vec![buds()], t, Duration::ZERO), (true, vec![(buds(), true)]));
            t += Duration::from_secs(30);
            tracker.update(vec![], t, Duration::ZERO);
            t += DISCONNECT_GRACE;
            assert_eq!(tracker.update(vec![], t, Duration::ZERO), (true, vec![(buds(), false)]));
            t += Duration::from_secs(30);
        }
    }

    #[test]
    fn connect_then_disconnect_both_alert() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        assert_eq!(tracker.update(vec![buds()], t0, QUIET), (true, vec![(buds(), true)]));
        let t1 = t0 + Duration::from_secs(60);
        tracker.update(vec![], t1, QUIET);
        assert_eq!(tracker.update(vec![], t1 + DISCONNECT_GRACE, QUIET), (true, vec![(buds(), false)]));
    }

    /// Lists what is connected now: `cargo test lists_connected -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_connected() {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        let classic = BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected).unwrap();
        let le = BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected).unwrap();
        let devices = connected(&classic, &le, &mut HashMap::new()).expect("lookup");
        println!("{} connected", devices.len());
        for d in devices {
            println!("  {:?} ({:?})", d.name, d.kind);
        }
    }
}
