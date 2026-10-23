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

use tiny_skia::Pixmap;
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

/// Posted to the notch window when new notifications are queued.
pub const WM_TOAST: u32 = WM_APP + 5;

const POLL: Duration = Duration::from_millis(700);
/// App icons are decoded to this size (physical pixels).
const ICON_SIZE: u32 = 96;

pub struct Toast {
    pub app: String,
    /// Sender or subject; the app name when the toast has no title.
    pub title: String,
    pub body: String,
    pub icon: Option<Pixmap>,
    /// Opens the app; empty for apps Windows gives no id for.
    pub app_id: String,
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
        .and_then(|logo| media::decode_square(&logo, ICON_SIZE).ok().flatten());

    Some(Toast { app, title, body, icon, app_id })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

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
