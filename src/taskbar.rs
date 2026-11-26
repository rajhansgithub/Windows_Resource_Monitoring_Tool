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
        self.rect == o.rect
            && self.tray == o.tray
            && self.edge == o.edge
            && self.dpi == o.dpi
            && self.retracted == o.retracted
    }
}

pub fn monitor_info(h: HMONITOR) -> Option<(Rect, Rect)> {
    monitor_info_ex(h).map(|(m, w, _)| (m, w))
}

fn monitor_info_ex(h: HMONITOR) -> Option<(Rect, Rect, [u16; 32])> {
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    (unsafe { GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) } != 0).then(
        || {
            (
                mi.monitorInfo.rcMonitor.into(),
                mi.monitorInfo.rcWork.into(),
                mi.szDevice,
            )
        },
    )
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

    let tray = if primary {
        find_tray(hwnd, rect, edge, dpi)
    } else {
        find_clock(hwnd, rect, edge, dpi)
    };
    Some(Taskbar {
        hwnd,
        rect,
        edge,
        tray,
        monitor,
        work,
        dpi,
        retracted,
        primary,
        device,
    })
}

fn valid_child(t: Option<Rect>, rect: Rect, edge: Edge) -> Option<Rect> {
    let t = t?;
    let inside = t.l >= rect.l - 1 && t.r <= rect.r + 1 && t.t >= rect.t - 1 && t.b <= rect.b + 1;
    (inside && !t.is_empty() && t.w() < rect.w() && (edge.horizontal() || t.h() < rect.h()))
        .then_some(t)
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
        Rect {
            l: rect.r - guess,
            ..rect
        }
    } else {
        Rect {
            t: rect.b - guess,
            ..rect
        }
    }
}

fn find_tray(tb: HWND, rect: Rect, edge: Edge, dpi: u32) -> Rect {
    if let Some(t) = valid_child(window_rect(find_child(tb, "TrayNotifyWnd")), rect, edge) {
        return t;
    }
    // Fallback: guess a typical tray size at the far end of the taskbar.
    let guess = (260 * dpi / 96) as i32;
    if edge.horizontal() {
        Rect {
            l: rect.r - guess,
            ..rect
        }
    } else {
        Rect {
            t: rect.b - guess,
            ..rect
        }
    }
}

/// Shell surfaces (Start, Search, flyouts, Alt-Tab, desktop, lock screen…).
/// They often cover a whole monitor while invisible or animating, so they
/// must never count as a full-screen app.
const SHELL_PROCESSES: [&str; 8] = [
    "explorer.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "searchapp.exe",
    "shellhost.exe",
    "textinputhost.exe",
    "lockapp.exe",
];

const SHELL_CLASSES: [&str; 9] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "TopLevelWindowForOverflowXamlIsland",
    "MultitaskingViewFrame",
    "ForegroundStaging",
];

/// Image file name (lower-case) of the process that owns `h`.
pub fn process_name(h: HWND) -> String {
    use windows_sys::Win32::System::Threading::*;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if p.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 520];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) != 0;
        windows_sys::Win32::Foundation::CloseHandle(p);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..n as usize]);
        path.rsplit('\\').next().unwrap_or("").to_ascii_lowercase()
    }
}

/// Hidden by the window manager (other virtual desktop, suspended UWP, shell
/// surfaces waiting off-screen…).
pub fn is_cloaked(h: HWND) -> bool {
    use windows_sys::Win32::Graphics::Dwm::*;
    let mut v: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, &mut v as *mut u32 as *mut _, 4) == 0
            && v != 0
    }
}

/// What the user actually sees: the DWM frame without the invisible resize
/// borders (GetWindowRect includes ~8px of those).
pub fn visible_bounds(h: HWND) -> Option<Rect> {
    use windows_sys::Win32::Graphics::Dwm::*;
    let mut r: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    } == 0;
    if ok {
        Some(r.into())
    } else {
        window_rect(h)
    }
}

/// Is a full-screen app (game, video, presentation) showing on this
/// taskbar's monitor? Decided per monitor, the way Explorer decides to drop
/// its taskbar: the foreground window must exactly cover its own monitor and
/// be a real app window — not a shell surface, cloaked window or maximized
/// window. Returns (fullscreen, reason for the diagnostics log).
pub fn fullscreen_active(tb: &Taskbar, own: &[HWND]) -> (bool, String) {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() {
        return (false, "no foreground window".into());
    }
    let root = unsafe { GetAncestor(fg, GA_ROOT) };
    let root = if root.is_null() { fg } else { root };
    if own.contains(&root) || own.contains(&fg) {
        return (false, "our window".into());
    }
    let hmon = unsafe { MonitorFromWindow(root, MONITOR_DEFAULTTONEAREST) };
    let Some((mon, _)) = monitor_info(hmon) else {
        return (false, "no monitor".into());
    };
    if mon != tb.monitor {
        return (false, "foreground is on another monitor".into());
    }
    let cls = class_name(root);
    let style = unsafe { GetWindowLongW(root, GWL_STYLE) } as u32;
    let ex = unsafe { GetWindowLongW(root, GWL_EXSTYLE) } as u32;
    let desc = |why: &str| format!("{} [{} / {}]", why, cls, process_name(root));
    if style & WS_VISIBLE == 0 || style & WS_MINIMIZE != 0 {
        return (false, desc("not visible"));
    }
    if is_cloaked(root) {
        return (false, desc("cloaked"));
    }
    if SHELL_CLASSES.contains(&cls.as_str()) {
        return (false, desc("shell window"));
    }
    if ex & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT) != 0
        && ex & WS_EX_APPWINDOW == 0
    {
        return (false, desc("tool/overlay window"));
    }
    // Exclusive full-screen games / presentation mode, on this monitor.
    let mut st: QUERY_USER_NOTIFICATION_STATE = 0;
    let exclusive = unsafe { SHQueryUserNotificationState(&mut st) } == 0
        && (st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE);
    let Some(b) = visible_bounds(root) else {
        return (false, desc("no bounds"));
    };
    let covers = b.l <= mon.l + 1 && b.t <= mon.t + 1 && b.r >= mon.r - 1 && b.b >= mon.b - 1;
    if !covers && !exclusive {
        return (false, desc("doesn't cover the monitor"));
    }
    // With auto-hide, a maximized window also covers the monitor — not full-screen.
    if style & WS_MAXIMIZE != 0 && style & WS_CAPTION == WS_CAPTION {
        return (false, desc("maximized window"));
    }
    let proc = process_name(root);
    if SHELL_PROCESSES.contains(&proc.as_str()) {
        return (false, desc("shell process"));
    }
    (
        true,
        desc(if exclusive {
            "exclusive full-screen"
        } else {
            "covers the monitor"
        }),
    )
}

/// Placeholder when Explorer isn't running: bottom of the primary monitor.
pub fn fallback() -> Option<Taskbar> {
    let hmon = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let (mon, work, device) = monitor_info_ex(hmon)?;
    let dpi = monitor_dpi(hmon);
    let rect = Rect {
        t: mon.b - (48 * dpi / 96) as i32,
        ..mon
    };
    Some(Taskbar {
        hwnd: std::ptr::null_mut(),
        rect,
        edge: Edge::Bottom,
        tray: Rect {
            l: rect.r - (200 * dpi / 96) as i32,
            ..rect
        },
        monitor: mon,
        work,
        dpi,
        retracted: false,
        primary: true,
        device,
    })
}
