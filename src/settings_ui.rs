//! Draws the settings window in the style of macOS System Settings: an
//! inset sidebar of panes, and grouped rows of switches, segmented
//! controls and pop-up buttons, in light or dark. Layout and hit testing
//! live here too, so the window (`settings_window`) only routes input.
//! All layout is in logical (96 DPI) pixels, scaled by `scale`.

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path, PathBuilder,
    Pattern, Pixmap, Point, SpreadMode, Stroke, Transform,
};

use crate::battery::Battery;
use crate::font::Font;
use crate::palette::Rgb;
use crate::render::{
    self, rounded_rect, ControlsView, Fonts, LyricsView, OutputsView, Scene, Shape, ShelfView, COLLAPSED,
    COLLAPSED_BATTERY_WIDTH,
    EXPANDED,
};
use crate::settings::{bluetooth_quiet_label, AlertDuration, AlertKind, NotchWidth, Settings};

pub const WIDTH: f32 = 700.0;
pub const HEIGHT: f32 = 420.0;

const SIDEBAR_INSET: f32 = 8.0;
const SIDEBAR_W: f32 = 200.0;
const SIDEBAR_ITEM_X: f32 = SIDEBAR_INSET + 8.0;
const SIDEBAR_ITEM_W: f32 = SIDEBAR_W - 16.0;
const SIDEBAR_ITEMS_Y: f32 = 76.0;
const SIDEBAR_ITEM_H: f32 = 32.0;
const CONTENT_X: f32 = SIDEBAR_INSET + SIDEBAR_W + 28.0;
const CONTENT_W: f32 = WIDTH - CONTENT_X - 28.0;
const TITLE_BASELINE: f32 = 44.0;
const BODY_Y: f32 = 64.0;
const PREVIEW_H: f32 = 116.0;
const CARD_RADIUS: f32 = 10.0;
const ROW_PAD: f32 = 14.0;
const ROW_H: f32 = 48.0;
const ICON: f32 = 26.0;
const SWITCH_W: f32 = 40.0;
const SWITCH_H: f32 = 24.0;
const SEGMENT_H: f32 = 26.0;
const SEGMENT_W: f32 = 62.0;
const POPUP_W: f32 = 176.0;
const RESET_W: f32 = 156.0;
const BUTTON_H: f32 = 28.0;

/// Where the notch preview and the cards below it start.
const CARD_Y: f32 = BODY_Y + PREVIEW_H + 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    General,
    Alerts,
    Notch,
}

impl Pane {
    pub const ALL: [Self; 3] = [Self::General, Self::Alerts, Self::Notch];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Alerts => "Alerts",
            Self::Notch => "Notch",
        }
    }
}

/// Everything that can be clicked or focused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    Sidebar,
    Alert(AlertKind),
    Autostart,
    Width,
    Duration,
    BluetoothQuiet,
    Reset,
}

/// A point on a control: which segment, for segmented controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Pane(Pane),
    Control(Control),
    Segment(Control, usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}

struct Colors {
    window: Rgb,
    sidebar: Rgb,
    card: Rgb,
    label: Rgb,
    secondary: Rgb,
    separator: Rgb,
    accent: Rgb,
    green: Rgb,
    switch_off: Rgb,
    segment_bg: Rgb,
    segment_selected: Rgb,
    button: Rgb,
    button_border: Rgb,
    wallpaper: [Rgb; 2],
}

/// Apple's system colors for each appearance.
fn colors(theme: Theme) -> Colors {
    match theme {
        Theme::Light => Colors {
            window: [242, 242, 247],
            sidebar: [230, 230, 235],
            card: [255, 255, 255],
            label: [29, 29, 31],
            secondary: [134, 134, 139],
            separator: [229, 229, 234],
            accent: [0, 122, 255],
            green: [52, 199, 89],
            switch_off: [226, 226, 231],
            segment_bg: [229, 229, 234],
            segment_selected: [255, 255, 255],
            button: [255, 255, 255],
            button_border: [209, 209, 214],
            wallpaper: [[94, 154, 255], [182, 120, 250]],
        },
        Theme::Dark => Colors {
            window: [30, 30, 32],
            sidebar: [42, 42, 45],
            card: [44, 44, 46],
            label: [245, 245, 247],
            secondary: [152, 152, 157],
            separator: [58, 58, 60],
            accent: [10, 132, 255],
            green: [48, 209, 88],
            switch_off: [72, 72, 76],
            segment_bg: [58, 58, 61],
            segment_selected: [99, 99, 102],
            button: [72, 72, 76],
            button_border: [88, 88, 92],
            wallpaper: [[30, 64, 150], [92, 36, 140]],
        },
    }
}

const WHITE: Rgb = [255, 255, 255];
const RED: Rgb = [255, 59, 48];
const PINK: Rgb = [255, 45, 85];
const ORANGE: Rgb = [255, 149, 0];
const PURPLE: Rgb = [175, 82, 222];
const BLUE: Rgb = [0, 122, 255];
const GRAY: Rgb = [142, 142, 147];
const GREEN: Rgb = [52, 199, 89];
const INDIGO: Rgb = [88, 86, 214];

