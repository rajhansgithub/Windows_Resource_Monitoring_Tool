//! Resource Monitor — ultra-light taskbar resource monitor for Windows 10/11.
#![windows_subsystem = "windows"]

mod achievements;
mod app;
mod autostart;
mod fmt;
mod icons;
mod insights;
mod monitor;
mod nav;
mod overlay;
mod popup;
mod render;
mod report;
mod sensors;
mod settings;
mod settings_ui;
mod taskbar;
mod tray;
mod update;
mod update_ui;
mod usage;
mod util;

use util::wide;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn main() {
    // Crash log: a panic aborts the process, but first record where and why.
    std::panic::set_hook(Box::new(|info| {
        util::diag(&format!("CRASH v{}: {}", env!("CARGO_PKG_VERSION"), info));
    }));
    unsafe {
        // Manifest already declares PerMonitorV2; this covers odd launch paths.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let report = std::env::args().any(|a| a.eq_ignore_ascii_case("--report"));
        let after_update = std::env::args().any(|a| a.eq_ignore_ascii_case("--after-update"));
        let achievements = std::env::args().any(|a| a.eq_ignore_ascii_case("--achievements"));
        let insights = std::env::args().any(|a| a.eq_ignore_ascii_case("--insights"));
        let monitor = std::env::args().any(|a| a.eq_ignore_ascii_case("--monitor"));
        let name = wide("Local\\ResourceMonitor.SingleInstance");
        let mut mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        // Just updated: the previous version is still shutting down; wait for it.
        let mut tries = 0;
        while after_update && GetLastError() == ERROR_ALREADY_EXISTS && tries < 50 {
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

        // Low priority: never steal CPU from what the user is actually doing.
        SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        // EcoQoS: run on efficient cores / low clocks. We never need speed.
        let mut state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &mut state as *mut _ as *mut _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );

        let hinst = GetModuleHandleW(std::ptr::null());
        update::cleanup();
        if app::create(hinst, after_update).is_none() {
            return;
        }
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

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        CloseHandle(mutex);
    }
}
