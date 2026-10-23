//! Control Center switches: Wi-Fi and Bluetooth radios, airplane mode, and
//! screen brightness. A worker thread runs the (sometimes slow) system
//! calls and reports the resulting state.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use windows::core::{w, Error, Result, BOOL, BSTR};
use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitor, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::{VARIANT, VT_I4, VT_UI1};
use windows::Win32::System::Wmi::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window after the state has been (re)read.
pub const WM_CONTROLS: u32 = WM_APP + 7;

/// Must run on the main thread before anything else uses COM: WMI (screen
/// brightness) rejects calls made at the default impersonation level, and
/// this process-wide setting can only be made once, first.
pub fn init_com_security() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        );
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct ControlState {
    /// `None` when the machine has no such radio.
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
    /// Percent; `None` when no screen offers software brightness (an
    /// external monitor without DDC/CI, say).
    pub brightness: Option<u8>,
}

impl ControlState {
    /// Airplane mode, as far as this app can tell: every radio off.
    pub fn airplane(&self) -> bool {
        self.wifi != Some(true) && self.bluetooth != Some(true) && (self.wifi.is_some() || self.bluetooth.is_some())
    }
}

#[derive(Clone, Copy)]
pub enum ControlCommand {
    Refresh,
    SetWifi(bool),
    SetBluetooth(bool),
    SetAirplane(bool),
    SetBrightness(u8),
}

pub struct Controls {
    commands: Sender<ControlCommand>,
    state: Arc<Mutex<ControlState>>,
}

impl Controls {
    /// Starts the worker; it posts `WM_CONTROLS` to `hwnd` after each batch.
    pub fn spawn(hwnd: HWND) -> Self {
        let (commands, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(ControlState::default()));
        let shared = state.clone();
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, rx, shared) {
                eprintln!("controls worker stopped: {e}");
            }
        });
        let controls = Self { commands, state };
        controls.send(ControlCommand::Refresh);
        controls
    }

    pub fn send(&self, command: ControlCommand) {
        let _ = self.commands.send(command);
    }

    pub fn state(&self) -> ControlState {
        *self.state.lock().unwrap()
    }
}

fn run(hwnd: isize, rx: Receiver<ControlCommand>, shared: Arc<Mutex<ControlState>>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    // Without access the radio list is still readable; switching just fails.
    let _ = Radio::RequestAccessAsync().and_then(|op| op.get());
    let wmi = connect_wmi().ok();
    let mut externals = external_displays();

    while let Ok(first) = rx.recv() {
        // A brightness drag sends a burst; only the latest value matters.
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        let last_brightness = batch.iter().rposition(|c| matches!(c, ControlCommand::SetBrightness(_)));
        for (i, command) in batch.into_iter().enumerate() {
            let result = match command {
                ControlCommand::Refresh => {
                    // Monitors may have been plugged in or out since.
                    externals = external_displays();
                    Ok(())
                }
                ControlCommand::SetWifi(on) => set_radio(RadioKind::WiFi, on),
                ControlCommand::SetBluetooth(on) => set_radio(RadioKind::Bluetooth, on),
                ControlCommand::SetAirplane(on) => {
                    set_radio(RadioKind::WiFi, !on).and(set_radio(RadioKind::Bluetooth, !on))
                }
                // One slider for every screen: the laptop panel and any
                // external monitors.
                ControlCommand::SetBrightness(percent) if Some(i) == last_brightness => {
                    for display in &externals {
                        display.set_percent(percent);
                    }
                    match &wmi {
                        Some(wmi) => set_brightness(wmi, percent),
                        None => Ok(()),
                    }
                }
                ControlCommand::SetBrightness(_) => Ok(()),
            };
            if let Err(e) = result {
                eprintln!("control failed: {e}");
            }
        }

        // The slider shows the laptop panel's level, or an external
        // monitor's on a desktop.
        let panel = wmi.as_ref().and_then(|wmi| brightness(wmi).ok());
        let state = ControlState {
            wifi: radio_state(RadioKind::WiFi),
            bluetooth: radio_state(RadioKind::Bluetooth),
            brightness: panel.or_else(|| externals.iter().find_map(ExternalDisplay::percent)),
        };
        *shared.lock().unwrap() = state;
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as _)), WM_CONTROLS, WPARAM(0), LPARAM(0));
        }
    }
    Ok(())
}

fn find_radio(kind: RadioKind) -> Option<Radio> {
    let radios = Radio::GetRadiosAsync().and_then(|op| op.get()).ok()?;
    radios.into_iter().find(|r| r.Kind().is_ok_and(|k| k == kind))
}

fn radio_state(kind: RadioKind) -> Option<bool> {
    find_radio(kind).and_then(|r| r.State().ok()).map(|s| s == RadioState::On)
}

fn set_radio(kind: RadioKind, on: bool) -> Result<()> {
    let Some(radio) = find_radio(kind) else { return Ok(()) };
    let state = if on { RadioState::On } else { RadioState::Off };
    radio.SetStateAsync(state)?.get()?;
    Ok(())
}

/// An external monitor whose brightness can be set over DDC/CI (the link
/// most monitors offer over HDMI/DisplayPort; some have it off in their
/// own menu).
struct ExternalDisplay {
    handle: HANDLE,
    min: u32,
    max: u32,
}

impl ExternalDisplay {
    fn percent(&self) -> Option<u8> {
        let (mut min, mut current, mut max) = (0, 0, 0);
        if unsafe { GetMonitorBrightness(self.handle, &mut min, &mut current, &mut max) } == 0 || max <= min {
            return None;
        }
        Some((((current.clamp(min, max) - min) * 100) / (max - min)) as u8)
    }