/// What the window shows, animated values included.
pub struct View {
    pub theme: Theme,
    pub pane: Pane,
    pub settings: Settings,
    /// How far each switch is on, 0..1: the alerts in `AlertKind::ALL`
    /// order, then Start at login.
    pub switches: [f32; 6],
    /// The selection of the width and duration controls, as a fractional
    /// segment index while it slides.
    pub width_selection: f32,
    pub duration_selection: f32,
    /// The preview notch's resting width.
    pub preview_width: f32,
    pub battery: Option<Battery>,
    /// The time on the preview notch's clock.
    pub time: String,
    /// The control with keyboard focus, when its ring should show.
    pub focus: Option<Control>,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, (x, y): (f32, f32)) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Index of the switch for `control` in `View::switches`.
pub fn switch_index(control: Control) -> Option<usize> {
    match control {
        Control::Alert(kind) => AlertKind::ALL.iter().position(|&k| k == kind),
        Control::Autostart => Some(AlertKind::ALL.len()),
        _ => None,
    }
}

/// The controls of `pane` in Tab order (the sidebar first).
pub fn focus_order(pane: Pane) -> Vec<Control> {
    let mut order = vec![Control::Sidebar];
    match pane {
        Pane::General => order.extend([Control::Autostart, Control::Reset]),
        Pane::Alerts => order.extend(AlertKind::ALL.map(Control::Alert)),
        Pane::Notch => order.extend([Control::Width, Control::Duration, Control::BluetoothQuiet]),
    }
    order
}

/// Number of segments of a segmented control.
pub fn segments(control: Control) -> usize {
    match control {
        Control::Width => NotchWidth::ALL.len(),
        Control::Duration => AlertDuration::ALL.len(),
        _ => 0,
    }
}

fn sidebar_item(pane: Pane) -> Rect {
    let i = Pane::ALL.iter().position(|&p| p == pane).unwrap_or(0);
    Rect { x: SIDEBAR_ITEM_X, y: SIDEBAR_ITEMS_Y + i as f32 * SIDEBAR_ITEM_H, w: SIDEBAR_ITEM_W, h: SIDEBAR_ITEM_H }
}

/// A row of the card in `pane` that holds `control`, and the row's height.
fn row(pane: Pane, control: Control) -> Option<Rect> {
    let rows: Vec<(Control, f32)> = match pane {
        Pane::General => vec![(Control::Autostart, ROW_H)],
        Pane::Alerts => AlertKind::ALL.map(|kind| (Control::Alert(kind), ROW_H)).to_vec(),
        Pane::Notch => vec![(Control::Width, 52.0), (Control::Duration, 52.0), (Control::BluetoothQuiet, 56.0)],
    };
    let mut y = card_top(pane);
    for (c, h) in rows {
        if c == control {
            return Some(Rect { x: CONTENT_X, y, w: CONTENT_W, h });
        }
        y += h;
    }
    None
}

fn card_top(pane: Pane) -> f32 {
    match pane {
        Pane::Alerts => BODY_Y,
        Pane::General | Pane::Notch => CARD_Y,
    }
}

/// The interactive part of `control` on `pane`.
pub fn control_rect(pane: Pane, control: Control) -> Option<Rect> {
    if control == Control::Sidebar {
        return Some(sidebar_item(pane));
    }
    if control == Control::Reset {
        return (pane == Pane::General).then(|| {
            let card = row(pane, Control::Autostart).expect("General has Start at login");
            Rect { x: CONTENT_X + CONTENT_W - RESET_W, y: card.y + card.h + 16.0, w: RESET_W, h: BUTTON_H }
        });
    }
    let r = row(pane, control)?;
    let right = r.x + r.w - ROW_PAD;
    Some(match control {
        Control::Alert(_) | Control::Autostart => {
            Rect { x: right - SWITCH_W, y: r.y + (r.h - SWITCH_H) / 2.0, w: SWITCH_W, h: SWITCH_H }
        }
        Control::Width | Control::Duration => {
            let w = SEGMENT_W * segments(control) as f32;
            Rect { x: right - w, y: r.y + (r.h - SEGMENT_H) / 2.0, w, h: SEGMENT_H }
        }
        Control::BluetoothQuiet => Rect { x: right - POPUP_W, y: r.y + (r.h - SEGMENT_H) / 2.0, w: POPUP_W, h: SEGMENT_H },
        Control::Sidebar | Control::Reset => unreachable!(),
    })
}

/// What is under `pos` while `pane` shows. A switch's whole row counts.
pub fn hit(pane: Pane, pos: (f32, f32)) -> Option<Hit> {
    if let Some(&p) = Pane::ALL.iter().find(|&&p| sidebar_item(p).contains(pos)) {
        return Some(Hit::Pane(p));
    }
    for control in focus_order(pane).into_iter().skip(1) {
        let Some(rect) = control_rect(pane, control) else { continue };
        match control {
            Control::Alert(_) | Control::Autostart => {
                if row(pane, control).is_some_and(|r| r.contains(pos)) {
                    return Some(Hit::Control(control));
                }
            }
            Control::Width | Control::Duration if rect.contains(pos) => {
                let i = ((pos.0 - rect.x) / SEGMENT_W) as usize;
                return Some(Hit::Segment(control, i.min(segments(control) - 1)));
            }
            _ if rect.contains(pos) => return Some(Hit::Control(control)),
            _ => {}
        }
    }
    None
}

