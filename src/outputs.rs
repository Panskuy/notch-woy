//! Audio output devices for the player's output picker, and switching the
//! default one (what Quick Settings' output list and the volume mixer do).
//!
//! Windows has no public call to change the default device; this uses the
//! `IPolicyConfig` interface the Sound settings themselves use, as tools
//! like EarTrumpet and SoundSwitch do.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::thread;

use windows::core::{Result, GUID, HRESULT};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

use policy_config::set_default;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window when the device list has been (re)read.
pub const WM_OUTPUTS: u32 = WM_APP + 10;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum OutputKind {
    Speaker,
    Headphones,
    /// HDMI or DisplayPort audio, through a monitor or TV.
    Display,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Output {
    pub id: String,
    pub name: String,
    pub kind: OutputKind,
    /// The current default output.
    pub default: bool,
}

/// The output list, read on a worker thread on request.
pub struct Outputs {
    hwnd: isize,
    list: Arc<Mutex<Vec<Output>>>,
}

impl Outputs {
    pub fn new(hwnd: HWND) -> Self {
        Self { hwnd: hwnd.0 as isize, list: Arc::default() }
    }

    pub fn list(&self) -> Vec<Output> {
        self.list.lock().unwrap().clone()
    }

    /// Re-reads the devices; posts `WM_OUTPUTS` when done.
    pub fn refresh(&self) {
        self.run(None);
    }

    /// Makes `id` the default output for everything (media, calls,
    /// system sounds), then re-reads the list.
    pub fn select(&self, id: String) {
        self.run(Some(id));
    }

    fn run(&self, select: Option<String>) {
        let (hwnd, list) = (self.hwnd, self.list.clone());
        thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            if let Some(id) = select {
                if let Err(e) = set_default(&id) {
                    eprintln!("switching output failed: {e}");
                }
            }
            match read() {
                Ok(outputs) => *list.lock().unwrap() = outputs,
                Err(e) => eprintln!("reading outputs failed: {e}"),
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as _)), WM_OUTPUTS, WPARAM(0), LPARAM(0));
            }
        });
    }
}

const FRIENDLY_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
const FORM_FACTOR: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e), pid: 0 };

/// Active output devices, in Windows' order.
fn read() -> Result<Vec<Output>> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default_id = devices.GetDefaultAudioEndpoint(eRender, eConsole).and_then(|d| take_id(d.GetId()?)).ok();
        let all = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut outputs = Vec::new();
        for i in 0..all.GetCount()? {
            let device = all.Item(i)?;
            let id = take_id(device.GetId()?)?;
            let store = device.OpenPropertyStore(STGM_READ)?;
            let name = store.GetValue(&FRIENDLY_NAME).map(|v| v.to_string()).unwrap_or_default();
            let form = store.GetValue(&FORM_FACTOR).ok().and_then(|v| u32::try_from(&v).ok()).unwrap_or(0);
            let kind = kind(form, &name);
            outputs.push(Output { default: default_id.as_ref() == Some(&id), id, name, kind });
        }
        Ok(outputs)
    }
}

/// From the endpoint's form factor (EndpointFormFactor), or its name:
/// monitors over HDMI often report themselves as plain speakers.
fn kind(form: u32, name: &str) -> OutputKind {
    let name = name.to_lowercase();
    match form {
        // Headphones, Headset, Handset.
        3 | 5 | 6 => OutputKind::Headphones,
        // DigitalAudioDisplayDevice.
        8 => OutputKind::Display,
        _ if ["monitor", "display", "hdmi", "tv"].iter().any(|w| name.split(|c: char| !c.is_alphanumeric()).any(|word| word == *w))
            || name.contains("high definition audio") && !name.contains("realtek") =>
        {
            OutputKind::Display
        }
        _ => OutputKind::Speaker,
    }
}

/// Copies a COM-allocated id string and frees it.
unsafe fn take_id(id: windows::core::PWSTR) -> Result<String> {
    unsafe {
        let text = id.to_string();
        CoTaskMemFree(Some(id.0 as *const c_void));
        text.map_err(|_| windows::core::Error::from_hresult(HRESULT(0x80070057u32 as i32)))
    }
}

/// The COM interface keeps its own (Windows) method names.
mod policy_config {
    #![allow(non_snake_case)]

    use std::ffi::c_void;

    use windows::core::{IUnknown, IUnknown_Vtbl, Result, GUID, HRESULT, PCWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{eCommunications, eConsole, eMultimedia, ERole};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    /// Makes `id` the default output for media, calls and system sounds.
    pub fn set_default(id: &str) -> Result<()> {
        let wide: Vec<u16> = id.encode_utf16().chain([0]).collect();
        unsafe {
            let policy: IPolicyConfig = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
            for role in [eConsole, eMultimedia, eCommunications] {
                policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role).ok()?;
            }
        }
        Ok(())
    }

    /// Undocumented; the methods before `SetDefaultEndpoint` are declared only
    /// to keep the vtable in order.
    #[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
        fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
        fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
        fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, period: *mut i64, minimum: *mut i64) -> HRESULT;
        fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn GetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
        fn SetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
        fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists outputs and "switches" to the one already default, which
    /// changes nothing: `cargo test lists_outputs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_outputs() {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        let outputs = read().unwrap();
        for o in &outputs {
            println!("{:?} {:?}{}", o.kind, o.name, if o.default { " (default)" } else { "" });
        }
        let current = outputs.iter().find(|o| o.default).expect("a default output");
        set_default(&current.id).unwrap();
        assert!(read().unwrap().iter().any(|o| o.default && o.id == current.id));
    }

    #[test]
    fn kinds_from_form_factor() {
        assert_eq!(kind(3, "Headphones (Realtek(R) Audio)"), OutputKind::Headphones);
        assert_eq!(kind(5, "Galaxy Buds"), OutputKind::Headphones);
        assert_eq!(kind(8, "LG TV"), OutputKind::Display);
        assert_eq!(kind(1, "Speakers (Realtek(R) Audio)"), OutputKind::Speaker);
        assert_eq!(kind(1, "Mi Monitor (NVIDIA High Definition Audio)"), OutputKind::Display);
        assert_eq!(kind(10, "System (Elgato Virtual Audio)"), OutputKind::Speaker);
    }
}
