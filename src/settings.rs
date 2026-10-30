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

/// How big the notch opens when hovered, against the default. The whole
/// open notch scales: its shape and everything on it. The discriminants
/// are what is stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenSize {
    Normal = 0,
    Compact = 1,
    Large = 2,
}

impl OpenSize {
    /// In the order they are offered.
    pub const ALL: [Self; 3] = [Self::Compact, Self::Normal, Self::Large];

    /// Scale of the open notch.
    pub fn zoom(self) -> f32 {
        match self {
            Self::Compact => 0.9,
            Self::Normal => 1.0,
            Self::Large => 1.1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Normal => "Normal",
            Self::Large => "Large",
        }
    }
}

/// A page of the open notch. The discriminants are what is stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Page {
    Shelf = 0,
    Player = 1,
    Controls = 2,
}

impl Page {
    pub const ALL: [Self; 3] = [Self::Shelf, Self::Player, Self::Controls];

    pub fn label(self) -> &'static str {
        match self {
            Self::Shelf => "Shelf",
            Self::Player => "Now Playing",
            Self::Controls => "Control Center",
        }
    }

    fn from_u32(v: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|&p| p as u32 == v)
    }
}

/// Every order the three pages can be in; `to_bits` sends the index.
const PAGE_ORDERS: [[Page; 3]; 6] = [
    [Page::Shelf, Page::Player, Page::Controls],
    [Page::Shelf, Page::Controls, Page::Player],
    [Page::Player, Page::Shelf, Page::Controls],
    [Page::Player, Page::Controls, Page::Shelf],
    [Page::Controls, Page::Shelf, Page::Player],
    [Page::Controls, Page::Player, Page::Shelf],
];

/// Stored as one DWORD: a page per 4 bits, left to right.
fn order_to_u32(order: [Page; 3]) -> u32 {
    order[0] as u32 | (order[1] as u32) << 4 | (order[2] as u32) << 8
}

