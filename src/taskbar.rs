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
    /// Main taskbar (has the tray) vs. a secondary-monitor taskbar.
    pub primary: bool,
    /// Monitor device name, e.g. `\\.\DISPLAY2` (UTF-16, NUL-terminated).
    pub device: [u16; 32],
}

impl Taskbar {
    /// Short stable id for settings, e.g. "DISPLAY2".
    pub fn key(&self) -> String {
        let s = from_wide(&self.device);
        s.trim_start_matches(['\\', '.']).to_string()
    }
    /// "Display 2" — the number Windows Settings shows.
    pub fn label(&self) -> String {
        let k = self.key();
        match k.strip_prefix("DISPLAY") {
            Some(n) => format!("Display {}", n),
            None => k,
        }
    }
    pub fn same_geometry(&self, o: &Taskbar) -> bool {
        self.rect == o.rect && self.tray == o.tray && self.edge == o.edge && self.dpi == o.dpi
            && self.retracted == o.retracted
    }
}

pub fn monitor_info(h: HMONITOR) -> Option<(Rect, Rect)> {
    monitor_info_ex(h).map(|(m, w, _)| (m, w))
}

fn monitor_info_ex(h: HMONITOR) -> Option<(Rect, Rect, [u16; 32])> {
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    (unsafe { GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) } != 0)
        .then(|| (mi.monitorInfo.rcMonitor.into(), mi.monitorInfo.rcWork.into(), mi.szDevice))
}

pub fn monitor_dpi(h: HMONITOR) -> u32 {
    let (mut x, mut y) = (96u32, 96u32);
    if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y) } != 0 {
        return 96;
    }
    x.max(96)
}

/// Every visible taskbar: the main one first, then one per extra monitor
/// ("Show my taskbar on all displays").
pub fn locate_all() -> Vec<Taskbar> {
    let mut v = Vec::new();
    if let Some(t) = describe(find_window("Shell_TrayWnd"), true) {
        v.push(t);
    }
    let cls = wide("Shell_SecondaryTrayWnd");
    let mut h: HWND = std::ptr::null_mut();
    for _ in 0..16 {
        h = unsafe { FindWindowExW(std::ptr::null_mut(), h, cls.as_ptr(), std::ptr::null()) };
        if h.is_null() {
            break;
        }
        let visible = unsafe { IsWindowVisible(h) } != 0;
        if let (true, Some(t)) = (visible, describe(h, false)) {
            if !v.iter().any(|o: &Taskbar| o.device == t.device) {
                v.push(t);
            }
        }
    }
    v
}

fn describe(hwnd: HWND, primary: bool) -> Option<Taskbar> {
    let rect = window_rect(hwnd)?;
    if rect.is_empty() {
        return None;
    }
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (monitor, work, device) = monitor_info_ex(hmon)?;
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

    let tray = if primary { find_tray(hwnd, rect, edge, dpi) } else { find_clock(hwnd, rect, edge, dpi) };
    Some(Taskbar { hwnd, rect, edge, tray, monitor, work, dpi, retracted, primary, device })
}

fn valid_child(t: Option<Rect>, rect: Rect, edge: Edge) -> Option<Rect> {
    let t = t?;
    let inside = t.l >= rect.l - 1 && t.r <= rect.r + 1 && t.t >= rect.t - 1 && t.b <= rect.b + 1;
    (inside && !t.is_empty() && t.w() < rect.w() && (edge.horizontal() || t.h() < rect.h())).then_some(t)
}

/// Secondary taskbars have no tray; anchor on their clock if it is a real
/// window (Win10), else reserve room for the Win11 clock.
fn find_clock(tb: HWND, rect: Rect, edge: Edge, dpi: u32) -> Rect {
    for cls in ["ClockButton", "TrayClockWClass"] {
        if let Some(r) = valid_child(window_rect(find_child(tb, cls)), rect, edge) {
            return r;
        }
    }
    let guess = (if edge.horizontal() { 110 } else { 70 } * dpi / 96) as i32;
    if edge.horizontal() {
        Rect { l: rect.r - guess, ..rect }
    } else {
        Rect { t: rect.b - guess, ..rect }
    }
}

fn find_tray(tb: HWND, rect: Rect, edge: Edge, dpi: u32) -> Rect {
    if let Some(t) = valid_child(window_rect(find_child(tb, "TrayNotifyWnd")), rect, edge) {
        return t;
    }
    // Fallback: guess a typical tray size at the far end of the taskbar.
    let guess = (260 * dpi / 96) as i32;
    if edge.horizontal() {
        Rect { l: rect.r - guess, ..rect }
    } else {
        Rect { t: rect.b - guess, ..rect }
    }
}

/// True when a full-screen app/game/presentation owns this taskbar's monitor.
/// Other monitors keep their widgets.
pub fn fullscreen_active(tb: &Taskbar, own: &[HWND]) -> bool {
    let fg = unsafe { GetForegroundWindow() };
    let fg_mon = if fg.is_null() {
        None
    } else {
        monitor_info(unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONULL) }).map(|m| m.0)
    };
    let on_this = fg_mon.map_or(tb.primary, |m| m == tb.monitor);

    let mut st: QUERY_USER_NOTIFICATION_STATE = 0;
    if on_this
        && unsafe { SHQueryUserNotificationState(&mut st) } == 0
        && (st == QUNS_BUSY || st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE)
    {
        return true;
    }
    if fg.is_null() || own.contains(&fg) || !on_this {
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
    // A maximized normal window overhangs the monitor by its frame; that isn't full-screen.
    if style & WS_MAXIMIZE != 0 && style & (WS_CAPTION | WS_THICKFRAME) != 0 {
        return false;
    }
    let mon = tb.monitor;
    r.l <= mon.l && r.t <= mon.t && r.r >= mon.r && r.b >= mon.b
}

/// Placeholder when Explorer isn't running: bottom of the primary monitor.
pub fn fallback() -> Option<Taskbar> {
    let hmon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let (mon, work, device) = monitor_info_ex(hmon)?;
    let dpi = monitor_dpi(hmon);
    let rect = Rect { t: mon.b - (48 * dpi / 96) as i32, ..mon };
    Some(Taskbar {
        hwnd: std::ptr::null_mut(),
        rect,
        edge: Edge::Bottom,
        tray: Rect { l: rect.r - (200 * dpi / 96) as i32, ..rect },
        monitor: mon,
        work,
        dpi,
        retracted: false,
        primary: true,
        device,
    })
}
