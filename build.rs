//! Embeds the app icon, version info and manifest in the exe. The tray icon
//! and the settings window's icon are loaded from this same resource, by
//! `ICON_ID`.

/// Asks for version 6 of the common controls, so the system dialogs the app
/// shows (the settings window's reset confirmation) have the current
/// Windows look rather than the classic one.
const MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>"#;

fn main() {
    println!("cargo:rerun-if-changed=assets/notch-woy.ico");
    let mut res = winresource::WindowsResource::new();
    // Keep in sync with `tray::ICON_ID` and `settings_window::ICON_ID`.
    res.set_icon_with_id("assets/notch-woy.ico", "1");
    res.set_manifest(MANIFEST);
    res.set("ProductName", "notch-woy");
    res.set("FileDescription", "notch-woy: a Dynamic Island for Windows");
    res.compile().expect("embed icon and version info");
}
