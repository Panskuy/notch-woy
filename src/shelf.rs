//! The file shelf: files dragged onto the notch are kept there, by path,
//! until dragged out somewhere else (which moves or copies them, as the
//! drop target decides), opened, shared, or removed.
//!
//! Like boring.notch's shelf, it keeps references, not copies (NotchDrop
//! copies into its own folder): nothing is written to disk but the list,
//! and big files go on the shelf instantly. The list survives restarts, in
//! `%LOCALAPPDATA%\notch-woy\shelf.txt`; files that are gone drop off it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use tiny_skia::{IntSize, Pixmap};
use windows::core::{implement, w, Interface, Ref, Result, HSTRING, PCWSTR};
use windows::ApplicationModel::DataTransfer::{DataRequestedEventArgs, DataTransferManager};
use windows::Foundation::TypedEventHandler;
use windows::Storage::{IStorageItem, StorageFile};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, POINTL, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::{
    CoInitializeEx, IDataObject, COINIT_APARTMENTTHREADED, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::System::WinRT::RoGetActivationFactory;
use windows::Win32::UI::Shell::{
    DragQueryFileW, IDataTransferManagerInterop, IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName,
    SHDoDragDrop, ShellExecuteW, BHID_DataObject, HDROP, SIIGBF_RESIZETOFIT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, PostMessageW, RegisterClassExW, SetForegroundWindow,
    SetLayeredWindowAttributes, SetTimer, SetWindowPos, LWA_ALPHA, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_SHOWNORMAL, WA_INACTIVE, WM_ACTIVATE, WM_APP,
    WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_collections::IIterable;

/// Posted to the notch window when a file's icon is ready.
pub const WM_SHELF: u32 = WM_APP + 12;

/// The shelf is for a handful of files at hand, not an archive.
const MAX_ITEMS: usize = 60;
/// Icons are drawn this big, in logical px.
pub const ICON: f32 = 44.0;

pub struct Item {
    pub path: PathBuf,
    pub name: String,
    /// The file's thumbnail or icon, once the worker has it.
    pub icon: Option<Arc<Pixmap>>,
}

pub struct Shelf {
    /// Newest first.
    items: Vec<Item>,
    icons: Sender<(PathBuf, u32)>,
    ready: Arc<Mutex<Vec<(PathBuf, Pixmap)>>>,
    /// Icon size in physical px.
    icon_px: u32,
    /// The share sheet's data handler, removed before the next share.
    share_token: Option<(DataTransferManager, i64)>,
}

impl Shelf {
    /// Loads the saved list and starts the icon worker, which posts
    /// `WM_SHELF` to `hwnd` as icons come in.
    pub fn new(hwnd: HWND, scale: f32) -> Self {
        let (icons, requests) = mpsc::channel::<(PathBuf, u32)>();
        let ready = Arc::new(Mutex::new(Vec::new()));
        let shared = ready.clone();
        let raw = hwnd.0 as isize;
        thread::spawn(move || {
            // The shell's thumbnail providers want an apartment of their own.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            for (path, px) in requests {
                if let Some(icon) = load_icon(&path, px) {
                    shared.lock().unwrap().push((path, icon));
                    unsafe {
                        let _ = PostMessageW(Some(HWND(raw as _)), WM_SHELF, WPARAM(0), LPARAM(0));
                    }
                }
            }
        });
        let mut shelf =
            Self { items: Vec::new(), icons, ready, icon_px: icon_px(scale), share_token: None };
        let saved = std::fs::read_to_string(list_path()).unwrap_or_default();
        let paths: Vec<PathBuf> = saved.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        // Saved newest first; added oldest first so the order is kept.
        shelf.add_paths(paths.into_iter().rev(), false);
        shelf
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Adds files (newest first), skipping ones already there or missing.
    /// Returns whether anything was added.
    /// Puts `paths` first (moving any already on it); returns how many.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        self.add_paths(paths, true)
    }

    fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>, save: bool) -> usize {
        let mut added = 0;
        for path in paths {
            if !path.exists() {
                continue;
            }
            self.items.retain(|item| item.path != path);
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
            let _ = self.icons.send((path.clone(), self.icon_px));
            self.items.insert(0, Item { path, name, icon: None });
            added += 1;
        }
        self.items.truncate(MAX_ITEMS);
        added = added.min(self.items.len());
        if added > 0 && save {
            self.save();
        }
        added
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.save();
    }

    /// Drops files that no longer exist (moved out, deleted elsewhere).
    /// Returns whether any went.
    pub fn prune(&mut self) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.path.exists());
        let changed = self.items.len() != before;
        if changed {
            self.save();
        }
        changed
    }

    /// Takes the icons the worker has finished.
    pub fn take_icons(&mut self) {
        for (path, icon) in self.ready.lock().unwrap().drain(..) {
            if let Some(item) = self.items.iter_mut().find(|item| item.path == path) {
                item.icon = Some(Arc::new(icon));
            }
        }
    }

    /// Redraws icons for a new display scale.
    pub fn set_scale(&mut self, scale: f32) {
        let px = icon_px(scale);
        if px != self.icon_px {
            self.icon_px = px;
            for item in &self.items {
                let _ = self.icons.send((item.path.clone(), px));
            }
        }
    }

    pub fn open(&self, index: usize) {
        if let Some(item) = self.items.get(index) {
            let path = HSTRING::from(item.path.as_os_str());
            unsafe {
                ShellExecuteW(None, w!("open"), &path, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
    }

    /// Opens Windows' share sheet for every file on the shelf, at `at`
    /// (screen px).
    ///
    /// The sheet will not open for the notch's own window: it never
    /// activates, and the sheet needs a window that can. So it gets an
    /// invisible stand-in there (see `share_anchor`), which must stay up
    /// while the sheet is open (hiding it closes the sheet).
    pub fn share(&mut self, at: POINT) -> Result<()> {
        let paths: Vec<HSTRING> = self.items.iter().map(|item| HSTRING::from(item.path.as_os_str())).collect();
        if paths.is_empty() {
            return Ok(());
        }
        let hwnd = share_anchor(at)?;
        let shown = self.show_share(hwnd, paths);
        if shown.is_err() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        shown
    }

    fn show_share(&mut self, hwnd: HWND, paths: Vec<HSTRING>) -> Result<()> {
        let interop: IDataTransferManagerInterop =
            unsafe { RoGetActivationFactory(&HSTRING::from("Windows.ApplicationModel.DataTransfer.DataTransferManager"))? };
        let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd)? };
        // One handler at a time; it carries the files of this share.
        if let Some((old, token)) = self.share_token.take() {
            let _ = old.RemoveDataRequested(token);
        }
        let count = paths.len();
        let handler = TypedEventHandler::new(move |_, args: Ref<DataRequestedEventArgs>| {
            let Some(args) = args.as_ref() else { return Ok(()) };
            let request = args.Request()?;
            let data = request.Data()?;
            let title = if count == 1 { "1 file".to_string() } else { format!("{count} files") };
            data.Properties()?.SetTitle(&HSTRING::from(title))?;
            let mut files: Vec<Option<IStorageItem>> = Vec::new();
            for path in &paths {
                if let Ok(file) = StorageFile::GetFileFromPathAsync(path).and_then(|op| op.get()) {
                    files.push(Some(file.cast()?));
                }
            }
            data.SetStorageItems(&IIterable::<IStorageItem>::from(files), true)?;
            Ok(())
        });
        let token = manager.DataRequested(&handler)?;
        self.share_token = Some((manager, token));
        unsafe { interop.ShowShareUIForWindow(hwnd) }
    }

    fn save(&self) {
        let list: String = self.items.iter().map(|item| format!("{}\n", item.path.display())).collect();
        let path = list_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, list) {
            eprintln!("saving the shelf failed: {e}");
        }
    }
}

