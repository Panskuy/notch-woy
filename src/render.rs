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
use crate::controls::{ControlState, DisplayLevel};
use crate::lyrics::Line;
use crate::media::{art_changed, track_changed, Command, NowPlaying};
use crate::notifications::Toast;
use crate::outputs::{Output, OutputKind};
use crate::palette::Rgb;
use crate::settings::Page;
use crate::shelf::{self, Item};

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

/// The shape `alert` grows into from a resting notch `rest_width` wide; at
/// least that wide, so a widened notch does not narrow for an alert.
pub fn alert_shape(alert: &Alert, rest_width: f32) -> Shape {
    let shape = match alert {
        Alert::Battery { .. } => BATTERY_ALERT,
        Alert::Volume { .. } => VOLUME_ALERT,
        Alert::Toast(_) => TOAST_ALERT,
        Alert::Bluetooth { .. } => BLUETOOTH_ALERT,
        Alert::NowPlaying { .. } => NOW_PLAYING_ALERT,
    };
    Shape { width: shape.width.max(rest_width), ..shape }
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
const CONTROLS_Y: f32 = 118.0;
/// The page dots' row, below the player's buttons and the other pages'
/// modules and tiles.
const PAGE_DOTS_Y: f32 = 144.0;
/// Synced lyrics: size; how bright the part of the sung line still to
/// come is (0 = as dim as the other lines, 1 = full); over how many
/// characters each one brightens as it is sung (a soft edge rather than a
/// hard one); and how far (logical px) a sung character rises.
const LYRIC_SIZE: f32 = 13.5;
const LYRIC_AHEAD: f32 = 0.3;
const LYRIC_WIPE: f32 = 1.6;
const LYRIC_LIFT: f32 = 1.0;
/// The resting notch's clock: inset, size, and the gap before the cover.
const CLOCK_X: f32 = 14.0;
const CLOCK_SIZE: f32 = 13.0;
const CLOCK_GAP: f32 = 8.0;
/// How strong the cover gradient stays behind the shelf and Control Center,
/// relative to behind the player.
const SIDE_PAGE_TINT: f32 = 0.6;
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

    pub fn regular(&self) -> &Font {
        &self.regular
    }

    pub fn semibold(&self) -> &Font {
        &self.semibold
    }

    pub fn bold(&self) -> &Font {
        &self.bold
    }

    /// `font` must be one of this set's fonts.
    pub fn glyph(&self, font: &Font, ch: char, px: f32) -> Rc<Glyph> {
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
    /// The resting notch's width (it depends on the battery and settings).
    pub rest_width: f32,
    /// The kind of Bluetooth device the resting notch shows, if any is
    /// connected.
    pub bluetooth: Option<DeviceKind>,
    pub alert: Option<Alert>,
    /// The alert `alert` is replacing, and how far (0..1) the swap is.
    pub alert_from: Option<(Alert, f32)>,
    /// Open notch page, animated: -1 = shelf (left), 0 = player, 1 = Control
    /// Center (right).
    pub page: f32,
    pub controls: ControlsView<'a>,
    /// Seconds on a running clock; drives the resting notch's equalizer.
    pub clock: f32,
    /// The equalizers' bar heights (0..1) from the output's peak meter;
    /// `None` and they animate on their own.
    pub levels: Option<[f32; crate::meter::BARS]>,
    /// Scale of the open notch (the Open size setting): its shape, from
    /// the window, and everything drawn on it, here. 1 is as designed.
    pub open_zoom: f32,
    /// The open notch's pages, left to right (the Layout setting): `page`
    /// -1, 0 and 1.
    pub page_order: [Page; 3],
    /// The time of day, as Windows formats it (no seconds, no AM/PM), shown
    /// on the left of the resting notch like the iPhone's status bar.
    pub time: &'a str,
    pub outputs: OutputsView<'a>,
    pub shelf: ShelfView<'a>,
}

/// The file shelf page, left of the player.
#[derive(Clone, Copy, Default)]
pub struct ShelfView<'a> {
    /// Newest first.
    pub items: &'a [Item],
    /// First tile shown, fractional while it scrolls.
    pub scroll: f32,
    pub hover: Option<ShelfHit>,
    pub pressed: Option<ShelfHit>,
    /// How far the drop zone has lit up for files dragged over it, 0..1
    /// (a spring: a little past 1 as it pops).
    pub drop_glow: f32,
    /// Files just dropped: how many (the first tiles), and seconds since.
    pub arrival: Option<Arrival>,
    /// Files just taken off the shelf, on their way into the bin.
    pub departures: &'a [Departure],
    /// A file was taken off at this index this many seconds ago: the tiles
    /// after it slide over to close the gap.
    pub closing: Option<(usize, f32)>,
}

/// A file taken off the shelf: its icon, thrown from its tile into the
/// Clear All button's bin, like a file dragged to the Trash on a Mac.
#[derive(Clone)]
pub struct Departure {
    pub icon: Option<Arc<Pixmap>>,
    /// Where its icon's center was (the shelf page's layout units).
    pub from: (f32, f32),
    /// Seconds since it was thrown; negative while it waits its turn (a
    /// Clear All throws them one after another).
    pub t: f32,
}

/// Seconds a thrown file takes to reach the bin.
const THROW: f32 = 0.55;
/// Between files thrown by Clear All.
pub const THROW_STAGGER: f32 = 0.06;
/// After a file lands: the lid closes and the bin gulps, this long.
const GULP: f32 = 0.3;
/// Seconds the tiles after a removed one take to close the gap.
const CLOSE_GAP: f32 = 0.32;

/// Seconds a thrown file stays on screen (flight, then the bin's gulp).
pub fn shelf_throw_length() -> f32 {
    THROW + GULP
}

/// Seconds the gap left by a removed file takes to close.
pub fn shelf_close_length() -> f32 {
    CLOSE_GAP
}

/// Where the icon of the file at `index` is drawn on the shelf page
/// (layout units), with `count` files scrolled to `scroll`; `None` when it
/// is scrolled out of sight.
pub fn shelf_icon_center(canvas_width: u32, scale: f32, count: usize, scroll: f32, index: usize) -> Option<(f32, f32)> {
    let g = ShelfGeometry::new(canvas_width as f32 / scale, count);
    let x = g.tile_x(index, scroll);
    let (x0, x1) = g.strip();
    (x + SHELF_TILE_W > x0 && x < x1).then_some((x + SHELF_TILE_W / 2.0, SHELF_ICON_Y + shelf::ICON / 2.0))
}

/// Files just dropped on the shelf.
#[derive(Clone, Copy)]
pub struct Arrival {
    /// How many: the first tiles.
    pub count: usize,
    /// Seconds since the drop.
    pub t: f32,
    /// Where they were let go (logical canvas px): they fly from there.
    pub from: (f32, f32),
}

/// Seconds a dropped file's icon takes to fly into its tile.
const FLIGHT: f32 = 0.55;
/// The drag image's size relative to a tile's icon, which a dropped file
/// starts out at and springs down from.
const FLIGHT_ZOOM: f32 = 1.45;
/// The tiles already there slide over, quicker than the flight, so the
/// new ones land in a free spot.
const ARRIVAL_SHIFT: f32 = 0.28;
/// Delay between successive dropped files' flights.
const ARRIVAL_STAGGER: f32 = 0.07;
/// Past this many, dropped files fly together (they land off the strip).
const SHELF_MAX_STAGGERED: usize = 6;
/// The ring of light spreading from a tile as its file lands.
const RIPPLE: f32 = 0.45;
/// How far into its flight a file touches down and its ripple starts.
const TOUCHDOWN: f32 = 0.7;

fn arrival_delay(index: usize) -> f32 {
    ARRIVAL_STAGGER * index.min(SHELF_MAX_STAGGERED) as f32
}

/// Seconds until all of `count` dropped files have landed and rippled.
pub fn shelf_arrival_length(count: usize) -> f32 {
    arrival_delay(count.saturating_sub(1)) + (FLIGHT * TOUCHDOWN + RIPPLE).max(FLIGHT)
}

/// Where a shelf tile's icon is and how it shows, mid-flight or at rest.
#[derive(Clone, Copy)]
struct TileMotion {
    /// Icon center.
    icon: (f32, f32),
    zoom: f32,
    icon_alpha: f32,
    /// The name and hover highlight; they show once the icon has landed.
    label_alpha: f32,
    /// The landing ripple's progress, 0..1, if it is spreading.
    ripple: Option<f32>,
}

impl TileMotion {
    fn at_rest(x: f32) -> Self {
        let icon = (x + SHELF_TILE_W / 2.0, SHELF_ICON_Y + shelf::ICON / 2.0);
        Self { icon, zoom: 1.0, icon_alpha: 1.0, label_alpha: 1.0, ripple: None }
    }

    /// A dropped file `t` seconds after the drop, the `index`-th of them,
    /// flying from `from` into the tile at `x`. Like a drop on iOS or
    /// macOS: the drag image curves from the pointer into its place,
    /// springing down from its own size (a little under, then back), and
    /// the name appears once it is down.
    fn flying(x: f32, index: usize, t: f32, from: (f32, f32)) -> Self {
        let rest = Self::at_rest(x);
        let p = ((t - arrival_delay(index)) / FLIGHT).clamp(0.0, 1.0);
        if p >= 1.0 && t - arrival_delay(index) >= FLIGHT * TOUCHDOWN + RIPPLE {
            return rest;
        }
        // Several files leave the pointer as a fanned stack, as dragged.
        let from = (from.0 + 5.0 * index.min(3) as f32, from.1 + 3.0 * index.min(3) as f32);
        // Kept low enough that the enlarged image is not cut off by the top
        // of the screen.
        let from = (from.0, from.1.max(shelf::ICON * FLIGHT_ZOOM / 2.0 + 4.0));
        // A swoop: sideways first, then down into the tile.
        let e = ease_out_cubic(p);
        let control = (rest.icon.0, from.1);
        let bezier = |a: f32, c: f32, b: f32| (1.0 - e) * (1.0 - e) * a + 2.0 * (1.0 - e) * e * c + e * e * b;
        let icon = (bezier(from.0, control.0, rest.icon.0), bezier(from.1, control.1, rest.icon.1));
        let zoom = FLIGHT_ZOOM + (1.0 - FLIGHT_ZOOM) * ease_out_back(p);
        let since_touchdown = t - arrival_delay(index) - FLIGHT * TOUCHDOWN;
        Self {
            icon,
            zoom,
            icon_alpha: (p * 5.0).min(1.0),
            label_alpha: ((p - TOUCHDOWN) / (1.0 - TOUCHDOWN)).clamp(0.0, 1.0),
            ripple: (since_touchdown >= 0.0).then(|| (since_touchdown / RIPPLE).min(1.0)),
        }
    }
}

