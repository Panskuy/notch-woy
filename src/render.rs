//! Draws the notch and its content. All layout is in logical (96 DPI) pixels
//! and scaled to physical pixels by `scale`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::font::{Font, Glyph};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint,
    Path, PathBuilder, Pattern, Pixmap, Point, Rect, SpreadMode, Stroke, Transform,
};

use crate::anim::{ease_out_back, ease_out_cubic};
use crate::battery::{Battery, BatteryEvent};
use crate::bluetooth::{Device, DeviceKind};
use crate::controls::ControlState;
use crate::lyrics::Line;
use crate::media::{art_changed, track_changed, Command, NowPlaying};
use crate::notifications::Toast;
use crate::outputs::{Output, OutputKind};
use crate::palette::Rgb;

/// Notch size. The top edge sits flush with the screen.
#[derive(Clone, Copy)]
pub struct Shape {
    pub width: f32,
    pub height: f32,
    /// Radius of the bottom corners.
    pub radius: f32,
}

pub const COLLAPSED: Shape = Shape { width: 190.0, height: 30.0, radius: 12.0 };
/// Collapsed width on machines with a battery, to fit its indicator.
pub const COLLAPSED_BATTERY_WIDTH: f32 = 230.0;
pub const EXPANDED: Shape = Shape { width: 520.0, height: 150.0, radius: 24.0 };
/// A brief battery alert (plugged in, low battery...).
pub const BATTERY_ALERT: Shape = Shape { width: 340.0, height: 40.0, radius: 18.0 };
/// Volume changes only stretch the island a little sideways, like the
/// iPhone's silent-mode switch: an icon on one side, a level on the other.
pub const VOLUME_ALERT: Shape = Shape { width: 300.0, height: 32.0, radius: 14.0 };

/// App notifications open into a banner card, like iOS.
pub const TOAST_ALERT: Shape = Shape { width: 420.0, height: 80.0, radius: 28.0 };
/// Resting notch while an app's window sits under it: a slim pill that
/// stays out of the way of title bars and tabs.
pub const MINI: Shape = Shape { width: 80.0, height: 10.0, radius: 5.0 };

/// A Bluetooth device connecting or disconnecting.
pub const BLUETOOTH_ALERT: Shape = Shape { width: 340.0, height: 40.0, radius: 18.0 };

pub fn alert_shape(alert: &Alert) -> Shape {
    match alert {
        Alert::Battery { .. } => BATTERY_ALERT,
        Alert::Volume { .. } => VOLUME_ALERT,
        Alert::Toast(_) => TOAST_ALERT,
        Alert::Bluetooth { .. } => BLUETOOTH_ALERT,
        Alert::NowPlaying { .. } => NOW_PLAYING_ALERT,
    }
}

/// A new track: cover, title and artist, with a live equalizer.
pub const NOW_PLAYING_ALERT: Shape = Shape { width: 360.0, height: 64.0, radius: 26.0 };

/// Concave curve joining the notch sides to the top edge of the screen.
const SHOULDER: f32 = 8.0;

/// Open width with the lyrics panel hidden: the player alone.
pub const PLAYER_ONLY_WIDTH: f32 = 420.0;

// Expanded layout, relative to the expanded notch's left edge. With lyrics
// shown, the player fills a `PANEL_W` panel and lyrics the rest, split by a
// hairline. With lyrics hidden, the player takes the whole width.
const PANEL_W: f32 = 250.0;
const ART_X: f32 = 22.0;
const ART_Y: f32 = 12.0;
const ART: f32 = 62.0;
const ART_RADIUS: f32 = 10.0;
const TEXT_X: f32 = 106.0;
/// Space kept right of the title/subtitle.
const TEXT_RIGHT_PAD: f32 = 14.0;
const TITLE_SIZE: f32 = 14.5;
const SUBTITLE_SIZE: f32 = 12.5;
const TITLE_BASELINE: f32 = 50.0;
const SUBTITLE_BASELINE: f32 = 66.0;
const TIME_LEFT: f32 = 24.0;
/// Remaining time's right edge, from the player's right edge.
const TIME_RIGHT_INSET: f32 = 15.0;
const PROGRESS_Y: f32 = 91.0;
/// Progress bar thickness, and how much thicker it gets while dragged.
const PROGRESS_H: f32 = 6.0;
const PROGRESS_GROW: f32 = 5.0;
const PROGRESS_X0: f32 = 76.0;
/// Progress bar's right end, from the player's right edge.
const PROGRESS_RIGHT_INSET: f32 = 66.0;
/// Kept clear of the page dots at the open notch's right edge.
const DOTS_CLEARANCE: f32 = 14.0;
const CONTROLS_Y: f32 = 118.0;
// Control centers in a `PANEL_W` player; wider players shift them to stay centered.
const LYRICS_BUTTON_X: f32 = 53.0;
const PREVIOUS_X: f32 = 92.0;
const PLAY_X: f32 = 131.0;
const NEXT_X: f32 = 170.0;
/// Mirrors the lyrics button across play/pause.
const OUTPUT_BUTTON_X: f32 = 2.0 * PLAY_X - LYRICS_BUTTON_X;
/// Output picker rows, from the top of the open notch.
const OUTPUT_TOP: f32 = 12.0;
const OUTPUT_ROW_H: f32 = 26.0;
/// Rows the picker shows at once; more scroll.
pub const OUTPUT_ROWS_SHOWN: usize = 5;
/// The picker's sheet, around its rows, and the AirPlay button it grows
/// out of (as a circle this wide).
const SHEET_PAD: f32 = 6.0;
const SHEET_RADIUS: f32 = 16.0;
const SHEET_FROM: f32 = 30.0;
const SHEET_RGB: Rgb = [28, 28, 31];
const BUTTON_HIT_RADIUS: f32 = 17.0;
const LYRICS_PAD: f32 = 18.0;
const LYRICS_BASELINE: f32 = 76.0;
/// Baseline of the line being sung; earlier lines scroll up above it.
const LYRIC_CURRENT_BASELINE: f32 = 62.0;
const LYRIC_LINE_H: f32 = 22.0;

const WHITE: Rgb = [255, 255, 255];
const GRAY: Rgb = [170, 170, 170];
const DIM: Rgb = [135, 135, 135];
const GREEN: Rgb = [48, 209, 88];
const YELLOW: Rgb = [255, 214, 10];
const RED: Rgb = [255, 69, 58];

/// The cover tint, which is muted for backgrounds, brightened enough to
/// read as a color on black.
fn vivid(rgb: Rgb) -> Rgb {
    let max = rgb.into_iter().max().unwrap_or(0).max(1) as f32;
    rgb.map(|c| ((c as f32 / max) * 235.0).round() as u8)
}

/// Battery capsule width over height.
const BATTERY_ASPECT: f32 = 2.15;

/// iOS-style battery coloring: green on power, yellow then red when low.
fn battery_color(b: Battery) -> Rgb {
    match b.percent {
        _ if b.plugged => GREEN,
        0..=10 => RED,
        11..=20 => YELLOW,
        _ => WHITE,
    }
}

pub struct Fonts {
    regular: Font,
    semibold: Font,
    bold: Font,
    extrabold: Font,
    /// Rasterized glyphs by (font, char, pixel size). Text is redrawn every
    /// animation frame, so rasterizing each time would dominate the frame.
    glyphs: RefCell<HashMap<(usize, char, u32), Rc<Glyph>>>,
}

/// Plenty for a few sizes of a few scripts; cleared rather than evicted.
const GLYPH_CACHE_LIMIT: usize = 4096;

impl Fonts {
    /// Inter, embedded in the exe (assets/fonts, SIL Open Font License).
    pub fn load() -> Option<Self> {
        Some(Self {
            regular: Font::embedded(include_bytes!("../assets/fonts/Inter-Regular.ttf"))?,
            semibold: Font::embedded(include_bytes!("../assets/fonts/Inter-SemiBold.ttf"))?,
            bold: Font::embedded(include_bytes!("../assets/fonts/Inter-Bold.ttf"))?,
            extrabold: Font::embedded(include_bytes!("../assets/fonts/Inter-ExtraBold.ttf"))?,
            glyphs: RefCell::default(),
        })
    }

    /// `font` must be one of this set's fonts.
    fn glyph(&self, font: &Font, ch: char, px: f32) -> Rc<Glyph> {
        let key = (font as *const Font as usize, ch, px.to_bits());
        let mut glyphs = self.glyphs.borrow_mut();
        if let Some(glyph) = glyphs.get(&key) {
            return glyph.clone();
        }
        if glyphs.len() >= GLYPH_CACHE_LIMIT {
            glyphs.clear();
        }
        let glyph = Rc::new(font.rasterize(ch, px));
        glyphs.insert(key, glyph.clone());
        glyph
    }
}

/// Everything that varies from frame to frame.
pub struct Scene<'a> {
    pub shape: Shape,
    /// Width the open layout is laid out for; animates when the lyrics
    /// panel is toggled, independently of the hover animation.
    pub panel_width: f32,
    /// How far the lyrics panel is shown, 0..1.
    pub lyrics_reveal: f32,
    pub media: Option<&'a NowPlaying>,
    pub transition: Option<Transition<'a>>,
    pub lyrics: LyricsView<'a>,
    /// Progress bar position (0..1) while its knob is being dragged.
    pub seek: Option<f32>,
    /// How far the progress bar has swelled for a drag, 0..1 (springy).
    pub seek_grow: f32,
    /// `None` on machines without a battery.
    pub battery: Option<Battery>,
    /// The kind of Bluetooth device the resting notch shows, if any is
    /// connected.
    pub bluetooth: Option<DeviceKind>,
    pub alert: Option<Alert>,
    /// The alert `alert` is replacing, and how far (0..1) the swap is.
    pub alert_from: Option<(Alert, f32)>,
    /// Open notch page, animated: 0 = player, 1 = Control Center.
    pub page: f32,
    pub controls: ControlsView<'a>,
    /// Seconds on a running clock; drives the resting notch's equalizer.
    pub clock: f32,
    pub outputs: OutputsView<'a>,
}

/// The player's audio output picker.
#[derive(Clone, Copy, Default)]
pub struct OutputsView<'a> {
    pub list: &'a [Output],
    /// How far it has replaced the player, 0..1.
    pub reveal: f32,
    /// First row shown, fractional while scrolling.
    pub scroll: f32,
    /// Row the highlight sits on, fractional while it slides to a new pick.
    pub highlight: f32,
    /// Row under the pointer, fractional as the hover glides between rows,
    /// and how much it shows (0..1).
    pub hover: f32,
    pub hover_alpha: f32,
    /// Row being pressed.
    pub pressed: Option<usize>,
}

/// What the Control Center page shows.
#[derive(Clone, Copy, Default)]
pub struct ControlsView<'a> {
    pub state: ControlState,
    /// 0..1.
    pub volume: f32,
    pub muted: bool,
    /// 0..1, overriding the reported brightness while its slider is dragged.
    pub brightness: Option<f32>,
    pub bluetooth_device: Option<&'a Device>,
    pub battery: Option<Battery>,
    pub media: Option<&'a NowPlaying>,
    /// Per toggle (`Toggle::index` order), how far it is shown switched on
    /// (0..1); animates between off and on colors.
    pub toggle_on: [f32; 4],
    /// Per toggle, its size (1 at rest, smaller while pressed).
    pub toggle_scale: [f32; 4],
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Toggle {
    Airplane,
    Wifi,
    Bluetooth,
    Mute,
}

impl Toggle {
    pub const ALL: [Toggle; 4] = [Toggle::Airplane, Toggle::Mute, Toggle::Wifi, Toggle::Bluetooth];

