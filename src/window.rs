use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tiny_skia::Pixmap;
use windows::core::{w, Error, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmFlush, DwmGetCompositionTimingInfo, DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DWM_TIMING_INFO,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Globalization::{GetTimeFormatEx, TIME_NOSECONDS, TIME_NOTIMEMARKER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::anim::Spring;
use crate::battery::{self, Battery};
use crate::bluetooth::{Bluetooth, Device, WM_BLUETOOTH};
use crate::controls::{ControlCommand, ControlState, Controls, DisplayLevel, WM_CONTROLS};
use crate::volume::{Volume, WM_VOLUME};
use crate::volume_keys::{self, VolumeKey, WM_VOLUME_KEY};
use crate::lyrics::{Fetcher, LyricsState, Query, WM_LYRICS};
use crate::media::{self, Command, Media, NowPlaying, WM_MEDIA};
use crate::meter::{Bars, Meter};
use crate::notifications::{Notifications, Toast, WM_TOAST};
use crate::outputs::{Output, Outputs, WM_OUTPUTS};
use crate::plugins::{Plugins, WM_PLUGIN};
use crate::settings::{AlertKind, Page, Settings};
use crate::shelf::{self, Shelf, WM_SHELF};
use crate::settings_window::{self, WM_SETTINGS_CHANGED};
use crate::tray::{self, WM_TRAY};
use crate::render::{
    self, Alert, Arrival, Button, Departure, ControlHit, ControlsView, Fonts, LyricsView, OutputsView, Scene, Shape, ShelfHit, ShelfView,
    Slider, Toggle,
    OUTPUT_ROWS_SHOWN,
    Transition, COLLAPSED, COLLAPSED_BATTERY_WIDTH, EXPANDED, MINI, PLAYER_ONLY_WIDTH, SHADOW_PAD,
};

/// Window canvas in logical pixels. It fits the widest open notch (Control
/// Center at the Large open size: 560 x 1.1, its shoulders and spring
/// overshoot); fully transparent pixels let mouse clicks pass through.
const WIN_W: f32 = 660.0;
const WIN_H: f32 = 200.0;

/// Longest frame step fed to the springs, so a stall does not make them jump.
const MAX_DT: f32 = 1.0 / 30.0;

const TRACK_TRANSITION: Duration = Duration::from_millis(600);

/// Ticks the progress bar and synced lyrics while the notch is open.
const PROGRESS_TIMER: usize = 1;
const PROGRESS_INTERVAL_MS: u32 = 100;

/// Fires once a touchpad scroll has been still this long: the fingers
/// lifted, so the pages settle.
const SCROLL_END_TIMER: usize = 4;
const SCROLL_END_MS: u32 = 120;

/// Animations that can run for hours (the resting notch's equalizer, the
/// sung lyric lighting up) are drawn on the display's vsync like the rest,
/// so they move evenly, but only about this often: every
/// `round(refresh / AMBIENT_FPS)`-th refresh (60 fps on a 60, 120 or 180 Hz
/// display, 72 on 144 Hz). A timer instead would fire unevenly (measured
/// 8 to 38 ms for 16 ms) and drift against the display.
const AMBIENT_FPS: f32 = 60.0;
/// The time of day as Windows formats it for the user (12 or 24 hours),
/// without seconds or AM/PM, as on the iPhone ("9:41").
pub fn clock_text() -> String {
    let mut buf = [0u16; 32];
    let len = unsafe { GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS | TIME_NOTIMEMARKER, None, PCWSTR::null(), Some(&mut buf)) };
    // The length counts the terminating null.
    String::from_utf16_lossy(&buf[..(len.max(1) - 1) as usize]).trim().to_owned()
}

/// Arms `CLOCK_TIMER` for just after the next minute starts.
fn schedule_clock(hwnd: HWND) {
    let now = unsafe { GetLocalTime() };
    let into_minute = now.wSecond as u32 * 1000 + now.wMilliseconds as u32;
    unsafe {
        SetTimer(Some(hwnd), CLOCK_TIMER, 60_000 - into_minute.min(59_999) + 50, None);
    }
}

/// Re-reads the time and redraws if the shown minute changed.
fn update_clock() {
    let result = with_app(|app| {
        let time = clock_text();
        if time == app.time {
            return Ok(());
        }
        app.time = time;
        app.invalidate()
    });
    if let Some(Err(e)) = result {
        eprintln!("clock redraw failed: {e}");
    }
}

/// Page travel (0..100) per unit of wheel delta: two mouse notches' worth
/// of touchpad scrolling turns a page.
const PAGE_PER_WHEEL_UNIT: f32 = 100.0 / 240.0;

/// Past either end page (shelf at -100, Control Center at 100), travel is
/// resisted and capped, like iOS rubber-banding.
fn rubber_band(raw: f32) -> f32 {
    const LIMIT: f32 = 18.0;
    let stretch = |over: f32| LIMIT * (1.0 - 1.0 / (over * 0.55 / LIMIT + 1.0));
    if raw < -100.0 {
        -100.0 - stretch(-100.0 - raw)
    } else if raw > 100.0 {
        100.0 + stretch(raw - 100.0)
    } else {
        raw
    }
}

/// A second look at what is under the notch, shortly after the active
/// window changes.
const RECHECK_TIMER: usize = 3;
const RECHECK_MS: u32 = 250;

/// While hidden for a full-screen app, looks again this often, so the
/// notch comes back even if the event that should bring it back is missed
/// (the app closes without another taking its place, a busy moment at
/// login...).
const HIDDEN_RECHECK_TIMER: usize = 6;
const HIDDEN_RECHECK_MS: u32 = 1000;

/// Fires at the turn of each minute, for the resting notch's clock.
const CLOCK_TIMER: usize = 9;

/// Once the notch has been at rest this long (and after start-up), the
/// pages it is not using go back to Windows; see `trim_working_set`.
const TRIM_TIMER: usize = 8;
const TRIM_IDLE_MS: u32 = 30_000;
/// While it stays at rest, trims again this often: the polling workers'
/// WinRT calls spread allocations over fresh heap pages, so the touched set
/// creeps up between trims.
const TRIM_REPEAT_MS: u32 = 5 * 60_000;

/// A file being dragged out of the shelf; the notch's own drop target
/// ignores that drag.
static DRAGGING_OUT: AtomicBool = AtomicBool::new(false);
/// Pointer travel (logical px) that turns a press on a shelf file into
/// dragging it out.
const DRAG_THRESHOLD: f32 = 6.0;

/// The notch window, for window events that arrive while the app state is
/// busy and must be retried.
static NOTCH_HWND: AtomicIsize = AtomicIsize::new(0);

/// Ends an alert after it has been shown this long.
const ALERT_TIMER: usize = 2;
const BATTERY_ALERT_MS: u32 = 3000;
/// Counted from the last change, so holding a volume key keeps it open.
const VOLUME_ALERT_MS: u32 = 1500;
const TOAST_ALERT_MS: u32 = 5000;
const BLUETOOTH_ALERT_MS: u32 = 3000;
const NOW_PLAYING_ALERT_MS: u32 = 4000;
/// Tracks reported this soon after start-up are already playing, not new.
const STARTUP_QUIET: Duration = Duration::from_secs(3);
/// How long a notification lingers after the pointer leaves it.
const TOAST_GRACE_MS: u32 = 2000;

/// Velocities (logical px/s) of the width and height "bump" on a new track.
const TRACK_PULSE: (f32, f32) = (320.0, 90.0);
/// The same, when one alert replaces another; softer, as the shape is
/// already out.
const ALERT_PULSE: (f32, f32) = (260.0, 60.0);
/// The same, when files are dropped on the notch: it swallows them.
const DROP_PULSE: (f32, f32) = (220.0, 70.0);
/// Closes the output picker once a pick's highlight has slid over.
const PICKER_CLOSE_TIMER: usize = 7;
const PICKER_CLOSE_MS: u32 = 420;

/// How long replaced alert content takes to slide out and the new in.
const ALERT_TRANSITION: Duration = Duration::from_millis(450);

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
    /// The progress bar swelling while dragged, in percent.
    seek_grow: Spring,
    /// `None` on machines without a battery.
    battery: Option<Battery>,
    /// The alert on screen; `leaving` once its time is up and the notch is
    /// shrinking back (its content fades with the shape).
    alert: Option<(Alert, bool)>,
    /// The alert being replaced, and when the replacement started.
    alert_transition: Option<(Alert, Instant)>,
    /// When the app started; a track found playing right then gets no Now
    /// Playing card.
    started: Instant,
    /// The resting notch's clock text; see `clock_text`.
    time: String,
    /// Every how many refreshes ambient animations draw; see `AMBIENT_FPS`.
    ambient_every: u32,
    /// Volume alert slider, in percent.
    volume: Spring,
    notifications: Notifications,
    bluetooth: Bluetooth,
    /// The device the resting notch's indicator shows.
    bluetooth_device: Option<Device>,
    audio: Volume,
    controls: Controls,
    control_state: ControlState,
    /// Open notch page, side by side: -100 (left), 0 (middle), 100
    /// (right); which page is where is `Settings::page_order`.
    page: Spring,
    /// While a touchpad drags the pages: where the fingers have taken them
    /// (-100..100, unclamped), before rubber-banding.
    page_drag: Option<f32>,
    /// The drag's latest direction: to the right.
    page_drag_rightward: bool,
    /// A Control Center slider being dragged, with its value (0..1).
    slider_drag: Option<(Slider, f32)>,
    /// The screens whose brightness can be set, as last read.
    displays: Vec<DisplayLevel>,
    /// The Displays sheet (each screen's brightness) is open over the
    /// Control Center.
    displays_open: bool,
    /// How far it is shown, 0..100.
    displays_reveal: Spring,
    /// A screen's slider in it being dragged, with its value (0..1).
    display_drag: Option<(usize, f32)>,
    /// Per toggle (`Toggle::index`), how switched on it looks, 0..100;
    /// eases the color change.
    toggle_on: [Spring; 4],
    /// Per toggle, its size in percent; dips while pressed and springs back.
    toggle_press: [Spring; 4],
    /// The active window reaches under the notch (a maximized window's
    /// title bar or tabs), so it shrinks to a mini pill.
    obstructed: bool,
    /// An app fills the whole monitor, so the notch is hidden.
    fullscreen: bool,
    /// The primary monitor's bounds, in physical pixels.
    monitor: RECT,
    /// The resting notch's equalizer is on screen and moving.
    equalizer_ticking: bool,
    /// Runs the plugins folder's scripts while enabled in Settings.
    plugins: Plugins,
    /// The output's peak meter and the equalizer bars made from it.
    meter: Meter,
    bars: Bars,
    /// When the bars last moved; `None` while no equalizer shows.
    bars_at: Option<Instant>,
    outputs: Outputs,
    /// The output devices, as last read.
    output_list: Vec<Output>,
    /// The output picker is open over the player.
    output_picker: bool,
    /// How far the picker has replaced the player, in percent.
    output_reveal: Spring,
    /// First picker row shown.
    output_scroll: Spring,
    /// Picker row the highlight is on; springs to a new pick.
    output_highlight: Spring,
    /// Picker row under the pointer, and how much its hover shows (percent).
    output_hover: Spring,
    output_hover_alpha: Spring,
    /// Picker row the mouse button went down on.
    output_pressed: Option<usize>,
    /// Click-through window for the open notch's shadow, and its canvas
    /// while it shows (freed at rest).
    shadow_hwnd: HWND,
    shadow: Option<(Pixmap, Surface)>,
    /// Width, height and radius the shadow was last drawn for.
    shadow_shape: Option<(f32, f32, f32)>,
    /// The tray menu's choices.
    settings: Settings,
    shelf: Shelf,
    /// First shelf tile shown.
    shelf_scroll: Spring,
    shelf_hover: Option<ShelfHit>,
    /// What the button went down on, and where (to tell a click from a drag).
    shelf_pressed: Option<(ShelfHit, (f32, f32))>,
    /// Files are being dragged over the notch: it is open as a drop zone.
    drop_hover: bool,
    /// How far the drop zone is lit up, 0..100; follows `drop_hover`.
    drop_glow: Spring,
    /// Files taken off the shelf, flying into the bin: when each is thrown
    /// (later for the ones waiting their turn), its icon, and where it was.
    shelf_departures: Vec<Thrown>,
    /// When a file was taken off, and its index: the gap closing.
    shelf_closing: Option<(Instant, usize)>,
    /// Files just dropped on the shelf: when, how many, and where (logical
    /// canvas px) they were let go.
    shelf_arrival: Option<(Instant, usize, (f32, f32))>,
}

