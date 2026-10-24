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
    let _keep = (fonts,);
}
