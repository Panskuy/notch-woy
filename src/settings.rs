//! User choices from the tray menu: which alerts show, for how long, how
//! wide the resting notch is, and how long a reconnecting Bluetooth device
//! stays quiet. Kept as DWORDs under `HKCU\Software\notch-woy`; a missing or
//! unknown value falls back to its default.

use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD};

/// Shared with `autostart`.
const KEY: PCWSTR = w!(r"Software\notch-woy");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlertKind {
    Battery,
    Volume,
    Bluetooth,
    Notifications,
    NowPlaying,
}

impl AlertKind {
    pub const ALL: [Self; 5] = [Self::Battery, Self::Volume, Self::Bluetooth, Self::Notifications, Self::NowPlaying];

    fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl AlertKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Battery => "Battery",
            Self::Volume => "Volume (replaces the Windows pop-up)",
            Self::Bluetooth => "Bluetooth devices",
            Self::Notifications => "App notifications",
            Self::NowPlaying => "New track",
        }
    }
}

/// How long alerts stay up, relative to each kind's own length. The
/// discriminants are what is stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlertDuration {
    Short = 0,
    Normal = 1,
    Long = 2,
}

impl AlertDuration {
    pub const ALL: [Self; 3] = [Self::Short, Self::Normal, Self::Long];

    /// `base_ms` scaled for this choice.
    pub fn scale(self, base_ms: u32) -> u32 {
        match self {
            Self::Short => base_ms * 3 / 5,
            Self::Normal => base_ms,
            Self::Long => base_ms * 8 / 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Short => "Short",
            Self::Normal => "Normal",
            Self::Long => "Long",
        }
    }
}

/// How much narrower or wider than the default the resting notch is. The
/// discriminants are what is stored: Narrow came later, so it is 3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotchWidth {
    Normal = 0,
    Wide = 1,
    Wider = 2,
    Narrow = 3,
}

impl NotchWidth {
    /// In the order they are offered.
    pub const ALL: [Self; 4] = [Self::Narrow, Self::Normal, Self::Wide, Self::Wider];

    /// Logical px added to the default resting width.
    pub fn extra(self) -> f32 {
        match self {
            Self::Narrow => -40.0,
            Self::Normal => 0.0,
            Self::Wide => 40.0,
            Self::Wider => 80.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Narrow => "Narrow",
            Self::Normal => "Normal",
            Self::Wide => "Wide",
            Self::Wider => "Wider",
        }
    }
}

/// Label for one of `BLUETOOTH_QUIET_MINUTES`.
pub fn bluetooth_quiet_label(minutes: u32) -> &'static str {
    match minutes {
        0 => "Alert every time",
        1 => "Quiet for 1 minute",
        5 => "Quiet for 5 minutes",
        15 => "Quiet for 15 minutes",
        _ => "Quiet for 1 hour",
    }
}

/// Choices for how long a Bluetooth device's repeated connect (or
/// disconnect) stays quiet after the first alert, in minutes; 0 alerts
/// every one.
pub const BLUETOOTH_QUIET_MINUTES: [u32; 5] = [0, 1, 5, 15, 60];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// Bit per `AlertKind`.
    alerts: u32,
    pub duration: AlertDuration,
    pub width: NotchWidth,
    /// One of `BLUETOOTH_QUIET_MINUTES`.
    pub bluetooth_quiet_minutes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            alerts: AlertKind::ALL.iter().fold(0, |bits, kind| bits | kind.bit()),
            duration: AlertDuration::Normal,
            width: NotchWidth::Normal,
            bluetooth_quiet_minutes: 5,
        }
    }
}

impl Settings {
    pub fn alert_enabled(&self, kind: AlertKind) -> bool {
        self.alerts & kind.bit() != 0
    }

    pub fn set_alert(&mut self, kind: AlertKind, on: bool) {
        if on {
            self.alerts |= kind.bit();
        } else {
            self.alerts &= !kind.bit();
        }
    }

    pub fn bluetooth_quiet(&self) -> Duration {
        Duration::from_secs(self.bluetooth_quiet_minutes as u64 * 60)
    }

