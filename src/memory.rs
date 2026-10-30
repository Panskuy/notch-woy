//! Measures where the process's private memory goes:
//! `cargo test --release memory_breakdown -- --ignored --nocapture --test-threads 1`.

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

fn private_mb() -> f64 {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut _ as *mut _,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
        .unwrap();
    }
    counters.PrivateUsage as f64 / (1024.0 * 1024.0)
}

/// Measures how often a 16 ms `SetTimer` really fires:
/// `cargo test timer_intervals -- --ignored --nocapture`.
#[test]
#[ignore]
fn timer_intervals() {
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, KillTimer, SetTimer, MSG, WM_TIMER};
    unsafe {
        let id = SetTimer(None, 0, 16, None);
        let mut msg = MSG::default();
        let mut last = std::time::Instant::now();
        let mut gaps = Vec::new();
        while gaps.len() < 40 && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER {
                let now = std::time::Instant::now();
                gaps.push((now - last).as_secs_f64() * 1000.0);
                last = now;
            }
        }
        let _ = KillTimer(None, id);
        gaps.remove(0);
        let avg = gaps.iter().sum::<f64>() / gaps.len() as f64;
        let max = gaps.iter().cloned().fold(0.0, f64::max);
        let min = gaps.iter().cloned().fold(f64::MAX, f64::min);
        println!("16 ms timer: avg {avg:.1} ms, min {min:.1}, max {max:.1}");
    }
}

/// Measures frame pacing by `DwmFlush`, which the animation loop uses:
/// `cargo test dwm_flush_intervals -- --ignored --nocapture`.
#[test]
#[ignore]
fn dwm_flush_intervals() {
    let mut gaps = Vec::new();
    let mut last = std::time::Instant::now();
    for _ in 0..61 {
        let _ = unsafe { windows::Win32::Graphics::Dwm::DwmFlush() };
        let now = std::time::Instant::now();
        gaps.push((now - last).as_secs_f64() * 1000.0);
        last = now;
    }
    gaps.remove(0);
    let avg = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let max = gaps.iter().cloned().fold(0.0, f64::max);
    let min = gaps.iter().cloned().fold(f64::MAX, f64::min);
    println!("DwmFlush: avg {avg:.1} ms, min {min:.1}, max {max:.1}");
}

/// Watches private memory while the Bluetooth watcher polls, to tell a
/// one-off cost from a leak:
/// `cargo test --release bluetooth_over_time -- --ignored --nocapture`.
#[test]
#[ignore]
fn bluetooth_over_time() {
    crate::controls::init_com_security();
    let hwnd = windows::Win32::Foundation::HWND::default();
    let _bluetooth = crate::bluetooth::Bluetooth::spawn(hwnd, std::time::Duration::ZERO);
    for i in 0..7 {
        std::thread::sleep(std::time::Duration::from_secs(if i == 0 { 3 } else { 10 }));
        println!("bluetooth after {:>2} s: {:.2} MB", 3 + i * 10, private_mb());
    }
}

#[test]
#[ignore]
fn memory_breakdown() {
    let mut last = private_mb();
    let mut step = |name: &str| {
        let now = private_mb();
        println!("{name:<28} +{:6.1} MB  (total {now:6.1} MB)", now - last);
        last = now;
    };
    step("baseline");
    crate::controls::init_com_security();
    step("COM");
    let fonts = crate::render::Fonts::load();
    step("fonts (ab_glyph)");
    // Each worker gets a moment to finish starting up. Its messages go to
    // no window and are dropped.
    let settle = || std::thread::sleep(std::time::Duration::from_millis(1500));
    let hwnd = windows::Win32::Foundation::HWND::default();
    let media = crate::media::Media::spawn(hwnd);
    settle();
    step("media session");
    let lyrics = crate::lyrics::Fetcher::spawn(hwnd);
    settle();
    step("lyrics (HttpClient)");
    let notifications = crate::notifications::Notifications::spawn(hwnd);
    settle();
    step("notifications");
    let bluetooth = crate::bluetooth::Bluetooth::spawn(hwnd, std::time::Duration::ZERO);
    settle();
    step("bluetooth");
    let volume = crate::volume::Volume::spawn(hwnd);
    settle();
    step("volume");
    let controls = crate::controls::Controls::spawn(hwnd);
    settle();
    step("controls (radios, WMI)");
    let outputs = crate::outputs::Outputs::new(hwnd);
    outputs.refresh();
    settle();
    step("outputs");
    let _keep = (fonts, media, lyrics, notifications, bluetooth, volume, controls, outputs);
}