/// How long an unused share anchor may linger if the sheet never hands
/// focus back to it.
const ANCHOR_LIFETIME_MS: u32 = 10 * 60_000;

thread_local! {
    /// When the current share anchor appeared (there is one at a time).
    static ANCHOR_SINCE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

/// A 1x1, fully transparent, activatable window at `at` for the share sheet
/// to hang from. It goes away when focus comes back to it once the sheet
/// has closed, or after `ANCHOR_LIFETIME_MS`. A tool window, so it has no
/// taskbar button or Alt+Tab entry.
fn share_anchor(at: POINT) -> Result<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("NotchWoyShareAnchor");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(anchor_proc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        // Fails harmlessly when already registered.
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("notch-woy share"),
            WS_POPUP,
            at.x,
            at.y,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA)?;
        // Not ShowWindow: its first call in a process takes the launcher's
        // show state instead.
        SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW)?;
        let _ = SetForegroundWindow(hwnd);
        ANCHOR_SINCE.set(Some(std::time::Instant::now()));
        SetTimer(Some(hwnd), 1, ANCHOR_LIFETIME_MS, None);
        Ok(hwnd)
    }
}

extern "system" fn anchor_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Focus back after the sheet (a moment after it opened): done.
            WM_ACTIVATE if wparam.0 & 0xFFFF != WA_INACTIVE as usize => {
                let since = ANCHOR_SINCE.get().map_or(0, |t| t.elapsed().as_millis());
                if since > 1000 {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_TIMER => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn icon_px(scale: f32) -> u32 {
    (ICON * scale).round() as u32
}

fn list_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("notch-woy").join("shelf.txt")
}

/// The file's thumbnail (pictures, videos, documents with previews) or
/// else its icon, fitted in `px` square, as a premultiplied pixmap.
fn load_icon(path: &Path, px: u32) -> Option<Pixmap> {
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let factory: IShellItemImageFactory = item.cast().ok()?;
        let size = SIZE { cx: px as i32, cy: px as i32 };
        let bitmap = factory.GetImage(size, SIIGBF_RESIZETOFIT).ok()?;
        let pixmap = bitmap_to_pixmap(bitmap);
        let _ = DeleteObject(bitmap.into());
        pixmap
    }
}

