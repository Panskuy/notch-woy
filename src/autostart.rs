use windows::core::w;
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

/// Registers the running executable to start at login via the per-user Run key.
/// Rewritten on every launch so the entry follows the exe if it moves.
pub fn enable() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let value: Vec<u16> = format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            w!("notch-woy"),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            (value.len() * size_of::<u16>()) as u32,
        )
        .ok()?;
    }
    Ok(())
}
