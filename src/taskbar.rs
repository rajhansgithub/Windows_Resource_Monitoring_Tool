//! Locate the taskbar / system tray, and detect full-screen apps.

use crate::util::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

impl Edge {
    pub fn horizontal(self) -> bool {
        matches!(self, Edge::Bottom | Edge::Top)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Taskbar {
    pub hwnd: HWND,
    pub rect: Rect,
    pub edge: Edge,
    /// The notification area (tray + clock). The widget sits just before it.
    pub tray: Rect,
    pub monitor: Rect,
    pub work: Rect,
    pub dpi: u32,
    /// Auto-hide taskbar currently slid off-screen.
    pub retracted: bool,
}

pub fn monitor_info(h: HMONITOR) -> Option<(Rect, Rect)> {
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (unsafe { GetMonitorInfoW(h, &mut mi) } != 0).then(|| (mi.rcMonitor.into(), mi.rcWork.into()))
}

pub fn monitor_dpi(h: HMONITOR) -> u32 {
    let (mut x, mut y) = (96u32, 96u32);
    if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y) } != 0 {
        return 96;
    }
    x.max(96)
}

pub fn locate() -> Option<Taskbar> {
    let hwnd = find_window("Shell_TrayWnd");
    let rect = window_rect(hwnd)?;
    if rect.is_empty() {
        return None;
    }
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
    let (monitor, work) = monitor_info(hmon)?;
    let dpi = monitor_dpi(hmon);

    let edge = if rect.w() >= rect.h() {
        if rect.t <= monitor.t + 2 && rect.b < monitor.b - 2 {
            Edge::Top
        } else {
            Edge::Bottom
        }
    } else if rect.l <= monitor.l + 2 && rect.r < monitor.r - 2 {
        Edge::Left
    } else {
        Edge::Right
    };

    // Visible thickness on the monitor (auto-hide leaves ~2px showing).
    let vis = Rect {
        l: rect.l.max(monitor.l),
        t: rect.t.max(monitor.t),
        r: rect.r.min(monitor.r),
        b: rect.b.min(monitor.b),
    };
    let thickness = if edge.horizontal() { vis.h() } else { vis.w() };
    let retracted = thickness < 8;

    let tray = find_tray(hwnd, rect, edge, dpi);
    Some(Taskbar { hwnd, rect, edge, tray, monitor, work, dpi, retracted })
}

fn find_tray(tb: HWND, rect: Rect, edge: Edge, dpi: u32) -> Rect {
    let tray = window_rect(find_child(tb, "TrayNotifyWnd"));
    if let Some(t) = tray {
        let inside = t.l >= rect.l - 1 && t.r <= rect.r + 1 && t.t >= rect.t - 1 && t.b <= rect.b + 1;
        if inside && !t.is_empty() && t.w() < rect.w() && (edge.horizontal() || t.h() < rect.h()) {
            return t;
        }
    }
    // Fallback: guess a typical tray size at the far end of the taskbar.
    let guess = (260 * dpi / 96) as i32;
    if edge.horizontal() {
        Rect { l: rect.r - guess, ..rect }
    } else {
        Rect { t: rect.b - guess, ..rect }
    }
}

/// True when a full-screen app/game/presentation owns the taskbar's monitor.
pub fn fullscreen_active(tb: &Taskbar, own: &[HWND]) -> bool {
    let mut st: QUERY_USER_NOTIFICATION_STATE = 0;
    if unsafe { SHQueryUserNotificationState(&mut st) } == 0
        && (st == QUNS_BUSY || st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE)
    {
        return true;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() || own.contains(&fg) || fg == tb.hwnd {
        return false;
    }
    let cls = class_name(fg);
    if matches!(
        cls.as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Windows.UI.Core.CoreWindow"
            | "XamlExplorerHostIslandWindow" | "TopLevelWindowForOverflowXamlIsland"
    ) {
        return false;
    }
    let Some(r) = window_rect(fg) else { return false };
    let style = unsafe { GetWindowLongW(fg, GWL_STYLE) } as u32;
    if style & WS_VISIBLE == 0 || style & WS_MINIMIZE != 0 {
        return false;
    }
    let hmon = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONULL) };
    let Some((mon, _)) = monitor_info(hmon) else { return false };
    mon == tb.monitor && r.l <= mon.l && r.t <= mon.t && r.r >= mon.r && r.b >= mon.b
}
