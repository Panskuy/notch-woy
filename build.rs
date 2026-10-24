//! Embeds the app icon and version info in the exe. The tray icon is
//! loaded from this same resource, by `ICON_ID`.

fn main() {
    println!("cargo:rerun-if-changed=assets/notch-woy.ico");
    let mut res = winresource::WindowsResource::new();
    // Keep in sync with `tray::ICON_ID`.
    res.set_icon_with_id("assets/notch-woy.ico", "1");
    res.set("ProductName", "notch-woy");
    res.set("FileDescription", "notch-woy: a Dynamic Island for Windows");
    res.compile().expect("embed icon and version info");
}
