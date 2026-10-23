//! Fetches lyrics from LRCLIB (https://lrclib.net) on a background thread.
//! Only the track's title, artist, album and duration are sent.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::core::{h, Result, HSTRING};
use windows::Data::Json::{JsonArray, JsonObject, JsonValueType};
use windows::Foundation::Uri;
use windows::Web::Http::{HttpClient, HttpStatusCode};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the notch window when a lookup finishes.
pub const WM_LYRICS: u32 = WM_APP + 3;

const API: &str = "https://lrclib.net/api";
/// Search results within this many seconds of the playing track count as a match.
const DURATION_TOLERANCE: f64 = 3.0;
/// Extra attempts after a failed request (LRCLIB sometimes answers 503).
const RETRIES: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_secs(3);
/// Highlights a line slightly early, so it lights up as the singer starts.
const LEAD: Duration = Duration::from_millis(200);

pub struct Line {
    pub time: Duration,
    pub text: String,
}

pub struct Lyrics {
    pub lines: Vec<Line>,
    /// Plain lyrics have no timestamps and cannot follow playback.
    pub synced: bool,
}

impl Lyrics {
    /// Index of the line being sung at `position`; `None` before the first.
    pub fn current_line(&self, position: Duration) -> Option<usize> {
        self.lines.partition_point(|l| l.time <= position + LEAD).checked_sub(1)
    }
}

#[derive(Clone)]
pub enum LyricsState {
    Loading,
    Missing,
    Found(Arc<Lyrics>),
}

#[derive(Clone)]
pub struct Query {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<Duration>,
}

type Key = (String, String);

pub struct Fetcher {
    requests: Sender<Query>,
    result: Arc<Mutex<Option<(Key, LyricsState)>>>,
}

impl Fetcher {
    /// Starts the lookup thread; it posts `WM_LYRICS` to `hwnd` after each lookup.
    pub fn spawn(hwnd: HWND) -> Self {
        let (requests, rx) = mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let shared = result.clone();
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, rx, shared) {
                eprintln!("lyrics thread stopped: {e}");
            }
        });
        Self { requests, result }
    }

    pub fn request(&self, query: Query) {
        let _ = self.requests.send(query);
    }

    /// The finished lookup for this track, if the last one was for it.
    pub fn result_for(&self, title: &str, artist: &str) -> Option<LyricsState> {
        let result = self.result.lock().unwrap();
        let (key, state) = result.as_ref()?;
        (key.0 == title && key.1 == artist).then(|| state.clone())
    }
}

fn run(hwnd: isize, rx: Receiver<Query>, shared: Arc<Mutex<Option<(Key, LyricsState)>>>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let client = HttpClient::new()?;
    // LRCLIB asks clients to identify themselves.
    client.DefaultRequestHeaders()?.UserAgent()?.TryParseAdd(h!("notch-woy/0.1"))?;
    let mut cache: HashMap<Key, Option<Arc<Lyrics>>> = HashMap::new();

    let mut next = rx.recv().ok();
    while let Some(mut query) = next.take() {
        // Skipping through tracks queues several requests; only the last matters.
        while let Ok(newer) = rx.try_recv() {
            query = newer;
        }
        let key = (query.title.clone(), query.artist.clone());
        let found = match cache.get(&key) {
            Some(found) => found.clone(),
            None => match lookup_with_retry(&client, &query, &rx) {
                Outcome::Done(found) => {
                    let found = found.map(Arc::new);
                    cache.insert(key.clone(), found.clone());
                    found
                }
                // Failures are not cached, so the next play tries again.
                Outcome::Failed => None,
                Outcome::Superseded(newer) => {
                    next = Some(newer);
                    continue;
                }
            },
        };
        let state = found.map_or(LyricsState::Missing, LyricsState::Found);
        *shared.lock().unwrap() = Some((key, state));
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as _)), WM_LYRICS, WPARAM(0), LPARAM(0));
        }
        next = rx.recv().ok();
    }
    Ok(())
}

enum Outcome {
    Done(Option<Lyrics>),
    Failed,
    /// Another track was requested while waiting to retry.
    Superseded(Query),
}