    /// Position in `ALL` and in `ControlsView`'s per-toggle arrays.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).expect("listed")
    }

    /// Whether it is switched on, for these readings.
    pub fn is_on(self, state: &ControlState, muted: bool) -> bool {
        match self {
            Toggle::Airplane => state.airplane(),
            Toggle::Wifi => state.wifi == Some(true),
            Toggle::Bluetooth => state.bluetooth == Some(true),
            Toggle::Mute => muted,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slider {
    Brightness,
    Volume,
}

/// Something under the pointer on the Control Center page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ControlHit {
    Toggle(Toggle),
    /// With the value (0..1) at the pointer.
    Slider(Slider, f32),
    /// The play/pause button on the Now Playing module.
    PlayPause,
}

/// A brief notification, shown in its `alert_shape`.
#[derive(Clone)]
pub enum Alert {
    Battery { event: BatteryEvent, battery: Battery },
    /// `level` is 0..1.
    Volume { level: f32, muted: bool },
    /// A toast from another app (WhatsApp, mail...).
    Toast(Arc<Toast>),
    Bluetooth { device: Device, connected: bool },
    /// A new track started; `since` drives the equalizer animation.
    NowPlaying { track: Box<NowPlaying>, since: Instant },
}

pub enum LyricsView<'a> {
    Loading,
    Missing,
    Plain(&'a [Line]),
    /// `scroll` is the (animated, fractional) index of the line being sung,
    /// -1 before the first one.
    Synced { lines: &'a [Line], scroll: f32 },
}

/// Something clickable in the open notch.
#[derive(Clone, Copy)]
pub enum Button {
    Media(Command),
    Lyrics,
    /// Opens the audio output picker.
    Output,
}

/// Horizontal layout of the open notch, in canvas coordinates.
#[derive(Clone, Copy)]
struct Layout {
    left: f32,
    width: f32,
    /// Width of the player part; the lyrics panel gets the rest.
    player: f32,
    /// How far the lyrics panel is shown, 0..1.
    lyrics: f32,
}

impl Layout {
    fn new(canvas_w: f32, panel_width: f32, lyrics_reveal: f32) -> Self {
        let lyrics = lyrics_reveal.clamp(0.0, 1.0);
        Self {
            left: (canvas_w - panel_width) / 2.0,
            width: panel_width,
            // The player shrinks from the full width to its panel as the
            // lyrics slide in.
            player: panel_width + (PANEL_W - panel_width) * lyrics,
            lyrics,
        }
    }

    /// Extra room on the player's right while it reaches the notch's edge
    /// (lyrics hidden), where the page dots sit.
    fn dots_clearance(&self) -> f32 {
        DOTS_CLEARANCE * (1.0 - self.lyrics)
    }

    /// Left and right ends of the progress bar.
    fn progress_span(&self) -> (f32, f32) {
        (self.left + PROGRESS_X0, self.left + self.player - PROGRESS_RIGHT_INSET - self.dots_clearance())
    }

    /// Shift that keeps the controls centered in a player wider than `PANEL_W`.
    fn controls_dx(&self) -> f32 {
        (self.player - PANEL_W) / 2.0
    }
}

/// A track change in progress: `from` is what was showing before.
pub struct Transition<'a> {
    pub from: Option<&'a NowPlaying>,
    /// Linear 0..1 over the transition's duration.
    pub progress: f32,
}

/// Animated offsets for one piece of content during a transition.
#[derive(Clone, Copy)]
struct Phase {
    alpha: f32,
    /// Vertical offset in logical pixels.
    dy: f32,
    zoom: f32,
}

impl Phase {
    const REST: Self = Self { alpha: 1.0, dy: 0.0, zoom: 1.0 };

    /// Old content drifts up, shrinks and fades during the first half.
    fn outgoing(p: f32) -> Self {
        let e = ease_out_cubic((p / 0.45).clamp(0.0, 1.0));
        Self { alpha: 1.0 - e, dy: -8.0 * e, zoom: 1.0 - 0.15 * e }
    }

    /// New content rises from below and grows in with a slight overshoot,
    /// overlapping the end of the outgoing phase.
    fn incoming(p: f32) -> Self {
        let t = ((p - 0.2) / 0.8).clamp(0.0, 1.0);
        let e = ease_out_cubic(t);
        Self { alpha: e, dy: 10.0 * (1.0 - e), zoom: 0.85 + 0.15 * ease_out_back(t) }
    }
}

/// What to draw for one element: the outgoing and incoming values while it
/// is changing, otherwise just the current value at rest.
fn slots<'a>(
    media: Option<&'a NowPlaying>,
    transition: Option<&Transition<'a>>,
    changed: fn(Option<&NowPlaying>, Option<&NowPlaying>) -> bool,
) -> Vec<(Option<&'a NowPlaying>, Phase)> {
    match transition {
        Some(tr) if changed(tr.from, media) => vec![
            (tr.from, Phase::outgoing(tr.progress)),
            (media, Phase::incoming(tr.progress)),
        ],
        _ => vec![(media, Phase::REST)],
    }
}

fn presence_changed(a: Option<&NowPlaying>, b: Option<&NowPlaying>) -> bool {
    a.is_some() != b.is_some()
}

/// How far the notch is between collapsed (0) and expanded (1).
fn expansion(shape: Shape) -> f32 {
    ((shape.height - COLLAPSED.height) / (EXPANDED.height - COLLAPSED.height)).clamp(0.0, 1.0)
}

/// Room the shadow's canvas has beyond the notch's on the left, right and
/// bottom, in logical px: the shadow spreads further than the notch's
/// canvas reaches.
pub const SHADOW_PAD: f32 = 40.0;

/// Wide, faint shadow around the open notch, centered on a canvas
/// `SHADOW_PAD` larger than the notch's. It deepens as the notch opens;
/// returns false when there is none (the notch is at rest).
pub fn draw_shadow(pixmap: &mut Pixmap, scale: f32, shape: Shape) -> bool {
    pixmap.fill(Color::TRANSPARENT);
    let t = expansion(shape);
    if t <= 0.0 {
        return false;
    }
    // No blur in tiny-skia: each pixel's darkness comes from its distance
    // to the notch (a rounded rect, nudged down), easing out to nothing.
    const BLUR: f32 = 70.0;
    const DROP: f32 = 8.0;
    const DARKEST: f32 = 0.26;
    let (pw, ph) = (pixmap.width() as usize, pixmap.height() as usize);
    let canvas_w = pw as f32 / scale;
    // Rect center and half extents, in logical px; its top is off screen.
    let (cx, half_w) = (canvas_w / 2.0, shape.width / 2.0);
    let (top, bottom) = (-shape.radius, shape.height + DROP);
    let (cy, half_h) = ((top + bottom) / 2.0, (bottom - top) / 2.0);
    let r = shape.radius.min(half_w).min(half_h);
    let peak = DARKEST * t;

    // Only pixels within reach of the shadow.
    let x0 = (((cx - half_w - BLUR) * scale).floor().max(0.0)) as usize;
    let x1 = (((cx + half_w + BLUR) * scale).ceil() as usize).min(pw);
    let y1 = (((bottom + BLUR) * scale).ceil() as usize).min(ph);
    let data = pixmap.data_mut();
    for py in 0..y1 {
        let qy = ((py as f32 + 0.5) / scale - cy).abs() - half_h + r;
        for px in x0..x1 {
            let qx = ((px as f32 + 0.5) / scale - cx).abs() - half_w + r;
            let outside = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
            let u = (outside / BLUR).clamp(0.0, 1.0);
            // Fades fast near the notch, then trails off far and faint.
            let alpha = peak * (1.0 - u).powi(2);
            // Black, premultiplied: only alpha is set.
            data[(py * pw + px) * 4 + 3] = (alpha * 255.0).round() as u8;
        }
    }
    true
}

/// Draws the notch centered horizontally at the top of a transparent canvas.
pub fn draw(pixmap: &mut Pixmap, scale: f32, scene: &Scene, fonts: Option<&Fonts>) {
    let Scene { shape, media, .. } = *scene;
    let transition = scene.transition.as_ref();
    pixmap.fill(Color::TRANSPARENT);
    let canvas_w = pixmap.width() as f32 / scale;
    let ts = Transform::from_scale(scale, scale);
    let Some(path) = notch_path(canvas_w, shape) else { return };

    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 255);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, FillRule::Winding, ts, None);

    // An alert's content shows as the notch grows into the alert shape, and
    // fades again if it keeps growing into the open notch.
    let alert_alpha = match &scene.alert {
        Some(alert) => {
            let target = alert_shape(alert);
            let rest_width = if scene.battery.is_some() { COLLAPSED_BATTERY_WIDTH } else { COLLAPSED.width };
            // How far along each dimension that changes; both must get there.
            let progress = |now: f32, from: f32, to: f32| {
                if (to - from).abs() > 1.0 { (now - from) / (to - from) } else { 1.0 }
            };
            let grown = progress(shape.width, rest_width, target.width)
                .min(progress(shape.height, COLLAPSED.height, target.height));
            let past = (shape.height - target.height) / 20.0;
            grown.clamp(0.0, 1.0) * (1.0 - past.clamp(0.0, 1.0))
        }
        None => 0.0,
    };

    // The open notch's content and gradient; an alert tall enough to reach
    // into its range (a notification card) keeps them hidden.
    let t = expansion(shape);
    let expanded_alpha = ((t - 0.35) / 0.65).clamp(0.0, 1.0) * (1.0 - alert_alpha);
    // The cover gradient belongs to the music page with lyrics showing; it
    // fades with the lyrics panel and scrolls away with the page.
    let page = scene.page.clamp(0.0, 1.0);
    // The output picker covers the player, gradient and all.
    let picker = scene.outputs.reveal.clamp(0.0, 1.0);
    let gradient_alpha = expanded_alpha * scene.lyrics_reveal.clamp(0.0, 1.0) * (1.0 - picker);
    if gradient_alpha > 0.0 && scene.page < 1.0 {
        let shift = scene.page * PAGE_TRAVEL;
        background(pixmap, &path, ts, shape.height, shift, media, transition, gradient_alpha);
    }

    // Content is clipped to the notch so it is revealed as the shape grows.
    let Some(mut clip) = Mask::new(pixmap.width(), pixmap.height()) else { return };
    clip.fill_path(&path, FillRule::Winding, true, ts);

    let mut canvas = Canvas { pixmap, clip: &clip, scale, fonts, dy: 0.0 };
    let (left, right) = ((canvas_w - shape.width) / 2.0, (canvas_w + shape.width) / 2.0);

    // The resting content fades out as the notch shrinks into its mini pill.
    let rest_width = if scene.battery.is_some() { COLLAPSED_BATTERY_WIDTH } else { COLLAPSED.width };
    let unshrunk = ((shape.width - MINI.width) / (rest_width - MINI.width)).clamp(0.0, 1.0);
    let collapsed_alpha = (1.0 - t * 3.0).clamp(0.0, 1.0) * (1.0 - alert_alpha) * unshrunk * unshrunk;
    if collapsed_alpha > 0.0 {
        canvas.collapsed(media, transition, scene.battery, scene.bluetooth, scene.clock, left, right, collapsed_alpha);
    }

    if let Some(alert) = scene.alert.as_ref().filter(|_| alert_alpha > 0.0) {
        match &scene.alert_from {
            // Old content drifts up and fades, the new rises in after it,
            // like the track change.
            Some((from, p)) => {
                let (out, incoming) = (Phase::outgoing(*p), Phase::incoming(*p));
                canvas.dy = out.dy;
                canvas.alert(from, left, right, alert_alpha * out.alpha);
                canvas.dy = incoming.dy;
                canvas.alert(alert, left, right, alert_alpha * incoming.alpha);
                canvas.dy = 0.0;
            }
            None => canvas.alert(alert, left, right, alert_alpha),
        }
    }

    if expanded_alpha > 0.0 {
        let layout = Layout::new(canvas_w, scene.panel_width, scene.lyrics_reveal);
        // Pages sit one above the other, Control Center on top, and scroll
        // as one, a full page per turn, the notch's edge cutting them off;
        // no fading.
        // Positions follow the raw value, which a touchpad can stretch a
        // little past either page (rubber band).
        let raw = scene.page;
        let p = page;
        if raw < 1.0 {
            let a = expanded_alpha;
            canvas.dy = raw * PAGE_TRAVEL;
            // The player dims away behind the sheet growing over it.
            let fade = (1.0 - picker * 1.3).clamp(0.0, 1.0);
            if fade > 0.0 {
                canvas.expanded(media, transition, &scene.lyrics, scene.seek, scene.seek_grow, layout, a * fade);
            }
            if scene.outputs.reveal > 0.001 {
                canvas.output_picker(&scene.outputs, layout, a);
            }
        }
        if raw > 0.0 {
            canvas.dy = -(1.0 - raw) * PAGE_TRAVEL;
            canvas.control_center(&scene.controls, layout.left, layout.width, expanded_alpha);
        }
        canvas.dy = 0.0;
        canvas.page_dots(layout.left + layout.width - 10.0, p, expanded_alpha);
    }
}

