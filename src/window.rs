use std::cell::RefCell;
use std::time::{Duration, Instant, SystemTime};

use tiny_skia::Pixmap;
use windows::core::{w, Error, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmFlush, DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::ABN_FULLSCREENAPP;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::anim::Spring;
use crate::appbar::{self, WM_APPBAR};
use crate::battery::{self, Battery};
use crate::bluetooth::{Bluetooth, Device, WM_BLUETOOTH};
use crate::controls::{ControlCommand, ControlState, Controls, WM_CONTROLS};
use crate::volume::{Volume, WM_VOLUME};
use crate::lyrics::{Fetcher, LyricsState, Query, WM_LYRICS};
use crate::media::{self, Command, Media, NowPlaying, WM_MEDIA};
use crate::notifications::{Notifications, WM_TOAST};
use crate::tray::{self, WM_TRAY};
use crate::render::{
    self, Alert, Button, ControlHit, ControlsView, Fonts, LyricsView, Scene, Shape, Slider, Toggle,
    Transition, COLLAPSED, COLLAPSED_BATTERY_WIDTH, EXPANDED, MINI, PLAYER_ONLY_WIDTH,
};

/// Window canvas in logical pixels. It fits the expanded notch plus spring
/// overshoot; fully transparent pixels let mouse clicks pass through.
const WIN_W: f32 = 600.0;
const WIN_H: f32 = 200.0;

/// Longest frame step fed to the springs, so a stall does not make them jump.
const MAX_DT: f32 = 1.0 / 30.0;

const TRACK_TRANSITION: Duration = Duration::from_millis(600);

/// Ticks the progress bar and synced lyrics while the notch is open.
const PROGRESS_TIMER: usize = 1;
const PROGRESS_INTERVAL_MS: u32 = 100;

/// Ends an alert after it has been shown this long.
const ALERT_TIMER: usize = 2;
const BATTERY_ALERT_MS: u32 = 3000;
/// Counted from the last change, so holding a volume key keeps it open.
const VOLUME_ALERT_MS: u32 = 1500;
const TOAST_ALERT_MS: u32 = 5000;
const BLUETOOTH_ALERT_MS: u32 = 3000;
/// How long a notification lingers after the pointer leaves it.
const TOAST_GRACE_MS: u32 = 2000;

/// Velocities (logical px/s) of the width and height "bump" on a new track.
const TRACK_PULSE: (f32, f32) = (320.0, 90.0);

// Lives in the large Win32_UI_Controls feature; not worth enabling for one constant.
const WM_MOUSELEAVE: u32 = 0x02A3;

struct App {
    hwnd: HWND,
    pixmap: Pixmap,
    surface: Surface,
    scale: f32,
    origin: POINT,
    hovered: bool,
    width: Spring,
    height: Spring,
    radius: Spring,
    last_tick: Instant,
    fonts: Option<Fonts>,
    media: Media,
    now_playing: Option<NowPlaying>,
    /// What was showing before the last media change, and when it started.
    transition: Option<(Option<NowPlaying>, Instant)>,
    lyrics: Fetcher,
    lyrics_state: LyricsState,
    /// Synced line being sung; drives `lyric_scroll` (in lines).
    lyric_line: Option<usize>,
    lyric_scroll: Spring,
    lyrics_open: bool,
    /// How far the lyrics panel is shown, 0..100 (percent keeps the spring's
    /// settle threshold meaningful).
    lyrics_reveal: Spring,
    /// Open width; animates when the lyrics panel is toggled.
    panel_width: Spring,
    /// Progress bar position (0..1) while its knob is being dragged.
    seeking: Option<f32>,
    /// `None` on machines without a battery.
    battery: Option<Battery>,
    /// The alert on screen; `leaving` once its time is up and the notch is
    /// shrinking back (its content fades with the shape).
    alert: Option<(Alert, bool)>,
    /// Volume alert slider, in percent.
    volume: Spring,
    notifications: Notifications,
    bluetooth: Bluetooth,
    /// The device the resting notch's indicator shows.
    bluetooth_device: Option<Device>,
    audio: Volume,
    controls: Controls,
    control_state: ControlState,
    /// Open notch page, 0 (player) .. 100 (Control Center).
    page: Spring,
    /// Wheel travel not yet turned into a page change.
    wheel: i32,
    /// A Control Center slider being dragged, with its value (0..1).
    slider_drag: Option<(Slider, f32)>,
    /// The active window reaches under the notch (a maximized window's
    /// title bar or tabs), so it shrinks to a mini pill.
    obstructed: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("NotchWoyWindow");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_win32());
        }

        // TOOLWINDOW hides it from the taskbar and Alt+Tab,
        // NOACTIVATE keeps it from stealing focus.
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("notch-woy"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )?;

        let battery = battery::read();
        battery::subscribe(hwnd);

        let app = App {
            hwnd,
            pixmap: Pixmap::new(1, 1).expect("1x1 pixmap"),
            surface: Surface::new(1, 1)?,
            scale: 1.0,
            origin: POINT::default(),
            hovered: false,
            width: Spring::new(collapsed_shape(battery).width),
            height: Spring::new(COLLAPSED.height),
            radius: Spring::new(COLLAPSED.radius),
            last_tick: Instant::now(),
            fonts: Fonts::load(),
            media: Media::spawn(hwnd),
            now_playing: None,
            transition: None,
            lyrics: Fetcher::spawn(hwnd),
            lyrics_state: LyricsState::Missing,
            lyric_line: None,
            lyric_scroll: Spring::new(-1.0),
            lyrics_open: true,
            lyrics_reveal: Spring::new(100.0),
            panel_width: Spring::new(EXPANDED.width),
            seeking: None,
            battery,
            alert: None,
            volume: Spring::new(0.0),
            notifications: Notifications::spawn(hwnd),
            bluetooth: Bluetooth::spawn(hwnd),
            bluetooth_device: None,
            audio: Volume::spawn(hwnd),
            controls: Controls::spawn(hwnd),
            control_state: ControlState::default(),
            page: Spring::new(0.0),
            wheel: 0,
            slider_drag: None,
            obstructed: false,
        };
        APP.with_borrow_mut(|slot| *slot = Some(app));
        appbar::register(hwnd);
        tray::add(hwnd);
        with_app(App::relayout).transpose()?;
        watch_windows();
        with_app(App::update_obstruction);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut msg = MSG::default();
        loop {
            if with_app(|app| app.is_animating()).unwrap_or(false) {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if let Some(Err(e)) = with_app(App::tick) {
                    eprintln!("frame failed: {e}");
                }
                // Paces frames to the compositor's vsync.
                let _ = DwmFlush();
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}

/// Re-checks what is under the notch whenever the active window changes,
/// moves, resizes, or is minimized or restored. Out-of-context hooks are
/// delivered through this thread's message loop, so no polling.
fn watch_windows() {
    unsafe {
        for (first, last) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ] {
            SetWinEventHook(first, last, None, Some(on_window_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        }
    }
}

unsafe extern "system" fn on_window_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    // Location changes fire for every caret and widget on screen; only the
    // active window's own frame matters.
    if event == EVENT_OBJECT_LOCATIONCHANGE
        && (object != OBJID_WINDOW.0 || hwnd != unsafe { GetForegroundWindow() })
    {
        return;
    }
    with_app(App::update_obstruction);
}

/// Whether `hwnd` is an ordinary window whose frame overlaps `area`.
fn covers(hwnd: HWND, area: RECT) -> bool {
    unsafe {
        if hwnd.is_invalid() || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        // The desktop and taskbar are always "under" the notch.
        let mut class = [0u16; 32];
        let len = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        // The visible frame, without the invisible resize borders.
        let mut frame = RECT::default();
        let got = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        );
        if got.is_err() && GetWindowRect(hwnd, &mut frame).is_err() {
            return false;
        }
        frame.left < area.right && frame.right > area.left && frame.top < area.bottom && frame.bottom > area.top
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints whether the active window reaches under a 230x30 notch at the
    /// top center of the primary monitor:
    /// `cargo test checks_active_window -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn checks_active_window() {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            assert!(GetMonitorInfoW(monitor, &mut info).as_bool());
            let m = info.rcMonitor;
            let center = (m.left + m.right) / 2;
            let area = RECT { left: center - 115, top: m.top, right: center + 115, bottom: m.top + 30 };
            let fg = GetForegroundWindow();
            let mut class = [0u16; 64];
            let len = GetClassNameW(fg, &mut class) as usize;
            println!(
                "active window class {:?}, maximized {}, under notch {}",
                String::from_utf16_lossy(&class[..len]),
                IsZoomed(fg).as_bool(),
                covers(fg, area)
            );
        }
    }
}

