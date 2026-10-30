//! The settings window, opened from the tray menu. Its content is drawn by
//! `settings_ui` (Apple-style panes, switches and segmented controls) and
//! shown with `SetDIBitsToDevice`; this module is the Win32 side: the
//! window, input, focus, animation, theme and DPI. Changes apply at once,
//! as in Apple's settings; there is no OK button.
//!
//! Animation frames come from the app's message loop, paced by `DwmFlush`
//! like the notch's own (see `is_animating` and `frame`): a 16 ms timer
//! really fires every 11 to 35 ms, which shows as stutter.

use std::cell::RefCell;
use std::time::Instant;

use tiny_skia::Pixmap;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, GetDC, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, ReleaseDC,
    SetDIBitsToDevice, ValidateRect, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::anim::Spring;
use crate::autostart;
use crate::battery::{self, Battery};
use crate::render::Fonts;
use crate::settings::{
    bluetooth_quiet_label, AlertDuration, AlertKind, NotchWidth, OpenSize, Page, Settings, BLUETOOTH_QUIET_MINUTES,
};
use crate::settings_ui::{
    self, focus_order, segments, switch_index, Control, Hit, Pane, PreviewCache, Theme, View, SWITCHES,
};

/// Posted to the notch window when a setting changes; `wparam` is
/// `Settings::to_bits`.
pub const WM_SETTINGS_CHANGED: u32 = WM_APP + 11;

const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);
const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(0);
/// The app icon `build.rs` embeds.
const ICON_ID: u16 = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Frames are integrated at most this long, so a stall does not jump.
const MAX_DT: f32 = 1.0 / 30.0;

struct Ui {
    hwnd: HWND,
    /// Where changes are reported.
    notch: HWND,
    scale: f32,
    /// Drawn as RGBA, then swizzled in place to the BGRA that GDI shows.
    pixmap: Pixmap,
    preview: PreviewCache,
    fonts: Fonts,
    theme: Theme,
    pane: Pane,
    settings: Settings,
    battery: Option<Battery>,
    /// Per switch (`settings_ui::switch_index`), how far on, 0..100.
    switches: [Spring; SWITCHES],
    /// Segment selections, in segment index x 100.
    width_selection: Spring,
    open_size_selection: Spring,
    duration_selection: Spring,
    /// The preview notch's width, logical px.
    preview_width: Spring,
    /// Per page (`Page as usize`), its place on the Layout stage, x 100.
    page_rows: [Spring; 3],
    /// A page's card pressed on the Layout stage, being (or about to be)
    /// dragged into another place.
    page_drag: Option<PageDrag>,
    focus: Control,
    /// The focus ring shows after keyboard use, as in Windows and macOS.
    focus_visible: bool,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tracking_leave: bool,
    animating: bool,
    last_tick: Instant,
}

/// A press on a page's card in the Layout pane.
#[derive(Clone, Copy)]
struct PageDrag {
    page: Page,
    /// Pointer x (logical) when pressed, and the card's left edge then.
    from_x: f32,
    card_x: f32,
    /// Moved far enough to count as a drag, not a click.
    moving: bool,
    /// The order it would make if let go now.
    order: [Page; 3],
}

/// Pointer travel (logical px) that turns a press on a card into a drag.
const DRAG_START: f32 = 3.0;

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Runs `f` on the window state; `None` if the window is closed or the
/// state is busy (a re-entrant call from inside a modal loop).
fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| {
        let mut slot = cell.try_borrow_mut().ok()?;
        slot.as_mut().map(f)
    })
}