/// The resting notch width the preview shows for `settings`.
pub fn preview_width(settings: Settings, battery: bool) -> f32 {
    let base = if battery { COLLAPSED_BATTERY_WIDTH } else { COLLAPSED.width };
    base + settings.width.extra()
}

struct Painter<'a> {
    pixmap: &'a mut Pixmap,
    scale: f32,
    fonts: &'a Fonts,
}

impl Painter<'_> {
    fn ts(&self) -> Transform {
        Transform::from_scale(self.scale, self.scale)
    }

    fn fill(&mut self, path: &Path, rgb: Rgb, alpha: f32) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha.clamp(0.0, 1.0) * 255.0) as u8));
        paint.anti_alias = true;
        let ts = self.ts();
        self.pixmap.fill_path(path, &paint, FillRule::EvenOdd, ts, None);
    }

    fn rrect(&mut self, r: Rect, radius: f32, rgb: Rgb, alpha: f32) {
        if let Some(path) = rounded_rect(r.x, r.y, r.w, r.h, radius) {
            self.fill(&path, rgb, alpha);
        }
    }

    fn stroke(&mut self, path: &Path, width: f32, rgb: Rgb, alpha: f32) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha.clamp(0.0, 1.0) * 255.0) as u8));
        paint.anti_alias = true;
        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        let ts = self.ts();
        self.pixmap.stroke_path(path, &paint, &stroke, ts, None);
    }

    /// A soft drop shadow under a raised element.
    fn shadow(&mut self, r: Rect, radius: f32, alpha: f32) {
        for (grow, a) in [(1.5, 0.05), (0.5, 0.08)] {
            let s = Rect { x: r.x - grow, y: r.y - grow + 1.0, w: r.w + 2.0 * grow, h: r.h + 2.0 * grow };
            self.rrect(s, radius + grow, [0, 0, 0], a * alpha);
        }
    }

    fn text_width(&self, font: &Font, text: &str, size: f32) -> f32 {
        text.chars().map(|c| font.advance(c, size * self.scale)).sum::<f32>() / self.scale
    }

    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, font: &Font, text: &str, size: f32, x: f32, baseline: f32, rgb: Rgb, alpha: f32) {
        let px = size * self.scale;
        let baseline = (baseline * self.scale).round() as i32;
        let mut pen = x * self.scale;
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        for ch in text.chars() {
            let glyph = self.fonts.glyph(font, ch, px);
            let (gx, gy) = (pen.round() as i32 + glyph.left, baseline + glyph.top);
            let data = self.pixmap.data_mut();
            for row in 0..glyph.height as i32 {
                let y = gy + row;
                if y < 0 || y >= ph {
                    continue;
                }
                for col in 0..glyph.width as i32 {
                    let x = gx + col;
                    if x < 0 || x >= pw {
                        continue;
                    }
                    let a = glyph.coverage[(row * glyph.width as i32 + col) as usize] as f32 / 255.0 * alpha;
                    if a <= 0.0 {
                        continue;
                    }
                    let i = (y * pw + x) as usize * 4;
                    let d = &mut data[i..i + 4];
                    for c in 0..3 {
                        d[c] = (rgb[c] as f32 * a + d[c] as f32 * (1.0 - a)) as u8;
                    }
                    d[3] = (255.0 * a + d[3] as f32 * (1.0 - a)) as u8;
                }
            }
            pen += glyph.advance;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text_centered(&mut self, font: &Font, text: &str, size: f32, cx: f32, baseline: f32, rgb: Rgb, alpha: f32) {
        let w = self.text_width(font, text, size);
        self.text(font, text, size, cx - w / 2.0, baseline, rgb, alpha);
    }

    fn focus_ring(&mut self, r: Rect, radius: f32, accent: Rgb) {
        if let Some(path) = rounded_rect(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0, radius + 3.0) {
            self.stroke(&path, 3.0, accent, 0.55);
        }
    }
}

/// The window background, for the title bar to match.
pub fn window_color(theme: Theme) -> Rgb {
    colors(theme).window
}

/// Draws the whole window into `pixmap` (`WIDTH` x `HEIGHT` logical px
/// at `scale`).
pub fn draw(pixmap: &mut Pixmap, scale: f32, fonts: &Fonts, view: &View, cache: &mut PreviewCache) {
    let c = colors(view.theme);
    pixmap.fill(Color::from_rgba8(c.window[0], c.window[1], c.window[2], 255));
    let mut p = Painter { pixmap, scale, fonts };
    sidebar(&mut p, &c, view);
    p.text(fonts.bold(), view.pane.title(), 20.0, CONTENT_X, TITLE_BASELINE, c.label, 1.0);
    match view.pane {
        Pane::General => general(&mut p, &c, view, cache),
        Pane::Alerts => alerts(&mut p, &c, view),
        Pane::Notch => notch(&mut p, &c, view, cache),
    }
}

fn sidebar(p: &mut Painter, c: &Colors, view: &View) {
    let panel = Rect { x: SIDEBAR_INSET, y: SIDEBAR_INSET, w: SIDEBAR_W, h: HEIGHT - 2.0 * SIDEBAR_INSET };
    p.rrect(panel, 12.0, c.sidebar, 1.0);
    // The app, as a header.
    let tile = Rect { x: SIDEBAR_ITEM_X + 6.0, y: 22.0, w: 34.0, h: 34.0 };
    p.rrect(tile, 9.0, c.button_border, 1.0);
    p.rrect(Rect { x: tile.x + 0.5, y: tile.y + 0.5, w: tile.w - 1.0, h: tile.h - 1.0 }, 8.5, [12, 12, 14], 1.0);
    p.rrect(Rect { x: tile.x + 7.0, y: tile.y + 8.0, w: 20.0, h: 7.0 }, 3.5, WHITE, 1.0);
    let text_x = tile.x + tile.w + 10.0;
    p.text(p.fonts.semibold(), "notch-woy", 14.0, text_x, 37.0, c.label, 1.0);
    let version = concat!("Version ", env!("CARGO_PKG_VERSION"));
    p.text(p.fonts.regular(), version, 11.5, text_x, 52.0, c.secondary, 1.0);

    for pane in Pane::ALL {
        let r = sidebar_item(pane);
        let selected = pane == view.pane;
        if selected {
            p.rrect(r, 7.0, c.accent, 1.0);
        } else if view.hover == Some(Hit::Pane(pane)) {
            p.rrect(r, 7.0, c.label, 0.06);
        }
        let icon = Rect { x: r.x + 6.0, y: r.y + 5.0, w: 22.0, h: 22.0 };
        pane_icon(p, pane, icon);
        let rgb = if selected { WHITE } else { c.label };
        p.text(p.fonts.regular(), pane.title(), 13.5, icon.x + icon.w + 9.0, r.y + 21.0, rgb, 1.0);
    }
    if view.focus == Some(Control::Sidebar) {
        p.focus_ring(sidebar_item(view.pane), 7.0, c.accent);
    }
}

/// A rounded card behind rows, with hairlines between them.
fn card(p: &mut Painter, c: &Colors, rows: &[Rect]) {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return };
    let r = Rect { x: first.x, y: first.y, w: first.w, h: last.y + last.h - first.y };
    p.rrect(r, CARD_RADIUS, c.card, 1.0);
    for row in &rows[1..] {
        // Inset past the icon, as in Apple's lists.
        let x = row.x + ROW_PAD + ICON + 12.0;
        p.rrect(Rect { x, y: row.y - 0.5, w: row.x + row.w - x, h: 1.0 }, 0.0, c.separator, 1.0);
    }
}

