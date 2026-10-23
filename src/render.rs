//! Draws the notch and its content. All layout is in logical (96 DPI) pixels
//! and scaled to physical pixels by `scale`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LinearGradient, Mask, Paint, Path,
    PathBuilder, Pattern, Pixmap, Point, Rect, SpreadMode, Stroke, Transform,
};

use crate::anim::{ease_out_back, ease_out_cubic};
use crate::battery::{Battery, BatteryEvent};
use crate::lyrics::Line;
use crate::media::{art_changed, track_changed, Command, NowPlaying};
use crate::notifications::Toast;
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
pub const TOAST_ALERT: Shape = Shape { width: 400.0, height: 84.0, radius: 30.0 };
/// Resting notch while an app's window sits under it: a slim pill that
/// stays out of the way of title bars and tabs.
pub const MINI: Shape = Shape { width: 80.0, height: 10.0, radius: 5.0 };

pub fn alert_shape(alert: &Alert) -> Shape {
    match alert {
        Alert::Battery { .. } => BATTERY_ALERT,
        Alert::Volume { .. } => VOLUME_ALERT,
        Alert::Toast(_) => TOAST_ALERT,
    }
}

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
const TIME_BASELINE: f32 = 96.0;
const TIME_LEFT: f32 = 24.0;
/// Remaining time's right edge, from the player's right edge.
const TIME_RIGHT_INSET: f32 = 15.0;
const PROGRESS_Y: f32 = 91.0;
const PROGRESS_X0: f32 = 76.0;
/// Progress bar's right end, from the player's right edge.
const PROGRESS_RIGHT_INSET: f32 = 66.0;
const CONTROLS_Y: f32 = 118.0;
// Control centers in a `PANEL_W` player; wider players shift them to stay centered.
const LYRICS_BUTTON_X: f32 = 53.0;
const PREVIOUS_X: f32 = 92.0;
const PLAY_X: f32 = 131.0;
const NEXT_X: f32 = 170.0;
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
    /// Rasterized glyphs by (font, char, pixel size). Text is redrawn every
    /// animation frame, so rasterizing each time would dominate the frame.
    glyphs: RefCell<HashMap<(bool, char, u32), Rc<(Metrics, Vec<u8>)>>>,
}

/// Plenty for a few sizes of a few scripts; cleared rather than evicted.
const GLYPH_CACHE_LIMIT: usize = 4096;

impl Fonts {
    /// Loads Segoe UI from the system font folder.
    pub fn load() -> Option<Self> {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let load = |name: &str| -> Option<Font> {
            let bytes = std::fs::read(format!(r"{dir}\Fonts\{name}")).ok()?;
            Font::from_bytes(bytes, FontSettings::default()).ok()
        };
        Some(Self {
            regular: load("segoeui.ttf")?,
            semibold: load("seguisb.ttf").or_else(|| load("segoeuib.ttf"))?,
            glyphs: RefCell::default(),
        })
    }

    /// `font` must be one of this set's fonts.
    fn glyph(&self, font: &Font, ch: char, px: f32) -> Rc<(Metrics, Vec<u8>)> {
        let key = (std::ptr::eq(font, &self.semibold), ch, px.to_bits());
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
    /// `None` on machines without a battery.
    pub battery: Option<Battery>,
    pub alert: Option<Alert>,
}

/// A brief notification, shown in its `alert_shape`.
#[derive(Clone)]
pub enum Alert {
    Battery { event: BatteryEvent, battery: Battery },
    /// `level` is 0..1.
    Volume { level: f32, muted: bool },
    /// A toast from another app (WhatsApp, mail...).
    Toast(Arc<Toast>),
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