fn lookup_with_retry(client: &HttpClient, query: &Query, rx: &Receiver<Query>) -> Outcome {
    for attempt in 0..=RETRIES {
        match lookup(client, query) {
            Ok(found) => return Outcome::Done(found),
            Err(e) => eprintln!("lyrics lookup failed (attempt {}): {e}", attempt + 1),
        }
        if attempt < RETRIES {
            match rx.recv_timeout(RETRY_DELAY) {
                Ok(newer) => return Outcome::Superseded(newer),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    Outcome::Failed
}

/// Tries an exact match first, then looser searches for titles that carry
/// extra decoration ("(Official Video)", "Artist - Song" on YouTube...).
///
/// A failed request does not stop the remaining ones, since LRCLIB answers
/// 503 now and then. But "not found" is only reported when every request
/// got a clean answer; otherwise the failure is returned so the lookup is
/// retried rather than cached as missing.
fn lookup(client: &HttpClient, q: &Query) -> Result<Option<Lyrics>> {
    let mut exact = vec![("track_name", q.title.as_str()), ("artist_name", q.artist.as_str())];
    if !q.album.is_empty() {
        exact.push(("album_name", &q.album));
    }
    let duration = q.duration.map(|d| d.as_secs().to_string());
    if let Some(d) = &duration {
        exact.push(("duration", d));
    }

    // Each request names the title its results must match.
    let title = clean_title(&q.title);
    let mut requests = vec![
        ("get", exact, title.as_str()),
        ("search", vec![("track_name", title.as_str()), ("artist_name", q.artist.as_str())], title.as_str()),
    ];
    if let Some((left, right)) = title.split_once(" - ") {
        // YouTube: "Artist - Song"; Spotify: "Song - Remastered 2011".
        requests.push(("search", vec![("track_name", right), ("artist_name", left)], right));
        requests.push(("search", vec![("track_name", left), ("artist_name", q.artist.as_str())], left));
    }

    let mut last_error = None;
    for (endpoint, params, expected) in requests {
        let body = match get(client, endpoint, &params) {
            Ok(Some(body)) => body,
            Ok(None) => continue,
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        };
        let records: Vec<JsonObject> = if endpoint == "get" {
            JsonObject::Parse(&body).ok().into_iter().collect()
        } else {
            JsonArray::Parse(&body)
                .ok()
                .map(|a| (0..a.Size().unwrap_or(0)).filter_map(|i| a.GetObjectAt(i).ok()).collect())
                .unwrap_or_default()
        };
        if let Some(lyrics) = best_match(&records, expected, q.duration) {
            return Ok(Some(lyrics));
        }
    }
    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// GETs `API/endpoint?params`; `Ok(None)` on 404.
fn get(client: &HttpClient, endpoint: &str, params: &[(&str, &str)]) -> Result<Option<HSTRING>> {
    let mut url = format!("{API}/{endpoint}?");
    for (i, (name, value)) in params.iter().enumerate() {
        let value = Uri::EscapeComponent(&HSTRING::from(*value))?;
        url += &format!("{}{name}={value}", if i > 0 { "&" } else { "" });
    }
    let response = client.GetAsync(&Uri::CreateUri(&HSTRING::from(url))?)?.get()?;
    if response.StatusCode()? == HttpStatusCode::NotFound {
        return Ok(None);
    }
    response.EnsureSuccessStatusCode()?;
    Ok(Some(response.Content()?.ReadAsStringAsync()?.get()?))
}

/// Picks the result whose title and length fit the playing track,
/// preferring synced lyrics.
fn best_match(records: &[JsonObject], expected: &str, duration: Option<Duration>) -> Option<Lyrics> {
    let matching: Vec<&JsonObject> = records
        .iter()
        .filter(|r| string_field(r, "trackName").is_some_and(|name| title_matches(&name, expected)))
        .filter(|r| match (duration, number_field(r, "duration")) {
            (Some(d), Some(theirs)) => (d.as_secs_f64() - theirs).abs() <= DURATION_TOLERANCE,
            _ => true,
        })
        .collect();
    let synced = matching.iter().find(|r| string_field(r, "syncedLyrics").is_some());
    synced.or(matching.first()).and_then(|r| parse_record(r))
}

/// Whether a result's track name fits the title searched for. Search is
/// fuzzy: without this, "Queen - Bohemian Rhapsody" can come back as
/// "Killer Queen". The name must appear in the title and cover a fair part
/// of it, so a song called just "Queen" does not match either.
fn title_matches(name: &str, expected: &str) -> bool {
    let normalize = |s: &str| -> String {
        clean_title(s).chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    };
    let (name, expected) = (normalize(name), normalize(expected));
    !name.is_empty()
        && expected.contains(&name)
        && name.chars().count() as f32 >= expected.chars().count() as f32 * 0.4
}

fn parse_record(record: &JsonObject) -> Option<Lyrics> {
    if let Some(lrc) = string_field(record, "syncedLyrics") {
        let lines = parse_lrc(&lrc);
        if !lines.is_empty() {
            return Some(Lyrics { lines, synced: true });
        }
    }
    if let Some(plain) = string_field(record, "plainLyrics") {
        let lines = plain
            .lines()
            .map(|text| Line { time: Duration::ZERO, text: text.trim().to_owned() })
            .collect();
        return Some(Lyrics { lines, synced: false });
    }
    let instrumental = record
        .GetNamedValue(h!("instrumental"))
        .and_then(|v| v.GetBoolean())
        .unwrap_or(false);
    instrumental.then(|| Lyrics {
        lines: vec![Line { time: Duration::ZERO, text: "Instrumental".into() }],
        synced: false,
    })
}

fn string_field(obj: &JsonObject, name: &str) -> Option<String> {
    let value = obj.GetNamedValue(&HSTRING::from(name)).ok()?;
    if value.ValueType().ok()? != JsonValueType::String {
        return None;
    }
    let text = value.GetString().ok()?.to_string_lossy();
    (!text.trim().is_empty()).then_some(text)
}

fn number_field(obj: &JsonObject, name: &str) -> Option<f64> {
    obj.GetNamedValue(&HSTRING::from(name)).ok()?.GetNumber().ok()
}

/// Parses LRC text ("[mm:ss.xx] words"). A line may carry several
/// timestamps; metadata tags like "[ar:...]" are skipped.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            let Some(time) = parse_timestamp(&tag[..end]) else { break };
            times.push(time);
            rest = tag[end + 1..].trim_start();
        }
        for time in times {
            lines.push(Line { time, text: rest.trim().to_owned() });
        }
    }
    lines.sort_by_key(|l| l.time);
    lines
}

fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (min, sec) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let sec: f64 = sec.trim().parse().ok()?;
    (sec >= 0.0).then(|| Duration::from_secs(min * 60) + Duration::from_secs_f64(sec))
}