/// Icon tile, title and optional subtitle on the left of a row.
fn row_label(p: &mut Painter, c: &Colors, r: Rect, icon: impl FnOnce(&mut Painter, Rect), title: &str, subtitle: Option<&str>) {
    let tile = Rect { x: r.x + ROW_PAD, y: r.y + (r.h - ICON) / 2.0, w: ICON, h: ICON };
    icon(p, tile);
    let x = tile.x + ICON + 12.0;
    match subtitle {
        Some(sub) => {
            p.text(p.fonts.regular(), title, 13.5, x, r.y + r.h / 2.0 - 2.0, c.label, 1.0);
            p.text(p.fonts.regular(), sub, 11.5, x, r.y + r.h / 2.0 + 13.0, c.secondary, 1.0);
        }
        None => p.text(p.fonts.regular(), title, 13.5, x, r.y + r.h / 2.0 + 5.0, c.label, 1.0),
    }
}

fn switch(p: &mut Painter, c: &Colors, view: &View, control: Control) {
    let (Some(r), Some(i)) = (control_rect(view.pane, control), switch_index(control)) else { return };
    let on = view.switches[i].clamp(0.0, 1.0);
    p.rrect(r, r.h / 2.0, lerp(c.switch_off, c.green, on), 1.0);
    // The knob widens a little while pressed, as on iOS.
    let pressed = view.pressed == Some(Hit::Control(control));
    let knob_w = if pressed { 24.0 } else { 20.0 };
    let travel = r.w - 4.0 - knob_w;
    let knob = Rect { x: r.x + 2.0 + travel * on, y: r.y + 2.0, w: knob_w, h: r.h - 4.0 };
    p.shadow(knob, knob.h / 2.0, 1.0);
    p.rrect(knob, knob.h / 2.0, WHITE, 1.0);
    if view.focus == Some(control) {
        p.focus_ring(r, r.h / 2.0, c.accent);
    }
}

fn segmented(p: &mut Painter, c: &Colors, view: &View, control: Control, labels: &[&str], selection: f32) {
    let Some(r) = control_rect(view.pane, control) else { return };
    p.rrect(r, 7.0, c.segment_bg, 1.0);
    let n = labels.len();
    let selected = selection.round().clamp(0.0, (n - 1) as f32) as usize;
    // Hairlines between unselected neighbours.
    for i in 1..n {
        if i != selected && i != selected + 1 {
            let x = r.x + i as f32 * SEGMENT_W;
            p.rrect(Rect { x: x - 0.5, y: r.y + 6.0, w: 1.0, h: r.h - 12.0 }, 0.0, c.secondary, 0.35);
        }
    }
    let thumb = Rect { x: r.x + 2.0 + selection * SEGMENT_W, y: r.y + 2.0, w: SEGMENT_W - 4.0, h: r.h - 4.0 };
    p.shadow(thumb, 5.5, 1.0);
    p.rrect(thumb, 5.5, c.segment_selected, 1.0);
    for (i, label) in labels.iter().enumerate() {
        let hovered = view.hover == Some(Hit::Segment(control, i)) && i != selected;
        if hovered {
            let h = Rect { x: r.x + 2.0 + i as f32 * SEGMENT_W, y: r.y + 2.0, w: SEGMENT_W - 4.0, h: r.h - 4.0 };
            p.rrect(h, 5.5, c.label, 0.05);
        }
        let font = if i == selected { p.fonts.semibold() } else { p.fonts.regular() };
        let cx = r.x + (i as f32 + 0.5) * SEGMENT_W;
        p.text_centered(font, label, 12.0, cx, r.y + 17.5, c.label, 1.0);
    }
    if view.focus == Some(control) {
        p.focus_ring(r, 7.0, c.accent);
    }
}

