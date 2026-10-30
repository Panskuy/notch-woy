//! Tracks connected Bluetooth devices (classic and Low Energy) on a
//! background thread and reports connects and disconnects.
//!
//! Polls instead of using a device watcher: a watcher filtered on
//! "connected" does not reliably report a device that stops matching.
//!
//! Battery levels are the ones Windows itself reports (as in Settings),
//! read from the PnP manager; see `read_batteries`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, Result, GUID, HSTRING, PCWSTR};
use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Locate_DevNodeW,
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{DEVPROPTYPE, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_STRING};
use windows::Win32::Foundation::{DEVPROPKEY, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window when devices connect or disconnect.
pub const WM_BLUETOOTH: u32 = WM_APP + 6;

const POLL: Duration = Duration::from_secs(2);
/// A device must stay gone this long to count as disconnected.
const DISCONNECT_GRACE: Duration = Duration::from_secs(8);
/// Battery levels are re-read every this many polls, as they change slowly;
/// sooner (`BATTERY_POLLS_PENDING`) while a connected device has none yet,
/// since Windows reads a newly connected device's level a moment later.
const BATTERY_POLLS: u32 = 15;
const BATTERY_POLLS_PENDING: u32 = 3;

/// Windows' battery level for a Bluetooth device, a byte in percent, kept
/// on the device's PnP node (Settings shows the same value).
const BATTERY_KEY: DEVPROPKEY =
    DEVPROPKEY { fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };
/// DEVPKEY_Bluetooth_DeviceAddress: the address as 12 hex digits.
const ADDRESS_KEY: DEVPROPKEY =
    DEVPROPKEY { fmtid: GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a), pid: 1 };

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
    /// Charge in percent, when Windows reports it (most earbuds and
    /// headsets, and LE keyboards and mice).
    pub battery: Option<u8>,
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
    // Kind and address cost a lookup and never change, so they are cached
    // by id.
    let mut details: HashMap<String, (DeviceKind, Option<u64>)> = HashMap::new();
    let mut batteries;
    let mut polls_since_batteries = 0;

    // Whatever is connected at start-up shows in the indicator, without alerts.
    let mut tracker = Tracker::default();
    loop {
        if let Some(found) = connected(&classic, &le, &mut details) {
            batteries = read_batteries();
            tracker.connected = with_batteries(found, &batteries);
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
        let Some(found) = connected(&classic, &le, &mut details) else { continue };
        polls_since_batteries += 1;
        // A device just connected is read at once, so its alert can show
        // its level if Windows has it already.
        let new = found.iter().any(|(d, _)| !tracker.connected.iter().any(|c| c.name == d.name));
        let pending = tracker.connected.iter().any(|d| d.battery.is_none());
        let due = if pending { BATTERY_POLLS_PENDING } else { BATTERY_POLLS };
        if new || polls_since_batteries >= due {
            batteries = read_batteries();
            polls_since_batteries = 0;
        }
        let now = with_batteries(found, &batteries);
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
            match self.connected.iter_mut().find(|d| d.name == device.name) {
                // Still connected; its battery level may have moved, which
                // updates the indicator but is no event.
                Some(known) => {
                    if known != device {
                        *known = device.clone();
                        changed = true;
                    }
                }
                None => {
                    self.connected.push(device.clone());
                    changed = true;
                    if self.note_change(&device.name, true, now, quiet) {
                        events.push((device.clone(), true));
                    }
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

/// Connected devices with their Bluetooth addresses, or `None` if a lookup
/// failed. A device that is both classic and LE (common for earbuds) is
/// listed once, by name, with both addresses: its id differs between the
/// two, so ids cannot tell devices apart, and Windows may report its battery
/// on either.
fn connected(
    classic: &HSTRING,
    le: &HSTRING,
    details: &mut HashMap<String, (DeviceKind, Option<u64>)>,
) -> Option<Vec<(Device, Vec<u64>)>> {
    let mut devices: Vec<(Device, Vec<u64>)> = Vec::new();
    for (selector, is_le) in [(classic, false), (le, true)] {
        let found = DeviceInformation::FindAllAsyncAqsFilter(selector).and_then(|op| op.get()).ok()?;
        for info in found {
            let (Ok(id), Ok(name)) = (info.Id(), info.Name()) else { continue };
            let (id, name) = (id.to_string(), name.to_string());
            if name.is_empty() {
                continue;
            }
            let (kind, address) = match details.get(&id) {
                Some(&known) => known,
                None => match lookup(&id, is_le) {
                    // Cached only when the lookup worked, so a failed one is
                    // tried again on the next poll.
                    Some((kind, address)) => {
                        // Some devices report no class at all; their name may still tell.
                        let kind = if kind == DeviceKind::Other { kind_from_name(&name) } else { kind };
                        *details.entry(id).or_insert((kind, Some(address)))
                    }
                    None => (kind_from_name(&name), None),
                },
            };
            match devices.iter_mut().find(|(d, _)| d.name == name) {
                Some((_, addresses)) => addresses.extend(address),
                None => devices.push((Device { name, kind, battery: None }, address.into_iter().collect())),
            }
        }
    }
    Some(devices)
}

/// The devices, each with its battery level from `batteries` (by address).
fn with_batteries(found: Vec<(Device, Vec<u64>)>, batteries: &HashMap<u64, u8>) -> Vec<Device> {
    found
        .into_iter()
        .map(|(mut device, addresses)| {
            device.battery = addresses.iter().find_map(|a| batteries.get(a)).copied();
            device
        })
        .collect()
}

/// Kind and address of the device with `id`, or `None` if it could not be
/// opened.
fn lookup(id: &str, is_le: bool) -> Option<(DeviceKind, u64)> {
    let id = HSTRING::from(id);
    if is_le {
        let device = BluetoothLEDevice::FromIdAsync(&id).and_then(|op| op.get()).ok()?;
        let kind = device.Appearance().and_then(|a| a.RawValue()).map_or(DeviceKind::Other, kind_from_appearance);
        Some((kind, device.BluetoothAddress().ok()?))
    } else {
        let device = BluetoothDevice::FromIdAsync(&id).and_then(|op| op.get()).ok()?;
        let kind = device.ClassOfDevice().and_then(|c| c.RawValue()).map_or(DeviceKind::Other, kind_from_class_of_device);
        Some((kind, device.BluetoothAddress().ok()?))
    }
}

/// Battery levels Windows reports for Bluetooth devices, by address. They
/// sit on the devices' PnP nodes: a classic device's hands-free node
/// (`BTHENUM\...`), an LE device's own node (`BTHLE\DEV_...`). Read from the
/// PnP manager, which needs no connection to the device, unlike its GATT
/// battery service, and covers classic devices too.
fn read_batteries() -> HashMap<u64, u8> {
    let mut levels = HashMap::new();
    for enumerator in [w!("BTHENUM"), w!("BTHLE")] {
        for id in device_ids(enumerator) {
            let mut node = 0u32;
            if unsafe { CM_Locate_DevNodeW(&mut node, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
                continue;
            }
            let Some(&[level]) = property(node, &BATTERY_KEY, DEVPROP_TYPE_BYTE).as_deref() else { continue };
            let address = property(node, &ADDRESS_KEY, DEVPROP_TYPE_STRING).and_then(|bytes| parse_address(&bytes));
            if let Some(address) = address.filter(|_| level <= 100) {
                levels.insert(address, level);
            }
        }
    }
    levels
}

/// Instance ids (NUL-terminated UTF-16) of the present devices under
/// `enumerator`.
fn device_ids(enumerator: PCWSTR) -> Vec<Vec<u16>> {
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    // The list can grow between sizing the buffer and filling it.
    for _ in 0..3 {
        let mut len = 0u32;
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, enumerator, flags) } != CR_SUCCESS {
            return Vec::new();
        }
        let mut buffer = vec![0u16; len as usize];
        match unsafe { CM_Get_Device_ID_ListW(enumerator, &mut buffer, flags) } {
            CR_SUCCESS => {
                return buffer
                    .split(|&unit| unit == 0)
                    .filter(|id| !id.is_empty())
                    .map(|id| id.iter().copied().chain(Some(0)).collect())
                    .collect();
            }
            CR_BUFFER_SMALL => continue,
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

/// A device node's property, if it is set and of type `expected`.
fn property(node: u32, key: &DEVPROPKEY, expected: DEVPROPTYPE) -> Option<Vec<u8>> {
    let mut kind = DEVPROPTYPE(0);
    let mut size = 0u32;
    // The first call only sizes the value.
    let _ = unsafe { CM_Get_DevNode_PropertyW(node, key, &mut kind, None, &mut size, 0) };
    if size == 0 || size > 512 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    let got = unsafe { CM_Get_DevNode_PropertyW(node, key, &mut kind, Some(buffer.as_mut_ptr()), &mut size, 0) };
    (got == CR_SUCCESS && kind == expected).then(|| {
        buffer.truncate(size as usize);
        buffer
    })
}

/// A Bluetooth address property (UTF-16 hex digits, either case) as a number.
fn parse_address(bytes: &[u8]) -> Option<u64> {
    let units: Vec<u16> =
        bytes.as_chunks::<2>().0.iter().map(|&pair| u16::from_le_bytes(pair)).take_while(|&u| u != 0).collect();
    u64::from_str_radix(&String::from_utf16(&units).ok()?, 16).ok()
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
        Device { name: "Galaxy Buds2 Pro".into(), kind: DeviceKind::Headphones, battery: None }
    }

    #[test]
    fn battery_changes_update_without_alerts() {
        let t0 = Instant::now();
        let mut tracker = Tracker::default();
        let charged = |level| Device { battery: Some(level), ..buds() };
        assert_eq!(tracker.update(vec![charged(80)], t0, QUIET), (true, vec![(charged(80), true)]));
        // A new level: the indicator changes, nothing is alerted.
        assert_eq!(tracker.update(vec![charged(75)], t0 + POLL, QUIET), (true, vec![]));
        assert_eq!(tracker.connected, vec![charged(75)]);
        assert_eq!(tracker.update(vec![charged(75)], t0 + POLL * 2, QUIET), (false, vec![]));
    }

    #[test]
    fn reads_addresses_and_levels_by_address() {
        let utf16 = |s: &str| s.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        assert_eq!(parse_address(&utf16("4142AEFF1990")), Some(0x4142AEFF1990));
        assert_eq!(parse_address(&utf16("cc405f2b9114")), Some(0xCC405F2B9114));
        assert_eq!(parse_address(&utf16("not hex")), None);
        // Earbuds listed under two addresses take the level of either.
        let batteries = HashMap::from([(2, 64)]);
        let found = vec![(buds(), vec![1, 2]), (Device { name: "Mouse".into(), ..buds() }, vec![3])];
        let devices = with_batteries(found, &batteries);
        assert_eq!(devices[0].battery, Some(64));
        assert_eq!(devices[1].battery, None);
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
        let found = connected(&classic, &le, &mut HashMap::new()).expect("lookup");
        let start = Instant::now();
        let batteries = read_batteries();
        println!("battery read: {:.2} ms, {} levels", start.elapsed().as_secs_f64() * 1000.0, batteries.len());
        let devices = with_batteries(found, &batteries);
        println!("{} connected", devices.len());
        for d in devices {
            println!("  {:?} ({:?}), battery {:?}", d.name, d.kind, d.battery);
        }
    }
}
