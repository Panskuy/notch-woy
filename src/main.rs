// Release build runs without a console window; debug keeps it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod autostart;
mod battery;
mod bluetooth;
mod controls;
mod font;
#[cfg(test)]
mod icon;
mod lyrics;
mod media;
mod meter;
#[cfg(test)]
mod memory;
mod notifications;
mod outputs;
mod palette;
mod plugins;
mod render;
mod settings;
mod shelf;
mod settings_ui;
mod settings_window;
mod tray;
mod volume;
mod volume_keys;
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