/// Reads a 32-bit shell bitmap. Its alpha is premultiplied; thumbnails of
/// opaque pictures may leave alpha at 0 throughout, which means opaque.
unsafe fn bitmap_to_pixmap(bitmap: HBITMAP) -> Option<Pixmap> {
    let mut info = BITMAP::default();
    if unsafe { GetObjectW(bitmap.into(), size_of::<BITMAP>() as i32, Some((&mut info as *mut BITMAP).cast())) } == 0 {
        return None;
    }
    let (w, h) = (info.bmWidth, info.bmHeight.abs());
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut header = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Top-down, like tiny-skia.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    let dc = unsafe { GetDC(None) };
    let rows = unsafe {
        GetDIBits(dc, bitmap, 0, h as u32, Some(bgra.as_mut_ptr().cast()), &mut header, DIB_RGB_COLORS)
    };
    unsafe { ReleaseDC(None, dc) };
    if rows == 0 {
        return None;
    }
    let opaque = bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
    let mut rgba = Vec::with_capacity(bgra.len());
    for &[b, g, r, a] in bgra.as_chunks::<4>().0 {
        let a = if opaque { 255 } else { a };
        // Premultiplied colors never exceed alpha; keep tiny-skia's
        // invariant even for odd bitmaps.
        rgba.extend([r.min(a), g.min(a), b.min(a), a]);
    }
    Pixmap::from_vec(rgba, IntSize::from_wh(w as u32, h as u32)?)
}

/// Starts a drag of the file at `path` out of the shelf, with the shell's
/// own drag image and drop source; returns once it is dropped or
/// cancelled. Blocks in a modal loop: call it with the app state released.
pub fn drag_out(hwnd: HWND, path: &Path) -> Result<DROPEFFECT> {
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        let data: IDataObject = item.BindToHandler(None, &BHID_DataObject)?;
        // Copy or move, as the place it lands decides (Explorer moves within
        // a drive, copies across); links where they are offered.
        SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK)
    }
}

/// What the notch window hears from a drag over it.
pub trait DropSink {
    /// Files are being dragged over the notch (`true`), or left (`false`).
    fn drag_hover(&self, files: bool);
    /// Files were dropped on the notch (which ends the drag hover).
    fn dropped(&self, paths: Vec<PathBuf>);
    /// The drag is the shelf's own (dragging a file out); not a drop here.
    fn dragging_out(&self) -> bool;
}

/// Lets the notch window take files dropped on it.
pub fn register_drop_target(hwnd: HWND, sink: Box<dyn DropSink>) -> Result<()> {
    unsafe {
        // Already in an STA (see `controls::init_com_security`); this adds
        // the OLE side drag and drop needs.
        OleInitialize(None)?;
        let target: IDropTarget = DropTarget { sink, files: std::cell::Cell::new(false) }.into();
        RegisterDragDrop(hwnd, &target)
    }
}