/// The resting notch, a little wider when it has a battery to show.
fn collapsed_shape(battery: Option<Battery>) -> Shape {
    match battery {
        Some(_) => Shape { width: COLLAPSED_BATTERY_WIDTH, ..COLLAPSED },
        None => COLLAPSED,
    }
}

/// Runs `f` on the app state. Returns `None` before the app exists or when
/// called re-entrantly while the state is already borrowed.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut slot = cell.try_borrow_mut().ok()?;
        slot.as_mut().map(f)
    })
}

impl App {
    fn is_animating(&self) -> bool {
        self.transition.is_some()
            || ![
                self.width,
                self.height,
                self.radius,
                self.lyric_scroll,
                self.lyrics_reveal,
                self.panel_width,
                self.volume,
                self.page,
            ]
            .iter()
            .all(Spring::is_settled)
    }

    /// Call before retargeting a spring, so the first frame's step is not
    /// measured from the last time anything moved.
    fn start_animation(&mut self) {
        if !self.is_animating() {
            self.last_tick = Instant::now();
        }
    }

    fn set_hovered(&mut self, hovered: bool) {
        // Dragging the progress knob or a slider past the notch's edge must
        // not close it.
        let dragging = self.seeking.is_some() || self.slider_drag.is_some();
        if self.hovered == hovered || !hovered && dragging {
            return;
        }
        self.hovered = hovered;
        self.retarget();
        if !hovered {
            // It opens on the player next time, as the island does.
            self.page = Spring::new(0.0);
            self.wheel = 0;
        }

        // A notification stays while the pointer rests on it, then gets a
        // short grace period once it leaves.
        if self.showing_toast() {
            unsafe {
                if hovered {
                    let _ = KillTimer(Some(self.hwnd), ALERT_TIMER);
                } else {
                    SetTimer(Some(self.hwnd), ALERT_TIMER, TOAST_GRACE_MS, None);
                }
            }
        }

        if hovered {
            // Lyrics are not tracked while closed; jump to the current line
            // instead of scrolling through everything sung meanwhile.
            self.update_lyric_line(true);
            self.rearm_mouse_leave();
            unsafe {
                SetTimer(Some(self.hwnd), PROGRESS_TIMER, PROGRESS_INTERVAL_MS, None);
            }
        } else {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), PROGRESS_TIMER);
            }
        }
    }

    /// Shrinks the notch to its mini pill while the active window reaches
    /// under its full resting size, and back when it does not.
    fn update_obstruction(&mut self) {
        let rest = collapsed_shape(self.battery);
        let half = (rest.width * self.scale / 2.0).round() as i32;
        let center = self.origin.x + self.pixmap.width() as i32 / 2;
        let area = RECT {
            left: center - half,
            top: self.origin.y,
            right: center + half,
            bottom: self.origin.y + (rest.height * self.scale).round() as i32,
        };
        let foreground = unsafe { GetForegroundWindow() };
        let obstructed = foreground != self.hwnd && covers(foreground, area);
        if obstructed != self.obstructed {
            self.obstructed = obstructed;
            self.retarget();
        }
    }

    /// Refreshes the indicator and alerts the latest connect/disconnect.
    fn on_bluetooth(&mut self) -> Result<()> {
        self.bluetooth_device = self.bluetooth.primary();
        if let Some((device, connected)) = self.bluetooth.take_latest_event() {
            self.show_alert(Alert::Bluetooth { device, connected }, BLUETOOTH_ALERT_MS);
        }
        self.invalidate()
    }

    fn on_toast(&mut self) -> Result<()> {
        let Some(toast) = self.notifications.take_latest() else { return Ok(()) };
        self.show_alert(Alert::Toast(toast), TOAST_ALERT_MS);
        if self.hovered {
            // Already under the pointer: hold it like a hovered one.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), ALERT_TIMER);
            }
        }
        self.invalidate()
    }

    fn showing_toast(&self) -> bool {
        matches!(self.alert, Some((Alert::Toast(_), false)))
    }

    /// Points the shape springs at what the notch should be showing: a
    /// notification (even when hovered, so it can be read and clicked),
    /// else open while hovered, else another alert while one is up, else
    /// at rest (a mini pill while a window sits under the notch).
    fn retarget(&mut self) {
        self.start_animation();
        let showing_alert = self.alert.as_ref().filter(|(_, leaving)| !leaving).map(|(alert, _)| alert);
        let target = if let Some(alert) = showing_alert.filter(|_| self.showing_toast()) {
            render::alert_shape(alert)
        } else if self.hovered {
            Shape { width: self.panel_width.target, ..EXPANDED }
        } else if let Some(alert) = showing_alert {
            render::alert_shape(alert)
        } else if self.obstructed {
            MINI
        } else {
            collapsed_shape(self.battery)
        };
        self.width.target = target.width;
        self.height.target = target.height;
        self.radius.target = target.radius;
    }

    /// Re-reads the battery; a change worth it (plugged in, low...) starts
    /// an alert.
    fn on_power_changed(&mut self) -> Result<()> {
        let now = battery::read();
        if now == self.battery {
            return Ok(());
        }
        let event = self.battery.zip(now).and_then(|(before, after)| battery::event(before, after));
        let had_battery = self.battery.is_some();
        self.battery = now;
        if let Some((event, battery)) = event.zip(now) {
            self.show_alert(Alert::Battery { event, battery }, BATTERY_ALERT_MS);
        } else if let (Some((Alert::Battery { battery, .. }, _)), Some(b)) = (self.alert.as_mut(), now) {
            // Keep a battery alert's level live while it is up.
            *battery = b;
        }
        if had_battery != now.is_some() {
            self.retarget();
        }
        self.invalidate()
    }

    /// Shows `alert` (replacing any other) until `shown_ms` pass without a
    /// new one.
    fn show_alert(&mut self, alert: Alert, shown_ms: u32) {
        self.alert = Some((alert, false));
        // Re-setting a running timer restarts its countdown.
        unsafe {
            SetTimer(Some(self.hwnd), ALERT_TIMER, shown_ms, None);
        }
        self.retarget();
    }

    /// A volume or mute change from any source.
    fn on_volume(&mut self, percent: u32, muted: bool) -> Result<()> {
        // The Control Center page already shows it, in its own slider.
        if self.on_controls_page() {
            return self.invalidate();
        }
        let showing = matches!(self.alert, Some((Alert::Volume { .. }, false)));
        if showing {
            // Slide from where the slider is now.
            self.start_animation();
            self.volume.target = percent as f32;
        } else {
            // A fresh alert starts at the new level rather than sliding from
            // whatever it showed last time.
            self.volume = Spring::new(percent as f32);
        }
        // The slider's level comes from `volume` at draw time.
        self.show_alert(Alert::Volume { level: 0.0, muted }, VOLUME_ALERT_MS);
        self.invalidate()
    }

    fn end_alert(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), ALERT_TIMER);
        }
        if let Some((_, leaving)) = self.alert.as_mut() {
            *leaving = true;
            self.retarget();
        }
    }

    fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(MAX_DT);
        self.last_tick = now;
        self.width.update(dt);
        self.height.update(dt);
        self.radius.update(dt);
        self.lyric_scroll.update(dt);
        self.lyrics_reveal.update(dt);
        self.panel_width.update(dt);
        self.volume.update(dt);
        self.page.update(dt);
        let result = self.redraw();
        if self.transition.as_ref().is_some_and(|(_, start)| start.elapsed() >= TRACK_TRANSITION) {
            self.transition = None;
        }
        // A finished alert is dropped once the notch has shrunk back.
        if self.alert.as_ref().is_some_and(|(_, leaving)| *leaving) && self.height.is_settled() {
            self.alert = None;
        }
        result
    }

    fn redraw(&mut self) -> Result<()> {
        let shape = Shape {
            width: self.width.value,
            height: self.height.value,
            radius: self.radius.value,
        };
        let transition = self.transition.as_ref().map(|(from, start)| Transition {
            from: from.as_ref(),
            progress: (start.elapsed().as_secs_f32() / TRACK_TRANSITION.as_secs_f32()).min(1.0),
        });
        let lyrics = match &self.lyrics_state {
            LyricsState::Loading => LyricsView::Loading,
            LyricsState::Missing => LyricsView::Missing,
            LyricsState::Found(l) if l.synced => LyricsView::Synced {
                lines: &l.lines,
                scroll: self.lyric_scroll.value,
            },
            LyricsState::Found(l) => LyricsView::Plain(&l.lines),
        };
        let (mut volume, muted) = self.audio.level();
        let mut brightness = None;
        // A dragged slider shows where the pointer is, not the last report.
        match self.slider_drag {
            Some((Slider::Volume, v)) => volume = v,
            Some((Slider::Brightness, v)) => brightness = Some(v),
            None => {}
        }
        let controls = ControlsView {
            state: self.control_state,
            volume,
            muted,
            brightness,
            bluetooth_device: self.bluetooth_device.as_ref().map(|d| d.name.as_str()),
            battery: self.battery,
        };
        let scene = Scene {
            shape,
            panel_width: self.panel_width.value,
            lyrics_reveal: self.lyrics_reveal.value / 100.0,
            media: self.now_playing.as_ref(),
            transition,
            lyrics,
            seek: self.seeking,
            battery: self.battery,
            bluetooth: self.bluetooth_device.as_ref().map(|d| d.kind),
            alert: self.alert.as_ref().map(|(alert, _)| match alert {
                Alert::Volume { muted, .. } => Alert::Volume { level: self.volume.value / 100.0, muted: *muted },
                other => other.clone(),
            }),
            page: self.page.value / 100.0,
            controls,
        };
        render::draw(&mut self.pixmap, self.scale, &scene, self.fonts.as_ref());
        self.surface.present(self.hwnd, &self.pixmap, self.origin)
    }

    /// Whether the open notch is showing (or turning to) the Control Center.
    fn on_controls_page(&self) -> bool {
        self.hovered && self.page.target > 50.0
    }

    /// Mouse wheel or touchpad scroll: down for Control Center, up for the
    /// player. Small touchpad steps add up until they make a full notch.
    fn on_wheel(&mut self, delta: i32) -> Result<()> {
        const STEP: i32 = 40;
        let open = self.hovered && self.height.value >= EXPANDED.height * 0.9 && !self.showing_toast();
        if !open {
            return Ok(());
        }
        self.wheel += delta;
        let target = if self.wheel <= -STEP {
            100.0
        } else if self.wheel >= STEP {
            0.0
        } else {
            return Ok(());
        };
        self.wheel = 0;
        if target != self.page.target {
            self.start_animation();
            self.page.target = target;
            if target > 50.0 {
                // Radios may have been switched elsewhere since last looked.
                self.controls.send(ControlCommand::Refresh);
            }
        }
        Ok(())
    }

    fn on_controls(&mut self) -> Result<()> {
        self.control_state = self.controls.state();
        self.invalidate()
    }

    /// A tap on a Control Center toggle. The tile flips right away; the
    /// worker's report follows.
    fn toggle(&mut self, toggle: Toggle) {
        let s = &mut self.control_state;
        match toggle {
            Toggle::Airplane => {
                let on = !s.airplane();
                self.controls.send(ControlCommand::SetAirplane(on));
                s.wifi = s.wifi.map(|_| !on);
                s.bluetooth = s.bluetooth.map(|_| !on);
            }
            Toggle::Wifi => {
                if let Some(wifi) = s.wifi.as_mut() {
                    *wifi = !*wifi;
                    self.controls.send(ControlCommand::SetWifi(*wifi));
                }
            }
            Toggle::Bluetooth => {
                if let Some(bluetooth) = s.bluetooth.as_mut() {
                    *bluetooth = !*bluetooth;
                    self.controls.send(ControlCommand::SetBluetooth(*bluetooth));
                }
            }
            Toggle::Mute => {
                let (_, muted) = self.audio.level();
                self.audio.set_mute(!muted);
            }
        }
    }

    fn apply_slider(&mut self, slider: Slider, value: f32) {
        self.slider_drag = Some((slider, value));
        match slider {
            Slider::Brightness => self.controls.send(ControlCommand::SetBrightness((value * 100.0).round() as u8)),
            Slider::Volume => self.audio.set_level(value),
        }
    }

    /// Moves the lyrics to the line being sung. `snap` jumps there without
    /// scrolling.
    fn update_lyric_line(&mut self, snap: bool) {
        let line = match (&self.lyrics_state, &self.now_playing) {
            (LyricsState::Found(l), Some(np)) if l.synced => {
                np.progress().and_then(|(position, _)| l.current_line(position))
            }
            _ => None,
        };
        let target = line.map_or(-1.0, |i| i as f32);
        if snap {
            self.lyric_scroll = Spring::new(target);
        } else if line != self.lyric_line {
            self.start_animation();
            self.lyric_scroll.target = target;
        }
        self.lyric_line = line;
    }

    fn on_lyrics(&mut self) -> Result<()> {
        let Some(np) = &self.now_playing else { return Ok(()) };
        // A lookup for a track that is no longer playing is ignored.
        let Some(state) = self.lyrics.result_for(&np.title, &np.artist) else { return Ok(()) };
        self.lyrics_state = state;
        self.update_lyric_line(true);
        self.invalidate()
    }

    fn on_timer(&mut self) -> Result<()> {
        self.update_lyric_line(false);
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        if playing { self.invalidate() } else { Ok(()) }
    }

    fn toggle_lyrics(&mut self) {
        self.start_animation();
        self.lyrics_open = !self.lyrics_open;
        self.lyrics_reveal.target = if self.lyrics_open { 100.0 } else { 0.0 };
        let width = if self.lyrics_open { EXPANDED.width } else { PLAYER_ONLY_WIDTH };
        self.panel_width.target = width;
        if self.hovered {
            self.width.target = width;
        }
    }

    /// Redraws now unless the animation loop is about to draw anyway.
    fn invalidate(&mut self) -> Result<()> {
        if self.is_animating() { Ok(()) } else { self.redraw() }
    }

    fn on_media_changed(&mut self) -> Result<()> {
        let next = self.media.current();
        let track = media::track_changed(self.now_playing.as_ref(), next.as_ref());
        let art = media::art_changed(self.now_playing.as_ref(), next.as_ref());

        if track || art {
            self.start_animation();
            // A change mid-transition starts over from what is showing now.
            self.transition = Some((self.now_playing.take(), Instant::now()));
            if track && next.is_some() {
                self.width.kick(TRACK_PULSE.0);
                self.height.kick(TRACK_PULSE.1);
            }
        }
        if track {
            self.lyrics_state = match &next {
                Some(np) => {
                    self.lyrics.request(Query {
                        title: np.title.clone(),
                        artist: np.artist.clone(),
                        album: np.album.clone(),
                        duration: np.timeline.map(|t| t.duration),
                    });
                    LyricsState::Loading
                }
                None => LyricsState::Missing,
            };
            self.lyric_line = None;
            self.lyric_scroll = Spring::new(-1.0);
        }
        self.now_playing = next;
        self.invalidate()
    }

    /// Where along the progress bar `pos` falls; see `render::progress_at`.
    fn progress_at(&self, pos: (f32, f32), grab: bool) -> Option<f32> {
        let reveal = self.lyrics_reveal.value / 100.0;
        render::progress_at(self.pixmap.width(), self.scale, self.panel_width.value, reveal, pos, grab)
    }

    /// Whether the open notch is showing a track that can be acted on.
    fn controls_ready(&self) -> bool {
        // Controls only respond once the notch is (nearly) fully open.
        self.now_playing.is_some() && self.height.value >= EXPANDED.height * 0.9
    }

    /// Where a press on the Control Center page lands, once it is in view.
    fn control_at(&self, x: f32, y: f32) -> Option<ControlHit> {
        let settled = self.on_controls_page() && self.page.value > 90.0;
        let open = self.height.value >= EXPANDED.height * 0.9;
        if !settled || !open {
            return None;
        }
        render::control_at(self.pixmap.width(), self.scale, self.panel_width.value, (x, y))
    }

    /// Starts dragging a Control Center slider or the progress knob if the
    /// press is on one.
    fn on_mouse_down(&mut self, x: f32, y: f32) -> Result<()> {
        if self.on_controls_page() {
            if let Some(ControlHit::Slider(slider, value)) = self.control_at(x, y) {
                self.apply_slider(slider, value);
                // Keep receiving moves and the release even off the notch.
                unsafe { SetCapture(self.hwnd) };
                return self.invalidate();
            }
            return Ok(());
        }
        let has_length = self.now_playing.as_ref().is_some_and(|np| np.timeline.is_some());
        if !self.controls_ready() || !has_length {
            return Ok(());
        }
        let Some(fraction) = self.progress_at((x, y), true) else { return Ok(()) };
        self.seeking = Some(fraction);
        // Keep receiving moves and the release even off the notch.
        unsafe { SetCapture(self.hwnd) };
        self.invalidate()
    }

    fn on_seek_move(&mut self, x: f32, y: f32) -> Result<()> {
        self.seeking = self.progress_at((x, y), false);
        self.invalidate()
    }

    fn on_slider_move(&mut self, y: f32) -> Result<()> {
        if let Some((slider, _)) = self.slider_drag {
            self.apply_slider(slider, render::slider_value(self.scale, y));
        }
        self.invalidate()
    }

    fn finish_slider(&mut self) -> Result<()> {
        // Cleared before releasing, so the capture change is not a cancel.
        self.slider_drag = None;
        unsafe {
            let _ = ReleaseCapture();
        }
        self.rearm_mouse_leave();
        self.invalidate()
    }

    /// Releases the knob: seeks the player there.
    fn finish_seek(&mut self) -> Result<()> {
        let Some(fraction) = self.seeking.take() else { return Ok(()) };
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(np) = self.now_playing.as_mut() {
            if let Some(timeline) = np.timeline.as_mut() {
                let position = timeline.duration.mul_f32(fraction);
                self.media.send(Command::Seek(position));
                // Show the new position now rather than snapping back until
                // the player reports it.
                timeline.position = position;
                timeline.updated = SystemTime::now();
            }
        }
        self.update_lyric_line(true);
        self.rearm_mouse_leave();
        self.invalidate()
    }

    /// Asks for WM_MOUSELEAVE once the cursor leaves the opaque area. If it
    /// already has (say, a drag ended outside), it arrives right away.
    fn rearm_mouse_leave(&self) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut tme);
        }
    }

    fn on_click(&mut self, x: f32, y: f32) -> Result<()> {
        // Tapping a notification opens its app, as on iPhone.
        if let Some((Alert::Toast(toast), false)) = &self.alert {
            toast.open_app();
            self.end_alert();
            return Ok(());
        }
        if self.on_controls_page() {
            if let Some(ControlHit::Toggle(toggle)) = self.control_at(x, y) {
                self.toggle(toggle);
                return self.invalidate();
            }
            return Ok(());
        }
        if !self.controls_ready() {
            return Ok(());
        }
        let button = render::button_at(
            self.pixmap.width(),
            self.scale,
            self.panel_width.value,
            self.lyrics_reveal.value / 100.0,
            x,
            y,
        );
        let command = match button {
            Some(Button::Media(command)) => command,
            Some(Button::Lyrics) => {
                self.toggle_lyrics();
                return Ok(());
            }
            None => return Ok(()),
        };
        self.media.send(command);
        // Flip the icon right away; the next poll confirms the real state.
        if let (Command::TogglePlayPause, Some(np)) = (command, self.now_playing.as_mut()) {
            np.playing = !np.playing;
            return self.invalidate();
        }
        Ok(())
    }

    /// Sizes the canvas for the primary monitor's DPI, pins it to the
    /// top-center of that monitor and redraws.
    fn relayout(&mut self) -> Result<()> {
        unsafe {
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return Err(Error::from_win32());
            }
            let (mut dpi_x, mut dpi_y) = (96, 96);
            GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)?;
            self.scale = dpi_x as f32 / 96.0;

            let w = (WIN_W * self.scale).round() as u32;
            let h = (WIN_H * self.scale).round() as u32;
            if self.pixmap.width() != w || self.pixmap.height() != h {
                self.pixmap = Pixmap::new(w, h).expect("non-zero canvas size");
                self.surface = Surface::new(w as i32, h as i32)?;
            }

            let mon = info.rcMonitor;
            self.origin = POINT {
                x: mon.left + (mon.right - mon.left - w as i32) / 2,
                y: mon.top,
            };
        }
        self.redraw()
    }
}