/// Opens the window, or brings it forward if it is open.
pub fn open(notch: HWND) {
    if let Some(hwnd) = with_ui(|ui| ui.hwnd) {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    if let Err(e) = create(notch) {
        eprintln!("settings window failed: {e}");
    }
}

/// Closes the window if it is open (the app is exiting).
pub fn close() {
    if let Some(hwnd) = with_ui(|ui| ui.hwnd) {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn create(notch: HWND) -> windows::core::Result<()> {
    let Some(fonts) = Fonts::load() else {
        return Err(windows::core::Error::from_win32());
    };
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("NotchWoySettings");
        // MAKEINTRESOURCE(ICON_ID).
        let icon_name = PCWSTR(ICON_ID as usize as *const u16);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hIcon: LoadIconW(Some(instance), icon_name).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        // Fails harmlessly when already registered by an earlier opening.
        RegisterClassExW(&wc);

        // Placed on the monitor under the pointer (where the tray menu was);
        // sized and centered once its DPI is known.
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let hwnd = CreateWindowExW(
            EX_STYLE,
            class,
            w!("notch-woy Settings"),
            STYLE,
            cursor.x,
            cursor.y,
            100,
            100,
            None,
            None,
            Some(instance),
            None,
        )?;
        let dpi = GetDpiForWindow(hwnd);
        let settings = Settings::load();
        let battery = battery::read();
        let autostart = autostart::is_enabled();
        let mut switches = [Spring::smooth(0.0); SWITCHES];
        for kind in AlertKind::ALL {
            if let Some(i) = switch_index(Control::Alert(kind)) {
                switches[i] = Spring::smooth(if settings.alert_enabled(kind) { 100.0 } else { 0.0 });
            }
        }
        switches[AlertKind::ALL.len()] = Spring::smooth(if autostart { 100.0 } else { 0.0 });
        if let Some(i) = switch_index(Control::Plugins) {
            switches[i] = Spring::smooth(if settings.plugins { 100.0 } else { 0.0 });
        }
        let theme = system_theme();
        let ui = Ui {
            hwnd,
            notch,
            scale: dpi as f32 / 96.0,
            pixmap: Pixmap::new(1, 1).expect("1x1 pixmap"),
            preview: PreviewCache::default(),
            fonts,
            theme,
            pane: Pane::General,
            settings,
            battery,
            switches,
            width_selection: Spring::smooth(width_index(settings) as f32 * 100.0),
            open_size_selection: Spring::smooth(open_size_index(settings) as f32 * 100.0),
            duration_selection: Spring::smooth(duration_index(settings) as f32 * 100.0),
            preview_width: Spring::new(settings_ui::preview_width(settings, battery.is_some())),
            page_rows: page_places(settings.page_order).map(|place| Spring::smooth(place * 100.0)),
            page_drag: None,
            focus: Control::Sidebar,
            focus_visible: false,
            hover: None,
            pressed: None,
            tracking_leave: false,
            animating: false,
            last_tick: Instant::now(),
        };
        UI.with_borrow_mut(|slot| *slot = Some(ui));
        apply_title_bar(hwnd, theme);
        center(hwnd, dpi, cursor);
        // Not ShowWindow: its first call in a process takes the launcher's
        // show state instead (hidden or minimized when the app was started
        // that way), and this window would never appear.
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(())
    }
}

fn width_index(settings: Settings) -> usize {
    NotchWidth::ALL.iter().position(|&w| w == settings.width).unwrap_or(0)
}

/// Per page (`Page as usize`), its place (0 = left) in `order`.
fn page_places(order: [Page; 3]) -> [f32; 3] {
    Page::ALL.map(|page| order.iter().position(|&p| p == page).unwrap_or(0) as f32)
}

fn open_size_index(settings: Settings) -> usize {
    OpenSize::ALL.iter().position(|&s| s == settings.open_size).unwrap_or(0)
}

fn duration_index(settings: Settings) -> usize {
    AlertDuration::ALL.iter().position(|&d| d == settings.duration).unwrap_or(0)
}

/// Light or dark, following the apps setting in Windows.
fn system_theme() -> Theme {
    let mut light = 1u32;
    let mut size = size_of::<u32>() as u32;
    let got = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut light as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if got.is_ok() && light == 0 { Theme::Dark } else { Theme::Light }
}

/// Dark or light title bar, colored like the window so they read as one
/// surface (the color needs Windows 11; older versions keep their own).
fn apply_title_bar(hwnd: HWND, theme: Theme) {
    let dark = BOOL::from(theme == Theme::Dark);
    let [r, g, b] = settings_ui::window_color(theme);
    let caption = COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16);
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const BOOL).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&caption as *const COLORREF).cast(), 4);
    }
}

