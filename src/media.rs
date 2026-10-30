//! Reads the system "now playing" session (Spotify, browsers, Media Player...)
//! on a background thread and sends media commands back to it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tiny_skia::{IntSize, Pixmap};
use windows::core::{IUnknown, Interface, Result};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Imaging::*;
use windows::Media::Control::*;
use windows::Storage::Streams::IRandomAccessStreamReference;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::palette::{self, Rgb};

/// Posted to the notch window whenever the shared state changes.
pub const WM_MEDIA: u32 = WM_APP + 2;

const POLL: Duration = Duration::from_millis(500);
/// After a track change the art is read again on every poll for this long,
/// whether or not the player said it changed: browsers (Chromium: Brave,
/// Chrome, Edge) publish the new title first, with the previous track's
/// art or their own icon, and the track's art a moment later.
const ART_SETTLE: Duration = Duration::from_secs(4);
/// Album art is decoded to a square of this size (physical pixels).
const ART_SIZE: u32 = 160;

#[derive(Clone)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub art: Option<Arc<Artwork>>,
    /// Missing for live streams and players that do not report it.
    pub timeline: Option<Timeline>,
}

/// Playback position as last reported by the player.
#[derive(Clone, Copy, PartialEq)]
pub struct Timeline {
    pub position: Duration,
    pub duration: Duration,
    pub updated: SystemTime,
}

pub struct Artwork {
    /// Premultiplied RGBA, ready for tiny-skia.
    pub pixmap: Pixmap,
    /// Color the expanded background fades into toward the bottom.
    pub tint: Rgb,
}

impl NowPlaying {
    fn same_as(&self, other: &NowPlaying) -> bool {
        self.title == other.title
            && self.artist == other.artist
            && self.album == other.album
            && self.playing == other.playing
            // Art read again for the same track keeps its `Arc` unless
            // the picture changed (`read`), so this notices a corrected one.
            && match (&self.art, &other.art) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (a, b) => a.is_none() == b.is_none(),
            }
            && self.timeline == other.timeline
    }

    /// "Artist - Album", or just the artist when there is no album.
    pub fn subtitle(&self) -> String {
        if self.album.is_empty() {
            self.artist.clone()
        } else {
            format!("{} - {}", self.artist, self.album)
        }
    }

    /// Elapsed and total time. Players only report the position now and
    /// then, so while playing it is extrapolated from the last report.
    pub fn progress(&self) -> Option<(Duration, Duration)> {
        let t = self.timeline?;
        let mut position = t.position;
        if self.playing {
            position += SystemTime::now().duration_since(t.updated).unwrap_or_default();
        }
        Some((position.min(t.duration), t.duration))
    }
}

/// Whether the title/artist differ, counting "nothing playing" as a value.
pub fn track_changed(a: Option<&NowPlaying>, b: Option<&NowPlaying>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.title != b.title || a.artist != b.artist,
        (None, None) => false,
        _ => true,
    }
}

/// Whether the album art differs. Unchanged art keeps the same `Arc`.
pub fn art_changed(a: Option<&NowPlaying>, b: Option<&NowPlaying>) -> bool {
    match (a.and_then(|a| a.art.as_ref()), b.and_then(|b| b.art.as_ref())) {
        (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
        (None, None) => track_changed(a, b),
        _ => true,
    }
}

#[derive(Clone, Copy)]
pub enum Command {
    Previous,
    TogglePlayPause,
    Next,
    /// Jump to this position from the start of the track.
    Seek(Duration),
}

pub struct Media {
    commands: Sender<Command>,
    state: Arc<Mutex<Option<NowPlaying>>>,
}

impl Media {
    /// Starts the media thread; it posts `WM_MEDIA` to `hwnd` on every change.
    pub fn spawn(hwnd: HWND) -> Self {
        let (commands, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(None));
        let shared = state.clone();
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, rx, shared) {
                eprintln!("media thread stopped: {e}");
            }
        });
        Self { commands, state }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn current(&self) -> Option<NowPlaying> {
        self.state.lock().unwrap().clone()
    }
}