pub fn revoke_drop_target(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

#[implement(IDropTarget)]
struct DropTarget {
    sink: Box<dyn DropSink>,
    /// The current drag carries files we can take.
    files: std::cell::Cell<bool>,
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// The effect to show for a drop the source allows as `allowed`: a copy
/// (the file stays where it is; the shelf only points at it), else a link.
/// Never a move, which would have the source delete the file.
fn drop_effect(allowed: DROPEFFECT) -> DROPEFFECT {
    if allowed.contains(DROPEFFECT_COPY) {
        DROPEFFECT_COPY
    } else if allowed.contains(DROPEFFECT_LINK) {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let files = !self.sink.dragging_out()
            && data.as_ref().is_some_and(|d| unsafe { d.QueryGetData(&hdrop_format()) }.is_ok());
        self.files.set(files);
        unsafe { *effect = if files { drop_effect(*effect) } else { DROPEFFECT_NONE } };
        if files {
            self.sink.drag_hover(true);
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = if self.files.get() { drop_effect(*effect) } else { DROPEFFECT_NONE } };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if self.files.replace(false) {
            self.sink.drag_hover(false);
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        // What the source allows, read before the answer overwrites it.
        let allowed = unsafe { *effect };
        let files = self.files.replace(false);
        let paths = if files { data.as_ref().map(dropped_paths).unwrap_or_default() } else { Vec::new() };
        unsafe { *effect = if paths.is_empty() { DROPEFFECT_NONE } else { drop_effect(allowed) } };
        if paths.is_empty() {
            if files {
                self.sink.drag_hover(false);
            }
        } else {
            // Ends the drag hover too, without closing the notch first.
            self.sink.dropped(paths);
        }
        Ok(())
    }
}

/// The file paths in a drop's CF_HDROP data.
fn dropped_paths(data: &IDataObject) -> Vec<PathBuf> {
    unsafe {
        let Ok(mut medium) = data.GetData(&hdrop_format()) else { return Vec::new() };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut paths = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buffer = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buffer)) as usize;
            paths.push(PathBuf::from(String::from_utf16_lossy(&buffer[..got])));
        }
        ReleaseStgMedium(&mut medium);
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_asks_the_source_to_move() {
        assert_eq!(drop_effect(DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK), DROPEFFECT_COPY);
        assert_eq!(drop_effect(DROPEFFECT_MOVE | DROPEFFECT_LINK), DROPEFFECT_LINK);
        assert_eq!(drop_effect(DROPEFFECT_MOVE), DROPEFFECT_NONE);
    }

    /// Opens the share sheet for a window styled like the notch (layered,
    /// topmost, tool window, never activated), with this test's exe on the
    /// shelf, and pumps messages for a few seconds:
    /// `cargo test shares_from_notch_window -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn shares_from_notch_window() {
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            // SHARE_TEST_STYLE=normal tries an ordinary window instead.
            let test_style = std::env::var("SHARE_TEST_STYLE").unwrap_or_default();
            let (ex, style) = match test_style.as_str() {
                "normal" => (WINDOW_EX_STYLE::default(), WS_OVERLAPPEDWINDOW | WS_VISIBLE),
                // An invisible, activatable stand-in at the top of the screen.
                "helper" => (WS_EX_LAYERED | WS_EX_TOPMOST, WS_POPUP | WS_VISIBLE),
                "helper-tool" => (WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, WS_POPUP | WS_VISIBLE),
                _ => (WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, WS_POPUP | WS_VISIBLE),
            };
            let hwnd = CreateWindowExW(
                ex,
                w!("STATIC"),
                w!("notch-woy share test"),
                style,
                200,
                200,
                300,
                120,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            if test_style.starts_with("helper") {
                let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 0, LWA_ALPHA);
            }
            let _ = SetForegroundWindow(hwnd);
            let mut shelf = Shelf::new(hwnd, 1.0);
            let exe = std::env::current_exe().unwrap();
            shelf.add([exe]);
            println!("share: {:?}", shelf.show_share(hwnd, vec![HSTRING::from(std::env::current_exe().unwrap().as_os_str())]));
            let start = std::time::Instant::now();
            let mut msg = MSG::default();
            let mut hidden = false;
            while start.elapsed().as_secs() < 6 {
                // SHARE_TEST_HIDE: hide the anchor once the sheet is up.
                if !hidden && start.elapsed().as_millis() > 1500 && std::env::var("SHARE_TEST_HIDE").is_ok() {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    hidden = true;
                    println!("anchor hidden");
                }
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            shelf.clear();
        }
    }

    /// Reads a real icon: `cargo test reads_shell_icon -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reads_shell_icon() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let exe = std::env::current_exe().unwrap();
        let icon = load_icon(&exe, 64).expect("an icon");
        println!("icon {}x{}", icon.width(), icon.height());
        assert!(icon.width() <= 64 && icon.height() <= 64);
    }
}
