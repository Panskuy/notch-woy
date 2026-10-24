//! Picks up new toast notifications (WhatsApp, mail, chat apps...) from the
//! Windows notification center.
//!
//! The change event needs a packaged app, so a background thread polls the
//! list instead and reports notifications it has not seen before. Nothing
//! leaves the machine; the content is only shown in the notch.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tiny_skia::{Pixmap, Transform};
use windows::core::{Interface, HSTRING, Result};
use windows::Foundation::Size;
use windows::Storage::Streams::IRandomAccessStreamReference;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_APP};

use crate::media;
use crate::palette::{self, Rgb};

/// Posted to the notch window when new notifications are queued.
pub const WM_TOAST: u32 = WM_APP + 5;

const POLL: Duration = Duration::from_millis(700);
/// App icons are decoded to this size (physical pixels).
const ICON_SIZE: u32 = 128;

pub struct Toast {
    pub app: String,
    /// Sender or subject; the app name when the toast has no title.
    pub title: String,
    pub body: String,
    pub icon: Option<Pixmap>,
    /// The icon is a full tile of its own; otherwise it is a glyph on a
    /// transparent background (as WhatsApp's) and gets a tile drawn under it.
    pub icon_is_tile: bool,
    /// Opens the app; empty for apps Windows gives no id for.
    pub app_id: String,
    /// The icon's main color, for the card's glow.
    pub tint: Option<Rgb>,
}

impl Toast {
    /// Brings up the app that sent the notification.
    pub fn open_app(&self) {
        if self.app_id.is_empty() {
            return;
        }
        let target = HSTRING::from(format!(r"shell:AppsFolder\{}", self.app_id));
        unsafe {
            ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from("explorer.exe"), &target, None, SW_SHOWNORMAL);
        }
    }
}

pub struct Notifications {
    queue: Arc<Mutex<VecDeque<Arc<Toast>>>>,
}

impl Notifications {
    /// Starts polling; posts `WM_TOAST` to `hwnd` when something new arrives.
    pub fn spawn(hwnd: HWND) -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let shared = queue.clone();
        // HWND is not Send; pass the raw handle value across.
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            if let Err(e) = run(raw, shared) {
                eprintln!("notification watcher stopped: {e}");
            }
        });
        Self { queue }
    }

    /// The newest unshown notification; older ones queued with it are
    /// dropped, as the notch shows one at a time.
    pub fn take_latest(&self) -> Option<Arc<Toast>> {
        let mut queue = self.queue.lock().unwrap();
        let latest = queue.pop_back();
        queue.clear();
        latest
    }
}

