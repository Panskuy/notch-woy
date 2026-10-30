//! How loud the default output device is playing, for the equalizers.
//!
//! Reads the WASAPI peak meter (`IAudioMeterInformation`) on the UI thread,
//! only while an equalizer shows: one COM call per frame, no capture thread
//! or FFT. It gives levels (the mix, left and right), not frequency bands;
//! `Bars` turns those into a few bars that each move their own way.
//!
//! The meter reads the stream before the device's volume and mute, so the
//! bars follow the music even with the volume down. A stream in exclusive
//! mode, or playing on another device, reads silent; `Bars` then reports
//! nothing and the equalizers fall back to their own animation.

use std::sync::atomic::Ordering;

use windows::core::Result;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

use crate::volume::DEFAULT_DEVICE_GENERATION;

pub struct Meter {
    devices: Option<IMMDeviceEnumerator>,
    meter: Option<IAudioMeterInformation>,
    /// The default device's id, to notice it changing.
    device: Option<String>,
    /// `DEFAULT_DEVICE_GENERATION` when the device was last looked up;
    /// `None` to look again. Looking up the default device is a call into
    /// the audio service that can take tens of milliseconds, so it is done
    /// once at start-up and again only when Windows says the default
    /// changed, never on a timer in the middle of an animation.
    followed: Option<u32>,
}

impl Meter {
    /// Needs COM on the calling thread. Finds the device right away, while
    /// nothing is animating.
    pub fn new() -> Self {
        let devices = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
        let mut meter = Self { devices, meter: None, device: None, followed: None };
        meter.follow_if_changed();
        meter
    }

    fn follow_if_changed(&mut self) {
        let generation = DEFAULT_DEVICE_GENERATION.load(Ordering::Relaxed);
        if self.followed == Some(generation) {
            return;
        }
        self.followed = Some(generation);
        if let Err(e) = self.follow_default() {
            eprintln!("audio meter: {e}");
            self.meter = None;
            self.device = None;
        }
    }

    /// Peaks (0..1) of the last device period, left and right (both the
    /// mix on a mono device); `None` without an output device.
    pub fn read(&mut self) -> Option<(f32, f32)> {
        self.follow_if_changed();
        let meter = self.meter.as_ref()?;
        let peaks = unsafe {
            match meter.GetMeteringChannelCount() {
                Ok(n) if n >= 2 => {
                    let mut channels = [0.0f32; 8];
                    let n = (n as usize).min(channels.len());
                    meter.GetChannelsPeakValues(&mut channels[..n]).map(|()| (channels[0], channels[1]))
                }
                _ => meter.GetPeakValue().map(|p| (p, p)),
            }
        };
        match peaks {
            Ok(peaks) => Some(peaks),
            Err(_) => {
                // The device went away; look again next time.
                self.meter = None;
                self.device = None;
                self.followed = None;
                None
            }
        }
    }

    /// Moves the meter to the default output device if that changed.
    fn follow_default(&mut self) -> Result<()> {
        let Some(devices) = &self.devices else { return Ok(()) };
        let Ok(device) = (unsafe { devices.GetDefaultAudioEndpoint(eRender, eConsole) }) else {
            // No output device at all.
            self.meter = None;
            self.device = None;
            return Ok(());
        };
        let id = unsafe {
            let raw = device.GetId()?;
            let id = raw.to_string().unwrap_or_default();
            CoTaskMemFree(Some(raw.0 as *const _));
            id
        };
        if self.meter.is_none() || self.device.as_deref() != Some(&id) {
            self.meter = Some(unsafe { device.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None)? });
            self.device = Some(id);
        }
        Ok(())
    }
}

/// Loudness range shown by the bars: from this many dB below full scale
/// (empty) up to full scale. Peaks are logarithmic to the ear.
const RANGE_DB: f32 = 36.0;
/// Below this peak the output counts as silent.
const SILENT: f32 = 1e-4;
/// Silent this long while a track plays: the meter is not seeing it
/// (exclusive mode, another device), so the bars stop following it.
const SILENT_FOR: f32 = 2.0;
/// The bars are sampled from a short history, each a little later than
/// the one before, so a beat runs across them rather than lifting them all
/// at once.
const SAMPLE_EVERY: f32 = 1.0 / 30.0;
const BAR_DELAY: usize = 2;
/// Rise and fall time constants (s) per bar: quick up, unhurried down,
/// each bar a little different so they never move in lockstep.
const ATTACK: f32 = 0.025;
const RELEASE: [f32; 4] = [0.12, 0.2, 0.15, 0.24];
/// The running average loudness the bars are measured against, so a loud
/// master does not pin them at the top: time constant (s) and how much of
/// it is taken away.
const AVERAGE: f32 = 1.5;
const AVERAGE_WEIGHT: f32 = 0.75;