fn scaled(v: f32, dpi: u32) -> i32 {
    (v * dpi as f32 / 96.0).round() as i32
}

/// Window size for the content at `dpi`.
fn window_size(dpi: u32) -> (i32, i32) {
    let mut rect = RECT { left: 0, top: 0, right: scaled(settings_ui::WIDTH, dpi), bottom: scaled(settings_ui::HEIGHT, dpi) };
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rect, STYLE, false, EX_STYLE, dpi);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Sizes the window and centers it in the work area of the monitor at `at`.
fn center(hwnd: HWND, dpi: u32, at: POINT) {
    let (w, h) = window_size(dpi);
    unsafe {
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() { info.rcWork } else { RECT::default() };
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + ((work.bottom - work.top - h) / 2).max(0);
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Steps that run a modal loop of their own or destroy the window, so they
/// run with the state released.
enum Modal {
    BluetoothMenu,
    /// The pages, to pick the one the notch opens on.
    StartPageMenu,
    ConfirmReset,
    Close,
}

impl Ui {
    fn view(&self) -> View {
        View {
            theme: self.theme,
            pane: self.pane,
            settings: self.settings,
            switches: self.switches.map(|s| s.value / 100.0),
            width_selection: self.width_selection.value / 100.0,
            open_size_selection: self.open_size_selection.value / 100.0,
            duration_selection: self.duration_selection.value / 100.0,
            preview_width: self.preview_width.value,
            page_rows: self.page_rows.each_ref().map(|s| s.value / 100.0),
            page_dragging: self.page_drag.filter(|d| d.moving).map(|d| d.page),
            battery: self.battery,
            time: crate::window::clock_text(),
            focus: self.focus_visible.then_some(self.focus),
            hover: self.hover,
            pressed: self.pressed,
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn springs(&mut self) -> impl Iterator<Item = &mut Spring> {
        self.switches
            .iter_mut()
            .chain([
                &mut self.width_selection,
                &mut self.open_size_selection,
                &mut self.duration_selection,
                &mut self.preview_width,
            ])
            .chain(self.page_rows.iter_mut())
    }

    /// Has the message loop run frames while a spring has somewhere to go.
    fn animate(&mut self) {
        if !self.animating {
            self.animating = true;
            self.last_tick = Instant::now();
        }
        self.invalidate();
    }

    /// Steps the springs and shows the result at once, rather than through
    /// WM_PAINT, which waits for an empty queue.
    fn frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(MAX_DT);
        self.last_tick = now;
        let mut settled = true;
        for spring in self.springs() {
            spring.update(dt);
            settled &= spring.is_settled();
        }
        self.animating = !settled;
        self.render();
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            self.blit(hdc);
            ReleaseDC(Some(self.hwnd), hdc);
            // Already drawn; no WM_PAINT needed for it.
            let _ = ValidateRect(Some(self.hwnd), None);
        }
    }

    /// Saves the settings, has the notch take them up, and moves the
    /// controls to them.
    fn commit(&mut self, settings: Settings) {
        self.settings = settings;
        settings.save();
        unsafe {
            let _ = PostMessageW(Some(self.notch), WM_SETTINGS_CHANGED, WPARAM(settings.to_bits()), LPARAM(0));
        }
        for kind in AlertKind::ALL {
            if let Some(i) = switch_index(Control::Alert(kind)) {
                self.switches[i].target = if settings.alert_enabled(kind) { 100.0 } else { 0.0 };
            }
        }
        if let Some(i) = switch_index(Control::Plugins) {
            self.switches[i].target = if settings.plugins { 100.0 } else { 0.0 };
        }
        self.width_selection.target = width_index(settings) as f32 * 100.0;
        self.open_size_selection.target = open_size_index(settings) as f32 * 100.0;
        self.duration_selection.target = duration_index(settings) as f32 * 100.0;
        self.preview_width.target = settings_ui::preview_width(settings, self.battery.is_some());
        for (row, place) in self.page_rows.iter_mut().zip(page_places(settings.page_order)) {
            row.target = place * 100.0;
        }
        self.animate();
    }

    fn sync_autostart(&mut self) {
        let on = autostart::is_enabled();
        let i = AlertKind::ALL.len();
        let target = if on { 100.0 } else { 0.0 };
        if self.switches[i].target != target {
            self.switches[i].target = target;
            self.animate();
        }
    }

    fn set_pane(&mut self, pane: Pane) {
        if self.pane != pane {
            self.pane = pane;
            self.hover = None;
            self.invalidate();
        }
    }

    /// Acts on a click or key press on `hit`; returns what needs a modal
    /// loop, to be run once the state is released.
    fn activate(&mut self, hit: Hit) -> Option<Modal> {
        match hit {
            Hit::Pane(pane) => {
                self.set_pane(pane);
                self.focus = Control::Sidebar;
            }
            Hit::Control(Control::Alert(kind)) => {
                let mut next = self.settings;
                next.set_alert(kind, !next.alert_enabled(kind));
                self.focus = Control::Alert(kind);
                self.commit(next);
            }
            Hit::Control(Control::Plugins) => {
                let mut next = self.settings;
                next.plugins = !next.plugins;
                self.focus = Control::Plugins;
                self.commit(next);
            }
            Hit::Control(Control::PluginsFolder) => {
                self.focus = Control::PluginsFolder;
                crate::plugins::open_folder();
            }
            Hit::Control(Control::Autostart) => {
                self.focus = Control::Autostart;
                if let Err(e) = autostart::set_enabled(!autostart::is_enabled()) {
                    eprintln!("autostart change failed: {e}");
                }
                // Shows what actually took effect.
                self.sync_autostart();
            }
            Hit::Segment(control, i) => {
                let mut next = self.settings;
                match control {
                    Control::Width => next.width = NotchWidth::ALL[i.min(NotchWidth::ALL.len() - 1)],
                    Control::OpenSize => next.open_size = OpenSize::ALL[i.min(OpenSize::ALL.len() - 1)],
                    Control::Duration => next.duration = AlertDuration::ALL[i.min(AlertDuration::ALL.len() - 1)],
                    _ => return None,
                }
                self.focus = control;
                if next != self.settings {
                    self.commit(next);
                }
            }
            Hit::Control(Control::BluetoothQuiet) => {
                self.focus = Control::BluetoothQuiet;
                return Some(Modal::BluetoothMenu);
            }
            Hit::Control(Control::StartPage) => {
                self.focus = Control::StartPage;
                return Some(Modal::StartPageMenu);
            }
            // A card clicked (not dragged: see `on_page_drag`): the notch
            // opens on its page.
            Hit::Control(Control::PageRow(i)) => {
                self.focus = Control::PageRow(i);
                let mut next = self.settings;
                next.start_page = next.page_order[i.min(2)];
                if next != self.settings {
                    self.commit(next);
                }
            }
            Hit::Control(Control::Reset) => {
                self.focus = Control::Reset;
                return Some(Modal::ConfirmReset);
            }
            Hit::Control(Control::Sidebar | Control::Width | Control::OpenSize | Control::Duration) => {}
        }
        self.invalidate();
        None
    }

    /// A key press; returns what needs a modal loop.
    fn on_key(&mut self, key: u16) -> Option<Modal> {
        let key = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(key);
        self.focus_visible = true;
        let order = focus_order(self.pane);
        let at = order.iter().position(|&c| c == self.focus).unwrap_or(0);
        match key {
            VK_TAB => {
                let back = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let n = order.len();
                self.focus = order[if back { (at + n - 1) % n } else { (at + 1) % n }];
            }
            VK_ESCAPE => return Some(Modal::Close),
            VK_UP | VK_DOWN if self.focus == Control::Sidebar => {
                let i = Pane::ALL.iter().position(|&p| p == self.pane).unwrap_or(0) as i32;
                let next = (i + if key == VK_UP { -1 } else { 1 }).clamp(0, Pane::ALL.len() as i32 - 1);
                self.set_pane(Pane::ALL[next as usize]);
            }
            VK_LEFT | VK_RIGHT if matches!(self.focus, Control::Width | Control::OpenSize | Control::Duration) => {
                let current = match self.focus {
                    Control::Width => width_index(self.settings),
                    Control::OpenSize => open_size_index(self.settings),
                    _ => duration_index(self.settings),
                } as i32;
                let next = (current + if key == VK_LEFT { -1 } else { 1 }).clamp(0, segments(self.focus) as i32 - 1);
                return self.activate(Hit::Segment(self.focus, next as usize));
            }
            VK_SPACE | VK_RETURN => match self.focus {
                Control::Sidebar => {}
                Control::Width | Control::OpenSize | Control::Duration => {}
                control => return self.activate(Hit::Control(control)),
            },
            VK_DOWN if self.focus == Control::BluetoothQuiet => return Some(Modal::BluetoothMenu),
            VK_DOWN if self.focus == Control::StartPage => return Some(Modal::StartPageMenu),
            // A focused page's card moves left or right.
            VK_LEFT | VK_RIGHT if matches!(self.focus, Control::PageRow(_)) => {
                let Control::PageRow(i) = self.focus else { unreachable!() };
                let to = if key == VK_LEFT { i.checked_sub(1) } else { Some(i + 1).filter(|&j| j < 3) };
                if let Some(to) = to {
                    let mut next = self.settings;
                    next.page_order.swap(i, to);
                    self.focus = Control::PageRow(to);
                    self.commit(next);
                }
            }
            _ => {}
        }
        self.invalidate();
        None
    }

    /// The pointer moved with a page's card pressed: once past
    /// `DRAG_START` the card follows it, and the others slide aside to make
    /// room where it would land.
    fn on_page_drag(&mut self, x: f32) {
        let Some(mut drag) = self.page_drag else { return };
        if !drag.moving && (x - drag.from_x).abs() < DRAG_START {
            return;
        }
        drag.moving = true;
        // Where the card is, in places, held a little within the stage.
        let first = settings_ui::page_card(0.0).x;
        let place = ((drag.card_x + x - drag.from_x - first) / settings_ui::PAGE_PITCH).clamp(-0.35, 2.35);
        drag.order = settings_ui::reorder(self.settings.page_order, drag.page, place);
        for (page, target) in Page::ALL.into_iter().zip(page_places(drag.order)) {
            let row = &mut self.page_rows[page as usize];
            if page == drag.page {
                // The dragged row stays under the pointer.
                *row = Spring::smooth(place * 100.0);
            } else {
                row.target = target * 100.0;
            }
        }
        self.page_drag = Some(drag);
        self.animate();
        self.invalidate();
    }

    /// Lets go of a page row: a drag puts the page where it was dropped.
    fn finish_page_drag(&mut self) {
        let Some(drag) = self.page_drag.take() else { return };
        if drag.moving {
            let mut next = self.settings;
            next.page_order = drag.order;
            self.focus = Control::PageRow(drag.order.iter().position(|&p| p == drag.page).unwrap_or(0));
            if next != self.settings {
                self.commit(next);
            } else {
                // Back where it was: settle into place.
                for (row, place) in self.page_rows.iter_mut().zip(page_places(self.settings.page_order)) {
                    row.target = place * 100.0;
                }
                self.animate();
            }
        }
        self.invalidate();
    }

    fn logical(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xFFFF) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
        (x / self.scale, y / self.scale)
    }

    /// Draws the window into the pixmap, sized to the client area, and
    /// swizzles it to BGRA for `blit`.
    fn render(&mut self) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        let (w, h) = ((rect.right - rect.left).max(1) as u32, (rect.bottom - rect.top).max(1) as u32);
        if (self.pixmap.width() != w || self.pixmap.height() != h)
            && let Some(pixmap) = Pixmap::new(w, h)
        {
            self.pixmap = pixmap;
        }
        let view = self.view();
        settings_ui::draw(&mut self.pixmap, self.scale, &self.fonts, &view, &mut self.preview);
        // tiny-skia is RGBA, GDI wants BGRA. Swapped in place: every frame
        // redraws every pixel, and a second buffer would double the memory.
        for px in self.pixmap.data_mut().as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }

    fn blit(&self, hdc: HDC) {
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // Negative height makes the DIB top-down, matching tiny-skia.
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            SetDIBitsToDevice(hdc, 0, 0, w, h, 0, 0, 0, h, self.pixmap.data().as_ptr().cast(), &info, DIB_RGB_COLORS);
        }
    }

    fn paint(&mut self) {
        self.render();
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            self.blit(hdc);
            let _ = EndPaint(self.hwnd, &ps);
        }
    }
}