/// A file thrown off the shelf: when, its icon, and where it was.
type Thrown = (Instant, Option<std::sync::Arc<Pixmap>>, (f32, f32));

/// Hands drags over the notch to the app.
struct NotchDrop;

impl shelf::DropSink for NotchDrop {
    fn drag_hover(&self, files: bool) {
        with_app(|app| app.on_drag_hover(files));
    }

    fn dropped(&self, paths: Vec<PathBuf>) {
        with_app(|app| app.on_drop(paths));
    }

    fn dragging_out(&self) -> bool {
        DRAGGING_OUT.load(Ordering::Relaxed)
    }
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

        // The open notch's shadow, in a window of its own just behind it:
        // TRANSPARENT lets the mouse through, which the notch's own window
        // cannot do for any pixel that is not fully clear.
        let shadow_class = w!("NotchWoyShadow");
        let shadow_wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(shadow_proc),
            hInstance: instance,
            lpszClassName: shadow_class,
            ..Default::default()
        };
        if RegisterClassExW(&shadow_wc) == 0 {
            return Err(Error::from_win32());
        }
        let shadow_hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            shadow_class,
            w!("notch-woy shadow"),
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
        let settings = Settings::load();

        let app = App {
            hwnd,
            pixmap: Pixmap::new(1, 1).expect("1x1 pixmap"),
            surface: Surface::new(1, 1)?,
            scale: 1.0,
            origin: POINT::default(),
            hovered: false,
            width: Spring::new(collapsed_shape(battery, settings).width),
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
            seek_grow: Spring::new(0.0),
            battery,
            alert: None,
            alert_transition: None,
            started: Instant::now(),
            time: clock_text(),
            ambient_every: ambient_every(),
            volume: Spring::new(0.0),
            notifications: Notifications::spawn(hwnd),
            bluetooth: Bluetooth::spawn(hwnd, settings.bluetooth_quiet()),
            bluetooth_device: None,
            audio: Volume::spawn(hwnd),
            controls: Controls::spawn(hwnd),
            control_state: ControlState::default(),
            page: Spring::smooth(page_slot(settings, settings.start_page)),
            page_drag: None,
            page_drag_rightward: false,
            slider_drag: None,
            displays: Vec::new(),
            displays_open: false,
            displays_reveal: Spring::new(0.0),
            display_drag: None,
            toggle_on: [Spring::smooth(0.0); 4],
            toggle_press: [Spring::new(100.0); 4],
            obstructed: false,
            fullscreen: false,
            monitor: RECT::default(),
            equalizer_ticking: false,
            plugins: Plugins::spawn(hwnd, settings.plugins),
            meter: Meter::new(),
            bars: Bars::default(),
            bars_at: None,
            outputs: Outputs::new(hwnd),
            output_list: Vec::new(),
            output_picker: false,
            output_reveal: Spring::new(0.0),
            output_scroll: Spring::smooth(0.0),
            output_highlight: Spring::new(0.0),
            output_hover: Spring::new(0.0),
            output_hover_alpha: Spring::smooth(0.0),
            output_pressed: None,
            shadow_hwnd,
            shadow: None,
            shadow_shape: None,
            settings,
            shelf: Shelf::new(hwnd, 1.0),
            shelf_scroll: Spring::smooth(0.0),
            shelf_hover: None,
            shelf_pressed: None,
            drop_hover: false,
            drop_glow: Spring::new(0.0),
            shelf_arrival: None,
            shelf_departures: Vec::new(),
            shelf_closing: None,
        };
        // Read ahead so the picker opens with the list already there.
        app.outputs.refresh();
        APP.with_borrow_mut(|slot| *slot = Some(app));
        NOTCH_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        tray::add(hwnd);
        if let Err(e) = shelf::register_drop_target(hwnd, Box::new(NotchDrop)) {
            eprintln!("the shelf takes no drops: {e}");
        }
        // Only while the notch shows volume alerts: otherwise the keys would
        // change the volume with nothing on screen.
        if settings.alert_enabled(AlertKind::Volume) {
            volume_keys::install(hwnd);
        }
        with_app(App::relayout).transpose()?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        watch_windows();
        // `notch-woy --settings` opens Settings right away (from a shortcut,
        // say, or to look at it without going through the tray).
        if std::env::args().any(|arg| arg == "--settings") {
            settings_window::open(hwnd);
        }
        // After showing: it hides the notch again if an app is full screen.
        with_app(App::update_obstruction);

        // Start-up leaves a lot behind (WinRT and device set-up) that idle
        // never touches again.
        SetTimer(Some(hwnd), TRIM_TIMER, TRIM_IDLE_MS, None);
        schedule_clock(hwnd);

        let mut msg = MSG::default();
        // Refreshes since the last ambient frame.
        let mut since_ambient = 0;
        loop {
            let (notch, ambient) = with_app(|app| (app.is_animating(), app.ambient().then_some(app.ambient_every)))
                .unwrap_or((false, None));
            if notch || ambient.is_some() || settings_window::is_animating() {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                since_ambient += 1;
                let ambient_due = ambient.is_some_and(|every| since_ambient >= every);
                if notch || ambient_due {
                    since_ambient = 0;
                    // Ambient frames only redraw: nothing springs, and the
                    // notch counts as at rest (it still trims its memory).
                    let frame = if notch { with_app(App::tick) } else { with_app(App::redraw) };
                    if let Some(Err(e)) = frame {
                        eprintln!("frame failed: {e}");
                    }
                }
                settings_window::frame();
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

/// Where `page` sits in `settings`' order, as a `page` value: -100, 0, 100.
fn page_slot(settings: Settings, page: Page) -> f32 {
    render::slot_of(settings.page_order, page) * 100.0
}

/// Every how many display refreshes ambient animations draw, for the
/// primary display's refresh rate (from the compositor).
fn ambient_every() -> u32 {
    let mut timing = DWM_TIMING_INFO { cbSize: size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let hz = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing) }
        .ok()
        .filter(|()| timing.rateRefresh.uiDenominator != 0)
        .map_or(60.0, |()| timing.rateRefresh.uiNumerator as f32 / timing.rateRefresh.uiDenominator as f32);
    (hz / AMBIENT_FPS).round().max(1.0) as u32
}

/// Hands the pages the app is not using back to Windows, so its memory in
/// Task Manager is what idle really touches. Start-up (WinRT, device and
/// audio set-up) leaves several MB behind that the notch never reads again;
/// pages it does need come back as soft faults (no disk read), which costs
/// microseconds the next time the notch moves.
pub fn trim_working_set() {
    render::release_scratch();
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Re-checks what is under the notch whenever the active window changes,
/// appears, moves, resizes, or is minimized or restored. Out-of-context
/// hooks are delivered through this thread's message loop, so no polling.
fn watch_windows() {
    unsafe {
        for (first, last) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            // A newly opened app often becomes active before it is shown,
            // or while still cloaked for its opening animation (Store and
            // Chromium apps), and then appears without moving.
            (EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_UNCLOAKED, EVENT_OBJECT_UNCLOAKED),
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
    // Object events fire for every caret and widget on screen; only the
    // active window itself matters.
    let object_event = matches!(event, EVENT_OBJECT_SHOW | EVENT_OBJECT_LOCATIONCHANGE | EVENT_OBJECT_UNCLOAKED);
    if object_event && (object != OBJID_WINDOW.0 || hwnd != unsafe { GetForegroundWindow() }) {
        return;
    }
    let handled = with_app(|app| {
        app.update_obstruction();
        if event == EVENT_SYSTEM_FOREGROUND {
            // Some apps size themselves just after activating; look again.
            unsafe {
                SetTimer(Some(app.hwnd), RECHECK_TIMER, RECHECK_MS, None);
            }
        }
    });
    // The state was borrowed (the hook ran inside another handler): look
    // shortly instead of dropping the change.
    if handled.is_none() {
        let notch = NOTCH_HWND.load(Ordering::Relaxed);
        if notch != 0 {
            unsafe {
                SetTimer(Some(HWND(notch as _)), RECHECK_TIMER, RECHECK_MS, None);
            }
        }
    }
}

/// The visible frame of an ordinary app window: `None` for hidden or
/// minimized windows, and for the desktop and taskbar, which fill the
/// screen without being apps.
fn app_frame(hwnd: HWND) -> Option<RECT> {
    unsafe {
        if hwnd.is_invalid() || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let mut class = [0u16; 32];
        let len = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return None;
        }
        // Screen-sized overlays that are no app: GPU overlays (NVIDIA's is
        // NOACTIVATE), the pen canvas (TRANSPARENT), and shell windows kept
        // "visible" but cloaked (the touch keyboard's host). Any of them
        // can be the active window right after login, and would otherwise
        // count as a full-screen app and hide the notch.
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        if ex.contains(WS_EX_NOACTIVATE) || ex.contains(WS_EX_TRANSPARENT) {
            return None;
        }
        let mut cloaked = 0u32;
        let got = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        );
        if got.is_ok() && cloaked != 0 {
            return None;
        }
        // Without the invisible resize borders.
        let mut frame = RECT::default();
        let got = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        );
        if got.is_err() && GetWindowRect(hwnd, &mut frame).is_err() {
            return None;
        }
        Some(frame)
    }
}

