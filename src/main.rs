// Release build runs without a console window; debug keeps it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod appbar;
mod autostart;
mod battery;
mod bluetooth;
mod controls;
#[cfg(test)]
mod icon;
mod lyrics;
mod media;
mod notifications;
mod palette;
mod render;
mod tray;
mod volume;
mod window;

fn main() -> windows::core::Result<()> {
    // Before any other COM use; see the function for why.
    controls::init_com_security();
    // Only release builds turn autostart on by themselves, so `cargo run`
    // never points the Run key at a debug exe.
    #[cfg(not(debug_assertions))]
    if let Err(e) = autostart::apply_default() {
        eprintln!("autostart registration failed: {e}");
    }
    window::run()
}