/// Whether the window has animation frames to show; the message loop then
/// calls `frame` once per display refresh.
pub fn is_animating() -> bool {
    with_ui(|ui| ui.animating).unwrap_or(false)
}

pub fn is_open() -> bool {
    with_ui(|_| ()).is_some()
}

/// Shows the next animation frame.
pub fn frame() {
    with_ui(Ui::frame);
}

/// Runs a modal step with the state released, then applies its result.
fn run_modal(hwnd: HWND, modal: Modal) {
    match modal {
        Modal::BluetoothMenu => {
            let Some((settings, anchor)) = with_ui(|ui| {
                let rect = settings_ui::control_rect(ui.pane, Control::BluetoothQuiet);
                let anchor = rect.map(|r| POINT { x: (r.x * ui.scale) as i32, y: ((r.y + r.h + 4.0) * ui.scale) as i32 });
                (ui.settings, anchor.unwrap_or_default())
            }) else {
                return;
            };
            let Some(minutes) = bluetooth_menu(hwnd, settings, anchor) else { return };
            with_ui(|ui| {
                let mut next = ui.settings;
                next.bluetooth_quiet_minutes = minutes;
                if next != ui.settings {
                    ui.commit(next);
                }
                ui.invalidate();
            });
        }
        Modal::StartPageMenu => {
            let Some((settings, anchor)) = with_ui(|ui| {
                let rect = settings_ui::control_rect(ui.pane, Control::StartPage);
                let anchor = rect.map(|r| POINT { x: (r.x * ui.scale) as i32, y: ((r.y + r.h + 4.0) * ui.scale) as i32 });
                (ui.settings, anchor.unwrap_or_default())
            }) else {
                return;
            };
            let Some(page) = start_page_menu(hwnd, settings, anchor) else { return };
            with_ui(|ui| {
                let mut next = ui.settings;
                next.start_page = page;
                if next != ui.settings {
                    ui.commit(next);
                }
                ui.invalidate();
            });
        }
        Modal::Close => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        Modal::ConfirmReset => {
            let answer = unsafe {
                MessageBoxW(
                    Some(hwnd),
                    w!("Reset all notch-woy settings to their defaults?"),
                    w!("notch-woy"),
                    MB_OKCANCEL | MB_ICONQUESTION,
                )
            };
            if answer == IDOK {
                with_ui(|ui| ui.commit(Settings::default()));
            }
        }
    }
}