fn run(hwnd: isize, rx: Receiver<Command>, shared: Arc<Mutex<Option<NowPlaying>>>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
    let mut last: Option<NowPlaying> = None;
    // Set by the current session when its title, artist or art change, so
    // the art is read again rather than kept from the first read.
    let art_stale = Arc::new(AtomicBool::new(false));
    // The session whose changes are watched: its identity and the handler's
    // registration, to move the handler when another session takes over.
    let mut watched: Option<(GlobalSystemMediaTransportControlsSession, usize, i64)> = None;
    // When the playing track (title and artist) last changed.
    let mut track_since = Instant::now();

    loop {
        match rx.recv_timeout(POLL) {
            Ok(command) => {
                if let Ok(session) = manager.GetCurrentSession() {
                    if let Err(e) = execute(&session, command) {
                        eprintln!("media command failed: {e}");
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        watch(&manager, &mut watched, &art_stale);
        let settling = track_since.elapsed() < ART_SETTLE;
        let recheck = art_stale.swap(false, Ordering::Relaxed) || settling;
        let next = read(&manager, last.as_ref(), recheck);
        if track_changed(last.as_ref(), next.as_ref()) {
            track_since = Instant::now();
        }
        let changed = match (&last, &next) {
            (Some(a), Some(b)) => !a.same_as(b),
            (None, None) => false,
            _ => true,
        };
        if changed {
            *shared.lock().unwrap() = next.clone();
            last = next;
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as _)), WM_MEDIA, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn execute(session: &GlobalSystemMediaTransportControlsSession, command: Command) -> Result<()> {
    let op = match command {
        Command::Previous => session.TrySkipPreviousAsync()?,
        Command::TogglePlayPause => session.TryTogglePlayPauseAsync()?,
        Command::Next => session.TrySkipNextAsync()?,
        Command::Seek(position) => {
            // Positions are 100 ns ticks on the player's own timeline,
            // which need not start at zero.
            let start = session.GetTimelineProperties()?.StartTime()?.Duration;
            session.TryChangePlaybackPositionAsync(start + (position.as_nanos() / 100) as i64)?
        }
    };
    op.get()?;
    Ok(())
}

/// Keeps a `MediaPropertiesChanged` handler on the current session, which
/// flags `stale` when its title, artist or art change.
fn watch(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    watched: &mut Option<(GlobalSystemMediaTransportControlsSession, usize, i64)>,
    stale: &Arc<AtomicBool>,
) {
    let current = manager.GetCurrentSession().ok();
    // COM identity: the same session always gives the same IUnknown.
    let id = current.as_ref().and_then(|s| s.cast::<IUnknown>().ok()).map(|u| u.as_raw() as usize);
    if watched.as_ref().map(|(_, id, _)| *id) == id {
        return;
    }
    if let Some((session, _, token)) = watched.take() {
        let _ = session.RemoveMediaPropertiesChanged(token);
    }
    let (Some(session), Some(id)) = (current, id) else { return };
    let flag = stale.clone();
    let handler = TypedEventHandler::new(move |_, _| {
        flag.store(true, Ordering::Relaxed);
        Ok(())
    });
    match session.MediaPropertiesChanged(&handler) {
        Ok(token) => *watched = Some((session, id, token)),
        Err(e) => eprintln!("media: cannot watch session: {e}"),
    }
    // A new session's art is read afresh.
    stale.store(true, Ordering::Relaxed);
}

/// Returns `None` when nothing is playing. While the track is unchanged its
/// art is decoded once and reused, unless `recheck` (the player said it
/// changed, or the track just started): then it is read again, and kept as
/// the same `Arc` when the picture is the same.
fn read(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    prev: Option<&NowPlaying>,
    recheck: bool,
) -> Option<NowPlaying> {
    let session = manager.GetCurrentSession().ok()?;
    let props = session.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let title = props.Title().ok()?.to_string_lossy();
    if title.is_empty() {
        return None;
    }
    let artist = props.Artist().map(|s| s.to_string_lossy()).unwrap_or_default();
    let album = props.AlbumTitle().map(|s| s.to_string_lossy()).unwrap_or_default();
    let timeline = session.GetTimelineProperties().ok().and_then(|t| read_timeline(&t));
    let playing = session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .is_ok_and(|s| s == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);

    let cached = prev
        .filter(|p| p.title == title && p.artist == artist)
        .and_then(|p| p.art.clone());
    // Players often publish the title before the thumbnail (or with the
    // wrong one), so a missing art is retried on the next poll, and a
    // present one read again while `recheck`.
    let art = match cached {
        Some(art) if !recheck => Some(art),
        cached => {
            let fresh = props.Thumbnail().ok().and_then(|thumb| decode_art(&thumb).ok().flatten());
            match (fresh, cached) {
                (Some(fresh), Some(cached)) if fresh.pixmap.data() == cached.pixmap.data() => Some(cached),
                (Some(fresh), _) => Some(Arc::new(fresh)),
                // No thumbnail now: keep what was shown for this track.
                (None, cached) => cached,
            }
        }
    };

    Some(NowPlaying { title, artist, album, playing, art, timeline })
}

fn read_timeline(t: &GlobalSystemMediaTransportControlsSessionTimelineProperties) -> Option<Timeline> {
    // WinRT times are 100 ns ticks; DateTime counts from 1601-01-01.
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    let ticks = |v: i64| Duration::from_nanos(v.max(0) as u64 * 100);

    let start = t.StartTime().ok()?.Duration;
    let end = t.EndTime().ok()?.Duration;
    let position = t.Position().ok()?.Duration;
    if end <= start {
        return None;
    }
    let updated = t.LastUpdatedTime().ok()?.UniversalTime;
    Some(Timeline {
        position: ticks(position - start),
        duration: ticks(end - start),
        updated: UNIX_EPOCH + ticks(updated - UNIX_EPOCH_TICKS),
    })
}

/// Decodes the thumbnail into a center-cropped `ART_SIZE` square.
fn decode_art(thumb: &IRandomAccessStreamReference) -> Result<Option<Artwork>> {
    Ok(decode_square(thumb, ART_SIZE)?.map(|pixmap| Artwork {
        tint: palette::tint(&pixmap),
        pixmap,
    }))
}

/// Decodes an image into a center-cropped `size` square of premultiplied
/// RGBA.
pub fn decode_square(source: &IRandomAccessStreamReference, size: u32) -> Result<Option<Pixmap>> {
    let stream = source.OpenReadAsync()?.get()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let (w, h) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    if w == 0 || h == 0 {
        return Ok(None);
    }

    // Scale so the short side fits, then crop the long side ("cover").
    let scale = size as f32 / w.min(h) as f32;
    let (sw, sh) = (
        ((w as f32 * scale).round() as u32).max(size),
        ((h as f32 * scale).round() as u32).max(size),
    );
    let transform = BitmapTransform::new()?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    transform.SetScaledWidth(sw)?;
    transform.SetScaledHeight(sh)?;
    transform.SetBounds(BitmapBounds {
        X: (sw - size) / 2,
        Y: (sh - size) / 2,
        Width: size,
        Height: size,
    })?;

    let pixels = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Premultiplied,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .get()?
        .DetachPixelData()?;
    let Some(size) = IntSize::from_wh(size, size) else { return Ok(None) };
    Ok(Pixmap::from_vec(pixels.to_vec(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(art: Option<Arc<Artwork>>) -> NowPlaying {
        NowPlaying {
            title: "Song".into(),
            artist: "Artist".into(),
            album: String::new(),
            playing: true,
            art,
            timeline: None,
        }
    }

    fn artwork(shade: u8) -> Arc<Artwork> {
        let mut pixmap = Pixmap::new(4, 4).unwrap();
        pixmap.fill(tiny_skia::Color::from_rgba8(shade, shade, shade, 255));
        Arc::new(Artwork { tint: [shade; 3], pixmap })
    }

    #[test]
    fn notices_corrected_art() {
        // A browser first shows its own icon, then the track's cover: same
        // title and artist, so only the art tells them apart.
        let (icon, cover) = (artwork(10), artwork(200));
        let first = track(Some(icon.clone()));
        let corrected = track(Some(cover));
        assert!(!first.same_as(&corrected));
        assert!(art_changed(Some(&first), Some(&corrected)));
        assert!(!track_changed(Some(&first), Some(&corrected)));
        // The same art read again (kept as the same Arc) is no change.
        let again = track(Some(icon));
        assert!(first.same_as(&again));
        assert!(!art_changed(Some(&first), Some(&again)));
    }
}