fn button(p: &mut Painter, c: &Colors, view: &View, control: Control, label: &str, chevrons: bool) {
    let Some(r) = control_rect(view.pane, control) else { return };
    p.shadow(r, 6.0, 0.6);
    p.rrect(r, 6.0, c.button_border, 1.0);
    let inner = Rect { x: r.x + 0.5, y: r.y + 0.5, w: r.w - 1.0, h: r.h - 1.0 };
    p.rrect(inner, 5.5, c.button, 1.0);
    if view.pressed == Some(Hit::Control(control)) {
        p.rrect(inner, 5.5, c.label, 0.08);
    } else if view.hover == Some(Hit::Control(control)) {
        p.rrect(inner, 5.5, c.label, 0.04);
    }
    let baseline = r.y + r.h / 2.0 + 4.5;
    if chevrons {
        p.text(p.fonts.regular(), label, 12.5, r.x + 10.0, baseline, c.label, 1.0);
        // Up and down chevrons on an accent tile, as on macOS pop-ups.
        let tile = Rect { x: r.x + r.w - 20.0, y: r.y + 4.0, w: 16.0, h: r.h - 8.0 };
        p.rrect(tile, 4.0, c.accent, 1.0);
        let (cx, cy) = (tile.x + tile.w / 2.0, tile.y + tile.h / 2.0);
        for d in [-1.0, 1.0] {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 3.0, cy + d * 1.5);
            pb.line_to(cx, cy + d * 4.5);
            pb.line_to(cx + 3.0, cy + d * 1.5);
            if let Some(path) = pb.finish() {
                p.stroke(&path, 1.5, WHITE, 1.0);
            }
        }
    } else {
        p.text_centered(p.fonts.regular(), label, 12.5, r.x + r.w / 2.0, baseline, c.label, 1.0);
    }
    if view.focus == Some(control) {
        p.focus_ring(r, 6.0, c.accent);
    }
}

/// Height of the notch's canvas in the preview: the resting notch and its
/// shoulders.
const PREVIEW_NOTCH_H: f32 = 44.0;

/// The preview's notch, drawn by the notch's own renderer and kept until
/// its width, the scale, the battery or the time changes, so frames that
/// only move a switch do not redraw it.
#[derive(Default)]
pub struct PreviewCache {
    canvas: Option<Pixmap>,
    key: Option<(u32, u32, Option<Battery>, String)>,
}

impl PreviewCache {
    fn notch(&mut self, p: &Painter, view: &View, width: f32) -> Option<&Pixmap> {
        let key = (view.preview_width.to_bits(), p.scale.to_bits(), view.battery, view.time.clone());
        if self.key.as_ref() != Some(&key) || self.canvas.is_none() {
            let (w, h) = ((width * p.scale).round() as u32, (PREVIEW_NOTCH_H * p.scale).round() as u32);
            let canvas = match self.canvas.take() {
                Some(c) if c.width() == w && c.height() == h => Some(c),
                _ => Pixmap::new(w, h),
            };
            self.canvas = canvas.map(|mut canvas| {
                let scene = Scene {
                    shape: Shape { width: view.preview_width, ..COLLAPSED },
                    panel_width: EXPANDED.width,
                    lyrics_reveal: 0.0,
                    media: None,
                    transition: None,
                    lyrics: LyricsView::Missing,
                    seek: None,
                    seek_grow: 0.0,
                    battery: view.battery,
                    rest_width: view.preview_width,
                    bluetooth: None,
                    alert: None,
                    alert_from: None,
                    page: 0.0,
                    controls: ControlsView::default(),
                    clock: 0.0,
                    levels: None,
                    time: &view.time,
                    outputs: OutputsView::default(),
                    shelf: ShelfView::default(),
                };
                render::draw(&mut canvas, p.scale, &scene, Some(p.fonts));
                canvas
            });
            self.key = Some(key);
        }
        self.canvas.as_ref()
    }
}

