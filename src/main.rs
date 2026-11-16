//! Resource Monitor — ultra-light taskbar resource monitor for Windows 10/11.
#![windows_subsystem = "windows"]

mod app;
mod autostart;
mod fmt;
mod overlay;
mod popup;
mod render;
mod report;
mod sensors;
mod settings;
mod taskbar;
mod tray;
mod usage;
mod util;

use util::wide;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn main() {
    unsafe {
        // Manifest already declares PerMonitorV2; this covers odd launch paths.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let report = std::env::args().any(|a| a.eq_ignore_ascii_case("--report"));
        let name = wide("Local\\ResourceMonitor.SingleInstance");
        let mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let h = util::find_window(app::MAIN_CLASS);
            if !h.is_null() {
                // Let the running instance bring its windows to the front.
                AllowSetForegroundWindow(ASFW_ANY);
                PostMessageW(h, app::WM_APP_ACTIVATE, report as usize, 0);
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
        if app::create(hinst).is_none() {
            return;
        }
        if report {
            if let Some(a) = app::app() {
                report::open(a);
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