/// Only a real order (each page once) is taken.
fn order_from_u32(v: u32) -> Option<[Page; 3]> {
    let order = [Page::from_u32(v & 0xF)?, Page::from_u32((v >> 4) & 0xF)?, Page::from_u32((v >> 8) & 0xF)?];
    PAGE_ORDERS.contains(&order).then_some(order)
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
    pub open_size: OpenSize,
    /// Run the scripts in the plugins folder (`plugins.rs`). Off until the
    /// user turns it on: nothing runs that they did not ask for.
    pub plugins: bool,
    /// The open notch's pages, left to right.
    pub page_order: [Page; 3],
    /// The page the notch opens on.
    pub start_page: Page,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            alerts: AlertKind::ALL.iter().fold(0, |bits, kind| bits | kind.bit()),
            duration: AlertDuration::Normal,
            width: NotchWidth::Normal,
            bluetooth_quiet_minutes: 5,
            open_size: OpenSize::Normal,
            plugins: false,
            page_order: PAGE_ORDERS[0],
            start_page: Page::Player,
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
        Self::from_values(
            read(w!("Alerts")),
            read(w!("AlertDuration")),
            read(w!("NotchWidth")),
            read(w!("BluetoothQuietMinutes")),
            read(w!("OpenSize")),
            read(w!("Plugins")),
            read(w!("PageOrder")),
            read(w!("StartPage")),
        )
    }

    /// Settings from stored values, each `None` when missing.
    #[allow(clippy::too_many_arguments)]
    fn from_values(
        alerts: Option<u32>,
        duration: Option<u32>,
        width: Option<u32>,
        quiet: Option<u32>,
        open_size: Option<u32>,
        plugins: Option<u32>,
        page_order: Option<u32>,
        start_page: Option<u32>,
    ) -> Self {
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
            open_size: open_size
                .and_then(|v| OpenSize::ALL.into_iter().find(|&s| s as u32 == v))
                .unwrap_or(default.open_size),
            plugins: plugins.map_or(default.plugins, |v| v == 1),
            page_order: page_order.and_then(order_from_u32).unwrap_or(default.page_order),
            start_page: start_page.and_then(Page::from_u32).unwrap_or(default.start_page),
        }
    }

    /// Packs the settings into one number, to post in a window message.
    /// The page order and start page take bits 29 to 33: a 64-bit `usize`
    /// (the app is x64 only).
    pub fn to_bits(self) -> usize {
        self.alerts as usize
            | (self.duration as usize) << 8
            | (self.width as usize) << 12
            | (self.bluetooth_quiet_minutes as usize) << 16
            | (self.open_size as usize) << 24
            | (self.plugins as usize) << 28
            | PAGE_ORDERS.iter().position(|&o| o == self.page_order).unwrap_or(0) << 29
            | (self.start_page as usize) << 32
    }

    pub fn from_bits(bits: usize) -> Self {
        let field = |shift: u32, mask: usize| Some(((bits >> shift) & mask) as u32);
        // Bluetooth quiet minutes are at most 60: eight bits.
        Self::from_values(
            field(0, 0xFF),
            field(8, 0xF),
            field(12, 0xF),
            field(16, 0xFF),
            field(24, 0xF),
            field(28, 0x1),
            PAGE_ORDERS.get((bits >> 29) & 0x7).map(|&o| order_to_u32(o)),
            field(32, 0x3),
        )
    }

    pub fn save(&self) {
        write(w!("Alerts"), self.alerts);
        write(w!("AlertDuration"), self.duration as u32);
        write(w!("NotchWidth"), self.width as u32);
        write(w!("BluetoothQuietMinutes"), self.bluetooth_quiet_minutes);
        write(w!("OpenSize"), self.open_size as u32);
        write(w!("Plugins"), self.plugins as u32);
        write(w!("PageOrder"), order_to_u32(self.page_order));
        write(w!("StartPage"), self.start_page as u32);
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
        assert_eq!(Settings::from_values(None, None, None, None, None, None, None, None), Settings::default());
        let odd = Settings::from_values(Some(u32::MAX), Some(9), Some(7), Some(3), Some(9), Some(7), Some(0x111), Some(7));
        assert_eq!(odd, Settings::default());
        // Stored numbers stay what they were before Narrow was added.
        assert_eq!(Settings::from_values(None, None, Some(0), None, None, None, None, None).width, NotchWidth::Normal);
        assert_eq!(Settings::from_values(None, None, Some(2), None, None, None, None, None).width, NotchWidth::Wider);
        assert_eq!(Settings::from_values(None, None, Some(3), None, None, None, None, None).width, NotchWidth::Narrow);
        let set = Settings::from_values(Some(0), Some(2), Some(1), Some(0), Some(2), Some(1), Some(0x012), Some(0));
        assert!(AlertKind::ALL.iter().all(|&kind| !set.alert_enabled(kind)));
        assert_eq!(set.duration, AlertDuration::Long);
        assert_eq!(set.width, NotchWidth::Wide);
        assert_eq!(set.bluetooth_quiet(), Duration::ZERO);
        assert_eq!(set.open_size, OpenSize::Large);
        assert!(set.plugins);
        assert_eq!(set.page_order, [Page::Controls, Page::Player, Page::Shelf]);
        assert_eq!(set.start_page, Page::Shelf);
    }

    #[test]
    fn round_trips_through_bits() {
        let mut settings = Settings::default();
        assert_eq!(Settings::from_bits(settings.to_bits()), settings);
        settings.set_alert(AlertKind::Battery, false);
        settings.set_alert(AlertKind::NowPlaying, false);
        settings.duration = AlertDuration::Short;
        settings.width = NotchWidth::Narrow;
        settings.open_size = OpenSize::Compact;
        settings.plugins = true;
        settings.page_order = [Page::Player, Page::Controls, Page::Shelf];
        settings.start_page = Page::Controls;
        settings.bluetooth_quiet_minutes = 60;
        assert_eq!(Settings::from_bits(settings.to_bits()), settings);
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
