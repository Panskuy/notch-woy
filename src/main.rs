// Release build runs without a console window; debug keeps it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod appbar;
mod battery;
mod bluetooth;
// Only release builds register for login, so `cargo run` never points the
// Run key at a debug exe.
#[cfg(not(debug_assertions))]
mod autostart;
mod lyrics;
mod media;
mod notifications;
mod palette;
mod render;
mod volume;
mod window;

fn main() -> windows::core::Result<()> {
    #[cfg(not(debug_assertions))]
    if let Err(e) = autostart::enable() {
        eprintln!("autostart registration failed: {e}");
    }
    window::run()
}