/// A desktop strip with the resting notch at its top edge, at the chosen
/// width, drawn by the notch's own renderer.
fn preview(p: &mut Painter, c: &Colors, view: &View, cache: &mut PreviewCache) {
    let r = Rect { x: CONTENT_X, y: BODY_Y, w: CONTENT_W, h: PREVIEW_H };
    let Some(card) = rounded_rect(r.x, r.y, r.w, r.h, CARD_RADIUS) else { return };
    let gradient = LinearGradient::new(
        Point::from_xy(r.x, r.y),
        Point::from_xy(r.x + r.w, r.y + r.h),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(c.wallpaper[0][0], c.wallpaper[0][1], c.wallpaper[0][2], 255)),
            GradientStop::new(1.0, Color::from_rgba8(c.wallpaper[1][0], c.wallpaper[1][1], c.wallpaper[1][2], 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    );
    if let Some(shader) = gradient {
        let paint = Paint { shader, anti_alias: true, ..Default::default() };
        let ts = p.ts();
        p.pixmap.fill_path(&card, &paint, FillRule::Winding, ts, None);
    }

    // The notch renderer centers the notch on its canvas and hangs it from
    // the top edge, like the real screen. Its corners are clear, so the
    // canvas needs no clipping to the card.
    let canvas = cache.notch(p, view, r.w);
    let Some(canvas) = canvas else { return };
    let pattern = Pattern::new(
        canvas.as_ref(),
        SpreadMode::Pad,
        FilterQuality::Nearest,
        1.0,
        Transform::from_row(1.0 / p.scale, 0.0, 0.0, 1.0 / p.scale, r.x, r.y),
    );
    if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w, PREVIEW_NOTCH_H) {
        let strip = PathBuilder::from_rect(rect);
        let paint = Paint { shader: pattern, ..Default::default() };
        let ts = p.ts();
        p.pixmap.fill_path(&strip, &paint, FillRule::Winding, ts, None);
    }
    let label = format!("{} notch", view.settings.width.label());
    p.text(p.fonts.semibold(), &label, 11.5, r.x + 14.0, r.y + r.h - 14.0, WHITE, 0.85);
}

fn general(p: &mut Painter, c: &Colors, view: &View, cache: &mut PreviewCache) {
    preview(p, c, view, cache);
    let Some(r) = row(Pane::General, Control::Autostart) else { return };
    card(p, c, &[r]);
    row_label(p, c, r, |p, t| tile_icon(p, t, GRAY, Glyph::Power), "Start at login", Some("Opens notch-woy when you sign in"));
    switch(p, c, view, Control::Autostart);
    button(p, c, view, Control::Reset, "Reset to Defaults...", false);
    if let Some(reset) = control_rect(Pane::General, Control::Reset) {
        let note = "Changes apply right away.";
        p.text(p.fonts.regular(), note, 11.5, CONTENT_X + 2.0, reset.y + 18.0, c.secondary, 1.0);
    }
}

fn alerts(p: &mut Painter, c: &Colors, view: &View) {
    let rows: Vec<Rect> = AlertKind::ALL.iter().filter_map(|&k| row(Pane::Alerts, Control::Alert(k))).collect();
    card(p, c, &rows);
    for (kind, r) in AlertKind::ALL.into_iter().zip(rows) {
        let (rgb, glyph, title, subtitle) = match kind {
            AlertKind::Battery => (GREEN, Glyph::Battery, "Battery", "Charging, unplugged, low and full"),
            AlertKind::Volume => (PINK, Glyph::Speaker, "Volume", "Replaces the Windows volume pop-up"),
            AlertKind::Bluetooth => (BLUE, Glyph::Bluetooth, "Bluetooth devices", "When a device connects or disconnects"),
            AlertKind::Notifications => (RED, Glyph::Bell, "App notifications", "Messages, mail and other apps"),
            AlertKind::NowPlaying => (PINK, Glyph::Note, "New track", "Cover, title and artist when a song starts"),
        };
        row_label(p, c, r, |p, t| tile_icon(p, t, rgb, glyph), title, Some(subtitle));
        switch(p, c, view, Control::Alert(kind));
    }
    if let Some(last) = row(Pane::Alerts, Control::Alert(AlertKind::NowPlaying)) {
        let note = "Alerts grow out of the notch, then shrink back into it.";
        p.text(p.fonts.regular(), note, 11.5, CONTENT_X + 2.0, last.y + last.h + 20.0, c.secondary, 1.0);
    }
}

fn notch(p: &mut Painter, c: &Colors, view: &View, cache: &mut PreviewCache) {
    preview(p, c, view, cache);
    let controls = [Control::Width, Control::Duration, Control::BluetoothQuiet];
    let rows: Vec<Rect> = controls.iter().filter_map(|&k| row(Pane::Notch, k)).collect();
    card(p, c, &rows);
    let [width, duration, quiet] = [rows[0], rows[1], rows[2]];
    row_label(p, c, width, |p, t| tile_icon(p, t, PURPLE, Glyph::Arrows), "Resting width", None);
    segmented(p, c, view, Control::Width, &NotchWidth::ALL.map(NotchWidth::label), view.width_selection);
    row_label(p, c, duration, |p, t| tile_icon(p, t, ORANGE, Glyph::Clock), "Alert duration", None);
    let durations = AlertDuration::ALL.map(AlertDuration::label);
    segmented(p, c, view, Control::Duration, &durations, view.duration_selection);
    let subtitle = Some("After a device's first alert");
    row_label(p, c, quiet, |p, t| tile_icon(p, t, BLUE, Glyph::Bluetooth), "Bluetooth reconnects", subtitle);
    let label = bluetooth_quiet_label(view.settings.bluetooth_quiet_minutes);
    button(p, c, view, Control::BluetoothQuiet, label, true);
}