/// What is under the pointer on the shelf page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ShelfHit {
    Item(usize),
    /// The little x on a hovered tile.
    Remove(usize),
    Share,
    Clear,
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
    /// The screens whose brightness can be set, each on its own slider in
    /// the Displays sheet; with two or more, the brightness slider's symbol
    /// becomes a pair of screens that opens it.
    pub displays: &'a [DisplayLevel],
    /// How far the Displays sheet is open, 0..1 (a spring: a little past 1
    /// as it pops).
    pub displays_reveal: f32,
    /// A screen's slider being dragged, with its value (0..1).
    pub display_drag: Option<(usize, f32)>,
}

/// What is under the pointer on the open Displays sheet.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DisplayHit {
    /// A screen's slider, with the value (0..1) at the pointer.
    Slider(usize, f32),
    /// On the sheet, off the sliders.
    Sheet,
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
    /// The brightness slider's screens symbol (two or more screens): opens
    /// the Displays sheet.
    Displays,
    /// The Now Playing module, off its play/pause button: goes to the
    /// player's page.
    NowPlaying,
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
    /// -1 before the first one. `sung`: that line's index and how many of
    /// its characters have been sung (fractional), to light it up word by
    /// word.
    Synced { lines: &'a [Line], scroll: f32, sung: Option<(usize, f32)> },
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

thread_local! {
    /// A canvas-sized mask reused by `Canvas::fill_within` frame after
    /// frame, rather than allocated for every slider.
    static SCRATCH_MASK: RefCell<Option<Mask>> = const { RefCell::new(None) };
}

/// Frees what drawing keeps between frames for speed, while the notch is
/// at rest (`window::trim_working_set`); it comes back on the next frame.
pub fn release_scratch() {
    SCRATCH_MASK.with_borrow_mut(|slot| *slot = None);
}

/// How far the notch is between collapsed (0) and expanded (1), for an
/// open notch scaled by `zoom`.
fn expansion(shape: Shape, zoom: f32) -> f32 {
    ((shape.height - COLLAPSED.height) / (EXPANDED.height * zoom - COLLAPSED.height)).clamp(0.0, 1.0)
}

/// Room the shadow's canvas has beyond the notch's on the left, right and
/// bottom, in logical px: the shadow spreads further than the notch's
/// canvas reaches.
pub const SHADOW_PAD: f32 = 40.0;

/// Wide, faint shadow around the open notch, centered on a canvas
/// `SHADOW_PAD` larger than the notch's. It deepens as the notch opens;
/// returns false when there is none (the notch is at rest).
pub fn draw_shadow(pixmap: &mut Pixmap, scale: f32, shape: Shape, zoom: f32) -> bool {
    pixmap.fill(Color::TRANSPARENT);
    let t = expansion(shape, zoom);
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
    let alpha_at = |px: usize, qy: f32| {
        let qx = ((px as f32 + 0.5) / scale - cx).abs() - half_w + r;
        let outside = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
        let u = (outside / BLUR).clamp(0.0, 1.0);
        // Fades fast near the notch, then trails off far and faint.
        (peak * (1.0 - u).powi(2) * 255.0).round() as u8
    };
    // The shadow is symmetric about the canvas's middle: the left half is
    // worked out and mirrored. Rows level with the notch's straight sides
    // (above its rounded bottom corners) only differ across, so one is
    // worked out and copied down. This runs every frame the notch changes
    // size; a few hundred thousand pixel distances were too many for a
    // 180 Hz frame.
    let half = pw.div_ceil(2).min(x1).max(x0);
    let fill_row = |row: &mut [u8], qy: f32| {
        for px in x0..half {
            let a = alpha_at(px, qy);
            row[px] = a;
            row[pw - 1 - px] = a;
        }
    };
    let (mut straight, mut curved) = (vec![0u8; pw], vec![0u8; pw]);
    fill_row(&mut straight, 0.0);
    let data = pixmap.data_mut();
    for py in 0..y1 {
        let qy = ((py as f32 + 0.5) / scale - cy).abs() - half_h + r;
        let row = if qy <= 0.0 {
            &straight
        } else {
            fill_row(&mut curved, qy);
            &curved
        };
        // Black, premultiplied: only alpha is set.
        let line = &mut data[py * pw * 4..(py + 1) * pw * 4];
        for px in x0..x1 {
            line[px * 4 + 3] = row[px];
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
            let rest_width = scene.rest_width;
            let target = alert_shape(alert, rest_width);
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
    let zoom = scene.open_zoom;
    let t = expansion(shape, zoom);
    let expanded_alpha = ((t - 0.35) / 0.65).clamp(0.0, 1.0) * (1.0 - alert_alpha);
    // -1 left, 0 middle, 1 right; which page is where is `page_order`.
    let page = scene.page.clamp(-1.0, 1.0);
    // The cover gradient comes with the lyrics panel. It is the backdrop of
    // the whole open notch, not of the player page: it stays put while the
    // pages slide over it (no hard edge between a tinted and a black page),
    // a little dimmer behind the side pages, whose glass modules take on
    // its color.
    // The output picker covers the player, gradient and all.
    let picker = scene.outputs.reveal.clamp(0.0, 1.0);
    // How far the view is from the player's page.
    let side = (page - slot_of(scene.page_order, Page::Player)).abs().min(1.0);
    let backdrop = 1.0 - (1.0 - SIDE_PAGE_TINT) * side;
    let gradient_alpha =
        expanded_alpha * scene.lyrics_reveal.clamp(0.0, 1.0) * (1.0 - picker * (1.0 - side)) * backdrop;
    if gradient_alpha > 0.0 {
        background(pixmap, &path, ts, shape.height, media, transition, gradient_alpha);
    }

    // Content is clipped to the notch so it is revealed as the shape grows.
    let Some(mut clip) = Mask::new(pixmap.width(), pixmap.height()) else { return };
    clip.fill_path(&path, FillRule::Winding, true, ts);

    let mut canvas = Canvas { pixmap, clip: &clip, scale, fonts, dx: 0.0, dy: 0.0 };
    let (left, right) = ((canvas_w - shape.width) / 2.0, (canvas_w + shape.width) / 2.0);

    // The resting content fades out as the notch shrinks into its mini pill.
    let unshrunk = ((shape.width - MINI.width) / (scene.rest_width - MINI.width)).clamp(0.0, 1.0);
    let collapsed_alpha = (1.0 - t * 3.0).clamp(0.0, 1.0) * (1.0 - alert_alpha) * unshrunk * unshrunk;
    if collapsed_alpha > 0.0 {
        let indicators = (scene.battery, scene.bluetooth);
        let equalizer = (scene.clock, scene.levels);
        canvas.collapsed(media, transition, indicators, equalizer, scene.time, left, right, collapsed_alpha);
    }

    if let Some(alert) = scene.alert.as_ref().filter(|_| alert_alpha > 0.0) {
        match &scene.alert_from {
            // Old content drifts up and fades, the new rises in after it,
            // like the track change.
            Some((from, p)) => {
                let (out, incoming) = (Phase::outgoing(*p), Phase::incoming(*p));
                canvas.dy = out.dy;
                canvas.alert(from, scene.levels, left, right, alert_alpha * out.alpha);
                canvas.dy = incoming.dy;
                canvas.alert(alert, scene.levels, left, right, alert_alpha * incoming.alpha);
                canvas.dy = 0.0;
            }
            None => canvas.alert(alert, scene.levels, left, right, alert_alpha),
        }
    }

    if expanded_alpha > 0.0 {
        // The open notch is laid out at its designed size and drawn scaled:
        // in its own units the canvas is narrower (or wider) by the zoom.
        canvas.scale = scale * zoom;
        let canvas_w = canvas_w / zoom;
        let layout = Layout::new(canvas_w, scene.panel_width, scene.lyrics_reveal);
        // Pages sit side by side, edge to edge, in the order of the Layout
        // setting, each centered and as wide as it needs; they slide as one
        // while the notch's width follows, its edge cutting them off; no
        // fading. Positions follow the raw value, which a touchpad can
        // stretch a little past either end (rubber band).
        let raw = scene.page;
        let widths = scene.page_order.map(|p| page_width(p, layout.width, scene.shelf.items.len()));
        // Centers of the left, middle and right pages, the middle at 0.
        let centers = [-(widths[0] + widths[1]) / 2.0, 0.0, (widths[1] + widths[2]) / 2.0];
        let view = if raw >= 0.0 { raw * centers[2] } else { -raw * centers[0] };
        let at = |page: Page| {
            let slot = scene.page_order.iter().position(|&p| p == page).unwrap_or(1);
            let shown = (raw - (slot as f32 - 1.0)).abs() < 1.0;
            shown.then_some(centers[slot] - view)
        };
        if let Some(dx) = at(Page::Player) {
            let a = expanded_alpha;
            canvas.dx = dx;
            // The player dims away behind the sheet growing over it.
            let fade = (1.0 - picker * 1.3).clamp(0.0, 1.0);
            if fade > 0.0 {
                canvas.expanded(media, transition, &scene.lyrics, scene.seek, scene.seek_grow, layout, a * fade);
            }
            if scene.outputs.reveal > 0.001 {
                canvas.output_picker(&scene.outputs, layout, a);
            }
        }
        if let Some(dx) = at(Page::Controls) {
            canvas.dx = dx;
            let left = (canvas_w - CONTROLS_WIDTH) / 2.0;
            canvas.control_center(&scene.controls, left, CONTROLS_WIDTH, expanded_alpha);
        }
        if let Some(dx) = at(Page::Shelf) {
            canvas.dx = dx;
            canvas.shelf(&scene.shelf, canvas_w, scene.clock, expanded_alpha);
        }
        canvas.dx = 0.0;
        canvas.page_dots(canvas_w / 2.0, page, expanded_alpha);
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
            vec![GradientStop::new(0.0, black), GradientStop::new(0.3, black), GradientStop::new(1.0, tint)],
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

/// The bin's lid (0 shut .. 1 open) and gulp (0..1) while files are thrown
/// at it: the lid opens as one nears, and when it lands snaps shut while
/// the bin swells and settles.
fn bin_motion(departures: &[Departure]) -> (f32, f32) {
    let (mut lid, mut gulp) = (0.0f32, 0.0f32);
    for d in departures {
        let p = d.t / THROW;
        if (0.3..1.0).contains(&p) {
            lid = lid.max(((p - 0.3) / 0.35).min(1.0));
        }
        let after = (d.t - THROW) / GULP;
        if (0.0..1.0).contains(&after) {
            // Shuts fast with a little bounce; the swell rises and falls.
            lid = lid.max((1.0 - after * 3.0).max(0.0) - 0.15 * (after * 9.0).sin() * (1.0 - after));
            gulp = gulp.max((std::f32::consts::PI * after).sin());
        }
    }
    (lid.clamp(-0.2, 1.0), gulp)
}

/// The shelf page's parts, for `count` files in an open notch as wide as
/// they need (`shelf_width`).
struct ShelfGeometry {
    left: f32,
    width: f32,
    /// Where the first tile sits, unscrolled: the tiles are centered while
    /// they fit.
    first: f32,
}

const SHELF_TOP: f32 = 44.0;
const SHELF_TILE_W: f32 = 76.0;
const SHELF_STEP: f32 = 80.0;
const SHELF_TILE_H: f32 = 92.0;
const SHELF_ICON_Y: f32 = SHELF_TOP + 6.0;
const SHELF_BUTTON_Y: f32 = 25.0;
const SHELF_BUTTON_R: f32 = 13.0;
const SHELF_CLEAR_W: f32 = 86.0;

/// The tile strip's inset from the shelf page's sides.
const SHELF_PAD: f32 = 18.0;
/// The shelf page is never narrower than this, so the empty drop zone's
/// hint fits, nor wider than the widest open notch for the player.
const SHELF_MIN_W: f32 = 400.0;
const SHELF_MAX_W: f32 = EXPANDED.width;

/// Width of the open notch on the shelf page: snug around its files.
pub fn shelf_width(count: usize) -> f32 {
    let tiles = count as f32 * SHELF_STEP - (SHELF_STEP - SHELF_TILE_W);
    (2.0 * SHELF_PAD + tiles).clamp(SHELF_MIN_W, SHELF_MAX_W)
}

/// Width of the open notch on the Control Center page: wider than the
/// player, so the Now Playing and Bluetooth modules have room for names.
pub const CONTROLS_WIDTH: f32 = 560.0;

/// The open notch's width for `page` (-1 left, 0 middle, 1 right of
/// `order`), with a player `player` wide and `shelf_count` files; between two
/// pages (a touchpad drag) it goes from one width to the other.
pub fn open_width(order: [Page; 3], player: f32, page: f32, shelf_count: usize) -> f32 {
    let page = page.clamp(-1.0, 1.0);
    let width = |slot: usize| page_width(order[slot], player, shelf_count);
    let side = if page >= 0.0 { width(2) } else { width(0) };
    width(1) + (side - width(1)) * page.abs()
}

/// How wide the open notch is on `page`.
fn page_width(page: Page, player: f32, shelf_count: usize) -> f32 {
    match page {
        Page::Shelf => shelf_width(shelf_count),
        Page::Player => player,
        Page::Controls => CONTROLS_WIDTH,
    }
}

/// Where `page` sits in `order`: -1 left, 0 middle, 1 right.
pub fn slot_of(order: [Page; 3], page: Page) -> f32 {
    order.iter().position(|&p| p == page).map_or(0.0, |i| i as f32 - 1.0)
}

impl ShelfGeometry {
    fn new(canvas_w: f32, count: usize) -> Self {
        let width = shelf_width(count);
        let left = (canvas_w - width) / 2.0;
        let mut g = Self { left, width, first: left + SHELF_PAD };
        if count <= g.visible() {
            let (x0, x1) = g.strip();
            let tiles = count as f32 * SHELF_STEP - (SHELF_STEP - SHELF_TILE_W);
            g.first = x0 + ((x1 - x0 - tiles) / 2.0).max(0.0);
        }
        g
    }

    /// Left and right ends of the tile strip.
    fn strip(&self) -> (f32, f32) {
        (self.left + SHELF_PAD, self.left + self.width - SHELF_PAD)
    }

    /// Tiles that fit side by side.
    fn visible(&self) -> usize {
        let (x0, x1) = self.strip();
        (((x1 - x0 + SHELF_STEP - SHELF_TILE_W) / SHELF_STEP) as usize).max(1)
    }

    fn tile_x(&self, index: usize, scroll: f32) -> f32 {
        self.first + (index as f32 - scroll) * SHELF_STEP
    }

    /// The Clear All button: a pill with a bin and its label.
    fn clear_rect(&self) -> Rect {
        let w = SHELF_CLEAR_W;
        Rect::from_xywh(self.left + self.width - 22.0 - w, SHELF_BUTTON_Y - SHELF_BUTTON_R, w, 2.0 * SHELF_BUTTON_R)
            .expect("positive size")
    }

    /// Where the bin is drawn in the Clear All button.
    fn bin_center(&self) -> (f32, f32) {
        (self.clear_rect().left() + 16.0, SHELF_BUTTON_Y)
    }

    fn share_center(&self) -> (f32, f32) {
        (self.clear_rect().left() - 8.0 - SHELF_BUTTON_R, SHELF_BUTTON_Y)
    }

    /// The remove button of a tile at `tile_x`.
    fn remove_center(tile_x: f32) -> (f32, f32) {
        (tile_x + (SHELF_TILE_W + shelf::ICON) / 2.0 - 2.0, SHELF_ICON_Y + 2.0)
    }
}

/// How far the shelf can scroll with `count` files.
pub fn shelf_max_scroll(canvas_width: u32, scale: f32, count: usize) -> f32 {
    let geometry = ShelfGeometry::new(canvas_width as f32 / scale, count);
    count.saturating_sub(geometry.visible()) as f32
}

/// What is at (x, y) (physical px) on the shelf page.
pub fn shelf_at(
    canvas_width: u32,
    scale: f32,
    count: usize,
    scroll: f32,
    (x, y): (f32, f32),
) -> Option<ShelfHit> {
    let (x, y) = (x / scale, y / scale);
    let g = ShelfGeometry::new(canvas_width as f32 / scale, count);
    let near = |(cx, cy): (f32, f32), r: f32| (x - cx).hypot(y - cy) <= r;
    if count > 0 {
        if near(g.share_center(), SHELF_BUTTON_R) {
            return Some(ShelfHit::Share);
        }
        let clear = g.clear_rect();
        if (clear.left() - 2.0..=clear.right() + 2.0).contains(&x) && (clear.top() - 2.0..=clear.bottom() + 2.0).contains(&y) {
            return Some(ShelfHit::Clear);
        }
    }
    let (x0, x1) = g.strip();
    if x < x0 || x > x1 || !(SHELF_TOP..SHELF_TOP + SHELF_TILE_H).contains(&y) {
        return None;
    }
    let index = ((x - g.first) / SHELF_STEP + scroll).floor();
    if index < 0.0 || index as usize >= count {
        return None;
    }
    let index = index as usize;
    let tile_x = g.tile_x(index, scroll);
    if x > tile_x + SHELF_TILE_W {
        return None;
    }
    if near(ShelfGeometry::remove_center(tile_x), 9.0) {
        return Some(ShelfHit::Remove(index));
    }
    Some(ShelfHit::Item(index))
}

/// Left and right ends of the output picker's rows in an open notch at
/// `left`, `width` wide.
fn output_row_span(left: f32, width: f32) -> (f32, f32) {
    (left + 16.0, left + width - 16.0)
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
/// The brightness slider's foot that opens the Displays sheet (two or more
/// screens), measured up from its bottom.
const SLIDER_SYMBOL_ZONE: f32 = 36.0;
/// The Displays sheet: its inset in the Control Center, header, and the
/// height of each screen's row (name above a slider).
const DISPLAYS_INSET: f32 = 8.0;
const DISPLAYS_HEADER: f32 = 32.0;
const DISPLAY_ROW_MAX: f32 = 44.0;
const DISPLAY_SLIDER_H: f32 = 20.0;
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
pub fn control_at(canvas_width: u32, scale: f32, multi: bool, (x, y): (f32, f32)) -> Option<ControlHit> {
    let left = (canvas_width as f32 / scale - CONTROLS_WIDTH) / 2.0;
    let (x, y) = (x / scale - left, y / scale);
    // With several screens, the symbol at the brightness slider's foot is
    // a button of its own.
    let symbol_y = CC_TOP + CC_HEIGHT - SLIDER_SYMBOL_ZONE;
    if multi && (BRIGHTNESS_X..=BRIGHTNESS_X + SLIDER_W).contains(&x) && (symbol_y..=CC_TOP + CC_HEIGHT).contains(&y) {
        return Some(ControlHit::Displays);
    }
    if let Some((toggle, _, _)) = toggles().into_iter().find(|&(_, cx, cy)| (x - cx).hypot(y - cy) <= TOGGLE_RADIUS) {
        return Some(ControlHit::Toggle(toggle));
    }
    let modules_w = modules_width(CONTROLS_WIDTH);
    let media_right = MODULES_X + modules_w;
    let on_media_row = (CC_TOP..=CC_TOP + MODULE_H).contains(&y);
    if modules_w >= MODULES_MIN_W && on_media_row && (MODULES_X..=media_right).contains(&x) {
        let on_button = x >= media_right - MEDIA_BUTTON_W;
        return Some(if on_button { ControlHit::PlayPause } else { ControlHit::NowPlaying });
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

/// The Displays sheet's parts, for `count` screens in a Control Center at
/// `left`, `width` wide.
struct DisplaysGeometry {
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    row_h: f32,
}

/// One screen's row: where its name sits and where its slider's track
/// starts.
struct DisplayRow {
    name_baseline: f32,
    track_y: f32,
}

impl DisplaysGeometry {
    fn new(left: f32, width: f32, count: usize) -> Self {
        let (y0, y1) = (CC_TOP, CC_TOP + CC_HEIGHT);
        let rows = (y1 - y0 - DISPLAYS_HEADER - 4.0) / count.max(1) as f32;
        Self { x0: left + DISPLAYS_INSET, x1: left + width - DISPLAYS_INSET, y0, y1, row_h: rows.min(DISPLAY_ROW_MAX) }
    }

    fn row(&self, i: usize) -> DisplayRow {
        let top = self.y0 + DISPLAYS_HEADER + i as f32 * self.row_h;
        // With little room the name and slider move closer.
        let gap = (self.row_h - DISPLAY_SLIDER_H - 14.0).clamp(2.0, 8.0);
        DisplayRow { name_baseline: top + 11.0, track_y: top + 11.0 + gap }
    }

    /// Left and right ends of the sliders' tracks.
    fn track(&self) -> (f32, f32) {
        (self.x0 + 16.0, self.x1 - 16.0)
    }
}

/// What on the open Displays sheet is under a point in physical canvas
/// pixels, for `count` screens; `None` off the sheet.
pub fn display_at(canvas_width: u32, scale: f32, count: usize, (x, y): (f32, f32)) -> Option<DisplayHit> {
    let left = (canvas_width as f32 / scale - CONTROLS_WIDTH) / 2.0;
    let (x, y) = (x / scale, y / scale);
    let g = DisplaysGeometry::new(left, CONTROLS_WIDTH, count);
    if x < g.x0 || x > g.x1 || y < g.y0 || y > g.y1 {
        return None;
    }
    let (tx0, tx1) = g.track();
    for i in 0..count {
        let row = g.row(i);
        // A little taller than the track, for an easier grab.
        if (row.track_y - 6.0..=row.track_y + DISPLAY_SLIDER_H + 6.0).contains(&y) && (tx0 - 8.0..=tx1 + 8.0).contains(&x) {
            return Some(DisplayHit::Slider(i, display_value(canvas_width, scale, count, x * scale)));
        }
    }
    Some(DisplayHit::Sheet)
}

/// A screen slider's value (0..1) at a physical x, clamped; for dragging
/// past either end.
pub fn display_value(canvas_width: u32, scale: f32, count: usize, x: f32) -> f32 {
    let left = (canvas_width as f32 / scale - CONTROLS_WIDTH) / 2.0;
    let (tx0, tx1) = DisplaysGeometry::new(left, CONTROLS_WIDTH, count).track();
    ((x / scale - tx0) / (tx1 - tx0)).clamp(0.0, 1.0)
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

pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
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
    /// Offset (logical px) for everything drawn; slides pages sideways
    /// (`dx`) and content in and out (`dy`).
    dx: f32,
    dy: f32,
}

impl Canvas<'_> {
    /// Logical to physical pixels, with the page offset.
    fn transform(&self) -> Transform {
        Transform::from_row(self.scale, 0.0, 0.0, self.scale, self.dx * self.scale, self.dy * self.scale)
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
        let multi = view.displays.len() > 1;
        self.slider(left + BRIGHTNESS_X, brightness, Slider::Brightness, multi, alpha);
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
        if view.displays_reveal > 0.001 {
            self.displays_sheet(view, left, width, alpha);
        }
    }

    /// Each screen's brightness on its own slider, like the Display module
    /// of Control Center on a Mac: a sheet that grows out of the brightness
    /// slider over the Control Center, a row per screen with its symbol
    /// (laptop or monitor), name and level. Rows fade in once it is nearly
    /// full size, so none shows cut off by its growing edge.
    fn displays_sheet(&mut self, view: &ControlsView, left: f32, width: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let r = view.displays_reveal.max(0.0);
        let g = DisplaysGeometry::new(left, width, view.displays.len());
        let from = (left + BRIGHTNESS_X, CC_TOP, left + BRIGHTNESS_X + SLIDER_W, CC_TOP + CC_HEIGHT);
        let lerp = |a: f32, b: f32| a + (b - a) * r;
        let (l, t) = (lerp(from.0, g.x0), lerp(from.1, g.y0));
        let (rt, b) = (lerp(from.2, g.x1), lerp(from.3, g.y1));
        let radius = lerp(24.0, 22.0);
        let Some(sheet) = rounded_rect(l, t, rt - l, b - t, radius) else { return };
        self.fill(&sheet, SHEET_RGB, alpha * (r * 4.0).min(1.0));
        self.stroke(&sheet, 1.0, WHITE, alpha * 0.08 * r.min(1.0));
        let rows = ((r - 0.85) / 0.15).clamp(0.0, 1.0);
        if rows <= 0.0 {
            return;
        }
        let a = alpha * rows;
        // Rows settle a few px up into place as they appear.
        let rise = (1.0 - ease_out_cubic(rows)) * 6.0;
        self.dy += rise;
        self.text(&fonts.semibold, "Displays", 13.0, g.x0 + 16.0, g.y0 + 21.0, f32::MAX, WHITE, a);
        for (i, display) in view.displays.iter().enumerate() {
            let row = g.row(i);
            let level = match view.display_drag {
                Some((d, v)) if d == i => Some(v),
                _ => display.percent.map(|p| p as f32 / 100.0),
            };
            // Symbol and name, the level on the right.
            let (sx, name_y) = (g.x0 + 24.0, row.name_baseline);
            if display.internal {
                self.laptop_icon(sx, name_y - 4.0, a);
            } else {
                self.output_icon(OutputKind::Display, sx, name_y - 4.0, a);
            }
            let percent = level.map_or("–".to_string(), |v| format!("{}%", (v * 100.0).round() as u32));
            self.text_right(&fonts.semibold, &percent, 11.5, g.x1 - 16.0, name_y, GRAY, a);
            let name_max = g.x1 - 16.0 - 40.0 - (g.x0 + 40.0);
            self.text(&fonts.semibold, &display.name, 12.5, g.x0 + 40.0, name_y, name_max, WHITE, a);
            // The slider: a track, the level up to the pointer, and a small
            // sun that darkens once the white covers it.
            let (tx0, tx1) = g.track();
            let Some(track) = rounded_rect(tx0, row.track_y, tx1 - tx0, DISPLAY_SLIDER_H, DISPLAY_SLIDER_H / 2.0) else {
                continue;
            };
            let available = a * if level.is_some() { 1.0 } else { 0.4 };
            self.fill(&track, WHITE, available * 0.16);
            let v = level.unwrap_or(0.0).clamp(0.0, 1.0);
            let x = tx0 + (tx1 - tx0) * v;
            if let Some(fill) = Rect::from_ltrb(tx0, row.track_y, x.max(tx0 + 0.5), row.track_y + DISPLAY_SLIDER_H) {
                self.fill_within(&PathBuilder::from_rect(fill), &track, WHITE, available * 0.95);
            }
            let sun = (tx0 + 12.0, row.track_y + DISPLAY_SLIDER_H / 2.0);
            self.small_sun(sun.0, sun.1, x > sun.0 + 6.0, available);
        }
        self.dy -= rise;
    }

    /// A laptop: a screen over a wider base, centered on (cx, cy).
    fn laptop_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        if let Some(screen) = rounded_rect(cx - 7.0, cy - 6.0, 14.0, 9.5, 1.8) {
            self.stroke(&screen, 1.6, WHITE, alpha);
        }
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 9.5, cy + 6.0);
        pb.line_to(cx + 9.5, cy + 6.0);
        if let Some(base) = pb.finish() {
            self.stroke(&base, 1.6, WHITE, alpha);
        }
    }

    /// Two screens, one behind the other: the brightness slider's symbol
    /// when there are several (it opens the Displays sheet).
    fn screens_icon(&mut self, cx: f32, cy: f32, dark: bool, alpha: f32) {
        let rgb = if dark { [40, 40, 40] } else { WHITE };
        if let Some(back) = rounded_rect(cx - 4.0, cy - 7.5, 12.0, 8.5, 1.6) {
            self.stroke(&back, 1.4, rgb, alpha * 0.7);
        }
        if let Some(front) = rounded_rect(cx - 8.0, cy - 3.5, 12.0, 8.5, 1.6) {
            self.fill(&front, if dark { [235, 235, 235] } else { [44, 44, 46] }, alpha);
            self.stroke(&front, 1.4, rgb, alpha);
        }
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 5.0, cy + 7.5);
        pb.line_to(cx + 1.0, cy + 7.5);
        if let Some(stand) = pb.finish() {
            self.stroke(&stand, 1.4, rgb, alpha);
        }
    }

    /// `sun_icon`, smaller, for a screen's slider in the Displays sheet.
    fn small_sun(&mut self, cx: f32, cy: f32, dark: bool, alpha: f32) {
        let rgb = if dark { [40, 40, 40] } else { WHITE };
        if let Some(disc) = PathBuilder::from_circle(cx, cy, 2.4) {
            self.fill(&disc, rgb, alpha);
        }
        let mut pb = PathBuilder::new();
        for i in 0..8 {
            let (sin, cos) = (i as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
            pb.move_to(cx + cos * 4.0, cy + sin * 4.0);
            pb.line_to(cx + cos * 5.4, cy + sin * 5.4);
        }
        if let Some(rays) = pb.finish() {
            self.stroke(&rays, 1.2, rgb, alpha);
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
        let connected = |d: &Device| match d.battery {
            Some(level) => format!("Connected · {level}%"),
            None => "Connected".to_string(),
        };
        let (title, detail) = match (device, view.state.bluetooth) {
            (Some(d), _) => (d.name.as_str(), connected(d)),
            (None, Some(true)) => ("Bluetooth", "Not connected".to_string()),
            (None, Some(false)) => ("Bluetooth", "Off".to_string()),
            (None, None) => ("Bluetooth", "Unavailable".to_string()),
        };
        let text_x = icon_x + 22.0;
        let max = x + w - 12.0 - text_x;
        self.text(&fonts.semibold, title, 12.5, text_x, mid - 2.0, max, WHITE, alpha);
        self.text(&fonts.regular, &detail, 11.0, text_x, mid + 12.0, max, GRAY, alpha);
    }

    /// A tall capsule that fills from the bottom, its symbol at the foot.
    /// `None` (no software brightness) draws it empty and dimmed. `alt`
    /// picks the symbol's other form: volume muted, or brightness for
    /// several screens (a pair of screens, a button for the Displays sheet).
    fn slider(&mut self, x: f32, value: Option<f32>, kind: Slider, alt: bool, alpha: f32) {
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
            // Several screens: a pair of screens, which opens the sheet.
            Slider::Brightness if alt => self.screens_icon(cx, cy, covered, a),
            Slider::Brightness => self.sun_icon(cx, cy, covered, a),
            Slider::Volume if covered => self.dark_speaker(cx - 6.0, cy, v, alt, a),
            Slider::Volume => self.speaker_icon(cx - 6.0, cy, v, alt, a),
        }
    }

    /// `speaker_icon` in near-black, for on top of a white slider level.
    fn dark_speaker(&mut self, x: f32, cy: f32, level: f32, muted: bool, alpha: f32) {
        // Drawn straight in near-black. (It used to be drawn white into a
        // canvas-sized layer and tinted: a full-canvas allocation, pass and
        // composite every frame for a 16 px icon.)
        self.speaker_icon_in(x, cy, level, muted, [40, 40, 40], alpha);
    }

    /// The shelf page: a header with Share and Clear, then the files as
    /// tiles (thumbnail or icon, name), or a drop zone while it is empty or
    /// files are dragged over.
    fn shelf(&mut self, view: &ShelfView, canvas_w: f32, clock: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let count = view.items.len();
        let g = ShelfGeometry::new(canvas_w, count);
        let title = match count {
            0 => "Shelf".to_string(),
            1 => "Shelf \u{b7} 1 file".to_string(),
            n => format!("Shelf \u{b7} {n} files"),
        };
        self.text(&fonts.semibold, &title, 13.0, g.left + 22.0, SHELF_BUTTON_Y + 4.5, 200.0, WHITE, alpha);
        // The last file thrown out, if any: the buttons stay until it is in
        // the bin, and an emptied shelf's drop zone comes in after.
        let last_throw = view.departures.iter().map(|d| d.t).reduce(f32::min);
        let settled = last_throw.map_or(1.0, |t| ((t - THROW * 0.8) / GULP).clamp(0.0, 1.0));
        let buttons = if count > 0 { 1.0 } else if last_throw.is_some() { 1.0 - settled } else { 0.0 };
        if buttons > 0.0 {
            let a = alpha * buttons;
            let lit = |hit| view.hover == Some(hit) || view.pressed == Some(hit);
            let (cx, cy) = g.share_center();
            if let Some(disc) = PathBuilder::from_circle(cx, cy, SHELF_BUTTON_R) {
                self.fill(&disc, WHITE, a * if lit(ShelfHit::Share) { 0.22 } else { 0.12 });
            }
            self.share_icon(cx, cy, a);
            let clear = g.clear_rect();
            if let Some(pill) = rounded_rect(clear.left(), clear.top(), clear.width(), clear.height(), SHELF_BUTTON_R) {
                self.fill(&pill, WHITE, a * if lit(ShelfHit::Clear) { 0.22 } else { 0.12 });
            }
            // The bin opens its lid as a file comes at it and gulps it down.
            let (lid, gulp) = bin_motion(view.departures);
            let (bx, by) = g.bin_center();
            self.trash_icon(bx, by, lid, 1.0 + 0.18 * gulp, a);
            self.text(&fonts.semibold, "Clear All", 11.5, bx + 11.0, SHELF_BUTTON_Y + 4.0, f32::MAX, WHITE, a);
        }

        // The drop zone: always there while the shelf is empty; over the
        // tiles only while files are dragged over it. It lights up blue
        // and swells a little (with the spring's pop), its dashes march
        // round, and an arrow bobs into a tray.
        let pop = view.drop_glow.max(0.0);
        let glow = pop.min(1.0);
        // One after the other, not over each other: what was showing fades
        // out over the first part of the glow, the drop prompt in over the
        // rest.
        let (before, after) = ((1.0 - glow / 0.45).clamp(0.0, 1.0), ((glow - 0.45) / 0.55).clamp(0.0, 1.0));
        let (x0, x1) = g.strip();
        let zone_alpha = alpha * if count == 0 { settled.max(glow) } else { glow };
        if zone_alpha > 0.01 {
            let grow = 3.0 * pop;
            let (w, h) = (x1 - x0 + 2.0 * grow, SHELF_TILE_H - 4.0 + 2.0 * grow);
            if let Some(zone) = rounded_rect(x0 - grow, SHELF_TOP - grow, w, h, 14.0 + grow) {
                self.fill(&zone, BLUE, zone_alpha * 0.18 * glow);
                let rgb = lerp_rgb(WHITE, BLUE, glow);
                self.dashed(&zone, rgb, zone_alpha * (0.35 + 0.55 * glow), -clock * 14.0 * glow);
            }
            let cx = (x0 + x1) / 2.0;
            let bob = (clock * 5.0).sin() * 2.5 * glow;
            let arrow = if count == 0 { 1.0 } else { after };
            self.tray_arrow(cx, SHELF_TOP + 22.0, bob, lerp_rgb(GRAY, BLUE, glow), zone_alpha * arrow);
            // The empty shelf's own hint shows only while it is empty.
            let idle = if count == 0 { before } else { 0.0 };
            let lines = [
                ("Drop files here", "Drag one out to move it; until then it stays where it is", idle),
                ("Drop to add to the shelf", "The files stay where they are", after),
            ];
            for (line1, line2, a) in lines {
                if a > 0.01 {
                    let a = alpha * a;
                    self.text_centered(&fonts.semibold, line1, 14.0, cx, SHELF_TOP + 51.0, WHITE, a);
                    self.text_centered(&fonts.regular, line2, 11.5, cx, SHELF_TOP + 68.0, GRAY, a);
                }
            }
        }
        if count == 0 {
            self.thrown(view, &g, alpha);
            return;
        }
        let tiles_alpha = alpha * before;

        for (i, item) in view.items.iter().enumerate() {
            let mut x = g.tile_x(i, view.scroll);
            // A file was just taken off: every tile glides from where it
            // was (with one more file, the row was laid out differently)
            // to its place, closing the gap.
            if let Some((gap, t)) = view.closing {
                let before = ShelfGeometry::new(canvas_w, count + 1);
                let was = before.tile_x(if i >= gap { i + 1 } else { i }, view.scroll);
                x += (was - x) * (1.0 - ease_out_cubic((t / CLOSE_GAP).clamp(0.0, 1.0)));
            }
            // Just dropped: the new files fly in, one after another, while
            // the ones already there slide over to make room. In flight they
            // are not held back by the drop zone fading out.
            let (motion, base) = match view.arrival {
                Some(arrival) if i < arrival.count => (TileMotion::flying(x, i, arrival.t, arrival.from), alpha),
                Some(arrival) => {
                    let p = (arrival.t / ARRIVAL_SHIFT).clamp(0.0, 1.0);
                    x -= arrival.count as f32 * SHELF_STEP * (1.0 - ease_out_cubic(p));
                    (TileMotion::at_rest(x), tiles_alpha)
                }
                None => (TileMotion::at_rest(x), tiles_alpha),
            };
            // Tiles fade out as they scroll past either end of the strip.
            let overflow = (x0 - x).max(x + SHELF_TILE_W - x1).max(0.0);
            let a = base * (1.0 - overflow / SHELF_TILE_W).clamp(0.0, 1.0);
            if a <= 0.01 {
                continue;
            }
            self.shelf_tile(view, i, item, x, motion, a);
        }
        self.thrown(view, &g, alpha);
    }

    /// The files on their way into the bin, over everything else on the
    /// shelf. Each is picked up (a small lift), then thrown in an arc
    /// toward the Clear All button's bin, shrinking and tipping over as it
    /// goes, and vanishes into it.
    fn thrown(&mut self, view: &ShelfView, g: &ShelfGeometry, alpha: f32) {
        let bin = g.bin_center();
        for d in view.departures {
            let p = (d.t / THROW).clamp(0.0, 1.0);
            if d.t >= THROW {
                continue;
            }
            // Smooth start and end; the path bends up over the shelf and
            // drops into the bin.
            let e = p * p * (3.0 - 2.0 * p);
            let control = ((d.from.0 + bin.0) / 2.0, d.from.1.min(bin.1) - 34.0);
            let bezier = |a: f32, c: f32, b: f32| (1.0 - e) * (1.0 - e) * a + 2.0 * (1.0 - e) * e * c + e * e * b;
            let (cx, cy) = (bezier(d.from.0, control.0, bin.0), bezier(d.from.1, control.1, bin.1));
            let lift = (std::f32::consts::PI * (p / 0.22).min(1.0)).sin() * 0.12;
            let zoom = (1.0 - 0.85 * p.powf(1.4)) * (1.0 + lift);
            let a = alpha * (1.0 - (p - 0.8) / 0.2).clamp(0.0, 1.0);
            let angle = -28.0 * e;
            match &d.icon {
                Some(icon) => self.icon_rotated(icon, cx, cy, zoom, angle, a),
                None => self.document_glyph(cx, cy, zoom, a),
            }
        }
    }

    /// `icon_image`, turned `angle` degrees about its center.
    fn icon_rotated(&mut self, img: &Pixmap, cx: f32, cy: f32, zoom: f32, angle: f32, alpha: f32) {
        let k = zoom / self.scale;
        let (w, h) = (img.width() as f32 * k, img.height() as f32 * k);
        let (x, y) = (cx - w / 2.0, cy - h / 2.0);
        let Some(rect) = Rect::from_xywh(x, y, w, h) else { return };
        let paint = Paint {
            shader: Pattern::new(
                img.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Bilinear,
                alpha,
                Transform::from_row(k, 0.0, 0.0, k, x, y),
            ),
            anti_alias: true,
            ..Default::default()
        };
        let ts = self.transform().pre_concat(Transform::from_rotate_at(angle, cx, cy));
        self.pixmap.fill_path(&PathBuilder::from_rect(rect), &paint, FillRule::Winding, ts, Some(self.clip));
    }

    /// One file's tile at `x`: highlight, icon (where `motion` puts it),
    /// name, and its remove button while hovered.
    fn shelf_tile(&mut self, view: &ShelfView, i: usize, item: &Item, x: f32, motion: TileMotion, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let hovered = matches!(view.hover, Some(ShelfHit::Item(j) | ShelfHit::Remove(j)) if j == i);
        let pressed = matches!(view.pressed, Some(ShelfHit::Item(j)) if j == i);
        let tile = rounded_rect(x, SHELF_TOP, SHELF_TILE_W, SHELF_TILE_H - 4.0, 12.0);
        // Landing: the tile flashes and a ring of light spreads from the
        // icon and fades.
        if let Some(r) = motion.ripple.filter(|&r| r < 1.0) {
            let fade = 1.0 - ease_out_cubic(r);
            if let Some(tile) = &tile {
                self.fill(tile, WHITE, alpha * 0.14 * fade);
            }
            let radius = shelf::ICON / 2.0 + 2.0 + 16.0 * ease_out_cubic(r);
            if let Some(ring) = PathBuilder::from_circle(motion.icon.0, motion.icon.1, radius) {
                self.stroke(&ring, 1.0 + 1.5 * fade, WHITE, alpha * 0.3 * fade);
            }
        }
        let a = alpha * motion.label_alpha;
        if (hovered || pressed)
            && let Some(tile) = &tile
        {
            self.fill(tile, WHITE, a * if pressed { 0.16 } else { 0.09 });
        }
        let (cx, cy) = motion.icon;
        let icon_alpha = alpha * motion.icon_alpha;
        match &item.icon {
            Some(icon) => self.icon_image(icon, cx, cy, motion.zoom, icon_alpha),
            None => self.document_glyph(cx, cy, motion.zoom, icon_alpha),
        }
        if a <= 0.01 {
            return;
        }
        let lines = wrap(&fonts.regular, &item.name, 11.0 * self.scale, (SHELF_TILE_W - 4.0) * self.scale, 2);
        for (row, line) in lines.iter().enumerate() {
            let baseline = SHELF_ICON_Y + shelf::ICON + 14.0 + row as f32 * 13.0;
            self.text_centered(&fonts.regular, line, 11.0, x + SHELF_TILE_W / 2.0, baseline, WHITE, a * 0.9);
        }
        if hovered {
            let (cx, cy) = ShelfGeometry::remove_center(x);
            let lit = view.hover == Some(ShelfHit::Remove(i));
            if let Some(disc) = PathBuilder::from_circle(cx, cy, 8.0) {
                self.fill(&disc, if lit { [90, 90, 94] } else { [60, 60, 64] }, a);
            }
            self.cross(cx, cy, 2.8, 1.5, WHITE, a);
        }
    }

    /// An arrow pointing down into an open tray, centered on (cx, cy); the
    /// arrow lifted by `bob` (the tray stays put).
    fn tray_arrow(&mut self, cx: f32, cy: f32, bob: f32, rgb: Rgb, alpha: f32) {
        let mut pb = PathBuilder::new();
        let (top, tip) = (cy - 11.0 + bob, cy + 2.0 + bob);
        pb.move_to(cx, top);
        pb.line_to(cx, tip);
        pb.move_to(cx - 5.0, tip - 5.0);
        pb.line_to(cx, tip);
        pb.line_to(cx + 5.0, tip - 5.0);
        if let Some(arrow) = pb.finish() {
            self.stroke(&arrow, 2.0, rgb, alpha);
        }
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 11.0, cy + 1.0);
        pb.line_to(cx - 11.0, cy + 8.0);
        pb.line_to(cx + 11.0, cy + 8.0);
        pb.line_to(cx + 11.0, cy + 1.0);
        if let Some(tray) = pb.finish() {
            self.stroke(&tray, 2.0, rgb, alpha);
        }
    }

    /// `img` (drawn at the display's scale) centered on (cx, cy), scaled by
    /// `zoom`, keeping its aspect.
    fn icon_image(&mut self, img: &Pixmap, cx: f32, cy: f32, zoom: f32, alpha: f32) {
        let k = zoom / self.scale;
        let (w, h) = (img.width() as f32 * k, img.height() as f32 * k);
        let (x, y) = (cx - w / 2.0, cy - h / 2.0);
        let Some(rect) = Rect::from_xywh(x, y, w, h) else { return };
        let paint = Paint {
            shader: Pattern::new(
                img.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Bilinear,
                alpha,
                Transform::from_row(k, 0.0, 0.0, k, x, y),
            ),
            anti_alias: true,
            ..Default::default()
        };
        let ts = self.transform();
        self.pixmap.fill_path(&PathBuilder::from_rect(rect), &paint, FillRule::Winding, ts, Some(self.clip));
    }

    /// A plain page with a folded corner, while a file's icon loads.
    fn document_glyph(&mut self, cx: f32, cy: f32, zoom: f32, alpha: f32) {
        let (w, h, fold) = (28.0 * zoom, 36.0 * zoom, 9.0 * zoom);
        let (x, y) = (cx - w / 2.0, cy - h / 2.0);
        let mut pb = PathBuilder::new();
        pb.move_to(x, y);
        pb.line_to(x + w - fold, y);
        pb.line_to(x + w, y + fold);
        pb.line_to(x + w, y + h);
        pb.line_to(x, y + h);
        pb.close();
        if let Some(page) = pb.finish() {
            self.fill(&page, WHITE, alpha * 0.85);
        }
    }

    /// Square with an arrow out of its top, as on Apple's share buttons.
    fn share_icon(&mut self, cx: f32, cy: f32, alpha: f32) {
        let mut pb = PathBuilder::new();
        pb.move_to(cx - 2.5, cy - 2.0);
        pb.line_to(cx - 5.5, cy - 2.0);
        pb.line_to(cx - 5.5, cy + 6.0);
        pb.line_to(cx + 5.5, cy + 6.0);
        pb.line_to(cx + 5.5, cy - 2.0);
        pb.line_to(cx + 2.5, cy - 2.0);
        pb.move_to(cx, cy + 2.0);
        pb.line_to(cx, cy - 7.0);
        pb.move_to(cx - 3.0, cy - 4.0);
        pb.line_to(cx, cy - 7.0);
        pb.line_to(cx + 3.0, cy - 4.0);
        if let Some(path) = pb.finish() {
            self.stroke(&path, 1.6, WHITE, alpha);
        }
    }

    /// A bin: lid with a handle, and a tapered body with two ribs.
    /// `lid` (0..1) tips the lid open about its right end; `scale` swells
    /// the whole bin about its center.
    fn trash_icon(&mut self, cx: f32, cy: f32, lid: f32, scale: f32, alpha: f32) {
        let at = |dx: f32, dy: f32| (cx + dx * scale, cy + dy * scale);
        let hinge = at(6.0, -4.0);
        // Positive turns the free (left) end up, y growing downward.
        let (sin, cos) = (lid * 40f32).to_radians().sin_cos();
        let turn = |(x, y): (f32, f32)| {
            let (dx, dy) = (x - hinge.0, y - hinge.1);
            (hinge.0 + dx * cos - dy * sin, hinge.1 + dx * sin + dy * cos)
        };
        // Strokes: the lid's bar and handle (turned), the body, two ribs.
        let lid_bar = [at(-6.0, -4.0), at(6.0, -4.0)].map(turn);
        let handle = [at(-2.0, -4.0), at(-2.0, -6.0), at(2.0, -6.0), at(2.0, -4.0)].map(turn);
        let body = [at(-4.5, -4.0), at(-3.8, 6.0), at(3.8, 6.0), at(4.5, -4.0)];
        let ribs = [[at(-1.4, -1.0), at(-1.2, 3.5)], [at(1.4, -1.0), at(1.2, 3.5)]];
        let mut pb = PathBuilder::new();
        let polyline: [&[(f32, f32)]; 5] = [&lid_bar, &handle, &body, &ribs[0], &ribs[1]];
        for points in polyline {
            pb.move_to(points[0].0, points[0].1);
            for &(x, y) in &points[1..] {
                pb.line_to(x, y);
            }
        }
        if let Some(path) = pb.finish() {
            self.stroke(&path, 1.5, WHITE, alpha);
        }
    }

    /// An x, `r` from center to each tip.
    #[allow(clippy::too_many_arguments)]
    fn cross(&mut self, cx: f32, cy: f32, r: f32, width: f32, rgb: Rgb, alpha: f32) {
        let mut pb = PathBuilder::new();
        pb.move_to(cx - r, cy - r);
        pb.line_to(cx + r, cy + r);
        pb.move_to(cx + r, cy - r);
        pb.line_to(cx - r, cy + r);
        if let Some(path) = pb.finish() {
            self.stroke(&path, width, rgb, alpha);
        }
    }

    /// A dashed outline, as macOS draws drop zones.
    /// Strokes `path` dashed; `phase` (logical px) moves the dashes along.
    fn dashed(&mut self, path: &Path, rgb: Rgb, alpha: f32, phase: f32) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha.clamp(0.0, 1.0) * 255.0) as u8));
        paint.anti_alias = true;
        let stroke = Stroke {
            width: 1.5,
            line_cap: LineCap::Round,
            dash: tiny_skia::StrokeDash::new(vec![6.0, 5.0], phase.rem_euclid(11.0)),
            ..Default::default()
        };
        let ts = self.transform();
        self.pixmap.stroke_path(path, &paint, &stroke, ts, Some(self.clip));
    }

    /// Three dots along the bottom, centered on `cx` (shelf, player,
    /// Control Center, left to right), the current page's bright.
    fn page_dots(&mut self, cx: f32, page: f32, alpha: f32) {
        for (i, x) in [cx - 9.0, cx, cx + 9.0].into_iter().enumerate() {
            // Brightness follows the slide, so the dots cross over with it.
            let current = match i {
                0 => (-page).max(0.0),
                1 => 1.0 - page.abs(),
                _ => page.max(0.0),
            };
            if let Some(dot) = PathBuilder::from_circle(x, PAGE_DOTS_Y, 2.2) {
                self.fill(&dot, WHITE, alpha * (0.3 + 0.6 * current));
            }
        }
    }

    /// Fills `path` clipped to `within` as well as to the notch.
    fn fill_within(&mut self, path: &Path, within: &Path, rgb: Rgb, alpha: f32) {
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let ts = self.transform();
        // Only `within`'s bounds can be non-zero: the clip is merged there,
        // not over the whole canvas (sliders and the progress bar are a few
        // hundred pixels of it). The mask is kept between calls.
        let Some(bounds) = within.bounds().transform(ts) else { return };
        let (x0, y0) = ((bounds.left().floor().max(0.0)) as usize, (bounds.top().floor().max(0.0)) as usize);
        let (x1, y1) = ((bounds.right().ceil() as usize + 1).min(w as usize), (bounds.bottom().ceil() as usize + 1).min(h as usize));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        SCRATCH_MASK.with_borrow_mut(|slot| {
            let mask = match slot.take() {
                Some(mut mask) if mask.width() == w && mask.height() == h => {
                    mask.data_mut().fill(0);
                    mask
                }
                _ => match Mask::new(w, h) {
                    Some(mask) => mask,
                    None => return,
                },
            };
            let mut mask = mask;
            mask.fill_path(within, FillRule::Winding, true, ts);
            let clip = self.clip.data();
            let data = mask.data_mut();
            for y in y0..y1 {
                let row = y * w as usize;
                for (m, c) in data[row + x0..row + x1].iter_mut().zip(&clip[row + x0..row + x1]) {
                    *m = ((*m as u16 * *c as u16) / 255) as u8;
                }
            }
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8));
            paint.anti_alias = true;
            self.pixmap.fill_path(path, &paint, FillRule::Winding, ts, Some(&mask));
            *slot = Some(mask);
        });
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
        (battery, bluetooth): (Option<Battery>, Option<DeviceKind>),
        (clock, levels): (f32, Option<[f32; crate::meter::BARS]>),
        time: &str,
        left: f32,
        right: f32,
        alpha: f32,
    ) {
        let mid = COLLAPSED.height / 2.0;
        // The time leads on the left, as on the iPhone; what is playing
        // follows it.
        let mut media_left = left + CLOCK_X;
        if let Some(fonts) = self.fonts.filter(|_| !time.is_empty()) {
            self.text(&fonts.semibold, time, CLOCK_SIZE, left + CLOCK_X, mid + CLOCK_SIZE * 0.36, f32::MAX, WHITE, alpha);
            media_left += text_width(&fonts.semibold, time, CLOCK_SIZE * self.scale) / self.scale + CLOCK_GAP;
        }
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
                self.art(&art.pixmap, media_left, mid - 10.0, 20.0, 5.0, alpha * phase.alpha, phase.zoom);
            }
        }
        for (slot, phase) in slots(media, transition, presence_changed) {
            let Some(np) = slot else { continue };
            let color = if np.playing { GREEN } else { GRAY };
            let x0 = match battery {
                Some(_) if np.art.is_some() => media_left + 28.0,
                Some(_) => media_left,
                None => right - 27.0,
            };
            for (i, rest) in [8.0, 14.0, 10.0].into_iter().enumerate() {
                // While playing, the bars follow the output's level; without
                // it, each mixes two waves at its own rates so the bounce
                // never visibly repeats. Paused, they hold still.
                let h = if let Some(levels) = levels.filter(|_| np.playing) {
                    3.0 + 13.0 * levels[i]
                } else if np.playing {
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

    fn alert(&mut self, alert: &Alert, levels: Option<[f32; crate::meter::BARS]>, left: f32, right: f32, alpha: f32) {
        match alert {
            Alert::Battery { event, battery } => self.battery_alert(*event, *battery, left, right, alpha),
            Alert::Volume { level, muted } => self.volume_alert(*level, *muted, left, right, alpha),
            Alert::Toast(toast) => self.toast_alert(toast, left, right, alpha),
            Alert::Bluetooth { device, connected } => self.bluetooth_alert(device, *connected, left, right, alpha),
            Alert::NowPlaying { track, since } => {
                self.now_playing_alert(track, since.elapsed().as_secs_f32(), levels, left, right, alpha)
            }
        }
    }

    /// Cover on the left, title and artist beside it, and an equalizer
    /// dancing in the cover's color on the right. `t` is seconds shown.
    #[allow(clippy::too_many_arguments)]
    fn now_playing_alert(
        &mut self,
        np: &NowPlaying,
        t: f32,
        levels: Option<[f32; crate::meter::BARS]>,
        left: f32,
        right: f32,
        alpha: f32,
    ) {
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
            // Each bar follows the output's level, or without it bounces
            // at its own rate; a paused track rests low.
            let swing = match levels {
                _ if !np.playing => 0.0,
                Some(levels) => levels[i],
                None => 0.5 + 0.5 * (t * (5.0 + i as f32 * 1.7) + i as f32 * 1.3).sin(),
            };
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

    /// Device icon and name on the left; on the right "Disconnected", or on
    /// connecting the device's battery if Windows reports it (as iOS shows
    /// AirPods), else "Connected".
    fn bluetooth_alert(&mut self, device: &Device, connected: bool, left: f32, right: f32, alpha: f32) {
        let Some(fonts) = self.fonts else { return };
        let mid = BLUETOOTH_ALERT.height / 2.0;
        self.device_icon(device.kind, left + 27.0, mid, alpha);

        let status_right = right - 20.0;
        let status_w = match device.battery.filter(|_| connected) {
            Some(percent) => {
                let h = 14.0;
                let w = Self::battery_glyph_width(h, false);
                let battery = Battery { percent, plugged: false, charging: false };
                self.battery_glyph(status_right - w, mid - h / 2.0, h, battery, alpha);
                w
            }
            None => {
                let (status, color) = if connected { ("Connected", GREEN) } else { ("Disconnected", GRAY) };
                self.text_right(&fonts.semibold, status, 13.0, status_right, mid + 4.5, color, alpha);
                text_width(&fonts.semibold, status, 13.0 * self.scale) / self.scale
            }
        };

        let x = left + 44.0;
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
        self.speaker_icon_in(x, mid, level, muted, WHITE, alpha);
    }

    /// `speaker_icon` in `rgb`.
    #[allow(clippy::too_many_arguments)]
    fn speaker_icon_in(&mut self, x: f32, mid: f32, level: f32, muted: bool, rgb: Rgb, alpha: f32) {
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
            paint.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8));
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
                // A dark edge cut around the slash; on a dark icon, the icon's own color.
                self.stroke(&slash, 4.0, if rgb == WHITE { [0, 0, 0] } else { rgb }, alpha);
                self.stroke(&slash, 1.6, rgb, alpha);
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
                self.stroke(&arc, 1.6, rgb, alpha * if on { 1.0 } else { 0.25 });
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
        let mut inner = Canvas { pixmap: &mut *self.pixmap, clip: &clip, scale: self.scale, fonts: self.fonts, dx: self.dx, dy: self.dy };
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
                    let max = layout.player - TEXT_X - TEXT_RIGHT_PAD;
                    let title = TITLE_BASELINE + phase.dy;
                    let subtitle = SUBTITLE_BASELINE + phase.dy;
                    self.text_heavy(&fonts.extrabold, &np.title, TITLE_SIZE, x, title, max, WHITE, a);
                    self.text(&fonts.extrabold, &np.subtitle(), SUBTITLE_SIZE, x, subtitle, max, GRAY, a);
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
            LyricsView::Synced { lines, scroll, sung } => {
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
                    match sung.filter(|&(at, _)| at == i && !line.text.is_empty()) {
                        // The line being sung lights up as it is sung, like
                        // Apple Music: the part still to come stays
                        // translucent.
                        Some((_, chars)) => {
                            let ahead = lerp_rgb(DIM, WHITE, focus * LYRIC_AHEAD);
                            self.glyphs(&fonts.extrabold, text, LYRIC_SIZE, x, baseline, max_width, |i| {
                                let t = ((chars - i as f32) / LYRIC_WIPE).clamp(0.0, 1.0);
                                (lerp_rgb(ahead, rgb, t), a, LYRIC_LIFT * t)
                            });
                        }
                        None => self.text(&fonts.extrabold, text, LYRIC_SIZE, x, baseline, max_width, rgb, a),
                    }
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
        let time_right = right - TIME_RIGHT_INSET;
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
        self.glyphs(font, text, size, x, baseline, max_width, |_| (rgb, alpha, 0.0));
    }

    /// Like `text`, heavier than the font: each glyph is laid down twice,
    /// a physical pixel apart, thickening its stems by that much. Inter's
    /// heaviest weight here is ExtraBold; this gets close to Black without
    /// another font file.
    #[allow(clippy::too_many_arguments)]
    fn text_heavy(
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
        // Overlapping, the two passes would be more opaque than one at the
        // glyph's edges; a little less alpha each keeps them even.
        let each = 1.0 - (1.0 - alpha.clamp(0.0, 1.0)).sqrt();
        let nudge = 1.0 / self.scale;
        // The second pass is one physical pixel right; the line keeps its
        // width by starting half of that left.
        self.glyphs(font, text, size, x - nudge / 2.0, baseline, max_width, |_| (rgb, each, 0.0));
        self.glyphs(font, text, size, x + nudge / 2.0, baseline, max_width, |_| (rgb, each, 0.0));
    }

    /// Lays out `text` like `text`, styling each character by its index:
    /// color, alpha, and how far (logical px) it is lifted.
    #[allow(clippy::too_many_arguments)]
    fn glyphs(
        &mut self,
        font: &Font,
        text: &str,
        size: f32,
        x: f32,
        baseline: f32,
        max_width: f32,
        mut style: impl FnMut(usize) -> (Rgb, f32, f32),
    ) {
        let px = size * self.scale;
        let text = ellipsize(font, text, px, max_width * self.scale);
        let baseline = (baseline + self.dy) * self.scale;
        let mut pen = (x + self.dx) * self.scale;
        for (i, ch) in text.chars().enumerate() {
            let glyph = match self.fonts {
                Some(fonts) => fonts.glyph(font, ch, px),
                None => Rc::new(font.rasterize(ch, px)),
            };
            let (rgb, alpha, lift) = style(i);
            let gx = pen.round() as i32 + glyph.left;
            let gy = (baseline - lift * self.scale).round() as i32 + glyph.top;
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
    use crate::settings::NotchWidth;

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
        draw_shadow(&mut shadow, scale, scene.shape, scene.open_zoom);
        out.draw_pixmap(-(pad as i32), 0, shadow.as_ref(), &Default::default(), Transform::identity(), None);
        out.draw_pixmap(0, 0, notch.as_ref(), &Default::default(), Transform::identity(), None);
        out.save_png(format!("{}/target/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    }

    /// Renders the open notch to `target/preview*.png` to check the layout
    /// without hovering: `cargo test preview -- --ignored`.
    #[test]
    fn now_playing_module_opens_the_player() {
        // 660 px canvas at 1x: Control Center from x 50.
        let left = (660.0 - CONTROLS_WIDTH) / 2.0;
        let y = CC_TOP + MODULE_H / 2.0;
        let right = left + MODULES_X + modules_width(CONTROLS_WIDTH);
        assert_eq!(control_at(660, 1.0, false, (left + MODULES_X + 30.0, y)), Some(ControlHit::NowPlaying));
        assert_eq!(control_at(660, 1.0, false, (right - 10.0, y)), Some(ControlHit::PlayPause));
        // Below it, the battery and Bluetooth modules are not buttons.
        assert_eq!(control_at(660, 1.0, false, (left + MODULES_X + 30.0, CC_TOP + MODULE_H + 20.0)), None);
    }

    #[test]
    fn displays_hit_testing() {
        // 660 px canvas at 1x: Control Center from x 50 (560 wide).
        let left = (660.0 - CONTROLS_WIDTH) / 2.0;
        let foot = (left + BRIGHTNESS_X + SLIDER_W / 2.0, CC_TOP + CC_HEIGHT - 12.0);
        // One screen: the foot is just the slider; several: the button.
        assert!(matches!(control_at(660, 1.0, false, foot), Some(ControlHit::Slider(Slider::Brightness, _))));
        assert_eq!(control_at(660, 1.0, true, foot), Some(ControlHit::Displays));
        // Higher up it is still the slider.
        let mid = (foot.0, CC_TOP + 40.0);
        assert!(matches!(control_at(660, 1.0, true, mid), Some(ControlHit::Slider(Slider::Brightness, _))));
        // On the open sheet: each row's slider, by where along it.
        let g = DisplaysGeometry::new(left, CONTROLS_WIDTH, 2);
        let (x0, x1) = g.track();
        for i in 0..2 {
            let y = g.row(i).track_y + DISPLAY_SLIDER_H / 2.0;
            assert_eq!(display_at(660, 1.0, 2, (x0, y)), Some(DisplayHit::Slider(i, 0.0)));
            assert_eq!(display_at(660, 1.0, 2, ((x0 + x1) / 2.0, y)), Some(DisplayHit::Slider(i, 0.5)));
        }
        assert_eq!(display_at(660, 1.0, 2, (left + 40.0, CC_TOP + 14.0)), Some(DisplayHit::Sheet), "the title");
        assert_eq!(display_at(660, 1.0, 2, (5.0, 70.0)), None, "off the sheet");
        assert_eq!(display_at(660, 1.0, 2, (330.0, 145.0)), None, "below the sheet");
        // Dragging keeps tracking past either end.
        assert_eq!(display_value(660, 1.0, 2, 0.0), 0.0);
        assert_eq!(display_value(660, 1.0, 2, 659.0), 1.0);
    }

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
                lyrics: LyricsView::Synced { lines: &lines, scroll: 1.0, sung: Some((1, 9.5)) },
                seek: None,
                seek_grow: 0.0,
                battery: Some(charging),
                rest_width: COLLAPSED_BATTERY_WIDTH,
                bluetooth: None,
                alert: None,
                alert_from: None,
                clock: 0.0,
                levels: None,
                open_zoom: 1.0,
                page_order: Page::ALL,
                time: "9:41",
                outputs: OutputsView::default(),
                shelf: ShelfView::default(),
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
                rest_width: COLLAPSED_BATTERY_WIDTH,
                bluetooth: None,
                alert: None,
                alert_from: None,
                clock: 0.0,
                levels: None,
                open_zoom: 1.0,
                page_order: Page::ALL,
                time: "9:41",
                outputs: OutputsView::default(),
                shelf: ShelfView::default(),
                page: 0.0,
                controls: ControlsView::default(),
            },
            "preview_player.png",
        );

        // Control Center: full width, player-only width, and mid-slide.
        let buds = Device { name: "Galaxy Buds2 Pro".into(), kind: DeviceKind::Headphones, battery: Some(72) };
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
            ..Default::default()
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
            rest_width: COLLAPSED_BATTERY_WIDTH,
            bluetooth: None,
            alert: None,
            alert_from: None,
            clock: 0.0,
            levels: None,
            open_zoom: 1.0,
            page_order: Page::ALL,
            time: "9:41",
            outputs: OutputsView::default(),
            shelf: ShelfView::default(),
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
        // The notch as wide as the page it is on.
        fn opened(scene: Scene<'_>) -> Scene<'_> {
            Scene {
                shape: Shape {
                    width: open_width(scene.page_order, scene.panel_width, scene.page, scene.shelf.items.len()),
                    ..EXPANDED
                },
                ..scene
            }
        }
        save_preview(&picker(1.0), "preview_outputs.png");
        // The Open size setting: the whole open notch scaled.
        for (zoom, name) in [(0.9, "preview_open_compact.png"), (1.1, "preview_open_large.png")] {
            let shape = Shape { width: EXPANDED.width * zoom, height: EXPANDED.height * zoom, radius: EXPANDED.radius * zoom };
            save_preview(&Scene { shape, open_zoom: zoom, ..page(EXPANDED.width, 0.0) }, name);
        }
        save_preview(&picker(0.15), "preview_outputs_opening_a.png");
        save_preview(&picker(0.45), "preview_outputs_opening_b.png");
        save_preview(&picker(0.75), "preview_outputs_opening_c.png");
        // The highlight on its way from the speakers to the monitor.
        let sliding = Scene {
            outputs: OutputsView { list: &outputs, reveal: 1.0, highlight: 0.6, hover: 1.0, hover_alpha: 1.0, ..Default::default() },
            ..page(EXPANDED.width, 0.0)
        };
        save_preview(&sliding, "preview_outputs_picking.png");
        save_preview(&opened(page(EXPANDED.width, 1.0)), "preview_controls.png");
        // A custom order (Layout setting): Control Center left, shelf right;
        // halfway from the player to Control Center.
        let order = [Page::Controls, Page::Player, Page::Shelf];
        for (name, at) in [("left", -1.0), ("between", -0.5), ("right", 1.0)] {
            let scene = Scene { page_order: order, ..page(EXPANDED.width, at) };
            save_preview(&opened(scene), &format!("preview_order_{name}.png"));
        }
        // A laptop with an external monitor: the brightness slider's symbol
        // becomes a pair of screens, which opens each screen's slider.
        let screens = [
            DisplayLevel { name: "Built-in Display".into(), internal: true, percent: Some(81) },
            DisplayLevel { name: "Mi Monitor".into(), internal: false, percent: Some(100) },
        ];
        for (name, reveal) in [("closed", 0.0), ("opening", 0.55), ("open", 1.0)] {
            let view = ControlsView { displays: &screens, displays_reveal: reveal, ..controls };
            let scene = Scene { controls: view, ..page(EXPANDED.width, 1.0) };
            save_preview(&opened(scene), &format!("preview_displays_{name}.png"));
        }
        save_preview(&opened(page(PLAYER_ONLY_WIDTH, 0.5)), "preview_controls_narrow_sliding.png");
        save_preview(&opened(page(EXPANDED.width, 0.5)), "preview_controls_sliding.png");

        // The shelf: files (one with a picture thumbnail, one hovered), then
        // empty, then with files coming in, then sliding up to the player.
        let mut picture = Pixmap::new(88, 66).unwrap();
        picture.fill(Color::from_rgba8(70, 130, 180, 255));
        let file = |name: &str, icon: Option<Arc<Pixmap>>| shelf::Item {
            path: std::path::PathBuf::from(format!("C:/{name}")),
            name: name.into(),
            icon,
        };
        let items = [
            file("Holiday photo.jpg", Some(Arc::new(picture))),
            file("Quarterly report final v2.docx", None),
            file("notes.txt", None),
        ];
        let full = ShelfView { items: &items, hover: Some(ShelfHit::Remove(1)), ..Default::default() };
        save_preview(&opened(Scene { shelf: full, ..page(EXPANDED.width, -1.0) }), "preview_shelf.png");
        save_preview(&opened(page(EXPANDED.width, -1.0)), "preview_shelf_empty.png");
        let incoming = ShelfView { items: &items, drop_glow: 1.0, ..Default::default() };
        save_preview(&opened(Scene { shelf: incoming, clock: 0.3, ..page(EXPANDED.width, -1.0) }), "preview_shelf_drop.png");
        // Lighting up halfway, and an empty shelf lit up.
        let lighting = ShelfView { items: &items, drop_glow: 0.5, ..Default::default() };
        save_preview(&opened(Scene { shelf: lighting, ..page(EXPANDED.width, -1.0) }), "preview_shelf_drop_lighting.png");
        let empty_lit = ShelfView { drop_glow: 1.0, ..Default::default() };
        save_preview(&opened(Scene { shelf: empty_lit, ..page(EXPANDED.width, -1.0) }), "preview_shelf_drop_empty.png");
        // Two files dropped on one already there: they fall in, it slides over.
        for (name, t) in [("a", 0.08), ("b", 0.2), ("c", 0.36), ("d", 0.5), ("e", 0.7)] {
            let arrival = Arrival { count: 2, t, from: (330.0, 20.0) };
            let arriving = ShelfView { items: &items, arrival: Some(arrival), ..Default::default() };
            let file = format!("preview_shelf_arrival_{name}.png");
            save_preview(&opened(Scene { shelf: arriving, ..page(EXPANDED.width, -1.0) }), &file);
        }
        let sliding = Scene { shelf: ShelfView { items: &items, ..Default::default() }, ..page(EXPANDED.width, -0.5) };
        save_preview(&opened(sliding), "preview_shelf_sliding.png");
        // Taking the middle file off: it is thrown into the bin while the
        // last slides over; then Clear All throwing the rest.
        let middle = shelf_icon_center(1200, 2.0, items.len(), 0.0, 1).unwrap();
        let kept = [file("Holiday photo.jpg", items[0].icon.clone()), file("notes.txt", None)];
        for (name, t) in [("a", 0.08), ("b", 0.22), ("c", 0.4), ("d", 0.6)] {
            let departures = [Departure { icon: None, from: middle, t }];
            let view = ShelfView { items: &kept, departures: &departures, closing: Some((1, t)), ..Default::default() };
            save_preview(&opened(Scene { shelf: view, ..page(EXPANDED.width, -1.0) }), &format!("preview_shelf_remove_{name}.png"));
        }
        let all: Vec<(f32, f32)> = (0..kept.len()).map(|i| shelf_icon_center(1200, 2.0, kept.len(), 0.0, i).unwrap()).collect();
        for (name, t) in [("a", 0.2), ("b", 0.45), ("c", 0.8)] {
            let departures: Vec<Departure> = kept
                .iter()
                .zip(&all)
                .enumerate()
                .map(|(i, (item, &from))| Departure { icon: item.icon.clone(), from, t: t - THROW_STAGGER * i as f32 })
                .collect();
            let view = ShelfView { departures: &departures, ..Default::default() };
            save_preview(&opened(Scene { shelf: view, ..page(EXPANDED.width, -1.0) }), &format!("preview_shelf_clear_{name}.png"));
        }

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
            rest_width: COLLAPSED_BATTERY_WIDTH,
            bluetooth: Some(DeviceKind::Headphones),
            alert,
            alert_from: None,
            clock: 0.0,
            levels: None,
            open_zoom: 1.0,
            page_order: Page::ALL,
            time: "9:41",
            outputs: OutputsView::default(),
            shelf: ShelfView::default(),
            page: 0.0,
            controls: ControlsView::default(),
        };
        let rest = Shape { width: COLLAPSED_BATTERY_WIDTH, ..COLLAPSED };
        save_preview(&small(rest, on_battery, None), "preview_collapsed.png");
        // The widest setting: same content, more room around it.
        let wider = Shape { width: COLLAPSED_BATTERY_WIDTH + NotchWidth::Wider.extra(), ..COLLAPSED };
        let scene = Scene { rest_width: wider.width, ..small(wider, on_battery, None) };
        save_preview(&scene, "preview_collapsed_wider.png");
        // The narrowest, at its fullest: art, equalizer, Bluetooth, and a
        // charging battery; then without a battery.
        let narrow = Shape { width: COLLAPSED_BATTERY_WIDTH + NotchWidth::Narrow.extra(), ..COLLAPSED };
        let scene = Scene { rest_width: narrow.width, ..small(narrow, charging, None) };
        save_preview(&scene, "preview_collapsed_narrow.png");
        let narrow = Shape { width: COLLAPSED.width + NotchWidth::Narrow.extra(), ..COLLAPSED };
        let scene = Scene { rest_width: narrow.width, battery: None, ..small(narrow, charging, None) };
        save_preview(&scene, "preview_collapsed_narrow_no_battery.png");
        let buds = Device { name: "Galaxy Buds2 Pro".into(), kind: DeviceKind::Headphones, battery: Some(72) };
        let connected = Alert::Bluetooth { device: buds, connected: true };
        save_preview(&small(BLUETOOTH_ALERT, on_battery, Some(connected)), "preview_bluetooth.png");
        // A low keyboard, and a device that reports no battery.
        let keyboard = Device { name: "AULA-S75Pro-1".into(), kind: DeviceKind::Keyboard, battery: Some(15) };
        let low = Alert::Bluetooth { device: keyboard, connected: true };
        save_preview(&small(BLUETOOTH_ALERT, on_battery, Some(low)), "preview_bluetooth_low.png");
        let phone = Device { name: "Pixel 8".into(), kind: DeviceKind::Phone, battery: None };
        let plain = Alert::Bluetooth { device: phone, connected: true };
        save_preview(&small(BLUETOOTH_ALERT, on_battery, Some(plain)), "preview_bluetooth_no_battery.png");
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
            link: None,
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
            link: None,
        };
        let toast = Alert::Toast(Arc::new(toast));
        save_preview(&small(TOAST_ALERT, on_battery, Some(toast)), "preview_toast.png");
        let glyph_toast = Alert::Toast(Arc::new(glyph_toast));
        save_preview(&small(TOAST_ALERT, on_battery, Some(glyph_toast)), "preview_toast_glyph.png");
        // A new track, 0.4 s in, the equalizer mid-bounce.
        let started = Instant::now() - Duration::from_millis(400);
        let now_playing = Alert::NowPlaying { track: Box::new(np.clone()), since: started };
        save_preview(&small(NOW_PLAYING_ALERT, on_battery, Some(now_playing)), "preview_now_playing.png");
        // With the bars following the music rather than animating.
        let levels = Some([0.35, 0.9, 0.6, 0.75]);
        let now_playing = Alert::NowPlaying { track: Box::new(np.clone()), since: started };
        let scene = Scene { levels, ..small(NOW_PLAYING_ALERT, on_battery, Some(now_playing)) };
        save_preview(&scene, "preview_now_playing_levels.png");
        save_preview(&Scene { levels, ..small(rest, on_battery, None) }, "preview_collapsed_levels.png");
        // NOTCH_LIVE_METER=1: a few frames from what this machine is playing
        // right now, through the real meter.
        if std::env::var_os("NOTCH_LIVE_METER").is_some() {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
            let mut meter = crate::meter::Meter::new();
            let mut bars = crate::meter::Bars::default();
            for frame in 0..120 {
                let levels = bars.update(meter.read(), 1.0 / 30.0);
                if frame >= 60 && frame % 15 == 0 {
                    println!("frame {frame}: {levels:?}");
                    let now_playing = Alert::NowPlaying { track: Box::new(np.clone()), since: started };
                    let scene = Scene { levels, ..small(NOW_PLAYING_ALERT, on_battery, Some(now_playing)) };
                    save_preview(&scene, &format!("preview_live_alert_{frame}.png"));
                    save_preview(&Scene { levels, ..small(rest, on_battery, None) }, &format!("preview_live_rest_{frame}.png"));
                }
                std::thread::sleep(Duration::from_millis(33));
            }
        }

        // A window under the notch: the mini pill, content faded out.
        save_preview(&small(MINI, on_battery, None), "preview_mini.png");

        // Every device icon side by side, on a black strip.
        let scale = 4.0;
        let mut strip = Pixmap::new((120.0 * scale) as u32, (30.0 * scale) as u32).unwrap();
        strip.fill(Color::BLACK);
        let mut clip = Mask::new(strip.width(), strip.height()).unwrap();
        clip.data_mut().fill(255);
        let fonts = Fonts::load();
        let mut canvas = Canvas { pixmap: &mut strip, clip: &clip, scale, fonts: fonts.as_ref(), dx: 0.0, dy: 0.0 };
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
