//! Plugins: scripts or programs in the plugins folder whose output the
//! notch shows, in the manner of xbar and SwiftBar on macOS (and their
//! plugin format, so the idea carries over).
//!
//! A plugin is a file named `{name}.{interval}.{ext}`, such as
//! `weather.10m.ps1`: run every interval (`s`, `m`, `h`, `d`; at least
//! `MIN_INTERVAL`, `DEFAULT_INTERVAL` without one) as its own process, with
//! no window. Its standard output (UTF-8; the console's code page is
//! accepted too) is read like SwiftBar's: the first line before a `---`
//! line is its title, optionally followed by `| href=...` (opened when the
//! alert is clicked); the first line after `---` is shown under it. When
//! the title changes, the notch shows it as an alert.
//!
//! Plugins run in their own processes on a worker thread: one that hangs
//! is killed after `TIMEOUT`, one that crashes takes nothing with it. They
//! only run while Plugins is turned on in Settings (off by default), and
//! only what the user put in the folder.

use std::collections::HashMap;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_APP};

/// Posted to the notch window when a plugin's title changed; take it with
/// `Plugins::take`.
pub const WM_PLUGIN: u32 = WM_APP + 13;

/// How often a plugin runs when its file name gives no interval.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);
/// The shortest interval honored: starting a script (PowerShell especially)
/// costs more than a moment, and the notch is no place for a ticker.
const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// A plugin still running after this is stopped.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How often the folder is looked at for plugins added or removed, and how
/// long the thread sleeps at most.
const TICK: Duration = Duration::from_secs(1);
const RESCAN: Duration = Duration::from_secs(5);
/// More output than this is not read (a plugin shows a line or two).
const MAX_OUTPUT: usize = 64 * 1024;
/// Keeps the title and body to what fits on an alert.
const MAX_LINE: usize = 200;

/// What a plugin has to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub title: String,
    pub body: String,
    /// Opened when the alert is clicked: a URL or a path.
    pub href: Option<String>,
}

/// A plugin's title change, for the notch.
#[derive(Clone, Debug)]
pub struct Update {
    /// The plugin's name: its file name before the interval.
    pub name: String,
    pub output: Output,
}

pub struct Plugins {
    enabled: Arc<AtomicBool>,
    updates: Arc<Mutex<Vec<Update>>>,
}

impl Plugins {
    /// Starts the worker thread; it runs plugins only while enabled.
    pub fn spawn(hwnd: HWND, enabled: bool) -> Self {
        let flag = Arc::new(AtomicBool::new(enabled));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (flag2, updates2) = (flag.clone(), updates.clone());
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || run(raw, flag2, updates2));
        Self { enabled: flag, updates }
    }

    pub fn set_enabled(&self, on: bool) {
        if on {
            // Ready for the user to drop plugins into.
            let _ = std::fs::create_dir_all(folder());
        }
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// The title changes since last asked, oldest first.
    pub fn take(&self) -> Vec<Update> {
        std::mem::take(&mut *self.updates.lock().unwrap())
    }
}

/// `%LOCALAPPDATA%\notch-woy\plugins`.
pub fn folder() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("notch-woy").join("plugins")
}

/// Opens the plugins folder in Explorer, making it first if need be.
pub fn open_folder() {
    let dir = folder();
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(dir.as_os_str()), None, None, SW_SHOWNORMAL);
    }
}

/// Opens a plugin's link: a web address or a file.
pub fn open_link(href: &str) {
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(href), None, None, SW_SHOWNORMAL);
    }
}

/// A plugin file found in the folder.
struct Plugin {
    name: String,
    interval: Duration,
    next: Instant,
    /// The file's last change: a plugin edited runs again right away.
    modified: Option<SystemTime>,
    /// The last title shown; `None` until it first ran, which only sets it
    /// (no alert for what a plugin says when the notch starts).
    last: Option<Output>,
}