/// Low/high words of `lparam` as signed client coordinates.
fn mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            if let Some(Err(e)) = with_app(App::relayout) {
                eprintln!("relayout failed: {e}");
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lparam);
            let result = with_app(|app| {
                if app.seeking.is_some() {
                    app.on_seek_move(x, y)
                } else if app.slider_drag.is_some() {
                    app.on_slider_move(y)
                } else {
                    app.set_hovered(true);
                    Ok(())
                }
            });
            if let Some(Err(e)) = result {
                eprintln!("seek redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|app| app.set_hovered(false));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = mouse_pos(lparam);
            if let Some(Err(e)) = with_app(|app| app.on_mouse_down(x, y)) {
                eprintln!("seek start failed: {e}");
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_pos(lparam);
            let result = with_app(|app| {
                if app.seeking.is_some() {
                    app.finish_seek()
                } else if app.slider_drag.is_some() {
                    app.finish_slider()
                } else {
                    app.on_click(x, y)
                }
            });
            if let Some(Err(e)) = result {
                eprintln!("click failed: {e}");
            }
            LRESULT(0)
        }
        // Capture taken away mid-drag (Alt+Tab, a dialog...): drop the drag
        // without seeking; a slider keeps the value it was dragged to. Our
        // own releases clear the drag state first.
        WM_CAPTURECHANGED => {
            let result = with_app(|app| {
                let slider = app.slider_drag.take().is_some();
                if app.seeking.take().is_some() || slider {
                    app.rearm_mouse_leave();
                    app.invalidate()
                } else {
                    Ok(())
                }
            });
            if let Some(Err(e)) = result {
                eprintln!("seek cancel failed: {e}");
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // High word of wparam: signed wheel travel, 120 per mouse notch.
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(Err(e)) = with_app(|app| app.on_wheel(delta)) {
                eprintln!("page change failed: {e}");
            }
            LRESULT(0)
        }
        WM_CONTROLS => {
            if let Some(Err(e)) = with_app(App::on_controls) {
                eprintln!("controls redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == PROGRESS_TIMER => {
            if let Some(Err(e)) = with_app(App::on_timer) {
                eprintln!("progress redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_VOLUME => {
            let (percent, muted) = (wparam.0 as u32, lparam.0 != 0);
            if let Some(Err(e)) = with_app(|app| app.on_volume(percent, muted)) {
                eprintln!("volume redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_BLUETOOTH => {
            if let Some(Err(e)) = with_app(App::on_bluetooth) {
                eprintln!("bluetooth redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_TOAST => {
            if let Some(Err(e)) = with_app(App::on_toast) {
                eprintln!("notification redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ALERT_TIMER => {
            with_app(App::end_alert);
            LRESULT(0)
        }
        WM_POWERBROADCAST
            if matches!(wparam.0 as u32, PBT_APMPOWERSTATUSCHANGE | PBT_POWERSETTINGCHANGE) =>
        {
            if let Some(Err(e)) = with_app(App::on_power_changed) {
                eprintln!("battery redraw failed: {e}");
            }
            LRESULT(1)
        }
        WM_LYRICS => {
            if let Some(Err(e)) = with_app(App::on_lyrics) {
                eprintln!("lyrics redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_MEDIA => {
            if let Some(Err(e)) = with_app(App::on_media_changed) {
                eprintln!("media redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TRAY => {
            tray::on_click(hwnd, lparam);
            LRESULT(0)
        }
        WM_APPBAR => {
            // Get out of the way of games and fullscreen video.
            if wparam.0 as u32 == ABN_FULLSCREENAPP {
                let cmd = if lparam.0 != 0 { SW_HIDE } else { SW_SHOWNOACTIVATE };
                unsafe {
                    let _ = ShowWindow(hwnd, cmd);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            appbar::unregister(hwnd);
            tray::remove(hwnd);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // Explorer restarted: its notification area forgot the icon.
        _ if msg != 0 && msg == tray::taskbar_created_message() => {
            tray::add(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// A 32-bit premultiplied BGRA DIB used as the source for UpdateLayeredWindow.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(width: i32, height: i32) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative height makes the DIB top-down, matching tiny-skia.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) => bitmap,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e);
                }
            };
            let old = SelectObject(dc, bitmap.into());
            Ok(Self { dc, bitmap, old, bits: bits.cast(), width, height })
        }
    }

    fn present(&mut self, hwnd: HWND, pixmap: &Pixmap, pos: POINT) -> Result<()> {
        let len = (self.width * self.height * 4) as usize;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, len) };
        // tiny-skia is premultiplied RGBA, GDI wants premultiplied BGRA.
        for (d, s) in dst.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }

        let size = SIZE { cx: self.width, cy: self.height };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(self.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