    fn set_percent(&self, percent: u8) {
        let value = self.min + (self.max - self.min) * percent.min(100) as u32 / 100;
        // Slow (tens of ms) and best effort: the monitor may refuse.
        unsafe { SetMonitorBrightness(self.handle, value) };
    }
}

impl Drop for ExternalDisplay {
    fn drop(&mut self) {
        let _ = unsafe { DestroyPhysicalMonitor(self.handle) };
    }
}

/// Every connected monitor that answers a DDC/CI brightness query. The
/// laptop's own panel does not (it goes through WMI), so it is left out.
fn external_displays() -> Vec<ExternalDisplay> {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        unsafe { (*(list.0 as *mut Vec<HMONITOR>)).push(monitor) };
        true.into()
    }
    let mut monitors: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut monitors as *mut Vec<HMONITOR> as isize));
    }

    let mut displays = Vec::new();
    for monitor in monitors {
        let mut count = 0;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count) }.is_err() || count == 0 {
            continue;
        }
        let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, &mut physical) }.is_err() {
            continue;
        }
        for p in physical {
            let (mut min, mut current, mut max) = (0, 0, 0);
            let answers = unsafe { GetMonitorBrightness(p.hPhysicalMonitor, &mut min, &mut current, &mut max) } != 0;
            if answers && max > min {
                displays.push(ExternalDisplay { handle: p.hPhysicalMonitor, min, max });
            } else {
                let _ = unsafe { DestroyPhysicalMonitor(p.hPhysicalMonitor) };
            }
        }
    }
    displays
}

fn connect_wmi() -> Result<IWbemServices> {
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        locator.ConnectServer(&BSTR::from(r"ROOT\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
    }
}

/// The first row of a WMI query.
fn first_row(wmi: &IWbemServices, query: &str) -> Result<IWbemClassObject> {
    unsafe {
        let rows = wmi.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        let mut row = [None; 1];
        let mut returned = 0;
        rows.Next(WBEM_INFINITE, &mut row, &mut returned).ok()?;
        row[0].take().ok_or_else(Error::empty)
    }
}

fn brightness(wmi: &IWbemServices) -> Result<u8> {
    let row = first_row(wmi, "SELECT CurrentBrightness FROM WmiMonitorBrightness")?;
    let mut value = VARIANT::default();
    unsafe {
        row.Get(w!("CurrentBrightness"), 0, &mut value, None, None)?;
        // A uint8 (VT_UI1).
        Ok(value.Anonymous.Anonymous.Anonymous.bVal)
    }
}

/// Calls `WmiMonitorBrightnessMethods.WmiSetBrightness(Timeout, Brightness)`.
fn set_brightness(wmi: &IWbemServices, percent: u8) -> Result<()> {
    unsafe {
        let instance = first_row(wmi, "SELECT * FROM WmiMonitorBrightnessMethods")?;
        let mut path = VARIANT::default();
        instance.Get(w!("__PATH"), 0, &mut path, None, None)?;
        let path = (*path.Anonymous.Anonymous.Anonymous.bstrVal).clone();

        let mut class = None;
        wmi.GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut class), None)?;
        let class = class.ok_or_else(Error::empty)?;
        let mut signature = None;
        class.GetMethod(w!("WmiSetBrightness"), 0, &mut signature, std::ptr::null_mut())?;
        let params = signature.ok_or_else(Error::empty)?.SpawnInstance(0)?;

        // WMI takes uint32 as VT_I4 and uint8 as VT_UI1.
        let mut timeout = VARIANT::default();
        (*timeout.Anonymous.Anonymous).vt = VT_I4;
        (*timeout.Anonymous.Anonymous).Anonymous.lVal = 0;
        params.Put(w!("Timeout"), 0, &timeout, 0)?;
        let mut level = VARIANT::default();
        (*level.Anonymous.Anonymous).vt = VT_UI1;
        (*level.Anonymous.Anonymous).Anonymous.bVal = percent.min(100);
        params.Put(w!("Brightness"), 0, &level, 0)?;

        wmi.ExecMethod(&path, &BSTR::from("WmiSetBrightness"), WBEM_GENERIC_FLAG_TYPE(0), None, &params, None, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the controls (no changes): `cargo test reads_controls -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_controls() {
        init_com_security();
        let wmi = connect_wmi().ok();
        let state = ControlState {
            wifi: radio_state(RadioKind::WiFi),
            bluetooth: radio_state(RadioKind::Bluetooth),
            brightness: wmi.as_ref().and_then(|wmi| brightness(wmi).ok()),
        };
        println!("{state:?}, airplane {}", state.airplane());
    }

    /// Lists monitors that answer DDC/CI brightness (no changes):
    /// `cargo test reads_external_displays -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_external_displays() {
        let displays = external_displays();
        println!("{} external display(s) with DDC/CI brightness", displays.len());
        for d in &displays {
            println!("  range {}..{}, now {:?}%", d.min, d.max, d.percent());
        }
    }

    /// Sets each external monitor to its current brightness (no visible
    /// change) the way the slider does:
    /// `cargo test sets_external_brightness -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn sets_external_brightness() {
        for d in external_displays() {
            let current = d.percent().unwrap();
            d.set_percent(current);
            println!("set external to its current {current}%: now {:?}%", d.percent());
        }
    }

    /// Sets the brightness to its current value (no visible change) through
    /// the same method call a slider uses:
    /// `cargo test sets_brightness -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn sets_brightness() {
        init_com_security();
        let wmi = connect_wmi().unwrap();
        let current = brightness(&wmi).unwrap();
        set_brightness(&wmi, current).unwrap();
        println!("set brightness to its current {current}%: ok, now {}%", brightness(&wmi).unwrap());
    }
}