    /// Left and right ends of the progress bar.
    fn progress_span(&self) -> (f32, f32) {
        (self.left + PROGRESS_X0, self.left + self.player - PROGRESS_RIGHT_INSET)
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
    if expanded_alpha > 0.0 {
        background(pixmap, &path, ts, shape.height, media, transition, expanded_alpha);
    }

    // Content is clipped to the notch so it is revealed as the shape grows.
    let Some(mut clip) = Mask::new(pixmap.width(), pixmap.height()) else { return };
    clip.fill_path(&path, FillRule::Winding, true, ts);

    let mut canvas = Canvas { pixmap, clip: &clip, scale, fonts };
    let (left, right) = ((canvas_w - shape.width) / 2.0, (canvas_w + shape.width) / 2.0);

    // The resting content fades out as the notch shrinks into its mini pill.
    let rest_width = if scene.battery.is_some() { COLLAPSED_BATTERY_WIDTH } else { COLLAPSED.width };
    let unshrunk = ((shape.width - MINI.width) / (rest_width - MINI.width)).clamp(0.0, 1.0);
    let collapsed_alpha = (1.0 - t * 3.0).clamp(0.0, 1.0) * (1.0 - alert_alpha) * unshrunk * unshrunk;
    if collapsed_alpha > 0.0 {
        canvas.collapsed(media, transition, scene.battery, left, right, collapsed_alpha);
    }

    if let Some(alert) = scene.alert.as_ref().filter(|_| alert_alpha > 0.0) {
        canvas.alert(alert, left, right, alert_alpha);
    }

    if expanded_alpha > 0.0 {
        let layout = Layout::new(canvas_w, scene.panel_width, scene.lyrics_reveal);
        canvas.expanded(media, transition, &scene.lyrics, scene.seek, layout, expanded_alpha);
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
    media: Option<&NowPlaying>,
    transition: Option<&Transition>,
    alpha: f32,
) {
    let colors = |np: Option<&NowPlaying>| np.and_then(|np| np.art.as_ref()).map(|art| art.tint);
    let current = colors(media);
    let mut paint_gradient = |tint: Rgb, a: f32| {
        let alpha = (a * 255.0) as u8;
        let black = Color::from_rgba8(0, 0, 0, alpha);
        let tint = Color::from_rgba8(tint[0], tint[1], tint[2], alpha);
        let shader = LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(0.0, height),
            // Solid black across the top third, where the notch meets the bezel.
            vec![
                GradientStop::new(0.0, black),
                GradientStop::new(0.3, black),
                GradientStop::new(1.0, tint),
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
    ]
    .into_iter()
    .find(|&(_, cx)| (x - left - cx).hypot(y - CONTROLS_Y) <= BUTTON_HIT_RADIUS)
    .map(|(button, _)| button)
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
}

impl Canvas<'_> {
    /// Mini art and equalizer bars; the battery, if any, takes the right
    /// side and pushes the equalizer next to the art.
    fn collapsed(
        &mut self,
        media: Option<&NowPlaying>,
        transition: Option<&Transition>,
        battery: Option<Battery>,
        left: f32,
        right: f32,
        alpha: f32,
    ) {
        let mid = COLLAPSED.height / 2.0;
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
            for (i, h) in [8.0, 14.0, 10.0].into_iter().enumerate() {
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
        }
    }

    /// iOS banner: app icon, sender in bold with "now", up to two lines of
    /// message.
    fn toast_alert(&mut self, toast: &Toast, left: f32, right: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let h = TOAST_ALERT.height;
        let icon = 40.0;
        let (icon_x, icon_y) = (left + 20.0, (h - icon) / 2.0 + 2.0);
        match &toast.icon {
            Some(pixmap) => self.art(pixmap, icon_x, icon_y, icon, 10.0, alpha, 1.0),
            None => {
                // No logo from Windows: a grey tile with the app's initial.
                if let Some(tile) = rounded_rect(icon_x, icon_y, icon, icon, 10.0) {
                    self.fill(&tile, [70, 70, 76], alpha);
                }
                let initial: String = toast.app.chars().take(1).collect::<String>().to_uppercase();
                let baseline = icon_y + icon / 2.0 + 6.5;
                self.text_centered(&fonts.semibold, &initial, 18.0, icon_x + icon / 2.0, baseline, WHITE, alpha);
            }
        }

        let x = icon_x + icon + 12.0;
        let text_right = right - 22.0;
        let px = self.scale;
        let now_w = text_width(&fonts.regular, "now", 12.0 * px) / px;
        let body_w = text_right - x;
        let body = wrap(&fonts.regular, &toast.body, 13.0 * px, body_w * px, 2);
        // With a one-line message the pair sits a little lower, centered.
        let title_baseline = if body.len() > 1 { 36.0 } else { 44.0 };
        self.text(&fonts.semibold, &toast.title, 14.0, x, title_baseline, body_w - now_w - 8.0, WHITE, alpha);
        self.text_right(&fonts.regular, "now", 12.0, text_right, title_baseline, GRAY, alpha * 0.8);
        for (i, line) in body.iter().enumerate() {
            let baseline = title_baseline + 18.0 + i as f32 * 17.0;
            self.text(&fonts.regular, line, 13.0, x, baseline, body_w, [225, 225, 225], alpha);
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
            let ts = Transform::from_scale(self.scale, self.scale);
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
        let stroke = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
        let ts = Transform::from_scale(self.scale, self.scale);
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

    fn expanded(
        &mut self,
        media: Option<&NowPlaying>,
        transition: Option<&Transition>,
        lyrics: &LyricsView,
        seek: Option<f32>,
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
                    let max = layout.player - TEXT_X - TEXT_RIGHT_PAD;
                    let title = TITLE_BASELINE + phase.dy;
                    let subtitle = SUBTITLE_BASELINE + phase.dy;
                    self.text(&fonts.semibold, &np.title, TITLE_SIZE, x, title, max, WHITE, a);
                    self.text(&fonts.regular, &np.subtitle(), SUBTITLE_SIZE, x, subtitle, max, GRAY, a);
                }
            }
        }

        for (slot, phase) in slots(media, transition, presence_changed) {
            let Some(np) = slot else { continue };
            let a = alpha * phase.alpha;
            // A drag belongs to the current track, not one fading out.
            let seek = seek.filter(|_| media.is_some_and(|m| std::ptr::eq(m, np)));
            self.progress(fonts, np, layout, seek, a);

            let controls = left + layout.controls_dx();
            self.lyrics_icon(controls + LYRICS_BUTTON_X, CONTROLS_Y, a);
            self.skip_icon(controls + PREVIOUS_X, CONTROLS_Y, false, a);
            if np.playing {
                self.pause_icon(controls + PLAY_X, CONTROLS_Y, a);
            } else {
                self.play_icon(controls + PLAY_X, CONTROLS_Y, a);
            }
            self.skip_icon(controls + NEXT_X, CONTROLS_Y, true, a);

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
    fn progress(&mut self, fonts: &Fonts, np: &NowPlaying, layout: Layout, seek: Option<f32>, alpha: f32) {
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
        self.text(&fonts.regular, &elapsed, 14.0, left + TIME_LEFT, TIME_BASELINE, PANEL_W, WHITE, alpha);
        self.text_right(&fonts.regular, &remaining, 14.0, right - TIME_RIGHT_INSET, TIME_BASELINE, WHITE, alpha);

        let (x0, x1) = layout.progress_span();
        let x = x0 + (x1 - x0) * fraction.clamp(0.0, 1.0);
        if let Some(track) = rounded_rect(x0, PROGRESS_Y - 1.5, x1 - x0, 3.0, 1.5) {
            self.fill(&track, WHITE, alpha * 0.3);
        }
        if let Some(played) = rounded_rect(x0, PROGRESS_Y - 1.5, x - x0, 3.0, 1.5) {
            self.fill(&played, WHITE, alpha * 0.9);
        }
        let knob_radius = if seek.is_some() { 6.0 } else { 3.0 };
        if let Some(knob) = PathBuilder::from_circle(x, PROGRESS_Y, knob_radius) {
            self.fill(&knob, WHITE, alpha);
        }
    }

    /// Fills with the even-odd rule so inner subpaths punch holes (icons).
    fn fill(&mut self, path: &Path, rgb: Rgb, alpha: f32) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8));
        paint.anti_alias = true;
        let ts = Transform::from_scale(self.scale, self.scale);
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
        let ts = Transform::from_scale(self.scale, self.scale);
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
        let baseline = (baseline * self.scale).round();
        let mut pen = x * self.scale;
        for ch in text.chars() {
            let glyph = match self.fonts {
                Some(fonts) => fonts.glyph(font, ch, px),
                None => Rc::new(font.rasterize(ch, px)),
            };
            let (m, coverage) = &*glyph;
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32) as i32;
            self.blend_coverage(gx, gy, m.width, m.height, coverage, rgb, alpha);
            pen += m.advance_width;
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

    fn save_preview(scene: &Scene, name: &str) {
        let scale = 2.0;
        let mut notch = Pixmap::new((600.0 * scale) as u32, (200.0 * scale) as u32).unwrap();
        draw(&mut notch, scale, scene, Fonts::load().as_ref());

        // Over a light desktop-like backdrop so the shape is visible.
        let mut out = Pixmap::new(notch.width(), notch.height()).unwrap();
        out.fill(Color::from_rgba8(225, 228, 235, 255));
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
                battery: Some(charging),
                alert: None,
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
                battery: Some(on_battery),
                alert: None,
            },
            "preview_player.png",
        );