fn run(hwnd: isize, queue: Arc<Mutex<VecDeque<Arc<Toast>>>>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let listener = UserNotificationListener::Current()?;
    if listener.RequestAccessAsync()?.get()? != UserNotificationListenerAccessStatus::Allowed {
        eprintln!("notification access denied (Settings > Privacy > Notifications)");
        return Ok(());
    }

    // What is already in the notification center at start-up is old news.
    let mut seen: HashSet<u32> = current(&listener)?.iter().filter_map(|n| n.Id().ok()).collect();
    loop {
        thread::sleep(POLL);
        let Ok(list) = current(&listener) else { continue };
        let mut fresh = Vec::new();
        let mut ids = HashSet::with_capacity(list.len());
        for n in &list {
            let Ok(id) = n.Id() else { continue };
            ids.insert(id);
            if !seen.contains(&id) {
                if let Some(toast) = read(n) {
                    fresh.push(Arc::new(toast));
                }
            }
        }
        // Only ids still listed are kept, so dismissed ones are forgotten.
        seen = ids;
        if !fresh.is_empty() {
            queue.lock().unwrap().extend(fresh);
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as _)), WM_TOAST, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn current(listener: &UserNotificationListener) -> Result<Vec<UserNotification>> {
    Ok(listener.GetNotificationsAsync(NotificationKinds::Toast)?.get()?.into_iter().collect())
}

fn read(n: &UserNotification) -> Option<Toast> {
    let info = n.AppInfo().ok();
    let display = info.as_ref().and_then(|i| i.DisplayInfo().ok());
    let app = display.as_ref().and_then(|d| d.DisplayName().ok()).map(|s| s.to_string()).unwrap_or_default();
    let app_id = info.and_then(|i| i.AppUserModelId().ok()).map(|s| s.to_string()).unwrap_or_default();

    let binding = n.Notification().ok()?.Visual().ok()?.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()?;
    let texts: Vec<String> = binding
        .GetTextElements()
        .ok()?
        .into_iter()
        .filter_map(|t| t.Text().ok().map(|s| s.to_string()))
        .filter(|s| !s.trim().is_empty())
        .collect();
    let (title, body) = match texts.split_first() {
        Some((title, rest)) => (title.clone(), rest.join("\n")),
        None => return None,
    };
    let title = if title.is_empty() { app.clone() } else { title };

    let icon = display
        .and_then(|d| d.GetLogo(Size { Width: ICON_SIZE as f32, Height: ICON_SIZE as f32 }).ok())
        .and_then(|logo| logo.cast::<IRandomAccessStreamReference>().ok())
        .and_then(|logo| media::decode_square(&logo, ICON_SIZE).ok().flatten())
        .map(|icon| trim(&icon));

    let tint = icon.as_ref().map(palette::tint);
    let icon_is_tile = icon.as_ref().is_some_and(is_tile);
    Some(Toast { app, title, body, icon, icon_is_tile, app_id, tint })
}

/// Crops the transparent margin app logos come with (often half the
/// image), keeping the result square and centered on the logo.
pub fn trim(icon: &Pixmap) -> Pixmap {
    let (w, h) = (icon.width() as i32, icon.height() as i32);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, -1, -1);
    for (i, px) in icon.pixels().iter().enumerate() {
        if px.alpha() > 16 {
            let (x, y) = (i as i32 % w, i as i32 / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    if x1 < x0 {
        return icon.clone();
    }
    let side = (x1 - x0 + 1).max(y1 - y0 + 1);
    let (cx, cy) = ((x0 + x1 + 1) / 2, (y0 + y1 + 1) / 2);
    let Some(mut out) = Pixmap::new(side as u32, side as u32) else { return icon.clone() };
    let offset = (side / 2 - cx, side / 2 - cy);
    out.draw_pixmap(offset.0, offset.1, icon.as_ref(), &Default::default(), Transform::identity(), None);
    out
}

/// Whether a (trimmed) logo is mostly opaque, i.e. a tile of its own.
pub fn is_tile(icon: &Pixmap) -> bool {
    let opaque = icon.pixels().iter().filter(|px| px.alpha() > 200).count();
    opaque as f32 > icon.pixels().len() as f32 * 0.8
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn trims_logo_margins() {
        // A 20x10 opaque bar in a 96 px transparent square, off center.
        let mut logo = Pixmap::new(96, 96).unwrap();
        let bar = tiny_skia::Rect::from_xywh(10.0, 60.0, 20.0, 10.0).unwrap();
        logo.fill_rect(bar, &tiny_skia::Paint::default(), Transform::identity(), None);
        let trimmed = trim(&logo);
        assert_eq!((trimmed.width(), trimmed.height()), (20, 20));
        assert!(!is_tile(&trimmed), "half the square is still empty");
        let full = Pixmap::new(8, 8).map(|mut p| {
            p.fill(tiny_skia::Color::BLACK);
            p
        });
        assert!(is_tile(&full.unwrap()));
    }

    /// Waits up to 20 s for a new notification and prints what was read
    /// from it: `cargo test reads_new_toast -- --ignored --nocapture`, then
    /// make something send a notification.
    #[test]
    #[ignore]
    fn reads_new_toast() {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap() };
        let listener = UserNotificationListener::Current().unwrap();
        let before: HashSet<u32> = current(&listener).unwrap().iter().filter_map(|n| n.Id().ok()).collect();
        println!("ready");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            thread::sleep(POLL);
            for n in current(&listener).unwrap() {
                if before.contains(&n.Id().unwrap()) {
                    continue;
                }
                let toast = read(&n).expect("readable toast");
                println!(
                    "app {:?} | title {:?} | body {:?} | icon {:?} | app id {:?}",
                    toast.app,
                    toast.title,
                    toast.body,
                    toast.icon.as_ref().map(|i| (i.width(), i.height())),
                    toast.app_id,
                );
                return;
            }
        }
        panic!("no new notification within 20 s");
    }
}