    pub fn load() -> Self {
        Self::from_values(read(w!("Alerts")), read(w!("AlertDuration")), read(w!("NotchWidth")), read(w!("BluetoothQuietMinutes")))
    }

    /// Settings from stored values, each `None` when missing.
    fn from_values(alerts: Option<u32>, duration: Option<u32>, width: Option<u32>, quiet: Option<u32>) -> Self {
        let default = Self::default();
        let all = default.alerts;
        Self {
            alerts: alerts.map_or(all, |bits| bits & all),
            duration: duration
                .and_then(|v| AlertDuration::ALL.into_iter().find(|&d| d as u32 == v))
                .unwrap_or(default.duration),
            width: width.and_then(|v| NotchWidth::ALL.into_iter().find(|&w| w as u32 == v)).unwrap_or(default.width),
            bluetooth_quiet_minutes: quiet
                .filter(|m| BLUETOOTH_QUIET_MINUTES.contains(m))
                .unwrap_or(default.bluetooth_quiet_minutes),
        }
    }

    pub fn save(&self) {
        write(w!("Alerts"), self.alerts);
        write(w!("AlertDuration"), self.duration as u32);
        write(w!("NotchWidth"), self.width as u32);
        write(w!("BluetoothQuietMinutes"), self.bluetooth_quiet_minutes);
    }
}

fn read(name: PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    }
    .is_ok()
    .then_some(value)
}

fn write(name: PCWSTR, value: u32) {
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            REG_DWORD.0,
            Some((&value as *const u32).cast()),
            size_of::<u32>() as u32,
        )
    };
    if result.is_err() {
        eprintln!("saving setting failed: {result:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_every_alert() {
        let settings = Settings::default();
        assert!(AlertKind::ALL.iter().all(|&kind| settings.alert_enabled(kind)));
        assert_eq!(settings.bluetooth_quiet(), Duration::from_secs(5 * 60));
    }

    #[test]
    fn toggles_one_alert() {
        let mut settings = Settings::default();
        settings.set_alert(AlertKind::Volume, false);
        assert!(!settings.alert_enabled(AlertKind::Volume));
        assert!(settings.alert_enabled(AlertKind::Battery));
        settings.set_alert(AlertKind::Volume, true);
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn bad_stored_values_fall_back() {
        assert_eq!(Settings::from_values(None, None, None, None), Settings::default());
        let odd = Settings::from_values(Some(u32::MAX), Some(9), Some(7), Some(3));
        assert_eq!(odd, Settings::default());
        // Stored numbers stay what they were before Narrow was added.
        assert_eq!(Settings::from_values(None, None, Some(0), None).width, NotchWidth::Normal);
        assert_eq!(Settings::from_values(None, None, Some(2), None).width, NotchWidth::Wider);
        assert_eq!(Settings::from_values(None, None, Some(3), None).width, NotchWidth::Narrow);
        let set = Settings::from_values(Some(0), Some(2), Some(1), Some(0));
        assert!(AlertKind::ALL.iter().all(|&kind| !set.alert_enabled(kind)));
        assert_eq!(set.duration, AlertDuration::Long);
        assert_eq!(set.width, NotchWidth::Wide);
        assert_eq!(set.bluetooth_quiet(), Duration::ZERO);
    }

    #[test]
    fn scales_durations() {
        assert_eq!(AlertDuration::Normal.scale(3000), 3000);
        assert_eq!(AlertDuration::Short.scale(3000), 1800);
        assert_eq!(AlertDuration::Long.scale(3000), 4800);
    }

    /// Writes and reads back the real registry values, restoring them:
    /// `cargo test round_trips_registry -- --ignored`.
    #[test]
    #[ignore]
    fn round_trips_registry() {
        let before = Settings::load();
        let mut changed = before;
        changed.set_alert(AlertKind::Bluetooth, !before.alert_enabled(AlertKind::Bluetooth));
        changed.duration = if before.duration == AlertDuration::Long { AlertDuration::Short } else { AlertDuration::Long };
        changed.save();
        assert_eq!(Settings::load(), changed);
        before.save();
        assert_eq!(Settings::load(), before);
    }
}