pub const BARS: usize = 4;

/// Turns meter peaks into equalizer bar heights.
#[derive(Default)]
pub struct Bars {
    heights: [f32; BARS],
    /// Recent loudness per channel (left, right), newest first.
    history: [[f32; 2]; BAR_DELAY * (BARS - 1) + 1],
    average: f32,
    since_sample: f32,
    silent_for: f32,
}

impl Bars {
    /// Advances by `dt` seconds with the latest peaks, and returns the bar
    /// heights (0..1); `None` while the meter is not seeing the music.
    pub fn update(&mut self, peaks: Option<(f32, f32)>, dt: f32) -> Option<[f32; BARS]> {
        let (left, right) = peaks?;
        let dt = dt.clamp(0.0, 0.25);
        if left.max(right) < SILENT {
            self.silent_for += dt;
        } else {
            self.silent_for = 0.0;
        }
        if self.silent_for >= SILENT_FOR {
            *self = Self { silent_for: self.silent_for, ..Self::default() };
            return None;
        }

        let (left, right) = (loudness(left), loudness(right));
        // Starting out, the average starts where the music is.
        if self.average == 0.0 {
            self.average = left.max(right);
        }
        self.average += (left.max(right) - self.average) * (1.0 - (-dt / AVERAGE).exp());
        self.since_sample += dt;
        while self.since_sample >= SAMPLE_EVERY {
            self.since_sample -= SAMPLE_EVERY;
            self.history.rotate_right(1);
            self.history[0] = [left, right];
        }
        let floor = self.average * AVERAGE_WEIGHT;
        for (i, height) in self.heights.iter_mut().enumerate() {
            // Alternate channels, each bar a little behind the last.
            let level = self.history[i * BAR_DELAY][i % 2];
            let target = ((level - floor) / (1.0 - floor)).clamp(0.0, 1.0);
            let tau = if target > *height { ATTACK } else { RELEASE[i] };
            *height += (target - *height) * (1.0 - (-dt / tau).exp());
        }
        Some(self.heights)
    }
}

/// A peak (0..1) as loudness on the bars' scale (0..1), in decibels.
fn loudness(peak: f32) -> f32 {
    if peak <= SILENT {
        return 0.0;
    }
    (1.0 + 20.0 * peak.log10() / RANGE_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_music() {
        let mut bars = Bars::default();
        let frame = 1.0 / 60.0;
        // Quiet, then a loud hit: the first bar jumps, later ones follow.
        for _ in 0..60 {
            bars.update(Some((0.02, 0.02)), frame);
        }
        let quiet = bars.update(Some((0.02, 0.02)), frame).unwrap();
        let mut hit = quiet;
        for _ in 0..3 {
            hit = bars.update(Some((0.9, 0.9)), frame).unwrap();
        }
        assert!(hit[0] > quiet[0] + 0.3, "{quiet:?} -> {hit:?}");
        assert!(hit[3] < hit[0], "the last bar lags: {hit:?}");
        let mut later = hit;
        for _ in 0..12 {
            later = bars.update(Some((0.9, 0.9)), frame).unwrap();
        }
        assert!(later[3] > hit[3] + 0.2, "{hit:?} -> {later:?}");
        // Back to quiet: they fall again, not instantly.
        let falling = bars.update(Some((0.02, 0.02)), frame).unwrap();
        assert!(falling[0] > 0.2, "{falling:?}");
    }

    #[test]
    fn gives_up_on_silence() {
        let mut bars = Bars::default();
        // A pause between songs keeps the bars.
        for _ in 0..60 {
            assert!(bars.update(Some((0.0, 0.0)), 1.0 / 60.0).is_some());
        }
        // Silent for long while playing: the meter cannot see it.
        for _ in 0..90 {
            bars.update(Some((0.0, 0.0)), 1.0 / 60.0);
        }
        assert!(bars.update(Some((0.0, 0.0)), 1.0 / 60.0).is_none());
        // Sound again: following again.
        assert!(bars.update(Some((0.5, 0.5)), 1.0 / 60.0).is_some());
        assert!(bars.update(None, 1.0 / 60.0).is_none());
    }

    #[test]
    fn loudness_is_in_decibels() {
        assert_eq!(loudness(1.0), 1.0);
        assert!((loudness(10f32.powf(-18.0 / 20.0)) - 0.5).abs() < 1e-4);
        assert_eq!(loudness(0.001), 0.0);
        assert_eq!(loudness(0.0), 0.0);
    }

    /// Prints the real meter while something plays:
    /// `cargo test reads_real_meter -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_real_meter() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let mut meter = Meter::new();
        let mut bars = Bars::default();
        for _ in 0..150 {
            let peaks = meter.read();
            let heights = bars.update(peaks, 1.0 / 30.0);
            println!("{peaks:?} {heights:?}");
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
}