#[derive(Clone, Copy)]
enum Glyph {
    Gear,
    Bell,
    Pill,
    Battery,
    Speaker,
    Bluetooth,
    Note,
    Power,
    Arrows,
    Clock,
}

fn pane_icon(p: &mut Painter, pane: Pane, r: Rect) {
    match pane {
        Pane::General => tile_icon(p, r, GRAY, Glyph::Gear),
        Pane::Alerts => tile_icon(p, r, RED, Glyph::Bell),
        Pane::Notch => tile_icon(p, r, INDIGO, Glyph::Pill),
    }
}

/// A colored rounded tile with a white glyph, as in Apple's settings.
fn tile_icon(p: &mut Painter, r: Rect, rgb: Rgb, glyph: Glyph) {
    p.rrect(r, r.w * 0.23, rgb, 1.0);
    // Glyphs are drawn for a 26 px tile and scaled to fit.
    let k = r.w / 26.0;
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    let at = |x: f32, y: f32| (cx + x * k, cy + y * k);
    let line = |p: &mut Painter, pts: &[(f32, f32)], width: f32| {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in pts.iter().enumerate() {
            let (x, y) = at(x, y);
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        if let Some(path) = pb.finish() {
            p.stroke(&path, width * k, WHITE, 1.0);
        }
    };
    let circle = |p: &mut Painter, x: f32, y: f32, radius: f32| {
        let (x, y) = at(x, y);
        if let Some(path) = PathBuilder::from_circle(x, y, radius * k) {
            p.fill(&path, WHITE, 1.0);
        }
    };
    let rect = |p: &mut Painter, x: f32, y: f32, w: f32, h: f32, radius: f32| {
        let (x, y) = at(x, y);
        p.rrect(Rect { x, y, w: w * k, h: h * k }, radius * k, WHITE, 1.0);
    };
    match glyph {
        Glyph::Gear => {
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                line(p, &[(a.cos() * 5.0, a.sin() * 5.0), (a.cos() * 8.0, a.sin() * 8.0)], 3.0);
            }
            let (x, y) = at(0.0, 0.0);
            if let Some(ring) = PathBuilder::from_circle(x, y, 4.5 * k) {
                p.stroke(&ring, 3.0 * k, WHITE, 1.0);
            }
        }
        Glyph::Bell => {
            let mut pb = PathBuilder::new();
            let (x0, y0) = at(-6.5, 4.0);
            pb.move_to(x0, y0);
            let (x1, y1) = at(-5.0, 1.5);
            pb.line_to(x1, y1);
            let (c1x, c1y) = at(-5.0, -7.5);
            let (c2x, c2y) = at(5.0, -7.5);
            let (x2, y2) = at(5.0, 1.5);
            pb.cubic_to(c1x, c1y, c2x, c2y, x2, y2);
            let (x3, y3) = at(6.5, 4.0);
            pb.line_to(x3, y3);
            pb.close();
            if let Some(path) = pb.finish() {
                p.fill(&path, WHITE, 1.0);
            }
            circle(p, 0.0, 6.0, 2.0);
        }
        Glyph::Pill => rect(p, -7.5, -6.0, 15.0, 5.0, 2.5),
        Glyph::Battery => {
            let (x, y) = at(-8.0, -4.5);
            if let Some(body) = rounded_rect(x, y, 14.5 * k, 9.0 * k, 2.5 * k) {
                p.stroke(&body, 1.5 * k, WHITE, 1.0);
            }
            rect(p, -6.0, -2.5, 8.0, 5.0, 1.0);
            rect(p, 7.5, -1.8, 1.8, 3.6, 0.8);
        }
        Glyph::Speaker => {
            let mut pb = PathBuilder::new();
            for (i, (x, y)) in [(-7.5, -2.5), (-4.5, -2.5), (0.0, -6.5), (0.0, 6.5), (-4.5, 2.5), (-7.5, 2.5)]
                .into_iter()
                .enumerate()
            {
                let (x, y) = at(x, y);
                if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
            }
            pb.close();
            if let Some(path) = pb.finish() {
                p.fill(&path, WHITE, 1.0);
            }
            line(p, &[(3.0, -3.0), (4.5, 0.0), (3.0, 3.0)], 1.6);
            line(p, &[(5.5, -5.5), (8.0, 0.0), (5.5, 5.5)], 1.6);
        }
        Glyph::Bluetooth => line(p, &[(-4.5, -4.0), (4.5, 4.0), (0.0, 8.0), (0.0, -8.0), (4.5, -4.0), (-4.5, 4.0)], 1.8),
        Glyph::Note => {
            circle(p, -3.5, 5.0, 3.0);
            line(p, &[(-0.8, 5.0), (-0.8, -7.0), (5.0, -5.0)], 1.8);
        }
        Glyph::Power => {
            line(p, &[(0.0, -7.5), (0.0, -1.0)], 2.0);
            let (x, y) = at(0.0, 1.0);
            let mut pb = PathBuilder::new();
            // An open ring, gap at the top.
            let r = 6.5 * k;
            let start = -std::f32::consts::FRAC_PI_2 + 0.7;
            for i in 0..=24 {
                let a = start + i as f32 / 24.0 * (std::f32::consts::TAU - 1.4);
                let (px, py) = (x + a.cos() * r, y + a.sin() * r);
                if i == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
            }
            if let Some(path) = pb.finish() {
                p.stroke(&path, 2.0 * k, WHITE, 1.0);
            }
        }
        Glyph::Arrows => {
            line(p, &[(-8.0, 0.0), (8.0, 0.0)], 1.8);
            line(p, &[(-5.0, -3.0), (-8.0, 0.0), (-5.0, 3.0)], 1.8);
            line(p, &[(5.0, -3.0), (8.0, 0.0), (5.0, 3.0)], 1.8);
        }
        Glyph::Clock => {
            let (x, y) = at(0.0, 0.0);
            if let Some(ring) = PathBuilder::from_circle(x, y, 7.5 * k) {
                p.stroke(&ring, 1.8 * k, WHITE, 1.0);
            }
            line(p, &[(0.0, -4.5), (0.0, 0.0), (3.5, 2.0)], 1.8);
        }
    }
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
    [mix(0), mix(1), mix(2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_match_drawn_controls() {
        // Sidebar items.
        let general = sidebar_item(Pane::General);
        assert_eq!(hit(Pane::Notch, (general.x + 5.0, general.y + 5.0)), Some(Hit::Pane(Pane::General)));
        // A switch's whole row toggles it.
        let row = row(Pane::Alerts, Control::Alert(AlertKind::Volume)).unwrap();
        assert_eq!(hit(Pane::Alerts, (row.x + 30.0, row.y + 10.0)), Some(Hit::Control(Control::Alert(AlertKind::Volume))));
        // Segments, left to right.
        let seg = control_rect(Pane::Notch, Control::Width).unwrap();
        for i in 0..4 {
            let x = seg.x + (i as f32 + 0.5) * SEGMENT_W;
            assert_eq!(hit(Pane::Notch, (x, seg.y + 10.0)), Some(Hit::Segment(Control::Width, i)));
        }
        // Controls of other panes are not there.
        let reset = control_rect(Pane::General, Control::Reset).unwrap();
        assert_eq!(hit(Pane::General, (reset.x + 5.0, reset.y + 5.0)), Some(Hit::Control(Control::Reset)));
        assert_eq!(control_rect(Pane::Alerts, Control::Reset), None);
    }

    #[test]
    fn everything_fits_the_window() {
        for pane in Pane::ALL {
            for control in focus_order(pane) {
                let r = control_rect(pane, control).unwrap();
                assert!(r.x >= 0.0 && r.x + r.w <= WIDTH && r.y + r.h <= HEIGHT - 20.0, "{pane:?} {control:?} {r:?}");
            }
        }
        // The segmented controls clear their row's title.
        let seg = control_rect(Pane::Notch, Control::Width).unwrap();
        assert!(seg.x > CONTENT_X + ROW_PAD + ICON + 12.0 + 100.0);
    }

    /// Times a frame of each pane while its animations run, at 125%:
    /// `cargo test --release times_frames -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn times_frames() {
        let fonts = Fonts::load().unwrap();
        let settings = Settings::default();
        let scale = 1.25;
        let mut pixmap = Pixmap::new((WIDTH * scale) as u32, (HEIGHT * scale) as u32).unwrap();
        let mut cache = PreviewCache::default();
        for pane in Pane::ALL {
            let frames = 120;
            let start = std::time::Instant::now();
            for i in 0..frames {
                let t = i as f32 / frames as f32;
                let view = View {
                    theme: Theme::Dark,
                    pane,
                    settings,
                    switches: [t; 6],
                    width_selection: t * 3.0,
                    duration_selection: t * 2.0,
                    preview_width: 190.0 + 120.0 * t,
                    battery: Some(Battery { percent: 76, plugged: false, charging: false }),
                    time: "9:41".into(),
                    focus: None,
                    hover: None,
                    pressed: None,
                };
                draw(&mut pixmap, scale, &fonts, &view, &mut cache);
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            println!("{pane:?}: {ms:.2} ms per frame");
        }
    }

    /// Renders every pane in both themes to `target/settings_*.png`:
    /// `cargo test settings_preview -- --ignored`.
    #[test]
    #[ignore]
    fn settings_preview() {
        let fonts = Fonts::load().unwrap();
        let mut settings = Settings::default();
        settings.set_alert(AlertKind::NowPlaying, false);
        let battery = Some(Battery { percent: 76, plugged: false, charging: false });
        for theme in [Theme::Light, Theme::Dark] {
            for pane in Pane::ALL {
                let scale = 2.0;
                let mut cache = PreviewCache::default();
                let mut pixmap = Pixmap::new((WIDTH * scale) as u32, (HEIGHT * scale) as u32).unwrap();
                let view = View {
                    theme,
                    pane,
                    settings,
                    switches: [1.0, 1.0, 1.0, 1.0, 0.0, 1.0],
                    width_selection: 1.0,
                    duration_selection: 1.0,
                    preview_width: preview_width(settings, true),
                    battery,
                    time: "9:41".into(),
                    focus: (pane == Pane::Notch).then_some(Control::Width),
                    hover: None,
                    pressed: None,
                };
                draw(&mut pixmap, scale, &fonts, &view, &mut cache);
                let name = format!("settings_{}_{:?}.png", pane.title().to_lowercase(), theme).to_lowercase();
                pixmap.save_png(format!("{}/target/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            }
        }
    }
}