/// Whether `hwnd` is an app window whose frame overlaps `area`.
fn covers(hwnd: HWND, area: RECT) -> bool {
    app_frame(hwnd).is_some_and(|f| f.left < area.right && f.right > area.left && f.top < area.bottom && f.bottom > area.top)
}

/// Whether `hwnd` is an app filling the whole monitor, taskbar included:
/// a game, full-screen video, a slideshow. Maximized windows stop at the
/// taskbar, so they do not count.
fn fills(hwnd: HWND, monitor: RECT) -> bool {
    app_frame(hwnd).is_some_and(|f| {
        f.left <= monitor.left && f.top <= monitor.top && f.right >= monitor.right && f.bottom >= monitor.bottom
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rubber_bands_past_the_pages() {
        // Between the pages, travel is untouched.
        assert_eq!(rubber_band(0.0), 0.0);
        assert_eq!(rubber_band(37.5), 37.5);
        assert_eq!(rubber_band(100.0), 100.0);
        assert_eq!(rubber_band(-40.0), -40.0);
        // Past them it is resisted, and never goes beyond the limit.
        let a = rubber_band(110.0);
        let b = rubber_band(140.0);
        assert!(a > 100.0 && b > a && b < 118.0, "{a} {b}");
        assert!(rubber_band(10_000.0) < 118.0);
        assert!(rubber_band(110.0) > 100.0 && rubber_band(110.0) < 110.0);
        // Down to the shelf the same way.
        assert_eq!(rubber_band(-100.0), -100.0);
        let below = rubber_band(-140.0);
        assert!(below < -100.0 && below > -118.0, "{below}");
    }

    #[test]
    fn ambient_frames_follow_the_display() {
        // About 60 fps whatever the refresh rate: every refresh at 60 Hz,
        // every third at 180 Hz.
        let every = ambient_every();
        println!("ambient frames every {every} refreshes");
        assert!((1..=5).contains(&every), "{every}");
    }

    #[test]
    fn clock_is_hours_and_minutes() {
        // Windows' own short time format, without seconds or AM/PM: "9:41",
        // "09:41" or "21:41", whatever the user's settings.
        let time = clock_text();
        let (hours, minutes) = time.split_once(':').expect(&time);
        assert!((1..=2).contains(&hours.len()) && hours.chars().all(|c| c.is_ascii_digit()), "{time}");
        assert!(minutes.len() == 2 && minutes.chars().all(|c| c.is_ascii_digit()), "{time}");
    }

    #[test]
    fn touchpad_settles_on_pages() {
        // From the player: a fifth of the way is enough, either way.
        assert_eq!(page_after_drag(25.0, true), 100.0);
        assert_eq!(page_after_drag(15.0, true), 0.0);
        assert_eq!(page_after_drag(-25.0, false), -100.0);
        assert_eq!(page_after_drag(-15.0, false), 0.0);
        // Back from the shelf, and past the ends.
        assert_eq!(page_after_drag(-75.0, true), 0.0);
        assert_eq!(page_after_drag(-90.0, true), -100.0);
        assert_eq!(page_after_drag(115.0, true), 100.0);
        assert_eq!(page_after_drag(-115.0, false), -100.0);
    }

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
                "active window class {:?}, maximized {}, under notch {}, full screen {}",
                String::from_utf16_lossy(&class[..len]),
                IsZoomed(fg).as_bool(),
                covers(fg, area),
                fills(fg, m)
            );
            // The desktop spans the monitor but must never count as a
            // full-screen app (the shell's own notice gets this wrong).
            let desktop = GetShellWindow();
            println!("desktop full screen {}", fills(desktop, m));
            assert!(!fills(desktop, m));
            // Nor screen-sized overlays: NVIDIA's, the pen canvas, the
            // cloaked touch keyboard host. Checked when present.
            for class in [w!("CEF-OSC-WIDGET"), w!("ShellHandwritingCanvas"), w!("Windows.UI.Core.CoreWindow")] {
                if let Ok(overlay) = FindWindowW(class, None) {
                    println!("{} full screen {}", class.display(), fills(overlay, m));
                    assert!(!fills(overlay, m));
                }
            }
        }
    }
}