/// The pages, in their order, as a menu under the Opens To pop-up at
/// `anchor` (client px); the chosen page, if any.
fn start_page_menu(hwnd: HWND, settings: Settings, mut anchor: POINT) -> Option<Page> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (i, page) in settings.page_order.into_iter().enumerate() {
            let checked = if page == settings.start_page { MF_CHECKED } else { MF_UNCHECKED };
            let label: Vec<u16> = page.label().encode_utf16().chain(Some(0)).collect();
            let _ = AppendMenuW(menu, MF_STRING | checked, i + 1, PCWSTR(label.as_ptr()));
        }
        let _ = ClientToScreen(hwnd, &mut anchor);
        let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, anchor.x, anchor.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        let index = (choice.0 as usize).checked_sub(1)?;
        settings.page_order.get(index).copied()
    }
}

/// The Bluetooth quiet-time choices as a menu under the pop-up button at
/// `anchor` (client px); the chosen minutes, if any.
fn bluetooth_menu(hwnd: HWND, settings: Settings, mut anchor: POINT) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (i, minutes) in BLUETOOTH_QUIET_MINUTES.into_iter().enumerate() {
            let checked = if minutes == settings.bluetooth_quiet_minutes { MF_CHECKED } else { MF_UNCHECKED };
            let label: Vec<u16> = bluetooth_quiet_label(minutes).encode_utf16().chain(Some(0)).collect();
            let _ = AppendMenuW(menu, MF_STRING | checked, i + 1, PCWSTR(label.as_ptr()));
        }
        let _ = ClientToScreen(hwnd, &mut anchor);
        let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, anchor.x, anchor.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        let index = (choice.0 as usize).checked_sub(1)?;
        BLUETOOTH_QUIET_MINUTES.get(index).copied()
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            if with_ui(Ui::paint).is_none() {
                // Not ours to draw (yet): validate so it is not asked again.
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut ps);
                    let _ = EndPaint(hwnd, &ps);
                }
            }
            LRESULT(0)
        }
        // Everything is painted; erasing first would flicker.
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            with_ui(|ui| {
                if !ui.tracking_leave {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    ui.tracking_leave = unsafe { TrackMouseEvent(&mut track) }.is_ok();
                }
                if ui.page_drag.is_some() {
                    ui.on_page_drag(ui.logical(lparam).0);
                    return;
                }
                let hover = settings_ui::hit(ui.pane, ui.logical(lparam));
                if hover != ui.hover {
                    ui.hover = hover;
                    ui.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_ui(|ui| {
                ui.tracking_leave = false;
                if ui.hover.take().is_some() {
                    ui.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            with_ui(|ui| {
                ui.focus_visible = false;
                let (x, _) = ui.logical(lparam);
                ui.pressed = settings_ui::hit(ui.pane, ui.logical(lparam));
                if let Some(Hit::Control(Control::PageRow(i))) = ui.pressed {
                    let card_x = settings_ui::page_card(i as f32).x;
                    let page = ui.settings.page_order[i];
                    ui.page_drag = Some(PageDrag { page, from_x: x, card_x, moving: false, order: ui.settings.page_order });
                }
                unsafe {
                    SetCapture(hwnd);
                }
                ui.invalidate();
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let modal = with_ui(|ui| {
                unsafe {
                    let _ = ReleaseCapture();
                }
                let pressed = ui.pressed.take();
                if ui.page_drag.is_some_and(|d| d.moving) {
                    ui.finish_page_drag();
                    return None;
                }
                ui.page_drag = None;
                let released = settings_ui::hit(ui.pane, ui.logical(lparam));
                ui.invalidate();
                // Only a click that starts and ends on the same thing counts.
                pressed.filter(|&p| Some(p) == released).and_then(|hit| ui.activate(hit))
            })
            .flatten();
            if let Some(modal) = modal {
                run_modal(hwnd, modal);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(Some(modal)) = with_ui(|ui| ui.on_key(wparam.0 as u16)) {
                run_modal(hwnd, modal);
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Start at login may have been changed from the tray menu.
            if wparam.0 & 0xFFFF != WA_INACTIVE as usize {
                with_ui(Ui::sync_autostart);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SETTINGCHANGE => {
            // Light or dark may have been switched in Windows.
            with_ui(|ui| {
                let theme = system_theme();
                if theme != ui.theme {
                    ui.theme = theme;
                    apply_title_bar(hwnd, theme);
                    ui.invalidate();
                }
            });
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xFFFF) as u32;
            with_ui(|ui| ui.scale = dpi as f32 / 96.0);
            // Windows suggests where the window goes at the new DPI.
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let (w, h) = window_size(dpi);
            unsafe {
                let _ = SetWindowPos(hwnd, None, suggested.left, suggested.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            with_ui(|ui| ui.invalidate());
            LRESULT(0)
        }
        WM_DESTROY => {
            // Destroyed from outside any state borrow (see `Modal::Close`),
            // so this cannot fail; checked anyway rather than panic.
            UI.with(|cell| {
                if let Ok(mut slot) = cell.try_borrow_mut() {
                    *slot = None;
                }
            });
            // Its canvas, fonts and glyphs are freed; hand the pages back.
            crate::window::trim_working_set();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