fn run(hwnd: isize, enabled: Arc<AtomicBool>, updates: Arc<Mutex<Vec<Update>>>) {
    let mut plugins: HashMap<PathBuf, Plugin> = HashMap::new();
    let mut scanned: Option<Instant> = None;
    loop {
        thread::sleep(TICK);
        if !enabled.load(Ordering::Relaxed) {
            // Starting over when turned back on: no alert for whatever
            // changed meanwhile.
            plugins.clear();
            scanned = None;
            continue;
        }
        if scanned.is_none_or(|at| at.elapsed() >= RESCAN) {
            scanned = Some(Instant::now());
            rescan(&mut plugins);
        }
        let now = Instant::now();
        let mut due: Vec<PathBuf> = plugins.iter().filter(|(_, p)| p.next <= now).map(|(path, _)| path.clone()).collect();
        due.sort();
        for path in due {
            if !enabled.load(Ordering::Relaxed) {
                break;
            }
            let output = execute(&path);
            let Some(plugin) = plugins.get_mut(&path) else { continue };
            plugin.next = Instant::now() + plugin.interval;
            let Some(output) = output else { continue };
            let changed = plugin.last.as_ref().is_some_and(|last| last.title != output.title);
            plugin.last = Some(output.clone());
            if changed && !output.title.is_empty() {
                updates.lock().unwrap().push(Update { name: plugin.name.clone(), output });
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_PLUGIN, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

/// Brings `plugins` in line with the folder: new files are added (and run
/// now), removed ones dropped, edited ones run again.
fn rescan(plugins: &mut HashMap<PathBuf, Plugin>) {
    let Ok(entries) = std::fs::read_dir(folder()) else {
        plugins.clear();
        return;
    };
    let mut seen = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some((name, interval)) = path.file_name().and_then(|n| n.to_str()).and_then(parse_name) else { continue };
        if !entry.file_type().is_ok_and(|t| t.is_file()) || launcher(&path).is_none() {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        seen.push(path.clone());
        let plugin = plugins.entry(path).or_insert_with(|| Plugin {
            name,
            interval,
            next: Instant::now(),
            modified,
            last: None,
        });
        if plugin.modified != modified {
            plugin.modified = modified;
            plugin.next = Instant::now();
        }
    }
    plugins.retain(|path, _| seen.contains(path));
}

/// A plugin's name and interval from its file name, `{name}.{interval}.{ext}`
/// or `{name}.{ext}`; `None` for files that are not plugins (hidden ones,
/// a readme...).
fn parse_name(file: &str) -> Option<(String, Duration)> {
    if file.starts_with('.') || file.starts_with('_') {
        return None;
    }
    let (stem, _ext) = file.rsplit_once('.')?;
    let (name, interval) = match stem.rsplit_once('.') {
        Some((name, tag)) => match parse_interval(tag) {
            Some(interval) => (name, interval),
            None => (stem, DEFAULT_INTERVAL),
        },
        None => (stem, DEFAULT_INTERVAL),
    };
    (!name.is_empty()).then(|| (name.to_owned(), interval.max(MIN_INTERVAL)))
}

/// "30s", "10m", "1h", "1d".
fn parse_interval(tag: &str) -> Option<Duration> {
    let split = tag.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = tag.split_at(split);
    let n: u64 = number.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// How to run a plugin file: the program, and the arguments before none.
fn launcher(path: &Path) -> Option<(PathBuf, Vec<std::ffi::OsString>)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let file = path.as_os_str().to_owned();
    Some(match ext.as_str() {
        "exe" => (path.to_owned(), Vec::new()),
        "cmd" | "bat" => ("cmd.exe".into(), vec!["/D".into(), "/C".into(), file]),
        "ps1" => (
            "powershell.exe".into(),
            vec!["-NoProfile".into(), "-NonInteractive".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), file],
        ),
        "py" => ("python.exe".into(), vec![file]),
        _ => return None,
    })
}

/// Runs a plugin once and reads what it printed; `None` if it could not
/// start, failed (non-zero exit) or ran too long.
fn execute(path: &Path) -> Option<Output> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let (program, args) = launcher(path)?;
    let mut child = Command::new(program)
        .args(args)
        // Relative paths in a plugin are from where it lives.
        .current_dir(path.parent().unwrap_or(Path::new(".")))
        .env("NOTCH_WOY", "1")
        .env("NOTCH_WOY_PLUGINS_PATH", folder())
        .env("NOTCH_WOY_PLUGIN_PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .inspect_err(|e| eprintln!("plugin {}: {e}", path.display()))
        .ok()?;
    // Read on the side so a chatty plugin cannot fill the pipe and stall.
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT as u64).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < TIMEOUT => thread::sleep(Duration::from_millis(50)),
            _ => {
                eprintln!("plugin {} timed out", path.display());
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status?.success() {
        return None;
    }
    Some(parse_output(&decode(&out)))
}

/// UTF-8, or else the console's code page (what cmd and PowerShell write to
/// a pipe by default).
fn decode(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.trim_start_matches('\u{feff}').to_owned();
    }
    unsafe {
        let n = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        let mut wide = vec![0u16; n.max(0) as usize];
        let n = MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide));
        String::from_utf16_lossy(&wide[..n.max(0) as usize])
    }
}

