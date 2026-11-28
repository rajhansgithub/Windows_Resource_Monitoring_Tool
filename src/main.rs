//! Resource Monitor — ultra-light taskbar resource monitor for Windows 10/11.
#![windows_subsystem = "windows"]

mod achievements;
mod app;
mod autostart;
mod driver;
mod fmt;
mod grid;
mod icons;
mod insights;
mod logo;
mod look;
mod monitor;
mod nav;
mod overlay;
mod pagetrans;
mod perf;
mod popup;
mod render;
mod report;
mod sensors;
mod settings;
mod settings_ui;
mod shell_icons;
mod taskbar;
mod tray;
mod update;
mod update_ui;
mod usage;
mod util;
mod vsync;

use util::wide;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn main() {
    perf::mark_start();
    // Crash log: a panic aborts the process, but first record where and why.
    std::panic::set_hook(Box::new(|info| {
        util::diag(&format!("CRASH v{}: {}", env!("CARGO_PKG_VERSION"), info));
    }));
    unsafe {
        // Manifest already declares PerMonitorV2; this covers odd launch paths.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // Elevated helper of the Full speed page: install missing drivers, then exit.
        // Runs before the single-instance check: the main copy is still running.
        let args: Vec<String> = std::env::args().collect();
        if let Some(i) = args.iter().position(|a| a.eq_ignore_ascii_case("--install-drivers")) {
            let ids = args.get(i + 1).map(String::as_str).unwrap_or("");
            std::process::exit(driver::run_install(&driver::decode_ids(ids)));
        }

        let report = std::env::args().any(|a| a.eq_ignore_ascii_case("--report"));
        let after_update = std::env::args().any(|a| a.eq_ignore_ascii_case("--after-update"));
        let achievements = std::env::args().any(|a| a.eq_ignore_ascii_case("--achievements"));
        let insights = std::env::args().any(|a| a.eq_ignore_ascii_case("--insights"));
        let monitor = std::env::args().any(|a| a.eq_ignore_ascii_case("--monitor"));
        let full_speed = std::env::args().any(|a| a.eq_ignore_ascii_case("--full-speed"));
        // Restarted as administrator from a device page: the old instance is closing.
        let elevated = std::env::args().any(|a| a.eq_ignore_ascii_case("--elevated"));
        let name = wide("Local\\ResourceMonitor.SingleInstance");
        let mut mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        // Just updated: the previous version is still shutting down; wait for it.
        let mut tries = 0;
        while (after_update || elevated) && GetLastError() == ERROR_ALREADY_EXISTS && tries < 50 {
            CloseHandle(mutex);
            std::thread::sleep(std::time::Duration::from_millis(200));
            mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            tries += 1;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let h = util::find_window(app::MAIN_CLASS);
            if !h.is_null() {
                // Let the running instance bring its windows to the front.
                AllowSetForegroundWindow(ASFW_ANY);
                let w = if monitor { 4 } else if insights { 3 } else if achievements { 2 } else { report as usize };
                PostMessageW(h, app::WM_APP_ACTIVATE, w, 0);
            }
            return;
        }

        // Low priority + EcoQoS while in the background (see util::interactive).
        util::interactive(false);

        let hinst = GetModuleHandleW(std::ptr::null());
        update::cleanup();
        if app::create(hinst, after_update).is_none() {
            return;
        }
        // First start of a new version: make Windows drop the old cached icons.
        shell_icons::refresh_once();
        if report {
            if let Some(a) = app::app() {
                report::open(a);
            }
        }
        if achievements {
            if let Some(a) = app::app() {
                a.open_achievements();
            }
        }
        if insights {
            if let Some(a) = app::app() {
                a.open_insights();
            }
        }
        if monitor {
            if let Some(a) = app::app() {
                settings_ui::open(a, nav::Page::Monitor);
            }
        }
        if full_speed {
            if let Some(a) = app::app() {
                settings_ui::open(a, nav::Page::FullSpeed);
            }
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        CloseHandle(mutex);
    }
}