/// The resting notch, a little wider when it has a battery to show, plus
/// the width setting's extra.
fn collapsed_shape(battery: Option<Battery>, settings: Settings) -> Shape {
    let width = if battery.is_some() { COLLAPSED_BATTERY_WIDTH } else { COLLAPSED.width };
    Shape { width: width + settings.width.extra(), ..COLLAPSED }
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
            || self.alert_transition.is_some()
            // Its equalizer moves for as long as it is on screen.
            || matches!(self.alert, Some((Alert::NowPlaying { .. }, _)))
            // The drop zone's dashes march while files are over it, and
            // dropped files fall into place.
            || self.drop_hover
            || self.shelf_arrival.is_some()
            || !self.shelf_departures.is_empty()
            || self.shelf_closing.is_some()
            || ![
                self.width,
                self.height,
                self.radius,
                self.lyric_scroll,
                self.lyrics_reveal,
                self.panel_width,
                self.volume,
                self.page,
                self.output_reveal,
                self.seek_grow,
                self.output_scroll,
                self.output_highlight,
                self.output_hover,
                self.output_hover_alpha,
                self.shelf_scroll,
                self.drop_glow,
                self.displays_reveal,
            ]
            .iter()
            .chain(&self.toggle_on)
            .chain(&self.toggle_press)
            .all(Spring::is_settled)
    }

    /// Points each toggle's color animation at its current state.
    fn sync_toggles(&mut self) {
        let (_, muted) = self.audio.level();
        for toggle in Toggle::ALL {
            let target = if toggle.is_on(&self.control_state, muted) { 100.0 } else { 0.0 };
            let spring = &mut self.toggle_on[toggle.index()];
            if spring.target != target {
                spring.target = target;
                self.start_animation();
            }
        }
    }

    /// Jumps the toggles to their state with no animation, for when the
    /// page comes into view.
    fn snap_toggles(&mut self) {
        self.sync_toggles();
        for spring in &mut self.toggle_on {
            *spring = Spring::smooth(spring.target);
        }
    }

    /// Lets go of any pressed toggle; it springs back to full size.
    fn release_toggles(&mut self) {
        for spring in &mut self.toggle_press {
            if spring.target != 100.0 {
                spring.target = 100.0;
            }
        }
        self.start_animation();
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
        let dragging = self.seeking.is_some() || self.slider_drag.is_some() || self.shelf_pressed.is_some();
        if self.hovered == hovered || !hovered && dragging {
            return;
        }
        self.hovered = hovered;
        self.retarget();
        if !hovered {
            // It opens on the start page (the Layout setting) next time;
            // unless files are being dragged onto it.
            if !self.drop_hover {
                self.page = Spring::smooth(self.slot(self.settings.start_page));
            }
            self.page_drag = None;
            self.shelf_hover = None;
            self.shelf_pressed = None;
            self.output_picker = false;
            self.output_reveal = Spring::new(0.0);
            self.output_hover_alpha = Spring::smooth(0.0);
            self.output_pressed = None;
            self.displays_open = false;
            self.displays_reveal = Spring::new(0.0);
            self.display_drag = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), PICKER_CLOSE_TIMER);
            }
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
            // Toggles open already showing their state, not fading into it.
            self.snap_toggles();
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
        let rest = collapsed_shape(self.battery, self.settings);
        let half = (rest.width * self.scale / 2.0).round() as i32;
        let center = self.origin.x + self.pixmap.width() as i32 / 2;
        let area = RECT {
            left: center - half,
            top: self.origin.y,
            right: center + half,
            bottom: self.origin.y + (rest.height * self.scale).round() as i32,
        };
        let foreground = unsafe { GetForegroundWindow() };
        let ours = foreground == self.hwnd;
        let obstructed = !ours && covers(foreground, area);
        if obstructed != self.obstructed {
            self.obstructed = obstructed;
            self.retarget();
        }

        // Out of the way of games and full-screen video. Checked here rather
        // than trusting the shell's full-screen notice, which also fires for
        // the desktop and would hide the notch until some app opened.
        let fullscreen = !ours && fills(foreground, self.monitor);
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            unsafe {
                if fullscreen {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                    SetTimer(Some(self.hwnd), HIDDEN_RECHECK_TIMER, HIDDEN_RECHECK_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), HIDDEN_RECHECK_TIMER);
                    // Back on top too: windows raised while it was hidden
                    // (other topmost ones) must not cover it.
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
            }
            if fullscreen {
                self.hide_shadow();
                self.shadow_shape = None;
            }
        }
    }

    /// Takes new tray menu choices into effect.
    fn apply_settings(&mut self, next: Settings) -> Result<()> {
        // The page on screen, to keep it there if the order changes.
        let showing = self.settings.page_order.iter().copied().min_by(|&a, &b| {
            let at = |p| (self.page.target - self.slot(p)).abs();
            at(a).total_cmp(&at(b))
        });
        let before = std::mem::replace(&mut self.settings, next);
        if next.page_order != before.page_order || next.start_page != before.start_page {
            self.page_drag = None;
            let target = match showing.filter(|_| self.hovered || self.drop_hover) {
                Some(page) => self.slot(page),
                None => self.slot(next.start_page),
            };
            self.page = Spring::smooth(target);
        }
        let volume_alerts = next.alert_enabled(AlertKind::Volume);
        if volume_alerts != before.alert_enabled(AlertKind::Volume) {
            // Without the notch's alert, the keys go back to Windows and its
            // own pop-up.
            if volume_alerts {
                volume_keys::install(self.hwnd);
            } else {
                volume_keys::uninstall();
            }
        }
        if next.bluetooth_quiet_minutes != before.bluetooth_quiet_minutes {
            self.bluetooth.set_repeat_quiet(next.bluetooth_quiet());
        }
        // An alert of a kind just turned off leaves right away.
        let kind = self.alert.as_ref().filter(|(_, leaving)| !leaving).map(|(alert, _)| match alert {
            Alert::Battery { .. } => AlertKind::Battery,
            Alert::Volume { .. } => AlertKind::Volume,
            Alert::Toast(_) => AlertKind::Notifications,
            Alert::Bluetooth { .. } => AlertKind::Bluetooth,
            Alert::NowPlaying { .. } => AlertKind::NowPlaying,
        });
        if kind.is_some_and(|kind| !next.alert_enabled(kind)) {
            self.end_alert();
        }
        if next.width != before.width {
            // The area that counts as under the notch changes with it.
            self.update_obstruction();
        }
        if next.plugins != before.plugins {
            self.plugins.set_enabled(next.plugins);
        }
        if next.open_size != before.open_size {
            // Shelf thumbnails are made at the size they show.
            let scale = self.open_scale();
            self.shelf.set_scale(scale);
        }
        self.retarget();
        self.invalidate()
    }

    /// Refreshes the indicator and alerts the latest connect/disconnect.
    fn on_bluetooth(&mut self) -> Result<()> {
        self.bluetooth_device = self.bluetooth.primary();
        // Taken either way, so a change made while alerts are off does not
        // show up once they are back on.
        let event = self.bluetooth.take_latest_event();
        if let Some((device, connected)) = event.filter(|_| self.settings.alert_enabled(AlertKind::Bluetooth)) {
            self.show_alert(Alert::Bluetooth { device, connected }, BLUETOOTH_ALERT_MS);
        }
        self.invalidate()
    }

    fn on_toast(&mut self) -> Result<()> {
        let Some(toast) = self.notifications.take_latest() else { return Ok(()) };
        if !self.settings.alert_enabled(AlertKind::Notifications) {
            return Ok(());
        }
        self.show_alert(Alert::Toast(toast), TOAST_ALERT_MS);
        if self.hovered {
            // Already under the pointer: hold it like a hovered one.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), ALERT_TIMER);
            }
        }
        self.invalidate()
    }

    /// A plugin's title changed: shown like a notification, under the
    /// plugin's name; a click opens its link.
    fn on_plugin(&mut self) -> Result<()> {
        let Some(update) = self.plugins.take().pop() else { return Ok(()) };
        if !self.settings.plugins {
            return Ok(());
        }
        let toast = Toast {
            app: update.name,
            title: update.output.title,
            body: update.output.body,
            icon: None,
            icon_is_tile: false,
            app_id: String::new(),
            tint: None,
            link: update.output.href,
        };
        self.show_alert(Alert::Toast(std::sync::Arc::new(toast)), TOAST_ALERT_MS);
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
        let rest = collapsed_shape(self.battery, self.settings);
        let target = if let Some(alert) = showing_alert.filter(|_| self.showing_toast()) {
            render::alert_shape(alert, rest.width)
        } else if self.hovered || self.drop_hover {
            let zoom = self.settings.open_size.zoom();
            Shape { width: self.open_width() * zoom, height: EXPANDED.height * zoom, radius: EXPANDED.radius * zoom }
        } else if let Some(alert) = showing_alert {
            render::alert_shape(alert, rest.width)
        } else if self.obstructed {
            MINI
        } else {
            rest
        };
        self.width.target = target.width;
        self.height.target = target.height;
        self.radius.target = target.radius;
    }

    /// Physical px per unit of the open notch's layout: the display scale
    /// times the Open size setting's. Hit tests on the open notch use it.
    fn open_scale(&self) -> f32 {
        self.scale * self.settings.open_size.zoom()
    }

    /// Whether the notch has (nearly) finished opening.
    fn is_open(&self) -> bool {
        self.height.value >= EXPANDED.height * self.settings.open_size.zoom() * 0.9
    }

    /// The open notch's width for the page it is on or heading to, in its
    /// layout's units (before the Open size setting's scale).
    fn open_width(&self) -> f32 {
        render::open_width(self.settings.page_order, self.panel_width.target, self.page.target / 100.0, self.shelf.len())
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
        let alerts = self.settings.alert_enabled(AlertKind::Battery);
        if let Some((event, battery)) = event.zip(now).filter(|_| alerts) {
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
        // Replacing an alert on screen (on battery → charging...) slides the
        // old content out and the new in, with a small bump of the island.
        // Volume updates repeat while a key is held and just update in place.
        if let Some((old, false)) = self.alert.take() {
            if !matches!((&old, &alert), (Alert::Volume { .. }, Alert::Volume { .. })) {
                self.start_animation();
                self.alert_transition = Some((old, Instant::now()));
                self.width.kick(ALERT_PULSE.0);
                self.height.kick(ALERT_PULSE.1);
            }
        }
        self.alert = Some((alert, false));
        let shown_ms = self.settings.duration.scale(shown_ms);
        // Re-setting a running timer restarts its countdown.
        unsafe {
            SetTimer(Some(self.hwnd), ALERT_TIMER, shown_ms, None);
        }
        self.retarget();
    }

    /// A volume key, taken over from Windows. The device's change report
    /// that follows brings up the notch's volume alert.
    fn on_volume_key(&mut self, key: VolumeKey) {
        let (level, muted) = self.audio.level();
        match key {
            VolumeKey::Mute => self.audio.set_mute(!muted),
            VolumeKey::Up | VolumeKey::Down => {
                // As in Windows, changing the volume unmutes.
                if muted {
                    self.audio.set_mute(false);
                }
                self.audio.set_level(volume_keys::step(level, key));
            }
        }
    }

    /// A volume or mute change from any source.
    fn on_volume(&mut self, percent: u32, muted: bool) -> Result<()> {
        // Mute may have changed (keyboard, another app): the Mute toggle.
        self.sync_toggles();
        // The Control Center page already shows it, in its own slider.
        if self.on_controls_page() || !self.settings.alert_enabled(AlertKind::Volume) {
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
        self.output_reveal.update(dt);
        self.seek_grow.update(dt);
        self.output_scroll.update(dt);
        self.output_highlight.update(dt);
        self.output_hover.update(dt);
        self.output_hover_alpha.update(dt);
        self.shelf_scroll.update(dt);
        self.drop_glow.update(dt);
        self.displays_reveal.update(dt);
        let throw = render::shelf_throw_length();
        self.shelf_departures.retain(|&(at, _, _)| Instant::now() < at || at.elapsed().as_secs_f32() < throw);
        if self.shelf_closing.is_some_and(|(at, _)| at.elapsed().as_secs_f32() >= render::shelf_close_length()) {
            self.shelf_closing = None;
        }
        if let Some((at, count, _)) = self.shelf_arrival
            && at.elapsed().as_secs_f32() >= render::shelf_arrival_length(count)
        {
            self.shelf_arrival = None;
        }
        for spring in self.toggle_on.iter_mut().chain(&mut self.toggle_press) {
            spring.update(dt);
        }
        let result = self.redraw();
        if self.transition.as_ref().is_some_and(|(_, start)| start.elapsed() >= TRACK_TRANSITION) {
            self.transition = None;
        }
        if self.alert_transition.as_ref().is_some_and(|(_, start)| start.elapsed() >= ALERT_TRANSITION) {
            self.alert_transition = None;
        }
        // A finished alert is dropped once the notch has shrunk back.
        if self.alert.as_ref().is_some_and(|(_, leaving)| *leaving) && self.height.is_settled() {
            self.alert = None;
        }
        if !self.hovered && !self.is_animating() {
            // At rest: trim once it stays that way (re-setting restarts it).
            unsafe {
                SetTimer(Some(self.hwnd), TRIM_TIMER, TRIM_IDLE_MS, None);
            }
        }
        result
    }

    /// The equalizer bars' heights (0..1) from what the output is playing,
    /// while an equalizer shows (the resting notch's or a new track's);
    /// `None` when none shows or the meter cannot see the music, and the
    /// equalizers animate on their own.
    fn equalizer_levels(&mut self) -> Option<[f32; crate::meter::BARS]> {
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        let new_track = matches!(self.alert, Some((Alert::NowPlaying { .. }, _)));
        if !playing || !(self.equalizer_ticking || new_track) {
            // The meter is only read while it is shown.
            self.bars_at = None;
            return None;
        }
        let now = Instant::now();
        let dt = self.bars_at.map_or(0.0, |at| now.duration_since(at).as_secs_f32());
        self.bars_at = Some(now);
        self.bars.update(self.meter.read(), dt)
    }

    fn redraw(&mut self) -> Result<()> {
        let levels = self.equalizer_levels();
        let now = Instant::now();
        let departures: Vec<Departure> = self
            .shelf_departures
            .iter()
            .map(|(at, icon, from)| Departure {
                icon: icon.clone(),
                from: *from,
                // Negative while it waits its turn.
                t: if now >= *at { (now - *at).as_secs_f32() } else { -(*at - now).as_secs_f32() },
            })
            .collect();
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
                sung: self.lyric_line.zip(self.now_playing.as_ref().and_then(NowPlaying::progress)).map(
                    |(line, (position, _))| (line, l.sung(line, position)),
                ),
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
            bluetooth_device: self.bluetooth_device.as_ref(),
            battery: self.battery,
            media: self.now_playing.as_ref(),
            toggle_on: self.toggle_on.map(|s| s.value / 100.0),
            toggle_scale: self.toggle_press.map(|s| s.value / 100.0),
            displays: &self.displays,
            displays_reveal: self.displays_reveal.value / 100.0,
            display_drag: self.display_drag,
        };
        let scene = Scene {
            shape,
            panel_width: self.panel_width.value,
            lyrics_reveal: self.lyrics_reveal.value / 100.0,
            media: self.now_playing.as_ref(),
            transition,
            lyrics,
            seek: self.seeking,
            seek_grow: self.seek_grow.value / 100.0,
            battery: self.battery,
            rest_width: collapsed_shape(self.battery, self.settings).width,
            bluetooth: self.bluetooth_device.as_ref().map(|d| d.kind),
            alert: self.alert.as_ref().map(|(alert, _)| match alert {
                Alert::Volume { muted, .. } => Alert::Volume { level: self.volume.value / 100.0, muted: *muted },
                other => other.clone(),
            }),
            alert_from: self.alert_transition.as_ref().map(|(from, start)| {
                (from.clone(), (start.elapsed().as_secs_f32() / ALERT_TRANSITION.as_secs_f32()).min(1.0))
            }),
            page: self.page.value / 100.0,
            controls,
            clock: self.started.elapsed().as_secs_f32(),
            levels,
            open_zoom: self.settings.open_size.zoom(),
            page_order: self.settings.page_order,
            time: &self.time,
            outputs: OutputsView {
                list: &self.output_list,
                reveal: self.output_reveal.value / 100.0,
                scroll: self.output_scroll.value,
                highlight: self.output_highlight.value,
                hover: self.output_hover.value,
                hover_alpha: self.output_hover_alpha.value / 100.0,
                pressed: self.output_pressed,
            },
            shelf: ShelfView {
                items: self.shelf.items(),
                scroll: self.shelf_scroll.value,
                hover: self.shelf_hover,
                pressed: self.shelf_pressed.map(|(hit, _)| hit),
                drop_glow: self.drop_glow.value / 100.0,
                arrival: self.shelf_arrival.map(|(at, count, from)| Arrival { count, t: at.elapsed().as_secs_f32(), from }),
                departures: &departures,
                closing: self.shelf_closing.map(|(at, index)| (index, at.elapsed().as_secs_f32())),
            },
        };
        render::draw(&mut self.pixmap, self.scale, &scene, self.fonts.as_ref());
        self.update_equalizer_timer();
        self.surface.present(self.hwnd, &self.pixmap, self.origin)?;
        self.update_shadow(shape)
    }

    /// Redraws the shadow when the notch's shape changed; hides it, and
    /// frees its canvas, once the notch is back at rest.
    fn update_shadow(&mut self, shape: Shape) -> Result<()> {
        let key = (shape.width, shape.height, shape.radius);
        if self.shadow_shape == Some(key) {
            return Ok(());
        }
        self.shadow_shape = Some(key);
        if shape.height <= COLLAPSED.height {
            self.hide_shadow();
            return Ok(());
        }
        let showing = self.shadow.is_some();
        // Wider than the notch's canvas by the same amount on either side,
        // so both stay centered on the screen.
        let pad = (SHADOW_PAD * self.scale).round() as u32;
        let origin = POINT { x: self.origin.x - pad as i32, y: self.origin.y };
        let (pixmap, surface) = match &mut self.shadow {
            Some(shadow) => shadow,
            None => {
                let (w, h) = (self.pixmap.width() + 2 * pad, self.pixmap.height() + pad);
                let pixmap = Pixmap::new(w, h).expect("non-zero canvas size");
                self.shadow.insert((pixmap, Surface::new(w as i32, h as i32)?))
            }
        };
        render::draw_shadow(pixmap, self.scale, shape, self.settings.open_size.zoom());
        surface.present(self.shadow_hwnd, pixmap, origin)?;
        if !showing {
            unsafe {
                // Just behind the notch.
                let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
                SetWindowPos(self.shadow_hwnd, Some(self.hwnd), 0, 0, 0, 0, flags)?;
            }
        }
        Ok(())
    }

    fn hide_shadow(&mut self) {
        if self.shadow.take().is_some() {
            unsafe {
                let _ = ShowWindow(self.shadow_hwnd, SW_HIDE);
            }
        }
    }

    /// Runs the equalizer tick only while the resting bars are on screen
    /// and moving.
    fn update_equalizer_timer(&mut self) {
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        let showing = playing && !self.hovered && self.alert.is_none() && !self.obstructed && !self.fullscreen;
        if showing == self.equalizer_ticking {
            return;
        }
        self.equalizer_ticking = showing;
    }

    /// Opens the output list over the player, scrolled to show the
    /// current output.
    fn open_output_picker(&mut self) {
        self.outputs.refresh();
        self.start_animation();
        self.output_picker = true;
        self.output_reveal.target = 100.0;
        let current = self.output_list.iter().position(|o| o.default).unwrap_or(0);
        let max = self.output_list.len().saturating_sub(OUTPUT_ROWS_SHOWN);
        self.output_scroll = Spring::smooth(current.saturating_sub(OUTPUT_ROWS_SHOWN - 1).min(max) as f32);
        self.output_highlight = Spring::new(current as f32);
        self.output_hover_alpha = Spring::smooth(0.0);
        self.output_pressed = None;
    }

    fn close_output_picker(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PICKER_CLOSE_TIMER);
        }
        if self.output_picker {
            self.start_animation();
            self.output_picker = false;
            self.output_reveal.target = 0.0;
            self.output_hover_alpha.target = 0.0;
            self.output_pressed = None;
        }
    }

    /// The picker row under the pointer, once the picker is open.
    fn output_row_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.output_picker || self.output_reveal.value < 0.8 {
            return None;
        }
        render::output_at(
            self.pixmap.width(),
            self.open_scale(),
            self.panel_width.value,
            self.output_scroll.value,
            self.output_list.len(),
            (x, y),
        )
    }

    /// The hover highlight follows the pointer from row to row.
    fn on_picker_hover(&mut self, x: f32, y: f32) {
        if !self.output_picker {
            return;
        }
        let row = self.output_row_at(x, y);
        let (alpha, target) = match row {
            Some(row) => (100.0, row as f32),
            None => (0.0, self.output_hover.target),
        };
        if self.output_hover_alpha.target == alpha && self.output_hover.target == target {
            return;
        }
        self.start_animation();
        // Appearing, it starts on its row rather than gliding in from the
        // last one.
        if self.output_hover_alpha.value < 1.0 {
            self.output_hover = Spring::new(target);
        }
        self.output_hover.target = target;
        self.output_hover_alpha.target = alpha;
        if row != self.output_pressed {
            self.output_pressed = None;
        }
    }

    fn on_outputs(&mut self) -> Result<()> {
        self.output_list = self.outputs.list();
        let max = self.output_list.len().saturating_sub(OUTPUT_ROWS_SHOWN) as f32;
        self.output_scroll.target = self.output_scroll.target.min(max);
        self.invalidate()
    }

    /// Whether the open notch is showing (or turning to) the Control Center.
    fn on_controls_page(&self) -> bool {
        self.hovered && (self.page.target - self.slot(Page::Controls)).abs() < 50.0
    }

    /// Where `page` sits, as a `page` value: -100, 0 or 100.
    fn slot(&self, page: Page) -> f32 {
        page_slot(self.settings, page)
    }

    fn scrollable(&self) -> bool {
        self.hovered && self.is_open() && !self.showing_toast()
    }

    /// Mouse wheel or touchpad scroll up (`delta` > 0) or down. Scrolls the
    /// output picker or the shelf's files when they are showing (the shelf
    /// hands a mouse wheel on to the pages once at its end); otherwise down
    /// turns to the page on the right, as Windows' sideways lists do.
    fn on_wheel(&mut self, delta: i32) -> Result<()> {
        if !self.scrollable() || delta == 0 {
            return Ok(());
        }
        self.start_animation();
        if self.displays_open {
            return Ok(());
        }
        if self.output_picker && !self.on_controls_page() {
            let max = self.output_list.len().saturating_sub(OUTPUT_ROWS_SHOWN) as f32;
            // A wheel notch is a row; a touchpad scrolls smoothly.
            let target = self.output_scroll.target - delta as f32 / 120.0;
            self.output_scroll.target = target.clamp(0.0, max);
            return Ok(());
        }
        if self.on_shelf_page() && self.page_drag.is_none() && self.shelf_max_scroll() > 0.0 {
            let before = self.shelf_scroll.target;
            self.scroll_shelf(-delta);
            // A touchpad stops at the end, so a scroll through the files
            // does not run on into the player.
            if self.shelf_scroll.target != before || delta.abs() < 120 {
                return Ok(());
            }
        }
        self.turn_pages(-delta);
        Ok(())
    }

    /// Sideways scroll (touchpads, tilt wheels), `delta` > 0 to the right:
    /// the pages follow.
    fn on_sideways_wheel(&mut self, delta: i32) {
        // The output picker covers the player; it only scrolls up and down.
        // The Displays sheet holds the Control Center in place.
        let sheet = (self.output_picker && !self.on_controls_page()) || self.displays_open;
        if !self.scrollable() || delta == 0 || sheet {
            return;
        }
        self.start_animation();
        self.turn_pages(delta);
    }

    /// Toward the page on the right (`delta` > 0, Control Center) or the
    /// left (the shelf). A mouse notch turns a whole page. A touchpad drags
    /// the pages along with the fingers (rubber-banding past either end)
    /// and lets go onto a page once the fingers stop.
    fn turn_pages(&mut self, delta: i32) {
        // Mouse wheels move in whole notches of 120 (a page each); touchpads
        // in small steps.
        if delta.abs() >= 120 && self.page_drag.is_none() {
            let step = if delta > 0 { 100.0 } else { -100.0 };
            self.settle_page((self.page.target + step).clamp(-100.0, 100.0));
            return;
        }
        let raw = self.page_drag.unwrap_or(self.page.target) + delta as f32 * PAGE_PER_WHEEL_UNIT;
        self.page_drag = Some(raw);
        self.page_drag_rightward = delta > 0;
        self.page.target = rubber_band(raw);
        self.retarget();
        unsafe {
            SetTimer(Some(self.hwnd), SCROLL_END_TIMER, SCROLL_END_MS, None);
        }
    }

    /// The fingers stopped: go to the page they were heading for once past
    /// a fifth of the way, else back.
    fn on_scroll_end(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SCROLL_END_TIMER);
        }
        let Some(raw) = self.page_drag.take() else { return };
        self.start_animation();
        self.settle_page(page_after_drag(raw, self.page_drag_rightward));
    }

    fn settle_page(&mut self, target: f32) {
        self.page.target = target;
        let controls = (target - self.slot(Page::Controls)).abs() < 50.0;
        if !controls {
            self.close_displays();
        }
        self.retarget();
        if controls {
            // Radios may have been switched elsewhere since last looked.
            self.controls.send(ControlCommand::Refresh);
        }
    }

    fn on_shelf_page(&self) -> bool {
        (self.hovered || self.drop_hover) && (self.page.target - self.slot(Page::Shelf)).abs() < 50.0
    }

    fn shelf_hit(&self, x: f32, y: f32) -> Option<ShelfHit> {
        let (count, scroll) = (self.shelf.len(), self.shelf_scroll.value);
        render::shelf_at(self.pixmap.width(), self.open_scale(), count, scroll, (x, y))
    }

    /// Files dragged over the notch (`true`) or away (`false`): it opens
    /// onto the shelf as a drop zone, and closes again unless the pointer
    /// rests on it.
    fn on_drag_hover(&mut self, files: bool) {
        if self.drop_hover == files {
            return;
        }
        self.start_animation();
        self.drop_hover = files;
        self.drop_glow.target = if files { 100.0 } else { 0.0 };
        if files {
            self.close_output_picker();
            self.page_drag = None;
            let shelf = self.slot(Page::Shelf);
            if self.hovered {
                self.page.target = shelf;
            } else {
                // Straight onto the shelf as it grows, not via another page.
                self.page = Spring::smooth(shelf);
            }
        } else if !self.hovered {
            self.page = Spring::smooth(self.slot(self.settings.start_page));
        }
        self.retarget();
        let _ = self.invalidate();
    }

    fn on_drop(&mut self, paths: Vec<PathBuf>) {
        self.start_animation();
        self.drop_hover = false;
        self.drop_glow.target = 0.0;
        let added = self.shelf.add(paths);
        if added > 0 {
            // The newest are first; bring them into view, falling in.
            self.shelf_scroll.target = 0.0;
            self.shelf_arrival = Some((Instant::now(), added, self.pointer()));
            // The notch swallows them.
            self.width.kick(DROP_PULSE.0);
            self.height.kick(DROP_PULSE.1);
        }
        // The pointer is on the notch: it stays open on the shelf.
        self.set_hovered(true);
        self.page.target = self.slot(Page::Shelf);
        self.retarget();
        let _ = self.invalidate();
    }

    /// The pointer in the open notch's layout units.
    fn pointer(&self) -> (f32, f32) {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let scale = self.open_scale();
        ((pt.x - self.origin.x) as f32 / scale, (pt.y - self.origin.y) as f32 / scale)
    }

    fn on_shelf_down(&mut self, x: f32, y: f32) -> Result<()> {
        if let Some(hit) = self.shelf_hit(x, y) {
            self.shelf_pressed = Some((hit, (x, y)));
            // A file pressed may be dragged off the notch.
            unsafe { SetCapture(self.hwnd) };
        }
        self.invalidate()
    }

    /// A pressed file moved far enough: its drag out of the shelf starts.
    /// Returns its path; the drag itself runs with the state released.
    fn shelf_drag_start(&mut self, x: f32, y: f32) -> Option<PathBuf> {
        let (ShelfHit::Item(index), (x0, y0)) = self.shelf_pressed? else { return None };
        if (x - x0).hypot(y - y0) < DRAG_THRESHOLD * self.scale {
            return None;
        }
        self.shelf_pressed = None;
        let _ = self.invalidate();
        self.shelf.items().get(index).map(|item| item.path.clone())
    }

    /// The button came up after a press on the shelf: a click if it is
    /// still on what it went down on.
    fn finish_shelf_press(&mut self, x: f32, y: f32) -> Result<()> {
        let Some((pressed, _)) = self.shelf_pressed.take() else { return Ok(()) };
        unsafe {
            let _ = ReleaseCapture();
        }
        if self.shelf_hit(x, y) == Some(pressed) {
            match pressed {
                ShelfHit::Item(index) => self.shelf.open(index),
                ShelfHit::Remove(index) => {
                    // Its icon is thrown into the bin; the tiles after it
                    // close the gap.
                    self.start_animation();
                    let now = Instant::now();
                    if let Some(from) = self.shelf_icon_center(index) {
                        let icon = self.shelf.items().get(index).and_then(|item| item.icon.clone());
                        self.shelf_departures.push((now, icon, from));
                    }
                    self.shelf_closing = Some((now, index));
                    self.shelf_arrival = None;
                    self.shelf.remove(index);
                    self.shelf_hover = None;
                    self.clamp_shelf_scroll();
                    self.retarget();
                }
                ShelfHit::Share => {
                    // The share sheet hangs below the notch's top center.
                    let anchor = POINT {
                        x: self.origin.x + self.pixmap.width() as i32 / 2,
                        y: self.origin.y + (EXPANDED.height * self.open_scale()) as i32,
                    };
                    if let Err(e) = self.shelf.share(anchor) {
                        eprintln!("share failed: {e}");
                    }
                }
                ShelfHit::Clear => {
                    // Every file in sight is thrown into the bin, one after
                    // the other, left to right.
                    self.start_animation();
                    let now = Instant::now();
                    let mut turn = 0;
                    for index in 0..self.shelf.len() {
                        let Some(from) = self.shelf_icon_center(index) else { continue };
                        let icon = self.shelf.items()[index].icon.clone();
                        let at = now + Duration::from_secs_f32(render::THROW_STAGGER * turn as f32);
                        self.shelf_departures.push((at, icon, from));
                        turn += 1;
                    }
                    self.shelf_closing = None;
                    self.shelf_arrival = None;
                    self.shelf.clear();
                    self.shelf_hover = None;
                    self.shelf_scroll = Spring::smooth(0.0);
                    self.retarget();
                }
            }
        }
        self.rearm_mouse_leave();
        self.invalidate()
    }

    /// After a drag out of the shelf: a moved file leaves it.
    fn after_drag_out(&mut self) {
        if self.shelf.prune() {
            self.shelf_hover = None;
            self.clamp_shelf_scroll();
            self.retarget();
        }
        self.rearm_mouse_leave();
        let _ = self.invalidate();
    }

    fn on_shelf_hover(&mut self, x: f32, y: f32) {
        let hit = if self.on_shelf_page() { self.shelf_hit(x, y) } else { None };
        if hit != self.shelf_hover {
            self.shelf_hover = hit;
            let _ = self.invalidate();
        }
    }

    /// Through the files; `delta` in wheel units, positive to the right
    /// (later files).
    fn scroll_shelf(&mut self, delta: i32) {
        self.start_animation();
        let max = self.shelf_max_scroll();
        self.shelf_scroll.target = (self.shelf_scroll.target + delta as f32 / 120.0).clamp(0.0, max);
    }

    /// Where the icon of the file at `index` is on the shelf page, if in
    /// sight.
    fn shelf_icon_center(&self, index: usize) -> Option<(f32, f32)> {
        let (count, scroll) = (self.shelf.len(), self.shelf_scroll.value);
        render::shelf_icon_center(self.pixmap.width(), self.open_scale(), count, scroll, index)
    }

    fn shelf_max_scroll(&self) -> f32 {
        render::shelf_max_scroll(self.pixmap.width(), self.open_scale(), self.shelf.len())
    }

    fn clamp_shelf_scroll(&mut self) {
        let max = self.shelf_max_scroll();
        if self.shelf_scroll.target > max {
            self.start_animation();
            self.shelf_scroll.target = max;
        }
    }

    fn on_controls(&mut self) -> Result<()> {
        self.control_state = self.controls.state();
        self.displays = self.controls.displays();
        if self.displays.len() < 2 {
            // A monitor unplugged: nothing left to set apart.
            self.close_displays();
        }
        self.sync_toggles();
        self.invalidate()
    }

    /// Opens the Displays sheet: each screen's brightness on its own slider.
    fn open_displays(&mut self) {
        self.start_animation();
        self.displays_open = true;
        self.displays_reveal.target = 100.0;
        // Levels may have changed from the monitors' own buttons.
        self.controls.send(ControlCommand::Refresh);
    }

    fn close_displays(&mut self) {
        if self.displays_open {
            self.start_animation();
            self.displays_open = false;
            self.displays_reveal.target = 0.0;
        }
        self.display_drag = None;
    }

    /// Sets one screen's brightness from its slider in the Displays sheet.
    fn apply_display(&mut self, display: usize, value: f32) {
        self.display_drag = Some((display, value));
        self.controls.send(ControlCommand::SetDisplayBrightness(display, (value * 100.0).round() as u8));
    }

    fn on_display_move(&mut self, x: f32) -> Result<()> {
        if let Some((display, _)) = self.display_drag {
            let value = render::display_value(self.pixmap.width(), self.open_scale(), self.displays.len(), x);
            self.apply_display(display, value);
        }
        self.invalidate()
    }

    fn finish_display_drag(&mut self) -> Result<()> {
        // Cleared before releasing, so the capture change is not a cancel.
        if let Some((display, value)) = self.display_drag.take()
            && let Some(level) = self.displays.get_mut(display)
        {
            // Shows what was set until the worker reports back.
            level.percent = Some((value * 100.0).round() as u8);
        }
        unsafe {
            let _ = ReleaseCapture();
        }
        self.rearm_mouse_leave();
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
        self.sync_toggles();
    }

    fn toggle_play_pause(&mut self) {
        let Some(np) = self.now_playing.as_mut() else { return };
        self.media.send(Command::TogglePlayPause);
        // Flip the icon right away; the next poll confirms the real state.
        np.playing = !np.playing;
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

    /// Whether something keeps moving with nothing else animating: the
    /// resting notch's equalizer, or the sung lyric lighting up. Drawn by the
    /// frame loop every `ambient_every` refreshes.
    fn ambient(&self) -> bool {
        let playing = self.now_playing.as_ref().is_some_and(|np| np.playing);
        let synced = matches!(&self.lyrics_state, LyricsState::Found(l) if l.synced);
        let karaoke = playing && synced && self.hovered && self.lyrics_open && self.is_open() && self.page.target == self.slot(Page::Player);
        self.equalizer_ticking || karaoke
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
        self.panel_width.target = if self.lyrics_open { EXPANDED.width } else { PLAYER_ONLY_WIDTH };
        if self.hovered {
            self.retarget();
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
        self.announce_track(track, next.as_ref());
        self.now_playing = next;
        self.invalidate()
    }

    /// Shows the Now Playing card for a new track, or refreshes the one up
    /// (the cover often arrives a moment after the title).
    fn announce_track(&mut self, track_changed: bool, next: Option<&NowPlaying>) {
        let Some(np) = next else { return };
        if !track_changed {
            if let Some((Alert::NowPlaying { track, .. }, false)) = self.alert.as_mut() {
                **track = np.clone();
            }
            return;
        }
        // Not for what was already playing when the app started (the media
        // watcher reports it within its first poll), in the mini pill, over
        // the open player, or over an app's notification.
        let starting = self.started.elapsed() < STARTUP_QUIET;
        if starting || !self.settings.alert_enabled(AlertKind::NowPlaying) || self.obstructed || self.hovered || self.showing_toast() {
            return;
        }
        let alert = Alert::NowPlaying { track: Box::new(np.clone()), since: Instant::now() };
        self.show_alert(alert, NOW_PLAYING_ALERT_MS);
    }

    /// Where along the progress bar `pos` falls; see `render::progress_at`.
    fn progress_at(&self, pos: (f32, f32), grab: bool) -> Option<f32> {
        let reveal = self.lyrics_reveal.value / 100.0;
        render::progress_at(self.pixmap.width(), self.open_scale(), self.panel_width.value, reveal, pos, grab)
    }

    /// Whether the open notch is showing a track that can be acted on.
    fn controls_ready(&self) -> bool {
        // Controls only respond once the notch is (nearly) fully open.
        self.now_playing.is_some() && self.is_open()
    }

    /// Where a press on the Control Center page lands, once it is in view.
    fn control_at(&self, x: f32, y: f32) -> Option<ControlHit> {
        let settled = self.on_controls_page() && (self.page.value - self.slot(Page::Controls)).abs() < 10.0;
        let open = self.is_open();
        if !settled || !open {
            return None;
        }
        render::control_at(self.pixmap.width(), self.open_scale(), self.displays.len() > 1, (x, y))
    }

    /// Starts dragging a Control Center slider or the progress knob if the
    /// press is on one.
    fn on_mouse_down(&mut self, x: f32, y: f32) -> Result<()> {
        if self.on_shelf_page() {
            return self.on_shelf_down(x, y);
        }
        if self.output_picker {
            self.output_pressed = self.output_row_at(x, y);
            return self.invalidate();
        }
        if self.on_controls_page() {
            // The Displays sheet takes presses while open: its sliders, and
            // anywhere off it closes it.
            if self.displays_open {
                let count = self.displays.len();
                match render::display_at(self.pixmap.width(), self.open_scale(), count, (x, y)) {
                    Some(render::DisplayHit::Slider(display, value)) => {
                        self.apply_display(display, value);
                        unsafe { SetCapture(self.hwnd) };
                    }
                    Some(render::DisplayHit::Sheet) => {}
                    None => self.close_displays(),
                }
                return self.invalidate();
            }
            match self.control_at(x, y) {
                Some(ControlHit::Displays) => {
                    self.open_displays();
                    return self.invalidate();
                }
                Some(ControlHit::Slider(slider, value)) => {
                    self.apply_slider(slider, value);
                    // Keep receiving moves and the release even off the notch.
                    unsafe { SetCapture(self.hwnd) };
                    return self.invalidate();
                }
                // Pressed in, like a button under a finger.
                Some(ControlHit::Toggle(toggle)) => {
                    self.start_animation();
                    self.toggle_press[toggle.index()].target = 88.0;
                }
                _ => {}
            }
            return Ok(());
        }
        let has_length = self.now_playing.as_ref().is_some_and(|np| np.timeline.is_some());
        if !self.controls_ready() || !has_length {
            return Ok(());
        }
        let Some(fraction) = self.progress_at((x, y), true) else { return Ok(()) };
        self.seeking = Some(fraction);
        self.start_animation();
        self.seek_grow.target = 100.0;
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
            self.apply_slider(slider, render::slider_value(self.open_scale(), y));
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
        self.start_animation();
        self.seek_grow.target = 0.0;
        unsafe {
            let _ = ReleaseCapture();
        }
        let duration = self.now_playing.as_ref().and_then(|np| np.timeline.as_ref()).map(|t| t.duration);
        if let Some(duration) = duration {
            self.seek_to(duration.mul_f32(fraction));
        }
        self.update_lyric_line(true);
        self.rearm_mouse_leave();
        self.invalidate()
    }

    fn seek_to(&mut self, position: Duration) {
        let Some(timeline) = self.now_playing.as_mut().and_then(|np| np.timeline.as_mut()) else { return };
        self.media.send(Command::Seek(position));
        // Show the new position now rather than snapping back until the
        // player reports it.
        timeline.position = position;
        timeline.updated = SystemTime::now();
    }

    /// The synced lyric line under a click, if the lyrics panel shows one.
    fn lyric_at(&self, x: f32, y: f32) -> Option<Duration> {
        let LyricsState::Found(lyrics) = &self.lyrics_state else { return None };
        if !lyrics.synced {
            return None;
        }
        let index = render::lyric_at(
            self.pixmap.width(),
            self.open_scale(),
            self.panel_width.value,
            self.lyrics_reveal.value / 100.0,
            self.lyric_scroll.value,
            lyrics.lines.len(),
            (x, y),
        )?;
        Some(lyrics.lines[index].time)
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
        // The shelf handles its own presses; nothing under it is the player's.
        if self.on_shelf_page() {
            return Ok(());
        }
        if self.on_controls_page() {
            self.release_toggles();
            // The Displays sheet handled the press (or it closed the sheet:
            // that click is not for what lies under it).
            if self.displays_open || self.displays_reveal.value > 1.0 {
                return self.invalidate();
            }
            match self.control_at(x, y) {
                Some(ControlHit::Toggle(toggle)) => self.toggle(toggle),
                Some(ControlHit::PlayPause) => self.toggle_play_pause(),
                // The Now Playing module opens the player, as on an iPhone.
                Some(ControlHit::NowPlaying) if self.now_playing.is_some() => {
                    self.start_animation();
                    let player = self.slot(Page::Player);
                    self.settle_page(player);
                }
                _ => {}
            }
            return self.invalidate();
        }
        if self.output_picker {
            let at = self.output_row_at(x, y);
            self.output_pressed = None;
            self.start_animation();
            match at {
                Some(index) => {
                    let id = self.output_list[index].id.clone();
                    // Ticked right away; the re-read list confirms it.
                    for output in &mut self.output_list {
                        output.default = output.id == id;
                    }
                    self.outputs.select(id);
                    // Let the highlight land before the picker goes.
                    self.output_highlight.target = index as f32;
                    unsafe {
                        SetTimer(Some(self.hwnd), PICKER_CLOSE_TIMER, PICKER_CLOSE_MS, None);
                    }
                }
                // A tap anywhere else just closes it.
                None => self.close_output_picker(),
            }
            return self.invalidate();
        }
        if !self.controls_ready() {
            return Ok(());
        }
        // Tapping a lyric line plays from there, as in Apple Music; the
        // lyrics scroll to it.
        let has_length = self.now_playing.as_ref().is_some_and(|np| np.timeline.is_some());
        if let Some(time) = self.lyric_at(x, y).filter(|_| has_length) {
            self.seek_to(time);
            self.update_lyric_line(false);
            return self.invalidate();
        }
        let button = render::button_at(
            self.pixmap.width(),
            self.open_scale(),
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
            Some(Button::Output) => {
                self.open_output_picker();
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
            // Redrawn below for the new size and position.
            self.hide_shadow();
            self.shadow_shape = None;

            let scale = self.open_scale();
            self.shelf.set_scale(scale);
            let mon = info.rcMonitor;
            self.monitor = mon;
            self.origin = POINT {
                x: mon.left + (mon.right - mon.left - w as i32) / 2,
                y: mon.top,
            };
        }
        self.redraw()
    }
}

/// The page (-100 shelf, 0 player, 100 Control Center) a touchpad drag that
/// let go at `raw` settles on: the next one in the drag's direction once a
/// fifth of the way there, else back.
fn page_after_drag(raw: f32, rightward: bool) -> f32 {
    let base = ((raw / 100.0).floor() * 100.0).clamp(-100.0, 100.0);
    let into = raw - base;
    let target = if rightward {
        if into > 20.0 { base + 100.0 } else { base }
    } else if into < 80.0 {
        base
    } else {
        base + 100.0
    };
    target.clamp(-100.0, 100.0)
}

/// Low/high words of `lparam` as signed client coordinates.
fn mouse_pos(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

extern "system" fn shadow_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            if let Some(Err(e)) = with_app(App::relayout) {
                eprintln!("relayout failed: {e}");
            }
            // The refresh rate may have changed with the display mode.
            with_app(|app| app.ambient_every = ambient_every());
            // The monitor's bounds changed (resolution or scale settling at
            // login): what fills or covers it must be judged again.
            with_app(App::update_obstruction);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lparam);
            // A file pressed on the shelf and moved: drag it out. The drag
            // runs a loop of its own, so the app state must be free.
            if let Some(path) = with_app(|app| app.shelf_drag_start(x, y)).flatten() {
                unsafe {
                    let _ = ReleaseCapture();
                }
                DRAGGING_OUT.store(true, Ordering::Relaxed);
                if let Err(e) = shelf::drag_out(hwnd, &path) {
                    eprintln!("drag out failed: {e}");
                }
                DRAGGING_OUT.store(false, Ordering::Relaxed);
                with_app(App::after_drag_out);
                return LRESULT(0);
            }
            let result = with_app(|app| {
                if app.seeking.is_some() {
                    app.on_seek_move(x, y)
                } else if app.slider_drag.is_some() {
                    app.on_slider_move(y)
                } else if app.display_drag.is_some() {
                    app.on_display_move(x)
                } else if app.shelf_pressed.is_some() {
                    Ok(())
                } else {
                    app.set_hovered(true);
                    app.on_picker_hover(x, y);
                    app.on_shelf_hover(x, y);
                    Ok(())
                }
            });
            if let Some(Err(e)) = result {
                eprintln!("seek redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            // Windows reports a leave whenever tracking breaks, including
            // when another program held the mouse (a drag just dropped here):
            // still over the notch, keep watching instead of closing it.
            let mut cursor = POINT::default();
            let over = unsafe { GetCursorPos(&mut cursor).is_ok() && WindowFromPoint(cursor) == hwnd };
            with_app(|app| if over { app.rearm_mouse_leave() } else { app.set_hovered(false) });
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
                if app.shelf_pressed.is_some() {
                    app.finish_shelf_press(x, y)
                } else if app.seeking.is_some() {
                    app.finish_seek()
                } else if app.slider_drag.is_some() {
                    app.finish_slider()
                } else if app.display_drag.is_some() {
                    app.finish_display_drag()
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
                if app.shelf_pressed.take().is_some() {
                    return app.invalidate();
                }
                let slider = app.slider_drag.take().is_some() | app.display_drag.take().is_some();
                if app.seeking.take().is_some() || slider {
                    app.start_animation();
                    app.seek_grow.target = 0.0;
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
        WM_MOUSEHWHEEL => {
            // High word of wparam: signed sideways travel, positive to the right.
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            with_app(|app| app.on_sideways_wheel(delta));
            LRESULT(0)
        }
        WM_PLUGIN => {
            if let Some(Err(e)) = with_app(App::on_plugin) {
                eprintln!("plugin alert failed: {e}");
            }
            LRESULT(0)
        }
        WM_SHELF => {
            if let Some(Err(e)) = with_app(|app| {
                app.shelf.take_icons();
                app.invalidate()
            }) {
                eprintln!("shelf redraw failed: {e}");
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
        WM_VOLUME_KEY => {
            if let Some(key) = VolumeKey::from_wparam(wparam) {
                with_app(|app| app.on_volume_key(key));
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
        WM_TIMER if wparam.0 == PICKER_CLOSE_TIMER => {
            with_app(|app| {
                app.close_output_picker();
                app.invalidate()
            });
            LRESULT(0)
        }
        WM_OUTPUTS => {
            if let Some(Err(e)) = with_app(App::on_outputs) {
                eprintln!("outputs redraw failed: {e}");
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == SCROLL_END_TIMER => {
            with_app(App::on_scroll_end);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == RECHECK_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), RECHECK_TIMER);
            }
            with_app(App::update_obstruction);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == HIDDEN_RECHECK_TIMER => {
            with_app(App::update_obstruction);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TRIM_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TRIM_TIMER);
            }
            let idle = with_app(|app| !app.hovered && !app.is_animating() && app.alert.is_none()).unwrap_or(false);
            // The settings window trims when it closes.
            if idle && !settings_window::is_open() {
                trim_working_set();
                unsafe {
                    SetTimer(Some(hwnd), TRIM_TIMER, TRIM_REPEAT_MS, None);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == CLOCK_TIMER => {
            schedule_clock(hwnd);
            update_clock();
            LRESULT(0)
        }
        // The clock was set, or the machine woke from sleep: the time may
        // have jumped.
        WM_TIMECHANGE => {
            schedule_clock(hwnd);
            update_clock();
            LRESULT(0)
        }
        WM_POWERBROADCAST if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC => {
            schedule_clock(hwnd);
            update_clock();
            LRESULT(1)
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
        WM_SETTINGS_CHANGED => {
            let next = Settings::from_bits(wparam.0);
            if let Some(Err(e)) = with_app(|app| app.apply_settings(next)) {
                eprintln!("applying settings failed: {e}");
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            settings_window::close();
            shelf::revoke_drop_target(hwnd);
            // Hand the volume keys back to Windows.
            volume_keys::uninstall();
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