/// Drops "(Official Video)", "[Lyrics]", "(feat. X)" and "| channel" tails.
pub fn clean_title(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in title.chars() {
        match c {
            '(' | '[' | '【' => depth += 1,
            ')' | ']' | '】' => depth = (depth - 1).max(0),
            '|' if depth == 0 => break,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let lines = parse_lrc("[ar:Someone]\n[00:12.50] Hello\n[01:02.00][00:05.00]Twice\n[00:20.00]\n");
        let got: Vec<(u64, &str)> = lines.iter().map(|l| (l.time.as_millis() as u64, l.text.as_str())).collect();
        assert_eq!(got, vec![(5000, "Twice"), (12500, "Hello"), (20000, ""), (62000, "Twice")]);
    }

    #[test]
    fn finds_current_line() {
        let lyrics = Lyrics { lines: parse_lrc("[00:10.00]a\n[00:20.00]b"), synced: true };
        assert_eq!(lyrics.current_line(Duration::from_secs(5)), None);
        assert_eq!(lyrics.current_line(Duration::from_secs(10)), Some(0));
        assert_eq!(lyrics.current_line(Duration::from_millis(19_900)), Some(1));
    }

    /// Hits the real LRCLIB API: `cargo test fetches_known_song -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_known_song() {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        let client = HttpClient::new().unwrap();
        // A YouTube-style title, to exercise the cleanup and fallback searches.
        let query = Query {
            title: "Queen - Bohemian Rhapsody (Official Video Remastered)".into(),
            artist: "Queen Official".into(),
            album: String::new(),
            duration: None,
        };
        // Same retrying path as the app; the sender stays alive so retries wait.
        let (_tx, rx) = mpsc::channel();
        let Outcome::Done(found) = lookup_with_retry(&client, &query, &rx) else {
            panic!("lookup failed after retries");
        };
        let lyrics = found.expect("lyrics found");
        assert!(lyrics.synced);
        assert!(lyrics.lines.len() > 20, "{} lines", lyrics.lines.len());
        println!("{} lines, first: {:?}", lyrics.lines.len(), lyrics.lines.first().map(|l| (l.time, &l.text)));
    }

    #[test]
    fn matches_titles() {
        assert!(title_matches("Bohemian Rhapsody", "Queen - Bohemian Rhapsody"));
        assert!(title_matches("Bohemian Rhapsody (Remastered 2011)", "Bohemian Rhapsody"));
        assert!(title_matches("Bohemian Rhapsody", "Bohemian Rhapsody - Remastered 2011"));
        assert!(!title_matches("Killer Queen", "Queen - Bohemian Rhapsody"));
        assert!(!title_matches("Queen", "Queen - Bohemian Rhapsody"));
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(
            clean_title("Maman Fvndy - Pilih Mama Muda (Official Music Video) | Channel"),
            "Maman Fvndy - Pilih Mama Muda"
        );
        assert_eq!(clean_title("Song [Lyrics] (feat. X)"), "Song");
    }
}