        // The resting notch with the battery indicator, then the alerts.
        let small = |shape: Shape, battery: Battery, alert: Option<Alert>| Scene {
            shape,
            panel_width: EXPANDED.width,
            lyrics_reveal: 1.0,
            media: Some(&np),
            transition: None,
            lyrics: LyricsView::Missing,
            seek: None,
            battery: Some(battery),
            alert,
        };
        let rest = Shape { width: COLLAPSED_BATTERY_WIDTH, ..COLLAPSED };
        save_preview(&small(rest, on_battery, None), "preview_collapsed.png");
        let plugged_in = Alert::Battery { event: BatteryEvent::PluggedIn, battery: charging };
        save_preview(&small(BATTERY_ALERT, charging, Some(plugged_in)), "preview_alert.png");
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
            app_id: String::new(),
        };
        let toast = Alert::Toast(Arc::new(toast));
        save_preview(&small(TOAST_ALERT, on_battery, Some(toast)), "preview_toast.png");
        // A window under the notch: the mini pill, content faded out.
        save_preview(&small(MINI, on_battery, None), "preview_mini.png");
    }
}

fn text_width(font: &Font, text: &str, px: f32) -> f32 {
    text.chars().map(|c| font.metrics(c, px).advance_width).sum()
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
    let budget = max - font.metrics('…', px).advance_width;
    let mut out = String::new();
    let mut width = 0.0;
    for c in text.chars() {
        width += font.metrics(c, px).advance_width;
        if width > budget {
            break;
        }
        out.push(c);
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}