/// Paints the album-art gradient over the black notch, black at the top
/// fading into the art's tint at the bottom. Crossfades to the new tint when
/// the art changes.
fn background(
    pixmap: &mut Pixmap,
    path: &Path,
    ts: Transform,
    height: f32,
    shift: f32,
    media: Option<&NowPlaying>,
    transition: Option<&Transition>,
    alpha: f32,
) {
    let colors = |np: Option<&NowPlaying>| np.and_then(|np| np.art.as_ref()).map(|art| art.tint);
    let current = colors(media);
    let mut paint_gradient = |tint: Rgb, a: f32| {
        let alpha = (a * 255.0) as u8;
        let black = Color::from_rgba8(0, 0, 0, alpha);
        let clear = Color::from_rgba8(tint[0], tint[1], tint[2], 0);
        let tint = Color::from_rgba8(tint[0], tint[1], tint[2], alpha);
        // Moves with the page; outside it (the other page scrolling in) is
        // left clear.
        let span = height + 2.0;
        let (start, end) = (1.0 / span, (height + 1.0) / span);
        let shader = LinearGradient::new(
            Point::from_xy(0.0, shift - 1.0),
            Point::from_xy(0.0, shift + height + 1.0),
            // Solid black across the top third, where the notch meets the bezel.
            vec![
                GradientStop::new(0.0, clear),
                GradientStop::new(start, black),
                GradientStop::new(start + 0.3 * height / span, black),
                GradientStop::new(end, tint),
                GradientStop::new(1.0, clear),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shader {
            let paint = Paint { shader, anti_alias: true, ..Default::default() };
            pixmap.fill_path(path, &paint, FillRule::Winding, ts, None);
        }
    };

    match transition {
        Some(tr) if art_changed(tr.from, media) => {
            let e = ease_out_cubic(tr.progress);
            // The old gradient stays opaque under the new one so the
            // crossfade does not dip through black; it only fades out when
            // there is nothing to replace it.
            if let Some(old) = colors(tr.from) {
                let fade = if current.is_some() { 1.0 } else { 1.0 - e };
                paint_gradient(old, alpha * fade);
            }
            if let Some(new) = current {
                paint_gradient(new, alpha * e);
            }
        }
        _ => {
            if let Some(stops) = current {
                paint_gradient(stops, alpha);
            }
        }
    }
}

/// Which media button, if any, is under a point in physical canvas pixels.
pub fn button_at(
    canvas_width: u32,
    scale: f32,
    panel_width: f32,
    lyrics_reveal: f32,
    x: f32,
    y: f32,
) -> Option<Button> {
    let (x, y) = (x / scale, y / scale);
    let layout = Layout::new(canvas_width as f32 / scale, panel_width, lyrics_reveal);
    let left = layout.left + layout.controls_dx();
    [
        (Button::Lyrics, LYRICS_BUTTON_X),
        (Button::Media(Command::Previous), PREVIOUS_X),
        (Button::Media(Command::TogglePlayPause), PLAY_X),
        (Button::Media(Command::Next), NEXT_X),
        (Button::Output, OUTPUT_BUTTON_X),
    ]
    .into_iter()
    .find(|&(_, cx)| (x - left - cx).hypot(y - CONTROLS_Y) <= BUTTON_HIT_RADIUS)
    .map(|(button, _)| button)
}

/// Left and right ends of the output picker's rows in an open notch at
/// `left`, `width` wide; clear of the page dots.
fn output_row_span(left: f32, width: f32) -> (f32, f32) {
    (left + 16.0, left + width - 24.0)
}

/// Which output picker row is under a point in physical canvas pixels;
/// `None` off the rows (which closes the picker).
pub fn output_at(canvas_width: u32, scale: f32, panel_width: f32, scroll: f32, count: usize, (x, y): (f32, f32)) -> Option<usize> {
    let (x, y) = (x / scale, y / scale);
    let left = (canvas_width as f32 / scale - panel_width) / 2.0;
    let (x0, x1) = output_row_span(left, panel_width);
    let bottom = OUTPUT_TOP + OUTPUT_ROW_H * OUTPUT_ROWS_SHOWN as f32;
    if x < x0 || x > x1 || y < OUTPUT_TOP || y >= bottom {
        return None;
    }
    let index = ((y - OUTPUT_TOP) / OUTPUT_ROW_H + scroll).floor();
    (index >= 0.0 && (index as usize) < count).then_some(index as usize)
}

/// How far (logical px) a page travels while sliding in or out.
const PAGE_TRAVEL: f32 = EXPANDED.height;

// Control Center layout, relative to the open notch's left edge, after iOS:
// a connectivity module with four round toggles, two tall sliders, and on
// the right a Now Playing module over a battery ring and a Bluetooth module.
const CC_TOP: f32 = 12.0;
const CC_HEIGHT: f32 = 126.0;
const PLATTER_X: f32 = 16.0;
const PLATTER: f32 = 126.0;
const TOGGLE_RADIUS: f32 = 23.0;
const SLIDER_W: f32 = 52.0;
const BRIGHTNESS_X: f32 = 156.0;
const VOLUME_X: f32 = 220.0;
const MODULES_X: f32 = 286.0;
const MODULE_GAP: f32 = 10.0;
/// Each of the right-hand rows: Now Playing, then battery + Bluetooth.
const MODULE_H: f32 = (CC_HEIGHT - MODULE_GAP) / 2.0;
const MODULE_RADIUS: f32 = 20.0;
/// Narrower than this, the right-hand modules are left out.
const MODULES_MIN_W: f32 = 90.0;
/// Width of the Now Playing module's play/pause hit area, at its right.
const MEDIA_BUTTON_W: f32 = 44.0;

const ORANGE: Rgb = [255, 159, 10];
const BLUE: Rgb = [10, 132, 255];
/// A switched-off toggle: a light grey disc on the module.
const TOGGLE_OFF: Rgb = [62, 62, 66];

/// Toggles in the platter's 2x2 grid, with their centers.
fn toggles() -> [(Toggle, f32, f32); 4] {
    let (a, b) = (PLATTER_X + 35.0, PLATTER_X + 91.0);
    let (top, bottom) = (CC_TOP + 35.0, CC_TOP + 91.0);
    let centers = [(a, top), (b, top), (a, bottom), (b, bottom)];
    Toggle::ALL.map(|t| (t, centers[t.index()].0, centers[t.index()].1))
}

/// Width of the right-hand modules column in an open notch `width` wide.
fn modules_width(width: f32) -> f32 {
    width - MODULES_X - 16.0
}

/// A circular arc of radius `r` around (cx, cy), from `start` degrees (0 =
/// right, -90 = top) sweeping `sweep` degrees clockwise.
fn arc(cx: f32, cy: f32, r: f32, start: f32, sweep: f32) -> Option<Path> {
    if sweep.abs() < 0.5 {
        return None;
    }
    // Cubic segments of at most 90°, each with control points at
    // 4/3 · tan(θ/4) of the radius along the tangents.
    let segments = (sweep.abs() / 90.0).ceil() as usize;
    let step = (sweep / segments as f32).to_radians();
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let mut angle = start.to_radians();
    let mut pb = PathBuilder::new();
    pb.move_to(cx + r * angle.cos(), cy + r * angle.sin());
    for _ in 0..segments {
        let next = angle + step;
        let (s0, c0) = angle.sin_cos();
        let (s1, c1) = next.sin_cos();
        pb.cubic_to(
            cx + r * (c0 - k * s0),
            cy + r * (s0 + k * c0),
            cx + r * (c1 + k * s1),
            cy + r * (s1 - k * c1),
            cx + r * c1,
            cy + r * s1,
        );
        angle = next;
    }
    pb.finish()
}

fn lerp_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// What on the Control Center page is under a point in physical canvas
/// pixels.
pub fn control_at(canvas_width: u32, scale: f32, panel_width: f32, (x, y): (f32, f32)) -> Option<ControlHit> {
    let left = (canvas_width as f32 / scale - panel_width) / 2.0;
    let (x, y) = (x / scale - left, y / scale);
    if let Some((toggle, _, _)) = toggles().into_iter().find(|&(_, cx, cy)| (x - cx).hypot(y - cy) <= TOGGLE_RADIUS) {
        return Some(ControlHit::Toggle(toggle));
    }
    let modules_w = modules_width(panel_width);
    let media_right = MODULES_X + modules_w;
    let on_media_button = x >= media_right - MEDIA_BUTTON_W && x <= media_right && y >= CC_TOP && y <= CC_TOP + MODULE_H;
    if modules_w >= MODULES_MIN_W && on_media_button {
        return Some(ControlHit::PlayPause);
    }
    let in_column = |x0: f32| x >= x0 && x <= x0 + SLIDER_W && y >= CC_TOP && y <= CC_TOP + CC_HEIGHT;
    let slider = if in_column(BRIGHTNESS_X) {
        Slider::Brightness
    } else if in_column(VOLUME_X) {
        Slider::Volume
    } else {
        return None;
    };
    Some(ControlHit::Slider(slider, slider_value(scale, y * scale)))
}

/// A slider's value (0..1) at a physical y, clamped; for dragging past
/// either end.
pub fn slider_value(scale: f32, y: f32) -> f32 {
    (1.0 - (y / scale - CC_TOP) / CC_HEIGHT).clamp(0.0, 1.0)
}

/// Where along the progress bar (0..1) a point in physical canvas pixels
/// falls. With `grab`, only points on or near the bar count (starting a
/// drag); without, any point maps to its clamped horizontal position
/// (continuing one).
pub fn progress_at(
    canvas_width: u32,
    scale: f32,
    panel_width: f32,
    lyrics_reveal: f32,
    (x, y): (f32, f32),
    grab: bool,
) -> Option<f32> {
    let (x, y) = (x / scale, y / scale);
    let layout = Layout::new(canvas_width as f32 / scale, panel_width, lyrics_reveal);
    let (x0, x1) = layout.progress_span();
    // The bar is 3 px tall; give it a comfortable grab area.
    const SLOP: f32 = 8.0;
    if grab && ((y - PROGRESS_Y).abs() > SLOP || x < x0 - SLOP || x > x1 + SLOP) {
        return None;
    }
    Some(((x - x0) / (x1 - x0)).clamp(0.0, 1.0))
}

/// Which synced lyric line (index into `line_count` lines) is under a point
/// in physical canvas pixels, with the lyrics scrolled to `scroll`.
pub fn lyric_at(
    canvas_width: u32,
    scale: f32,
    panel_width: f32,
    lyrics_reveal: f32,
    scroll: f32,
    line_count: usize,
    (x, y): (f32, f32),
) -> Option<usize> {
    let (x, y) = (x / scale, y / scale);
    let layout = Layout::new(canvas_width as f32 / scale, panel_width, lyrics_reveal);
    let in_panel = x >= layout.left + layout.player && x <= layout.left + layout.width;
    // Only the visible part of the panel, where lines have not faded out.
    if layout.lyrics < 0.9 || !in_panel || !(18.0..=EXPANDED.height - 8.0).contains(&y) {
        return None;
    }
    // Lines sit on their baselines; their text is centered ~5 px above.
    let d = (y - (LYRIC_CURRENT_BASELINE - 5.0)) / LYRIC_LINE_H;
    let index = (scroll + d).round();
    (index >= 0.0 && (index as usize) < line_count).then_some(index as usize)
}

/// "mm:ss", or "h:mm:ss" past an hour.
fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn notch_path(canvas_w: f32, shape: Shape) -> Option<Path> {
    // Shoulders shrink with a very flat notch (the mini pill) so they never
    // swallow its rounded corners.
    let (w, h, s) = (shape.width, shape.height, SHOULDER.min(shape.height * 0.4));
    if w <= 0.0 || h <= s {
        return None;
    }
    let r = shape.radius.min(w / 2.0).min(h - s).max(0.0);
    let k = r * KAPPA;
    let x = (canvas_w - w) / 2.0;

    let mut pb = PathBuilder::new();
    pb.move_to(x - s, 0.0);
    pb.quad_to(x, 0.0, x, s);
    pb.line_to(x, h - r);
    pb.cubic_to(x, h - r + k, x + r - k, h, x + r, h);
    pb.line_to(x + w - r, h);
    pb.cubic_to(x + w - r + k, h, x + w, h - r + k, x + w, h - r);
    pb.line_to(x + w, s);
    pb.quad_to(x + w, 0.0, x + w + s, 0.0);
    pb.close();
    pb.finish()
}

const TOAST_ICON: f32 = 44.0;

/// Where things sit on a notification card between `left` and `right`.
struct ToastLayout {
    icon_x: f32,
    icon_y: f32,
    text_x: f32,
    text_right: f32,
}

impl ToastLayout {
    fn new(left: f32, right: f32) -> Self {
        let (icon_x, icon_y) = (left + 20.0, 25.0);
        Self { icon_x, icon_y, text_x: icon_x + TOAST_ICON + 14.0, text_right: right - 22.0 }
    }
}

/// Cubic Bezier approximation of a quarter circle.
const KAPPA: f32 = 0.552_284_8;

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * KAPPA;
    let (rt, b) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(rt - r, y);
    pb.cubic_to(rt - r + k, y, rt, y + r - k, rt, y + r);
    pb.line_to(rt, b - r);
    pb.cubic_to(rt, b - r + k, rt - r + k, b, rt - r, b);
    pb.line_to(x + r, b);
    pb.cubic_to(x + r - k, b, x, b - r + k, x, b - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

struct Canvas<'a> {
    pixmap: &'a mut Pixmap,
    clip: &'a Mask,
    scale: f32,
    fonts: Option<&'a Fonts>,
    /// Vertical offset (logical px) for everything drawn; slides pages.
    dy: f32,
}

impl Canvas<'_> {
    /// Logical to physical pixels, with the page offset.
    fn transform(&self) -> Transform {
        Transform::from_row(self.scale, 0.0, 0.0, self.scale, 0.0, self.dy * self.scale)
    }

    /// The Control Center page: connectivity module, brightness and volume
    /// sliders, and the Now Playing, battery and Bluetooth modules if there
    /// is room.
    fn control_center(&mut self, view: &ControlsView, left: f32, width: f32, alpha: f32) {
        if let Some(platter) = rounded_rect(left + PLATTER_X, CC_TOP, PLATTER, CC_HEIGHT, 28.0) {
            self.glass(&platter, alpha);
        }
        let s = view.state;
        for (toggle, cx, cy) in toggles() {
            let (available, color) = match toggle {
                Toggle::Airplane => (s.wifi.is_some() || s.bluetooth.is_some(), ORANGE),
                Toggle::Wifi => (s.wifi.is_some(), BLUE),
                Toggle::Bluetooth => (s.bluetooth.is_some(), BLUE),
                Toggle::Mute => (true, RED),
            };
            let i = toggle.index();
            let a = alpha * if available { 1.0 } else { 0.4 };
            // The disc eases between grey and the toggle's color, and dips
            // in size while pressed.
            let radius = TOGGLE_RADIUS * view.toggle_scale[i];
            if let Some(circle) = PathBuilder::from_circle(left + cx, cy, radius) {
                self.fill(&circle, lerp_rgb(TOGGLE_OFF, color, view.toggle_on[i]), a);
            }
            let (x, y) = (left + cx, cy);
            match toggle {
                Toggle::Airplane => self.airplane_icon(x, y, a),
                Toggle::Wifi => self.wifi_icon(x, y + 1.0, a),
                Toggle::Bluetooth => self.device_icon(DeviceKind::Other, x, y, a),
                Toggle::Mute => self.speaker_icon(x - 6.0, y, if view.muted { 0.0 } else { 1.0 }, view.muted, a),
            }
        }

        let brightness = view.brightness.or(s.brightness.map(|b| b as f32 / 100.0));
        self.slider(left + BRIGHTNESS_X, brightness, Slider::Brightness, false, alpha);
        self.slider(left + VOLUME_X, Some(view.volume), Slider::Volume, view.muted, alpha);

        let modules_w = modules_width(width);
        if modules_w >= MODULES_MIN_W {
            let x = left + MODULES_X;
            let lower = CC_TOP + MODULE_H + MODULE_GAP;
            self.media_module(view.media, x, modules_w, alpha);
            self.battery_module(view.battery, x, lower, alpha);
            let bt_x = x + MODULE_H + MODULE_GAP;
            self.bluetooth_module(view, bt_x, lower, modules_w - MODULE_H - MODULE_GAP, alpha);
        }
    }

    /// A Control Center module's surface: translucent, with a faint rim of
    /// light so it reads as a raised pane of glass.
    fn glass(&mut self, path: &Path, alpha: f32) {
        self.fill(path, WHITE, alpha * 0.10);
        self.stroke(path, 1.0, WHITE, alpha * 0.08);
    }

    /// Cover, title and artist, and a play/pause button; like the media
    /// module in Control Center.
    fn media_module(&mut self, media: Option<&NowPlaying>, x: f32, w: f32, alpha: f32) {
        let Some(module) = rounded_rect(x, CC_TOP, w, MODULE_H, MODULE_RADIUS) else { return };
        self.glass(&module, alpha);
        let mid = CC_TOP + MODULE_H / 2.0;
        let art = 38.0;
        let art_x = x + (MODULE_H - art) / 2.0;
        match media.and_then(|np| np.art.as_ref()) {
            Some(a) => self.art(&a.pixmap, art_x, mid - art / 2.0, art, 9.0, alpha, 1.0),
            None => {
                if let Some(tile) = rounded_rect(art_x, mid - art / 2.0, art, art, 9.0) {
                    self.fill(&tile, WHITE, alpha * 0.12);
                }
            }
        }

        let button_x = x + w - MEDIA_BUTTON_W / 2.0;
        let playing = media.is_some_and(|np| np.playing);
        let button_alpha = alpha * if media.is_some() { 1.0 } else { 0.35 };
        if playing {
            self.pause_icon(button_x, mid, button_alpha);
        } else {
            self.play_icon(button_x, mid, button_alpha);
        }

        let Some(fonts) = self.fonts else { return };
        let text_x = art_x + art + 10.0;
        let max = x + w - MEDIA_BUTTON_W - text_x;
        if max < 40.0 {
            return;
        }
        match media {
            Some(np) => {
                self.text(&fonts.semibold, &np.title, 12.5, text_x, mid - 2.0, max, WHITE, alpha);
                self.text(&fonts.regular, &np.artist, 11.0, text_x, mid + 12.0, max, GRAY, alpha);
            }
            None => self.text(&fonts.semibold, "Not Playing", 12.5, text_x, mid + 4.5, max, DIM, alpha),
        }
    }

    /// A ring that fills with the charge, the percentage inside; like the
    /// iOS batteries widget.
    fn battery_module(&mut self, battery: Option<Battery>, x: f32, y: f32, alpha: f32) {
        let Some(module) = rounded_rect(x, y, MODULE_H, MODULE_H, MODULE_RADIUS) else { return };
        self.glass(&module, alpha);
        let (cx, cy, r) = (x + MODULE_H / 2.0, y + MODULE_H / 2.0, 18.0);
        if let Some(track) = PathBuilder::from_circle(cx, cy, r) {
            self.stroke(&track, 4.0, WHITE, alpha * 0.15);
        }
        let Some(fonts) = self.fonts else { return };
        let Some(b) = battery else {
            self.text_centered(&fonts.semibold, "—", 12.0, cx, cy + 4.0, DIM, alpha);
            return;
        };
        // Green as on iOS, unless low and not charging.
        let color = if !b.plugged && b.percent <= 20 { battery_color(b) } else { GREEN };
        let sweep = 360.0 * b.percent as f32 / 100.0;
        if let Some(level) = arc(cx, cy, r, -90.0, sweep) {
            self.stroke(&level, 4.0, color, alpha);
        }
        let text = b.percent.to_string();
        if b.plugged {
            // Number nudged up to make room for the bolt below it.
            self.text_centered(&fonts.semibold, &text, 12.0, cx, cy + 1.5, WHITE, alpha);
            self.bolt(cx - 3.0, cy + 4.0, 0.5, color, alpha);
        } else {
            self.text_centered(&fonts.semibold, &text, 12.0, cx, cy + 4.0, WHITE, alpha);
        }
    }

    /// The connected Bluetooth device's icon and name, or the radio's state.
    fn bluetooth_module(&mut self, view: &ControlsView, x: f32, y: f32, w: f32, alpha: f32) {
        let Some(module) = rounded_rect(x, y, w, MODULE_H, MODULE_RADIUS) else { return };
        self.glass(&module, alpha);
        let mid = y + MODULE_H / 2.0;
        let device = view.bluetooth_device.filter(|_| view.state.bluetooth == Some(true));
        let kind = device.map_or(DeviceKind::Other, |d| d.kind);
        let icon_alpha = alpha * if device.is_some() { 1.0 } else { 0.45 };

        // Too narrow for words: just the icon, centered.
        let fonts = self.fonts.filter(|_| w >= 100.0);
        let icon_x = if fonts.is_some() { x + 22.0 } else { x + w / 2.0 };
        if device.is_some() {
            // A blue disc behind the icon marks it connected.
            if let Some(disc) = PathBuilder::from_circle(icon_x, mid, 14.0) {
                self.fill(&disc, BLUE, alpha);
            }
        }
        self.device_icon(kind, icon_x, mid, icon_alpha);
        let Some(fonts) = fonts else { return };
        let (title, detail) = match (device, view.state.bluetooth) {
            (Some(d), _) => (d.name.as_str(), "Connected"),
            (None, Some(true)) => ("Bluetooth", "Not connected"),
            (None, Some(false)) => ("Bluetooth", "Off"),
            (None, None) => ("Bluetooth", "Unavailable"),
        };
        let text_x = icon_x + 22.0;
        let max = x + w - 12.0 - text_x;
        self.text(&fonts.semibold, title, 12.5, text_x, mid - 2.0, max, WHITE, alpha);
        self.text(&fonts.regular, detail, 11.0, text_x, mid + 12.0, max, GRAY, alpha);
    }

    /// A tall capsule that fills from the bottom, its symbol at the foot.
    /// `None` (no software brightness) draws it empty and dimmed.
    fn slider(&mut self, x: f32, value: Option<f32>, kind: Slider, muted: bool, alpha: f32) {
        let Some(capsule) = rounded_rect(x, CC_TOP, SLIDER_W, CC_HEIGHT, 24.0) else { return };
        let alpha = alpha * if value.is_some() { 1.0 } else { 0.4 };
        self.fill(&capsule, WHITE, alpha * 0.16);

        let v = value.unwrap_or(0.0).clamp(0.0, 1.0);
        let level_h = CC_HEIGHT * v;
        if level_h > 0.5 {
            // The level has a straight top edge, clipped to the capsule.
            let top = CC_TOP + CC_HEIGHT - level_h;
            if let Some(level) = Rect::from_xywh(x, top, SLIDER_W, level_h) {
                self.fill_within(&PathBuilder::from_rect(level), &capsule, WHITE, alpha * 0.95);
            }
        }

        // The symbol turns dark once the white level covers it.
        let (cx, cy) = (x + SLIDER_W / 2.0, CC_TOP + CC_HEIGHT - 20.0);
        let covered = level_h > 30.0;
        let a = alpha * if covered { 0.75 } else { 1.0 };
        match kind {
            Slider::Brightness => self.sun_icon(cx, cy, covered, a),
            Slider::Volume if covered => self.dark_speaker(cx - 6.0, cy, v, muted, a),
            Slider::Volume => self.speaker_icon(cx - 6.0, cy, v, muted, a),
        }
    }

    /// `speaker_icon` in near-black, for on top of a white slider level.
    fn dark_speaker(&mut self, x: f32, cy: f32, level: f32, muted: bool, alpha: f32) {
        // Drawn into a scratch layer, then tinted: the speaker paths are
        // white-only, and this keeps them in one place.
        let Some(mut layer) = Pixmap::new(self.pixmap.width(), self.pixmap.height()) else { return };
        let mut scratch = Canvas { pixmap: &mut layer, clip: self.clip, scale: self.scale, fonts: self.fonts, dy: self.dy };
        scratch.speaker_icon(x, cy, level, muted, 1.0);
        for px in layer.data_mut().chunks_exact_mut(4) {
            // Premultiplied white becomes premultiplied near-black.
            let a = px[3] as f32 / 255.0;
            let dark = (40.0 * a) as u8;
            px[0] = dark;
            px[1] = dark;
            px[2] = dark;
        }
        let paint = tiny_skia::PixmapPaint { opacity: alpha, ..Default::default() };
        self.pixmap.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
    }

    /// Two vertical dots on the right edge, the current page's bright.
    fn page_dots(&mut self, x: f32, page: f32, alpha: f32) {
        for (i, y) in [70.0, 80.0].into_iter().enumerate() {
            // Brightness follows the slide, so the dots cross over with it.
            // Top dot: Control Center, the page above.
            let current = if i == 0 { page } else { 1.0 - page };
            if let Some(dot) = PathBuilder::from_circle(x, y, 2.5) {
                self.fill(&dot, WHITE, alpha * (0.3 + 0.6 * current));
            }
        }
    }

    /// Fills `path` clipped to `within` as well as to the notch.
    fn fill_within(&mut self, path: &Path, within: &Path, rgb: Rgb, alpha: f32) {
        let Some(mut mask) = Mask::new(self.pixmap.width(), self.pixmap.height()) else { return };
        mask.fill_path(within, FillRule::Winding, true, self.transform());
        for (m, c) in mask.data_mut().iter_mut().zip(self.clip.data()) {
            *m = ((*m as u16 * *c as u16) / 255) as u8;
        }
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8));
        paint.anti_alias = true;
        self.pixmap.fill_path(path, &paint, FillRule::Winding, self.transform(), Some(&mask));
    }

    /// SF-style airplane, nose up, about 15 px tall.
    fn airplane_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        let half = [
            (0.0, -7.6),
            (1.4, -6.0),
            (1.4, -1.8),
            (7.4, 1.9),
            (7.4, 3.4),
            (1.4, 1.8),
            (1.4, 4.9),
            (3.5, 6.5),
            (3.5, 7.6),
            (0.0, 6.7),
        ];
        let mut pb = PathBuilder::new();
        // Right half going down, then mirrored back up the left side.
        let outline = half.iter().copied().chain(half.iter().rev().skip(1).map(|&(x, y)| (-x, y)));
        for (i, (x, y)) in outline.enumerate() {
            if i == 0 { pb.move_to(cx + x, cy + y) } else { pb.line_to(cx + x, cy + y) }
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.fill(&path, WHITE, alpha);
        }
    }

    /// Three arcs over a dot.
    fn wifi_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        let base = cy + 5.0;
        for r in [4.0, 7.5, 11.0] {
            // Upward 90° arc; quadratic control point at r / cos(45°).
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            let mut pb = PathBuilder::new();
            pb.move_to(cx - d, base - d);
            pb.quad_to(cx, base - r * std::f32::consts::SQRT_2, cx + d, base - d);
            if let Some(arc) = pb.finish() {
                self.stroke(&arc, 1.8, WHITE, alpha);
            }
        }
        if let Some(dot) = PathBuilder::from_circle(cx, base, 1.6) {
            self.fill(&dot, WHITE, alpha);
        }
    }

    /// A sun: disc and eight rays. Dark when on a white slider level.
    fn sun_icon(&mut self, cx: f32, cy: f32, dark: bool, alpha: f32) {
        let rgb = if dark { [40, 40, 40] } else { WHITE };
        if let Some(disc) = PathBuilder::from_circle(cx, cy, 3.4) {
            self.fill(&disc, rgb, alpha);
        }
        let mut pb = PathBuilder::new();
        for i in 0..8 {
            let angle = i as f32 * std::f32::consts::FRAC_PI_4;
            let (sin, cos) = angle.sin_cos();
            pb.move_to(cx + cos * 5.6, cy + sin * 5.6);
            pb.line_to(cx + cos * 7.6, cy + sin * 7.6);
        }
        if let Some(rays) = pb.finish() {
            self.stroke(&rays, 1.5, rgb, alpha);
        }
    }

    /// Mini art and equalizer bars; the battery, if any, takes the right
    /// side and pushes the equalizer next to the art. A connected Bluetooth
    /// device's icon sits just left of whatever is rightmost.
    #[allow(clippy::too_many_arguments)]
    fn collapsed(
        &mut self,
        media: Option<&NowPlaying>,
        transition: Option<&Transition>,
        battery: Option<Battery>,
        bluetooth: Option<DeviceKind>,
        clock: f32,
        left: f32,
        right: f32,
        alpha: f32,
    ) {
        let mid = COLLAPSED.height / 2.0;
        if let Some(kind) = bluetooth {
            let rightmost_left = match battery {
                Some(b) => right - 14.0 - Self::battery_glyph_width(13.0, b.plugged),
                // Without a battery the equalizer is the rightmost item.
                None if media.is_some() => right - 27.0,
                None => right - 5.0,
            };
            self.device_icon(kind, rightmost_left - 9.0 - 7.0, mid, alpha * 0.9);
        }
        for (slot, phase) in slots(media, transition, art_changed) {
            if let Some(art) = slot.and_then(|np| np.art.as_ref()) {
                self.art(&art.pixmap, left + 12.0, mid - 10.0, 20.0, 5.0, alpha * phase.alpha, phase.zoom);
            }
        }
        for (slot, phase) in slots(media, transition, presence_changed) {
            let Some(np) = slot else { continue };
            let color = if np.playing { GREEN } else { GRAY };
            let x0 = match battery {
                Some(_) if np.art.is_some() => left + 40.0,
                Some(_) => left + 12.0,
                None => right - 27.0,
            };
            for (i, rest) in [8.0, 14.0, 10.0].into_iter().enumerate() {
                // While playing, each bar mixes two waves at its own rates so
                // the bounce never visibly repeats; paused, they hold still.
                let h = if np.playing {
                    let f = i as f32;
                    let wave = (clock * (7.0 + f * 1.9) + f * 2.1).sin() * 0.6
                        + (clock * (11.3 - f * 2.3) + f * 0.7).sin() * 0.4;
                    9.0 + 6.0 * wave
                } else {
                    rest
                };
                let x = x0 + i as f32 * 5.0;
                if let Some(bar) = rounded_rect(x, mid - h / 2.0, 3.0, h, 1.5) {
                    self.fill(&bar, color, alpha * phase.alpha);
                }
            }
        }

        let Some(b) = battery else { return };
        let h = 13.0;
        let x = right - 14.0 - Self::battery_glyph_width(h, b.plugged);
        self.battery_glyph(x, mid - h / 2.0, h, b, alpha);
    }

    fn alert(&mut self, alert: &Alert, left: f32, right: f32, alpha: f32) {
        match alert {
            Alert::Battery { event, battery } => self.battery_alert(*event, *battery, left, right, alpha),
            Alert::Volume { level, muted } => self.volume_alert(*level, *muted, left, right, alpha),
            Alert::Toast(toast) => self.toast_alert(toast, left, right, alpha),
            Alert::Bluetooth { device, connected } => self.bluetooth_alert(device, *connected, left, right, alpha),
            Alert::NowPlaying { track, since } => {
                self.now_playing_alert(track, since.elapsed().as_secs_f32(), left, right, alpha)
            }
        }
    }

    /// Cover on the left, title and artist beside it, and an equalizer
    /// dancing in the cover's color on the right. `t` is seconds shown.
    fn now_playing_alert(&mut self, np: &NowPlaying, t: f32, left: f32, right: f32, alpha: f32) {
        // A little below center: the top edge meets the bezel.
        let mid = NOW_PLAYING_ALERT.height / 2.0 + 2.0;
        let art = 40.0;
        let (art_x, art_y) = (left + 16.0, mid - art / 2.0);
        match &np.art {
            Some(a) => self.art(&a.pixmap, art_x, art_y, art, 10.0, alpha, 1.0),
            None => {
                if let Some(tile) = rounded_rect(art_x, art_y, art, art, 10.0) {
                    self.fill(&tile, [60, 60, 66], alpha);
                }
            }
        }

        let (bar_w, gap, bars) = (3.5, 3.0, 4);
        let eq_w = bars as f32 * bar_w + (bars - 1) as f32 * gap;
        let eq_x = right - 22.0 - eq_w;
        let color = np.art.as_ref().map_or(GREEN, |a| vivid(a.tint));
        for i in 0..bars {
            // Each bar bounces at its own rate; a paused track rests low.
            let swing = if np.playing { 0.5 + 0.5 * (t * (5.0 + i as f32 * 1.7) + i as f32 * 1.3).sin() } else { 0.0 };
            let h = 5.0 + 17.0 * swing;
            let x = eq_x + i as f32 * (bar_w + gap);
            if let Some(bar) = rounded_rect(x, mid - h / 2.0, bar_w, h, bar_w / 2.0) {
                self.fill(&bar, color, alpha);
            }
        }

        let Some(fonts) = self.fonts else { return };
        let x = art_x + art + 12.0;
        let max = eq_x - 14.0 - x;
        self.text(&fonts.semibold, &np.title, 14.0, x, mid - 2.0, max, WHITE, alpha);
        self.text(&fonts.regular, &np.artist, 12.5, x, mid + 14.0, max, GRAY, alpha);
    }

    /// Device icon and name on the left, "Connected"/"Disconnected" on the right.
    fn bluetooth_alert(&mut self, device: &Device, connected: bool, left: f32, right: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let mid = BLUETOOTH_ALERT.height / 2.0;
        self.device_icon(device.kind, left + 27.0, mid, alpha);

        let (status, color) = if connected { ("Connected", GREEN) } else { ("Disconnected", GRAY) };
        let status_right = right - 20.0;
        self.text_right(&fonts.semibold, status, 13.0, status_right, mid + 4.5, color, alpha);

        let x = left + 44.0;
        let status_w = text_width(&fonts.semibold, status, 13.0 * self.scale) / self.scale;
        let max = status_right - status_w - 12.0 - x;
        self.text(&fonts.semibold, &device.name, 13.5, x, mid + 4.5, max, WHITE, alpha);
    }

    /// A small line icon for the device's kind, about 14 px, centered on
    /// (cx, cy). Unknown kinds get the Bluetooth rune.
    fn device_icon(&mut self, kind: DeviceKind, cx: f32, cy: f32, alpha: f32) {
        let mut pb = PathBuilder::new();
        match kind {
            DeviceKind::Headphones => {
                // Headband: a half circle drawn as two quarter arcs.
                let (r, y) = (5.5, cy + 1.0);
                let k = r * KAPPA;
                pb.move_to(cx - r, y);
                pb.cubic_to(cx - r, y - k, cx - k, y - r, cx, y - r);
                pb.cubic_to(cx + k, y - r, cx + r, y - k, cx + r, y);
                if let Some(band) = pb.finish() {
                    self.stroke(&band, 1.6, WHITE, alpha);
                }
                for x in [cx - r - 1.3, cx + r - 1.9] {
                    if let Some(cup) = rounded_rect(x, y - 0.5, 3.2, 6.0, 1.3) {
                        self.fill(&cup, WHITE, alpha);
                    }
                }
                return;
            }
            DeviceKind::Keyboard => {
                if let Some(body) = rounded_rect(cx - 7.0, cy - 4.5, 14.0, 9.0, 2.0) {
                    pb.push_path(&body);
                }
                if let Some(path) = pb.finish() {
                    self.stroke(&path, 1.3, WHITE, alpha);
                }
                for i in 0..4 {
                    let x = cx - 4.6 + i as f32 * 2.6;
                    if let Some(key) = rounded_rect(x, cy - 2.0, 1.6, 1.6, 0.4) {
                        self.fill(&key, WHITE, alpha);
                    }
                }
                if let Some(space) = rounded_rect(cx - 3.0, cy + 1.2, 6.0, 1.5, 0.5) {
                    self.fill(&space, WHITE, alpha);
                }
                return;
            }
            DeviceKind::Mouse => {
                if let Some(body) = rounded_rect(cx - 4.0, cy - 6.5, 8.0, 13.0, 4.0) {
                    pb.push_path(&body);
                }
                pb.move_to(cx, cy - 6.5);
                pb.line_to(cx, cy - 2.5);
            }
            DeviceKind::Phone => {
                if let Some(body) = rounded_rect(cx - 4.0, cy - 7.0, 8.0, 14.0, 2.0) {
                    pb.push_path(&body);
                }
                pb.move_to(cx - 1.3, cy + 4.6);
                pb.line_to(cx + 1.3, cy + 4.6);
            }
            DeviceKind::Other => {
                pb.move_to(cx - 3.5, cy - 3.5);
                pb.line_to(cx + 3.5, cy + 3.2);
                pb.line_to(cx, cy + 6.8);
                pb.line_to(cx, cy - 6.8);
                pb.line_to(cx + 3.5, cy - 3.2);
                pb.line_to(cx - 3.5, cy + 3.5);
            }
        }
        if let Some(path) = pb.finish() {
            self.stroke(&path, 1.4, WHITE, alpha);
        }
    }

    /// iOS banner: a glow in the app's color behind its icon, the app's
    /// name, the sender in bold, and up to two lines of message.
    fn toast_alert(&mut self, toast: &Toast, left: f32, right: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let l = ToastLayout::new(left, right);
        let accent = toast.tint.map_or(GRAY, vivid);
        self.glow(left, right, TOAST_ALERT.height, accent, alpha);

        match &toast.icon {
            Some(pixmap) if toast.icon_is_tile => self.art(pixmap, l.icon_x, l.icon_y, TOAST_ICON, 11.0, alpha, 1.0),
            // A bare glyph (WhatsApp's): on a dark tile washed in its color.
            Some(pixmap) => {
                if let Some(tile) = rounded_rect(l.icon_x, l.icon_y, TOAST_ICON, TOAST_ICON, 11.0) {
                    self.fill(&tile, [30, 30, 32], alpha);
                    self.fill(&tile, accent, alpha * 0.2);
                }
                let glyph = TOAST_ICON * 0.68;
                let inset = (TOAST_ICON - glyph) / 2.0;
                self.art(pixmap, l.icon_x + inset, l.icon_y + inset, glyph, 0.0, alpha, 1.0);
            }
            None => {
                // No logo from Windows: a tile in the app's color with its initial.
                if let Some(tile) = rounded_rect(l.icon_x, l.icon_y, TOAST_ICON, TOAST_ICON, 11.0) {
                    self.fill(&tile, if toast.tint.is_some() { accent } else { [70, 70, 76] }, alpha);
                }
                let initial: String = toast.app.chars().take(1).collect::<String>().to_uppercase();
                let baseline = l.icon_y + TOAST_ICON / 2.0 + 7.0;
                self.text_centered(&fonts.bold, &initial, 19.0, l.icon_x + TOAST_ICON / 2.0, baseline, WHITE, alpha);
            }
        }

        let px = self.scale;
        let width = l.text_right - l.text_x;
        let now_w = text_width(&fonts.semibold, "now", 11.5 * px) / px;
        self.text(&fonts.semibold, &toast.app, 11.5, l.text_x, 32.0, width - now_w - 8.0, DIM, alpha);
        self.text_right(&fonts.semibold, "now", 11.5, l.text_right, 32.0, DIM, alpha);
        self.text(&fonts.bold, &toast.title, 14.5, l.text_x, 49.0, width, WHITE, alpha);
        let body = wrap(&fonts.regular, &toast.body, 13.0 * px, width * px, 1);
        for (i, line) in body.iter().enumerate() {
            self.text(&fonts.regular, line, 13.0, l.text_x, 65.0 + i as f32 * 17.0, width, [215, 215, 215], alpha);
        }

    }

    /// A wash of `rgb` from the card's left edge, fading out to the right.
    fn glow(&mut self, left: f32, right: f32, height: f32, rgb: Rgb, alpha: f32) {
        let Some(rect) = Rect::from_xywh(left, 0.0, right - left, height) else { return };
        let color = |a: f32| Color::from_rgba8(rgb[0], rgb[1], rgb[2], (a * alpha * 255.0) as u8);
        let shader = LinearGradient::new(
            Point::from_xy(left, 0.0),
            Point::from_xy(left + (right - left) * 0.7, 0.0),
            vec![GradientStop::new(0.0, color(0.38)), GradientStop::new(0.45, color(0.12)), GradientStop::new(1.0, color(0.0))],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shader {
            let paint = Paint { shader, anti_alias: true, ..Default::default() };
            let ts = self.transform();
            self.pixmap.fill_path(&PathBuilder::from_rect(rect), &paint, FillRule::Winding, ts, Some(self.clip));
        }
    }

    /// Compact island: speaker on the left wing, a slim level bar on the
    /// right one. No numbers, as on iOS.
    fn volume_alert(&mut self, level: f32, muted: bool, left: f32, right: f32, alpha: f32) {
        let mid = VOLUME_ALERT.height / 2.0;
        let level = level.clamp(0.0, 1.0);
        self.speaker_icon(left + 16.0, mid, level, muted, alpha);

        let (x1, h) = (right - 18.0, 4.0);
        let x0 = x1 - 72.0;
        if let Some(track) = rounded_rect(x0, mid - h / 2.0, x1 - x0, h, h / 2.0) {
            self.fill(&track, WHITE, alpha * 0.25);
        }
        let filled = (x1 - x0) * level;
        if filled > 0.5 {
            if let Some(bar) = rounded_rect(x0, mid - h / 2.0, filled, h, (h / 2.0).min(filled / 2.0)) {
                // Muted keeps the level visible but greyed out.
                self.fill(&bar, WHITE, alpha * if muted { 0.35 } else { 1.0 });
            }
        }
    }

    /// SF Symbols-style speaker: a rounded body and cone, with up to three
    /// waves for the level, or a slash when muted. (x, mid) is the left
    /// edge of its body.
    fn speaker_icon(&mut self, x: f32, mid: f32, level: f32, muted: bool, alpha: f32) {
        let mut pb = PathBuilder::new();
        if let Some(body) = rounded_rect(x, mid - 3.0, 4.0, 6.0, 1.3) {
            pb.push_path(&body);
        }
        pb.move_to(x + 3.2, mid - 3.0);
        pb.line_to(x + 7.6, mid - 6.8);
        pb.quad_to(x + 8.6, mid - 7.6, x + 8.6, mid - 6.3);
        pb.line_to(x + 8.6, mid + 6.3);
        pb.quad_to(x + 8.6, mid + 7.6, x + 7.6, mid + 6.8);
        pb.line_to(x + 3.2, mid + 3.0);
        pb.close();
        if let Some(speaker) = pb.finish() {
            // Body and cone overlap; non-zero winding keeps the overlap solid.
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(255, 255, 255, (alpha * 255.0) as u8));
            paint.anti_alias = true;
            let ts = self.transform();
            self.pixmap.fill_path(&speaker, &paint, FillRule::Winding, ts, Some(self.clip));
        }

        if muted {
            // The slash gets a gap cut around it in the island's black, as
            // in "speaker.slash".
            let mut pb = PathBuilder::new();
            pb.move_to(x - 1.0, mid - 7.5);
            pb.line_to(x + 12.0, mid + 7.5);
            if let Some(slash) = pb.finish() {
                self.stroke(&slash, 4.0, [0, 0, 0], alpha);
                self.stroke(&slash, 1.6, WHITE, alpha);
            }
            return;
        }
        let waves = match level {
            l if l <= 0.0 => 0,
            l if l < 0.34 => 1,
            l if l < 0.67 => 2,
            _ => 3,
        };
        let cx = x + 8.6;
        for (i, r) in [3.6, 6.8, 10.0].into_iter().enumerate() {
            // A 90° arc around the speaker's mouth; for a symmetric arc the
            // quadratic control point sits at r / cos(45°) on the axis.
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            let mut pb = PathBuilder::new();
            pb.move_to(cx + d, mid - d);
            pb.quad_to(cx + r * std::f32::consts::SQRT_2, mid, cx + d, mid + d);
            if let Some(arc) = pb.finish() {
                // Waves above the level stay faintly visible, like the
                // variable-color speaker symbol.
                let on = i < waves;
                self.stroke(&arc, 1.6, WHITE, alpha * if on { 1.0 } else { 0.25 });
            }
        }
    }

    fn stroke(&mut self, path: &Path, width: f32, rgb: Rgb, alpha: f32) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8));
        paint.anti_alias = true;
        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        let ts = self.transform();
        self.pixmap.stroke_path(path, &paint, &stroke, ts, Some(self.clip));
    }

    /// The battery pill: what happened on the left, the level on the right.
    fn battery_alert(&mut self, event: BatteryEvent, b: Battery, left: f32, right: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let mid = BATTERY_ALERT.height / 2.0;
        let (label, glyph_is_bolt) = match event {
            BatteryEvent::PluggedIn => ("Charging", true),
            BatteryEvent::Full => ("Fully charged", true),
            BatteryEvent::Unplugged => ("On battery", false),
            BatteryEvent::Low(_) => ("Low battery", false),
        };
        // The glyph on the left says what happened: a bolt on power, a
        // warning-colored battery when low or unplugged.
        let x = left + 20.0;
        if glyph_is_bolt {
            self.bolt(x, mid - 7.0, 1.15, GREEN, alpha);
        } else {
            let color = battery_color(b);
            if let Some(cell) = rounded_rect(x - 1.0, mid - 4.5, 13.0, 9.0, 2.5) {
                self.fill(&cell, color, alpha);
            }
        }
        self.text(&fonts.semibold, label, 13.5, x + 18.0, mid + 4.5, BATTERY_ALERT.width, WHITE, alpha);

        let h = 17.0;
        let glyph_x = right - 20.0 - Self::battery_glyph_width(h, b.plugged);
        self.battery_glyph(glyph_x, mid - h / 2.0, h, b, alpha);
    }

    /// Width of `battery_glyph` at height `h`, including its nub and, when
    /// plugged in, the bolt after it.
    fn battery_glyph_width(h: f32, plugged: bool) -> f32 {
        let body = h * BATTERY_ASPECT;
        let nub = 1.0 + h * 0.13;
        body + nub + if plugged { 3.0 + h * 0.6 } else { 0.0 }
    }

    /// iOS-style battery: the capsule fills with the level and the
    /// percentage sits inside it in dark digits, with a bolt after it while
    /// on power. (x, y) is the body's top-left; `h` its height.
    fn battery_glyph(&mut self, x: f32, y: f32, h: f32, b: Battery, alpha: f32) {
        let w = h * BATTERY_ASPECT;
        let r = h * 0.32;
        // The empty part of the capsule, then the level over it.
        if let Some(body) = rounded_rect(x, y, w, h, r) {
            self.fill(&body, WHITE, alpha * 0.35);
        }
        let level = w * (b.percent as f32 / 100.0).clamp(0.0, 1.0);
        if level > 0.5 {
            let rect = rounded_rect(x, y, level, h, r.min(level / 2.0));
            if let Some(fill) = rect {
                self.fill(&fill, battery_color(b), alpha);
            }
        }
        let nub_w = h * 0.13;
        if let Some(nub) = rounded_rect(x + w + 1.0, y + h * 0.31, nub_w, h * 0.38, nub_w / 2.0) {
            self.fill(&nub, WHITE, alpha * 0.35);
        }

        if let Some(fonts) = self.fonts {
            let size = h * 0.78;
            let text = b.percent.to_string();
            // Digits sit on the capsule's optical center (cap height ~0.7 em).
            let baseline = y + h / 2.0 + size * 0.35;
            self.text_centered(&fonts.semibold, &text, size, x + w / 2.0, baseline, [0, 0, 0], alpha * 0.85);
        }

        if b.plugged {
            let s = h / 12.0 * 0.85;
            self.bolt(x + w + 1.0 + nub_w + 3.0, y + h / 2.0 - 6.0 * s, s, WHITE, alpha);
        }
    }

    /// Lightning bolt, 8x12 at `s` = 1, top-left at (x, y).
    fn bolt(&mut self, x: f32, y: f32, s: f32, rgb: Rgb, alpha: f32) {
        let points = [(4.6, 0.0), (0.0, 6.8), (3.6, 6.8), (2.8, 12.0), (8.0, 4.8), (4.4, 4.8), (5.6, 0.0)];
        let mut pb = PathBuilder::new();
        for (i, (px, py)) in points.into_iter().enumerate() {
            let (px, py) = (x + px * s, y + py * s);
            if i == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.fill(&path, rgb, alpha);
        }
    }

    /// AirPlay-style: rings open at the bottom over a triangle.
    fn output_button(&mut self, cx: f32, cy: f32, alpha: f32) {
        for r in [8.0, 4.0] {
            if let Some(ring) = arc(cx, cy - 1.5, r, 130.0, 280.0) {
                self.stroke(&ring, 1.8, WHITE, alpha);
            }
        }
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 5.5, cy + 9.0);
        pb.line_to(cx + 5.5, cy + 9.0);
        pb.line_to(cx, cy + 3.0);
        pb.close();
        if let Some(triangle) = pb.finish() {
            self.fill(&triangle, WHITE, alpha);
        }
    }

    /// The output devices, one row each, the current one highlighted and
    /// ticked.
    /// The output picker: a sheet that grows out of the AirPlay button
    /// (overshooting a touch, on a spring) into a panel over the player,
    /// uncovering the device rows as it goes; it shrinks back into the
    /// button to close.
    fn output_picker(&mut self, view: &OutputsView, layout: Layout, alpha: f32) {
        // A springy reveal can dip below 0 or pass 1 for a moment.
        let r = view.reveal.max(0.0);
        let (x0, x1) = output_row_span(layout.left, layout.width);
        let bottom = OUTPUT_TOP + OUTPUT_ROW_H * OUTPUT_ROWS_SHOWN as f32;
        let (bx, by) = (layout.left + layout.controls_dx() + OUTPUT_BUTTON_X, CONTROLS_Y);
        let lerp = |from: f32, to: f32| from + (to - from) * r;
        let half = SHEET_FROM / 2.0;
        let (l, t) = (lerp(bx - half, x0 - SHEET_PAD), lerp(by - half, OUTPUT_TOP - SHEET_PAD));
        let (rt, b) = (lerp(bx + half, x1 + SHEET_PAD), lerp(by + half, bottom + SHEET_PAD / 2.0));
        let radius = lerp(half, SHEET_RADIUS);
        let Some(sheet) = rounded_rect(l, t, rt - l, b - t, radius) else { return };
        self.fill(&sheet, SHEET_RGB, alpha * (r * 4.0).min(1.0));
        // The button's glyph melts into the sheet.
        let glyph = (1.0 - r * 3.0).clamp(0.0, 1.0);
        if glyph > 0.0 {
            self.output_button(bx, by, alpha * glyph);
        }

        // Rows show only inside the sheet, and fade in once it is nearly
        // full size, so none shows cut off by its growing edge.
        let rows = ((r - 0.85) / 0.15).clamp(0.0, 1.0);
        if rows <= 0.0 {
            return;
        }
        let mut clip = self.clip.clone();
        clip.intersect_path(&sheet, FillRule::Winding, true, self.transform());
        let mut inner = Canvas { pixmap: &mut *self.pixmap, clip: &clip, scale: self.scale, fonts: self.fonts, dy: self.dy };
        // Rows settle a few px up into place as they appear.
        inner.dy += (1.0 - ease_out_cubic(rows)) * 8.0;
        inner.output_rows(view, layout, alpha * rows);
    }

    /// The device rows: the current one highlighted and ticked (the
    /// highlight slides to a new pick), a softer highlight under the
    /// pointer, a brighter one while pressed.
    fn output_rows(&mut self, view: &OutputsView, layout: Layout, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let (x0, x1) = output_row_span(layout.left, layout.width);
        let bottom = OUTPUT_TOP + OUTPUT_ROW_H * OUTPUT_ROWS_SHOWN as f32;
        let row_y = |row: f32| OUTPUT_TOP + (row - view.scroll) * OUTPUT_ROW_H;
        // Rows scrolling past the top or bottom fade out.
        let inside = |y: f32| ((y - OUTPUT_TOP) / OUTPUT_ROW_H + 1.0).min((bottom - y) / OUTPUT_ROW_H).clamp(0.0, 1.0);
        let highlight = |canvas: &mut Self, row: f32, strength: f32| {
            let y = row_y(row);
            let a = alpha * strength * inside(y);
            if a > 0.0 {
                if let Some(rect) = rounded_rect(x0, y + 1.0, x1 - x0, OUTPUT_ROW_H - 2.0, 10.0) {
                    canvas.fill(&rect, WHITE, a);
                }
            }
        };
        if !view.list.is_empty() {
            highlight(self, view.highlight, 0.1);
            highlight(self, view.hover, 0.06 * view.hover_alpha);
            if let Some(row) = view.pressed {
                highlight(self, row as f32, 0.08);
            }
        }

        for (i, output) in view.list.iter().enumerate() {
            let y = row_y(i as f32);
            let a = alpha * inside(y);
            if a <= 0.0 {
                continue;
            }
            let mid = y + OUTPUT_ROW_H / 2.0;
            // How close the highlight is to this row: the check and the
            // bold name follow it.
            let picked = (1.0 - (view.highlight - i as f32).abs()).clamp(0.0, 1.0);
            self.output_icon(output.kind, x0 + 18.0, mid, a);
            let font = if picked > 0.5 { &fonts.semibold } else { &fonts.regular };
            let rgb = lerp_rgb([220, 220, 220], WHITE, picked);
            self.text(font, &output.name, 13.0, x0 + 38.0, mid + 4.5, x1 - x0 - 38.0 - 32.0, rgb, a);
            if picked > 0.0 {
                // Grows in from its center.
                let (cx, s) = (x1 - 16.0, ease_out_back(picked));
                let mut pb = PathBuilder::new();
                pb.move_to(cx - 5.0 * s, mid + 0.5 * s);
                pb.line_to(cx - 1.5 * s, mid + 4.0 * s);
                pb.line_to(cx + 5.0 * s, mid - 4.0 * s);
                if let Some(check) = pb.finish() {
                    self.stroke(&check, 2.0, BLUE, a * picked);
                }
            }
        }
        if view.list.is_empty() {
            let center = layout.left + layout.width / 2.0;
            self.text_centered(&fonts.semibold, "No audio outputs", 13.0, center, 80.0, DIM, alpha);
        }
    }

    fn output_icon(&mut self, kind: OutputKind, cx: f32, cy: f32, alpha: f32) {
        match kind {
            OutputKind::Headphones => self.device_icon(DeviceKind::Headphones, cx, cy, alpha),
            OutputKind::Speaker => self.speaker_icon(cx - 8.0, cy, 0.7, false, alpha),
            OutputKind::Display => {
                if let Some(screen) = rounded_rect(cx - 8.0, cy - 6.5, 16.0, 11.0, 2.0) {
                    self.stroke(&screen, 1.6, WHITE, alpha);
                }
                let mut pb = PathBuilder::new();
                pb.move_to(cx - 4.0, cy + 7.5);
                pb.line_to(cx + 4.0, cy + 7.5);
                if let Some(stand) = pb.finish() {
                    self.stroke(&stand, 1.6, WHITE, alpha);
                }
            }
        }
    }

    fn expanded(
        &mut self,
        media: Option<&NowPlaying>,
        transition: Option<&Transition>,
        lyrics: &LyricsView,
        seek: Option<f32>,
        seek_grow: f32,
        layout: Layout,
        alpha: f32,
    ) {
        let left = layout.left;
        for (slot, phase) in slots(media, transition, art_changed) {
            let Some(np) = slot else { continue };
            let a = alpha * phase.alpha;
            match &np.art {
                Some(art) => self.art(&art.pixmap, left + ART_X, ART_Y, ART, ART_RADIUS, a, phase.zoom),
                None => {
                    let size = ART * phase.zoom;
                    let inset = (ART - size) / 2.0;
                    let rect = rounded_rect(
                        left + ART_X + inset,
                        ART_Y + inset,
                        size,
                        size,
                        ART_RADIUS * phase.zoom,
                    );
                    if let Some(path) = rect {
                        self.fill(&path, [40, 40, 40], a);
                    }
                }
            }
        }

        let Some(fonts) = self.fonts else { return };
        for (slot, phase) in slots(media, transition, track_changed) {
            let a = alpha * phase.alpha;
            match slot {
                None => {
                    let center = left + layout.width / 2.0;
                    let baseline = 80.0 + phase.dy;
                    self.text_centered(&fonts.semibold, "Tidak ada media yang diputar", 13.0, center, baseline, DIM, a);
                }
                Some(np) => {
                    let x = left + TEXT_X;
                    let max = layout.player - TEXT_X - TEXT_RIGHT_PAD - layout.dots_clearance();
                    let title = TITLE_BASELINE + phase.dy;
                    let subtitle = SUBTITLE_BASELINE + phase.dy;
                    self.text(&fonts.extrabold, &np.title, TITLE_SIZE, x, title, max, WHITE, a);
                    self.text(&fonts.bold, &np.subtitle(), SUBTITLE_SIZE, x, subtitle, max, GRAY, a);
                }
            }
        }

        for (slot, phase) in slots(media, transition, presence_changed) {
            let Some(np) = slot else { continue };
            let a = alpha * phase.alpha;
            // A drag belongs to the current track, not one fading out.
            let seek = seek.filter(|_| media.is_some_and(|m| std::ptr::eq(m, np)));
            self.progress(fonts, np, layout, seek, seek_grow, a);

            let controls = left + layout.controls_dx();
            self.lyrics_icon(controls + LYRICS_BUTTON_X, CONTROLS_Y, a);
            self.skip_icon(controls + PREVIOUS_X, CONTROLS_Y, false, a);
            if np.playing {
                self.pause_icon(controls + PLAY_X, CONTROLS_Y, a);
            } else {
                self.play_icon(controls + PLAY_X, CONTROLS_Y, a);
            }
            self.skip_icon(controls + NEXT_X, CONTROLS_Y, true, a);
            self.output_button(controls + OUTPUT_BUTTON_X, CONTROLS_Y, a);

            let a = a * layout.lyrics;
            if a > 0.0 {
                if let Some(divider) = Rect::from_xywh(left + layout.player, 22.0, 1.0, EXPANDED.height - 44.0) {
                    self.fill(&PathBuilder::from_rect(divider), WHITE, a * 0.1);
                }
                self.lyrics(fonts, lyrics, layout, a);
            }
        }
    }

    fn lyrics(&mut self, fonts: &Fonts, view: &LyricsView, layout: Layout, alpha: f32) {
        let panel_left = layout.left + layout.player;
        let panel_right = layout.left + layout.width;
        let center = (panel_left + panel_right) / 2.0;
        let x = panel_left + LYRICS_PAD;
        let max_width = panel_right - x - LYRICS_PAD;
        match *view {
            LyricsView::Loading => {
                self.text_centered(&fonts.semibold, "Loading lyrics…", 12.5, center, LYRICS_BASELINE, DIM, alpha);
            }
            LyricsView::Missing => {
                self.text_centered(&fonts.semibold, "Lyrics not available", 12.5, center, LYRICS_BASELINE, DIM, alpha);
            }
            LyricsView::Plain(lines) => {
                let lines = lines.iter().filter(|l| !l.text.is_empty()).take(5);
                for (i, line) in lines.enumerate() {
                    let baseline = 40.0 + i as f32 * 20.0;
                    self.text(&fonts.regular, &line.text, 12.5, x, baseline, max_width, GRAY, alpha);
                }
            }
            LyricsView::Synced { lines, scroll } => {
                for (i, line) in lines.iter().enumerate() {
                    let d = i as f32 - scroll;
                    if !(-3.0..=4.0).contains(&d) {
                        continue;
                    }
                    let baseline = LYRIC_CURRENT_BASELINE + d * LYRIC_LINE_H;
                    // The sung line is bright; neighbours dim with distance
                    // and fade out toward the panel's top and bottom edges.
                    let focus = (1.0 - d.abs()).max(0.0);
                    let rgb = [0, 1, 2].map(|c| (DIM[c] as f32 + (WHITE[c] as f32 - DIM[c] as f32) * focus) as u8);
                    let edge = ((baseline - 18.0) / 20.0).clamp(0.0, 1.0)
                        * ((EXPANDED.height - 8.0 - baseline) / 24.0).clamp(0.0, 1.0);
                    let a = alpha * edge * (1.0 - d.abs() * 0.25).max(0.35);
                    // Empty LRC lines mark instrumental breaks.
                    let text = if line.text.is_empty() { "• • •" } else { &line.text };
                    self.text(&fonts.semibold, text, 13.5, x, baseline, max_width, rgb, a);
                }
            }
        }
    }

    /// Elapsed time, a thin bar with a knob, and time remaining. While the
    /// knob is dragged, `seek` (0..1) overrides the playback position and
    /// the knob grows.
    /// iPhone style: a thick rounded bar with no knob, white up to the
    /// playhead over a dim track, between small grey times. Dragged, it
    /// swells (`grow`, 0..1) and brightens. `seek` (0..1) overrides the
    /// playback position meanwhile.
    #[allow(clippy::too_many_arguments)]
    fn progress(&mut self, fonts: &Fonts, np: &NowPlaying, layout: Layout, seek: Option<f32>, grow: f32, alpha: f32) {
        let left = layout.left;
        let right = left + layout.player;
        let position = np.progress().map(|(pos, total)| match seek {
            Some(f) => (total.mul_f32(f.clamp(0.0, 1.0)), total),
            None => (pos, total),
        });
        let (elapsed, remaining, fraction) = match position {
            Some((pos, total)) => (
                format_time(pos),
                // Whole seconds on both sides so elapsed + remaining always
                // adds up to the track length.
                format!("-{}", format_time(Duration::from_secs(total.as_secs() - pos.as_secs()))),
                pos.as_secs_f32() / total.as_secs_f32(),
            ),
            None => ("--:--".to_owned(), "--:--".to_owned(), 0.0),
        };
        let times = alpha * (0.6 + 0.4 * grow.clamp(0.0, 1.0));
        let baseline = PROGRESS_Y + 4.0;
        self.text(&fonts.semibold, &elapsed, 12.0, left + TIME_LEFT, baseline, PANEL_W, WHITE, times);
        let time_right = right - TIME_RIGHT_INSET - layout.dots_clearance();
        self.text_right(&fonts.semibold, &remaining, 12.0, time_right, baseline, WHITE, times);

        let (x0, x1) = layout.progress_span();
        // A spring can overshoot a little; the bar bulges with it.
        let h = PROGRESS_H + PROGRESS_GROW * grow.max(0.0);
        let y = PROGRESS_Y - h / 2.0;
        let Some(track) = rounded_rect(x0, y, x1 - x0, h, h / 2.0) else { return };
        self.fill(&track, WHITE, alpha * (0.22 + 0.1 * grow.clamp(0.0, 1.0)));
        // The played part is cut from the track, so its left end is round
        // and the playhead edge straight.
        let x = x0 + (x1 - x0) * fraction.clamp(0.0, 1.0);
        if let Some(played) = Rect::from_xywh(x0, y, x - x0, h) {
            self.fill_within(&PathBuilder::from_rect(played), &track, WHITE, alpha * (0.9 + 0.1 * grow.clamp(0.0, 1.0)));
        }
    }

    /// Fills with the even-odd rule so inner subpaths punch holes (icons).
    fn fill(&mut self, path: &Path, rgb: Rgb, alpha: f32) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8));
        paint.anti_alias = true;
        let ts = self.transform();
        self.pixmap.fill_path(path, &paint, FillRule::EvenOdd, ts, Some(self.clip));
    }

    /// Draws art into a `size` square at (x, y), scaled about its center by `zoom`.
    #[allow(clippy::too_many_arguments)]
    fn art(&mut self, art: &Pixmap, x: f32, y: f32, size: f32, radius: f32, alpha: f32, zoom: f32) {
        let inset = size * (1.0 - zoom) / 2.0;
        let (x, y, size, radius) = (x + inset, y + inset, size * zoom, radius * zoom);
        let Some(path) = rounded_rect(x, y, size, size, radius) else { return };
        let fit = Transform::from_row(
            size / art.width() as f32,
            0.0,
            0.0,
            size / art.height() as f32,
            x,
            y,
        );
        let paint = Paint {
            shader: Pattern::new(art.as_ref(), SpreadMode::Pad, FilterQuality::Bicubic, alpha, fit),
            anti_alias: true,
            ..Default::default()
        };
        let ts = self.transform();
        self.pixmap.fill_path(&path, &paint, FillRule::Winding, ts, Some(self.clip));
    }

    fn play_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 6.0, cy - 8.5);
        pb.line_to(cx + 8.0, cy);
        pb.line_to(cx - 6.0, cy + 8.5);
        pb.close();
        if let Some(path) = pb.finish() {
            self.fill(&path, WHITE, alpha);
        }
    }

    fn pause_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        for x in [cx - 6.5, cx + 2.0] {
            if let Some(bar) = rounded_rect(x, cy - 8.5, 4.5, 17.0, 1.5) {
                self.fill(&bar, WHITE, alpha);
            }
        }
    }

    /// Two touching triangles, Apple style; mirrored for skip-back.
    fn skip_icon(&mut self, cx: f32, cy: f32, forward: bool, alpha: f32) {
        let d = if forward { 1.0 } else { -1.0 };
        let mut pb = PathBuilder::new();
        for base in [cx - 8.0 * d, cx] {
            pb.move_to(base, cy - 6.0);
            pb.line_to(base + 8.0 * d, cy);
            pb.line_to(base, cy + 6.0);
            pb.close();
        }
        if let Some(path) = pb.finish() {
            self.fill(&path, WHITE, alpha);
        }
    }

    /// Speech bubble with two quote holes.
    fn lyrics_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        let mut pb = PathBuilder::new();
        if let Some(bubble) = rounded_rect(cx - 8.0, cy - 7.0, 16.0, 12.0, 3.5) {
            pb.push_path(&bubble);
        }
        // Tail hangs off the flat part of the bottom edge without overlapping
        // the bubble, so the even-odd fill keeps it solid.
        pb.move_to(cx - 4.5, cy + 5.0);
        pb.line_to(cx - 4.5, cy + 8.5);
        pb.line_to(cx - 0.5, cy + 5.0);
        pb.close();
        pb.push_circle(cx - 2.6, cy - 1.0, 1.8);
        pb.push_circle(cx + 2.6, cy - 1.0, 1.8);
        if let Some(path) = pb.finish() {
            self.fill(&path, WHITE, alpha);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text_centered(&mut self, font: &Font, text: &str, size: f32, cx: f32, baseline: f32, rgb: Rgb, alpha: f32) {
        let width = text_width(font, text, size * self.scale) / self.scale;
        self.text(font, text, size, cx - width / 2.0, baseline, f32::MAX, rgb, alpha);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_right(&mut self, font: &Font, text: &str, size: f32, right: f32, baseline: f32, rgb: Rgb, alpha: f32) {
        let width = text_width(font, text, size * self.scale) / self.scale;
        self.text(font, text, size, right - width, baseline, f32::MAX, rgb, alpha);
    }

    /// Draws one line of text on `baseline`, ellipsized to `max_width`.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        font: &Font,
        text: &str,
        size: f32,
        x: f32,
        baseline: f32,
        max_width: f32,
        rgb: Rgb,
        alpha: f32,
    ) {
        let px = size * self.scale;
        let text = ellipsize(font, text, px, max_width * self.scale);
        let baseline = ((baseline + self.dy) * self.scale).round();
        let mut pen = x * self.scale;
        for ch in text.chars() {
            let glyph = match self.fonts {
                Some(fonts) => fonts.glyph(font, ch, px),
                None => Rc::new(font.rasterize(ch, px)),
            };
            let gx = pen.round() as i32 + glyph.left;
            let gy = baseline as i32 + glyph.top;
            self.blend_coverage(gx, gy, glyph.width, glyph.height, &glyph.coverage, rgb, alpha);
            pen += glyph.advance;
        }
    }

    /// Composites a glyph coverage map (source-over, premultiplied).
    #[allow(clippy::too_many_arguments)]
    fn blend_coverage(
        &mut self,
        gx: i32,
        gy: i32,
        w: usize,
        h: usize,
        coverage: &[u8],
        rgb: Rgb,
        alpha: f32,
    ) {
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let clip = self.clip.data();
        let data = self.pixmap.data_mut();
        for row in 0..h as i32 {
            let y = gy + row;
            if y < 0 || y >= ph {
                continue;
            }
            for col in 0..w as i32 {
                let x = gx + col;
                if x < 0 || x >= pw {
                    continue;
                }
                let idx = (y * pw + x) as usize;
                let cov = coverage[(row * w as i32 + col) as usize];
                let a = cov as f32 / 255.0 * alpha * clip[idx] as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let inv = 1.0 - a;
                let d = &mut data[idx * 4..idx * 4 + 4];
                d[0] = (rgb[0] as f32 * a + d[0] as f32 * inv) as u8;
                d[1] = (rgb[1] as f32 * a + d[1] as f32 * inv) as u8;
                d[2] = (rgb[2] as f32 * a + d[2] as f32 * inv) as u8;
                d[3] = (255.0 * a + d[3] as f32 * inv) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::SystemTime;

    use super::*;
    use crate::media::{Artwork, Timeline};
    use crate::palette;

    #[test]
    fn finds_the_tapped_lyric_line() {
        // A 600 px canvas at 1x, lyrics open, line 5 being sung.
        let at = |x: f32, y: f32| lyric_at(600, 1.0, EXPANDED.width, 1.0, 5.0, 20, (x, y));
        let panel_x = (600.0 - EXPANDED.width) / 2.0 + PANEL_W + 40.0;
        let current = LYRIC_CURRENT_BASELINE - 5.0;
        assert_eq!(at(panel_x, current), Some(5));
        assert_eq!(at(panel_x, current + LYRIC_LINE_H), Some(6));
        assert_eq!(at(panel_x, current - LYRIC_LINE_H), Some(4));
        // The player side is not lyrics.
        assert_eq!(at(panel_x - PANEL_W, current), None);
        // Past the last line.
        assert_eq!(lyric_at(600, 1.0, EXPANDED.width, 1.0, 19.0, 20, (panel_x, current + LYRIC_LINE_H)), None);
    }

    fn save_preview(scene: &Scene, name: &str) {
        let scale = 2.0;
        let mut notch = Pixmap::new((600.0 * scale) as u32, (200.0 * scale) as u32).unwrap();
        draw(&mut notch, scale, scene, Fonts::load().as_ref());

        // Over a light desktop-like backdrop so the shape is visible, with
        // the shadow window's content beneath.
        let mut out = Pixmap::new(notch.width(), notch.height()).unwrap();
        out.fill(Color::from_rgba8(225, 228, 235, 255));
        let pad = (SHADOW_PAD * scale) as u32;
        let mut shadow = Pixmap::new(notch.width() + 2 * pad, notch.height() + pad).unwrap();
        draw_shadow(&mut shadow, scale, scene.shape);
        out.draw_pixmap(-(pad as i32), 0, shadow.as_ref(), &Default::default(), Transform::identity(), None);
        out.draw_pixmap(0, 0, notch.as_ref(), &Default::default(), Transform::identity(), None);
        out.save_png(format!("{}/target/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    }

    /// Renders the open notch to `target/preview*.png` to check the layout
    /// without hovering: `cargo test preview -- --ignored`.
    #[test]
    fn progress_hit_testing() {
        // Lyrics shown, 1x scale, 600 px canvas: the bar spans x 116..224
        // (40 + 76 to 40 + 250 - 66) at y 91.
        let at = |x, y, grab| progress_at(600, 1.0, EXPANDED.width, 1.0, (x, y), grab);
        assert_eq!(at(116.0, 91.0, true), Some(0.0));
        assert_eq!(at(170.0, 95.0, true), Some(0.5));
        assert_eq!(at(170.0, 110.0, true), None, "too far below the bar");
        assert_eq!(at(60.0, 91.0, true), None, "left of the bar");
        // Dragging keeps tracking past either end.
        assert_eq!(at(500.0, 10.0, false), Some(1.0));
        assert_eq!(at(0.0, 150.0, false), Some(0.0));
    }

    #[test]
    #[ignore]
    fn preview() {
        let mut art = Pixmap::new(160, 160).unwrap();
        art.fill(Color::from_rgba8(205, 170, 195, 255));
        let np = NowPlaying {
            title: "Pilih Mama Muda".into(),
            artist: "Maman Fvndy".into(),
            album: "Pilih Mama Muda".into(),
            playing: true,
            art: Some(Arc::new(Artwork { tint: palette::tint(&art), pixmap: art })),
            timeline: Some(Timeline {
                position: Duration::from_secs(4),
                duration: Duration::from_secs(109),
                updated: SystemTime::now(),
            }),
        };

        let charging = Battery { percent: 85, plugged: true, charging: true };
        let on_battery = Battery { percent: 18, plugged: false, charging: false };
        let lines = crate::lyrics::parse_lrc(
            "[00:00.50]Previous lyric line\n[00:03.00]The line being sung right now\n\
             [00:05.50]Next line\n[00:08.00]A very long upcoming line that has to be cut off\n[00:10.50]\n",
        );
        save_preview(
            &Scene {
                shape: EXPANDED,
                panel_width: EXPANDED.width,
                lyrics_reveal: 1.0,
                media: Some(&np),
                transition: None,
                lyrics: LyricsView::Synced { lines: &lines, scroll: 1.0 },
                seek: None,
                seek_grow: 0.0,
                battery: Some(charging),
                bluetooth: None,
                alert: None,
                alert_from: None,
                clock: 0.0,
                outputs: OutputsView::default(),
                page: 0.0,
                controls: ControlsView::default(),
            },
            "preview.png",
        );

        // Lyrics hidden: the player alone at its fixed width, mid-drag at 60%.
        let np = NowPlaying {
            title: "A Rather Long Song Title (Extended Version)".into(),
            ..np
        };
        save_preview(
            &Scene {
                shape: Shape { width: PLAYER_ONLY_WIDTH, ..EXPANDED },
                panel_width: PLAYER_ONLY_WIDTH,
                lyrics_reveal: 0.0,
                media: Some(&np),
                transition: None,
                lyrics: LyricsView::Missing,
                seek: Some(0.6),
                seek_grow: 1.0,
                battery: Some(on_battery),
                bluetooth: None,
                alert: None,
                alert_from: None,
                clock: 0.0,
                outputs: OutputsView::default(),
                page: 0.0,
                controls: ControlsView::default(),
            },
            "preview_player.png",
        );

        // Control Center: full width, player-only width, and mid-slide.
        let buds = Device { name: "Galaxy Buds2 Pro".into(), kind: DeviceKind::Headphones };
        let state = ControlState { wifi: Some(true), bluetooth: Some(true), brightness: Some(70) };
        let controls = ControlsView {
            state,
            volume: 0.45,
            muted: false,
            brightness: None,
            bluetooth_device: Some(&buds),
            battery: Some(charging),
            media: Some(&np),
            toggle_on: Toggle::ALL.map(|t| if t.is_on(&state, false) { 1.0 } else { 0.0 }),
            // Mute pressed in, to show the press.
            toggle_scale: Toggle::ALL.map(|t| if t == Toggle::Mute { 0.88 } else { 1.0 }),
        };
        let page = |width: f32, page: f32| Scene {
            shape: Shape { width, ..EXPANDED },
            panel_width: width,
            lyrics_reveal: if width == EXPANDED.width { 1.0 } else { 0.0 },
            media: Some(&np),
            transition: None,
            lyrics: LyricsView::Missing,
            seek: None,
            seek_grow: 0.0,
            battery: Some(charging),
            bluetooth: None,
            alert: None,
            alert_from: None,
            clock: 0.0,
            outputs: OutputsView::default(),
            page,
            controls,
        };
        // The output picker over the player, the laptop speakers current.
        let outputs = [
            ("Speakers (Realtek(R) Audio)", OutputKind::Speaker, true),
            ("Mi Monitor (NVIDIA High Definition Audio)", OutputKind::Display, false),
            ("Headphones (Realtek(R) Audio)", OutputKind::Headphones, false),
            ("Galaxy Buds2 Pro", OutputKind::Headphones, false),
            ("System (Elgato Virtual Audio)", OutputKind::Speaker, false),
            ("Discord (Elgato Virtual Audio)", OutputKind::Speaker, false),
        ]
        .map(|(name, kind, default)| Output { id: name.into(), name: name.into(), kind, default });
        let picker = |reveal: f32| Scene {
            outputs: OutputsView { list: &outputs, reveal, highlight: 0.0, ..Default::default() },
            ..page(EXPANDED.width, 0.0)
        };
        save_preview(&picker(1.0), "preview_outputs.png");
        save_preview(&picker(0.15), "preview_outputs_opening_a.png");
        save_preview(&picker(0.45), "preview_outputs_opening_b.png");
        save_preview(&picker(0.75), "preview_outputs_opening_c.png");
        // The highlight on its way from the speakers to the monitor.
        let sliding = Scene {
            outputs: OutputsView { list: &outputs, reveal: 1.0, highlight: 0.6, hover: 1.0, hover_alpha: 1.0, ..Default::default() },
            ..page(EXPANDED.width, 0.0)
        };
        save_preview(&sliding, "preview_outputs_picking.png");
        save_preview(&page(EXPANDED.width, 1.0), "preview_controls.png");
        save_preview(&page(PLAYER_ONLY_WIDTH, 1.0), "preview_controls_narrow.png");
        save_preview(&page(EXPANDED.width, 0.5), "preview_controls_sliding.png");

        // The resting notch with the battery indicator, then the alerts.
        let small = |shape: Shape, battery: Battery, alert: Option<Alert>| Scene {
            shape,
            panel_width: EXPANDED.width,
            lyrics_reveal: 1.0,
            media: Some(&np),
            transition: None,
            lyrics: LyricsView::Missing,
            seek: None,
            seek_grow: 0.0,
            battery: Some(battery),
            bluetooth: Some(DeviceKind::Headphones),
            alert,
            alert_from: None,
            clock: 0.0,
            outputs: OutputsView::default(),
            page: 0.0,
            controls: ControlsView::default(),
        };
        let rest = Shape { width: COLLAPSED_BATTERY_WIDTH, ..COLLAPSED };
        save_preview(&small(rest, on_battery, None), "preview_collapsed.png");
        let buds = Device { name: "Galaxy Buds2 Pro".into(), kind: DeviceKind::Headphones };
        let connected = Alert::Bluetooth { device: buds, connected: true };
        save_preview(&small(BLUETOOTH_ALERT, on_battery, Some(connected)), "preview_bluetooth.png");
        let plugged_in = Alert::Battery { event: BatteryEvent::PluggedIn, battery: charging };
        save_preview(&small(BATTERY_ALERT, charging, Some(plugged_in.clone())), "preview_alert.png");
        // "On battery" being replaced by "Charging": early, middle, late.
        let unplugged = Alert::Battery { event: BatteryEvent::Unplugged, battery: on_battery };
        for (p, name) in [(0.15, "a"), (0.4, "b"), (0.7, "c")] {
            let scene = Scene {
                alert_from: Some((unplugged.clone(), p)),
                ..small(BATTERY_ALERT, charging, Some(plugged_in.clone()))
            };
            save_preview(&scene, &format!("preview_alert_swap_{name}.png"));
        }
        let volume = Alert::Volume { level: 0.6, muted: false };
        save_preview(&small(VOLUME_ALERT, on_battery, Some(volume)), "preview_volume.png");
        let muted = Alert::Volume { level: 0.6, muted: true };
        save_preview(&small(VOLUME_ALERT, on_battery, Some(muted)), "preview_volume_muted.png");

        // A chat notification with no app logo (initial tile), long enough
        // to wrap and be cut on the second line.
        let toast = Toast {
            app: "WhatsApp".into(),
            title: "Budi".into(),
            body: "Bro nanti malam jadi main futsal kan? Lapangannya udah aku booking jam 8, \
                   jangan telat ya soalnya cuma dapet dua jam"
                .into(),
            icon: None,
            icon_is_tile: false,
            app_id: String::new(),
            tint: Some([37, 211, 102]),
        };
        // As Windows hands over WhatsApp's logo: a small green glyph in a
        // wide transparent margin.
        let mut logo = Pixmap::new(128, 128).unwrap();
        let mut green = Paint::default();
        green.set_color_rgba8(37, 211, 102, 255);
        green.anti_alias = true;
        let ring = PathBuilder::from_circle(64.0, 64.0, 26.0).unwrap();
        let stroke = Stroke { width: 7.0, ..Default::default() };
        logo.stroke_path(&ring, &green, &stroke, Transform::identity(), None);
        let logo = crate::notifications::trim(&logo);
        let glyph_toast = Toast {
            title: "Indosat".into(),
            body: "oy".into(),
            icon_is_tile: crate::notifications::is_tile(&logo),
            icon: Some(logo),
            app: toast.app.clone(),
            app_id: String::new(),
            tint: toast.tint,
        };
        let toast = Alert::Toast(Arc::new(toast));
        save_preview(&small(TOAST_ALERT, on_battery, Some(toast)), "preview_toast.png");
        let glyph_toast = Alert::Toast(Arc::new(glyph_toast));
        save_preview(&small(TOAST_ALERT, on_battery, Some(glyph_toast)), "preview_toast_glyph.png");
        // A new track, 0.4 s in, the equalizer mid-bounce.
        let started = Instant::now() - Duration::from_millis(400);
        let now_playing = Alert::NowPlaying { track: Box::new(np.clone()), since: started };
        save_preview(&small(NOW_PLAYING_ALERT, on_battery, Some(now_playing)), "preview_now_playing.png");

        // A window under the notch: the mini pill, content faded out.
        save_preview(&small(MINI, on_battery, None), "preview_mini.png");

        // Every device icon side by side, on a black strip.
        let scale = 4.0;
        let mut strip = Pixmap::new((120.0 * scale) as u32, (30.0 * scale) as u32).unwrap();
        strip.fill(Color::BLACK);
        let mut clip = Mask::new(strip.width(), strip.height()).unwrap();
        clip.data_mut().fill(255);
        let fonts = Fonts::load();
        let mut canvas = Canvas { pixmap: &mut strip, clip: &clip, scale, fonts: fonts.as_ref(), dy: 0.0 };
        let kinds = [DeviceKind::Headphones, DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Phone, DeviceKind::Other];
        for (i, kind) in kinds.into_iter().enumerate() {
            canvas.device_icon(kind, 14.0 + i as f32 * 23.0, 15.0, 1.0);
        }
        strip.save_png(format!("{}/target/preview_device_icons.png", env!("CARGO_MANIFEST_DIR"))).unwrap();
    }
}

fn text_width(font: &Font, text: &str, px: f32) -> f32 {
    text.chars().map(|c| font.advance(c, px)).sum()
}

/// Breaks `text` into at most `max_lines` lines no wider than `max` (same
/// units as `px`), at spaces where possible; the last line is ellipsized
/// if text remains.
fn wrap(font: &Font, text: &str, px: f32, max: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let candidate = if line.is_empty() { (*word).to_owned() } else { format!("{line} {word}") };
        if line.is_empty() || text_width(font, &candidate, px) <= max {
            line = candidate;
            continue;
        }
        if lines.len() + 1 == max_lines {
            // Out of lines: the rest goes on this one, cut with an ellipsis.
            let rest = words[i..].join(" ");
            lines.push(ellipsize(font, &format!("{line} {rest}"), px, max));
            return lines;
        }
        lines.push(std::mem::replace(&mut line, (*word).to_owned()));
    }
    if !line.is_empty() {
        lines.push(ellipsize(font, &line, px, max));
    }
    lines
}

fn ellipsize(font: &Font, text: &str, px: f32, max: f32) -> String {
    if text_width(font, text, px) <= max {
        return text.to_owned();
    }
    let budget = max - font.advance('…', px);
    let mut out = String::new();
    let mut width = 0.0;
    for c in text.chars() {
        width += font.advance(c, px);
        if width > budget {
            break;
        }
        out.push(c);
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}
