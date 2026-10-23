//! Battery state, change notifications, and which changes deserve an alert.

use windows::core::GUID;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::Power::{GetSystemPowerStatus, RegisterPowerSettingNotification, SYSTEM_POWER_STATUS};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

// From the large Win32_System_SystemServices feature; not worth enabling for two constants.
const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);
const GUID_BATTERY_PERCENTAGE_REMAINING: GUID = GUID::from_u128(0xa7ad8041_b45a_4cae_87a3_eecbb468a9e1);

/// Below these, unplugged, a low-battery alert fires (most urgent first).
const LOW_LEVELS: [u8; 2] = [10, 20];

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Battery {
    pub percent: u8,
    /// On AC power, whether or not the battery is still taking charge.
    pub plugged: bool,
    pub charging: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BatteryEvent {
    PluggedIn,
    Unplugged,
    /// Dropped to this level (20 or 10) while unplugged.
    Low(u8),
    Full,
}

/// The current state, or `None` on machines without a battery.
pub fn read() -> Option<Battery> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    // 128: no system battery; 255: unknown status.
    if status.BatteryFlag & 128 != 0 || status.BatteryFlag == 255 || status.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery {
        percent: status.BatteryLifePercent,
        plugged: status.ACLineStatus == 1,
        charging: status.BatteryFlag & 8 != 0,
    })
}

/// Asks Windows to send `WM_POWERBROADCAST` to `hwnd` on every plug/unplug
/// and every 1% change; the default broadcast skips most percentage steps.
pub fn subscribe(hwnd: HWND) {
    for guid in [GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING] {
        // Lives as long as the window; nothing to unregister on exit.
        let _ = unsafe { RegisterPowerSettingNotification(HANDLE(hwnd.0), &guid, DEVICE_NOTIFY_WINDOW_HANDLE) };
    }
}

/// The alert, if any, that going from `before` to `after` deserves.
pub fn event(before: Battery, after: Battery) -> Option<BatteryEvent> {
    if !before.plugged && after.plugged {
        return Some(BatteryEvent::PluggedIn);
    }
    if before.plugged && !after.plugged {
        return Some(BatteryEvent::Unplugged);
    }
    if after.plugged && before.percent < 100 && after.percent == 100 {
        return Some(BatteryEvent::Full);
    }
    if !after.plugged {
        if let Some(&level) = LOW_LEVELS.iter().find(|&&l| before.percent > l && after.percent <= l) {
            return Some(BatteryEvent::Low(level));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(percent: u8, plugged: bool) -> Battery {
        Battery { percent, plugged, charging: plugged && percent < 100 }
    }

    #[test]
    fn plug_and_unplug() {
        assert_eq!(event(at(50, false), at(50, true)), Some(BatteryEvent::PluggedIn));
        assert_eq!(event(at(50, true), at(50, false)), Some(BatteryEvent::Unplugged));
    }

    #[test]
    fn full_only_when_reaching_100_on_ac() {
        assert_eq!(event(at(99, true), at(100, true)), Some(BatteryEvent::Full));
        assert_eq!(event(at(100, true), at(100, true)), None);
    }

    #[test]
    fn low_fires_once_per_threshold() {
        assert_eq!(event(at(21, false), at(20, false)), Some(BatteryEvent::Low(20)));
        assert_eq!(event(at(20, false), at(19, false)), None);
        assert_eq!(event(at(11, false), at(10, false)), Some(BatteryEvent::Low(10)));
        // A jump past both reports the more urgent one.
        assert_eq!(event(at(25, false), at(9, false)), Some(BatteryEvent::Low(10)));
        // Not while charging.
        assert_eq!(event(at(21, true), at(20, true)), None);
    }

    /// Prints this machine's battery: `cargo test reads_this_battery -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_this_battery() {
        println!("{:?}", read());
    }
}