/// SwiftBar's format, the parts the notch uses: the header's first line
/// (before `---`) with its `| href=` parameter, and the body's first line.
fn parse_output(text: &str) -> Output {
    let mut header = None;
    let mut body = None;
    let mut in_body = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("---") {
            if in_body && body.is_some() {
                break;
            }
            in_body = true;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if !in_body && header.is_none() {
            header = Some(line);
        } else if in_body && body.is_none() {
            body = Some(line);
        }
    }
    let (title, href) = split_params(header.unwrap_or(""));
    let (body, _) = split_params(body.unwrap_or(""));
    Output { title: clip(title), body: clip(body), href }
}

/// "Text | href=https://x color=red" into the text and its href.
fn split_params(line: &str) -> (&str, Option<String>) {
    let Some((text, params)) = line.split_once('|') else { return (line.trim(), None) };
    let href = params
        .split_whitespace()
        .find_map(|p| p.strip_prefix("href="))
        .map(|v| v.trim_matches(|c| c == '"' || c == '\'').to_owned())
        .filter(|v| !v.is_empty());
    (text.trim(), href)
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_LINE).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_names() {
        assert_eq!(parse_name("weather.10m.ps1"), Some(("weather".into(), Duration::from_secs(600))));
        assert_eq!(parse_name("build.30s.cmd"), Some(("build".into(), Duration::from_secs(30))));
        assert_eq!(parse_name("my.tool.1h.py"), Some(("my.tool".into(), Duration::from_secs(3600))));
        // No interval: the default; too short: the minimum.
        assert_eq!(parse_name("clock.ps1"), Some(("clock".into(), DEFAULT_INTERVAL)));
        assert_eq!(parse_name("fast.1s.ps1"), Some(("fast".into(), MIN_INTERVAL)));
        assert_eq!(parse_name("odd.10x.ps1"), Some(("odd.10x".into(), DEFAULT_INTERVAL)));
        assert_eq!(parse_name(".hidden.ps1"), None);
        assert_eq!(parse_name("README"), None);
        assert!(launcher(Path::new("notes.txt")).is_none());
        assert!(launcher(Path::new("a.5m.PS1")).is_some());
    }

    #[test]
    fn reads_swiftbar_output() {
        let out = parse_output("Build passed | href=https://ci.example/42 color=green\n---\nmain, 3 min ago\nmore\n");
        assert_eq!(out.title, "Build passed");
        assert_eq!(out.body, "main, 3 min ago");
        assert_eq!(out.href.as_deref(), Some("https://ci.example/42"));
        // Just a line.
        let out = parse_output("\u{1F327} 24°C\n");
        assert_eq!((out.title.as_str(), out.body.as_str(), out.href), ("\u{1F327} 24°C", "", None));
        // Header lines after the first are SwiftBar's rotating titles: the
        // first is kept.
        assert_eq!(parse_output("one\ntwo\n---\nbody").title, "one");
        assert_eq!(parse_output("").title, "");
    }

    #[test]
    fn decodes_console_output() {
        assert_eq!(decode("héllo".as_bytes()), "héllo");
        assert_eq!(decode(b"\xef\xbb\xbfhi"), "hi");
        // Not UTF-8: read in the console's code page rather than dropped.
        assert!(!decode(&[0x82, b'a']).is_empty());
    }

    /// Runs a real PowerShell plugin: `cargo test runs_a_plugin -- --ignored`.
    #[test]
    #[ignore]
    fn runs_a_plugin() {
        let dir = std::env::temp_dir().join("notch-woy-plugin-test");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("hello.5s.ps1");
        std::fs::write(&script, "Write-Output 'Hello from a plugin | href=https://example.com'\nWrite-Output '---'\nWrite-Output 'second line'\n").unwrap();
        let out = execute(&script).expect("ran");
        assert_eq!(out.title, "Hello from a plugin");
        assert_eq!(out.body, "second line");
        assert_eq!(out.href.as_deref(), Some("https://example.com"));
        let failing = dir.join("fail.5s.cmd");
        std::fs::write(&failing, "@echo nope\r\n@exit /b 1\r\n").unwrap();
        assert!(execute(&failing).is_none());
    }
}
