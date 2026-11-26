//! Where do the taskbar's app buttons end? Asked through UI Automation (the
//! accessibility API), on a background thread so a busy Explorer can never
//! stall the UI. Results are posted back to the controller window.
//!
//! No polling: each taskbar is checked once, when it first gets a widget, and
//! then only when the user asks (menu → "Re-check taskbar icons"). The thread
//! sleeps the rest of the time.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Accessibility::*;

/// wParam = taskbar HWND, lParam = screen x of the icons' right edge.
pub const WM_APP_ICONS: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 3;
/// wParam = taskbar HWND, lParam = screen x where its tray/clock area starts
/// (secondary taskbars only; they have no tray window to measure).
pub const WM_APP_TRAY: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 7;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub taskbar: isize,
    pub rect: crate::util::Rect,
    /// Tray left edge: buttons at or beyond this belong to the tray.
    pub limit: i32,
    /// Secondary-monitor taskbar (no tray window; clock position is guessed).
    pub secondary: bool,
}

#[derive(Default)]
struct Shared {
    targets: Vec<Target>,
    kick: bool,
    /// Taskbars already checked once.
    checked: Vec<isize>,
    quit: bool,
}

pub struct Watcher {
    shared: Arc<(Mutex<Shared>, Condvar)>,
}

impl Watcher {
    pub fn start(notify: isize) -> Watcher {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let s2 = shared.clone();
        let _ = std::thread::Builder::new()
            .name("icons".into())
            .stack_size(256 * 1024)
            .spawn(move || worker(s2, notify));
        Watcher { shared }
    }

    /// Taskbars to check.
    pub fn set_targets(&self, t: Vec<Target>) {
        let (m, cv) = &*self.shared;
        if let Ok(mut s) = m.lock() {
            if s.targets != t {
                s.targets = t;
                // A taskbar that has never been checked (startup, or one that
                // just got a widget) is checked once.
                if s.targets.iter().any(|t| !s.checked.contains(&t.taskbar)) {
                    s.kick = true;
                    cv.notify_one();
                }
            }
        }
    }

    /// Check again now (user clicked "Re-check taskbar icons").
    pub fn kick(&self) {
        let (m, cv) = &*self.shared;
        if let Ok(mut s) = m.lock() {
            s.kick = true;
            cv.notify_one();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let (m, cv) = &*self.shared;
        if let Ok(mut s) = m.lock() {
            s.quit = true;
            cv.notify_one();
        }
    }
}

struct Uia {
    auto: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    cond: IUIAutomationCondition,
}

impl Uia {
    fn new() -> Option<Uia> {
        unsafe {
            let auto: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let cache = auto.CreateCacheRequest().ok()?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId).ok()?;
            cache.AddProperty(UIA_ControlTypePropertyId).ok()?;
            cache.AddProperty(UIA_ClassNamePropertyId).ok()?;
            // Everything; buttons (app icons, Start, Search, Task view…) are picked below.
            let cond = auto.CreateTrueCondition().ok()?;
            Some(Uia { auto, cache, cond })
        }
    }

    /// (right edge of the app buttons, left edge of the tray/clock items).
    /// Right edge falls back to the taskbar's left edge when it has no buttons.
    fn measure(&self, t: &Target) -> Option<(i32, Option<i32>)> {
        unsafe {
            let hwnd = HWND(t.taskbar as *mut core::ffi::c_void);
            let root = self.auto.ElementFromHandleBuildCache(hwnd, &self.cache).ok()?;
            let all = root.FindAllBuildCache(TreeScope_Descendants, &self.cond, &self.cache).ok()?;
            let n = all.Length().ok()?;
            let tb = t.rect;
            let mut buttons: Vec<RECT> = Vec::new();
            let mut tray_l: Option<i32> = None;
            for i in 0..n {
                let Ok(e) = all.GetElement(i) else { continue };
                if e.CachedControlType().ok() != Some(UIA_ButtonControlTypeId) {
                    continue;
                }
                let Ok(r) = e.CachedBoundingRectangle() else { continue };
                if !is_icon(&r, tb, tb.r + 2) {
                    continue;
                }
                // Win11 tray items (clock, show-desktop, notification icons)
                // are "SystemTray.*"; they are not app icons.
                let cls = e.CachedClassName().map(|b| b.to_string()).unwrap_or_default();
                if cls.starts_with("SystemTray.") {
                    tray_l = Some(tray_l.map_or(r.left, |v: i32| v.min(r.left)));
                    continue;
                }
                buttons.push(r);
            }
            // Only trust tray items found on the right half of the taskbar.
            let tray_l = tray_l.filter(|&x| x > tb.l + tb.w() / 2);
            // Win11's tray buttons (incl. the "Show hidden icons" arrow) can sit
            // left of the TrayNotifyWnd window; use whichever starts first.
            let limit = match tray_l {
                Some(x) if t.secondary => x,
                Some(x) => x.min(t.limit),
                None => t.limit,
            };
            let right = buttons.iter().filter(|r| r.left < limit - 2).map(|r| r.right).max().unwrap_or(tb.l);
            Some((right, tray_l))
        }
    }
}

/// A visible button on the taskbar strip, left of the tray, not a container.
fn is_icon(r: &RECT, tb: crate::util::Rect, limit: i32) -> bool {
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    w > 0
        && h > 0
        && w < tb.w() / 2
        && r.left >= tb.l
        && r.top >= tb.t - 2
        && r.bottom <= tb.b + 2
        && r.left < limit - 2
}

fn worker(shared: Arc<(Mutex<Shared>, Condvar)>, notify: isize) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let uia = Uia::new();
    loop {
        let targets = {
            let (m, cv) = &*shared;
            let Ok(mut s) = m.lock() else { return };
            // Sleep until kicked. No periodic checks.
            while !s.kick && !s.quit {
                s = match cv.wait(s) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
            if s.quit {
                return;
            }
            s.kick = false;
            drop(s);
            // Right after sign-in Explorer is still building the taskbar and
            // tray (the "Show hidden icons" arrow appears late), so the
            // startup check waits until it has settled; otherwise a moment.
            let uptime_s = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } / 1000;
            let wait = if uptime_s < 180 { 20_000 } else { 400 };
            std::thread::sleep(Duration::from_millis(wait));
            let Ok(s) = m.lock() else { return };
            s.targets.clone()
        };
        if let Ok(mut s) = shared.0.lock() {
            for t in &targets {
                if !s.checked.contains(&t.taskbar) {
                    s.checked.push(t.taskbar);
                }
            }
        }
        let Some(uia) = &uia else { continue };
        for t in &targets {
            let Some((x, tray)) = uia.measure(t) else { continue };
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
                if let Some(tl) = tray {
                    PostMessageW(notify as _, WM_APP_TRAY, t.taskbar as usize, tl as isize);
                }
                PostMessageW(
                    notify as _,
                    WM_APP_ICONS,
                    t.taskbar as usize,
                    x as isize,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn icon_filter() {
        let tb = crate::util::Rect { l: 0, t: 1032, r: 1920, b: 1080 };
        let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
        assert!(is_icon(&r(900, 1036, 940, 1076), tb, 1700));
        assert!(!is_icon(&r(1710, 1036, 1750, 1076), tb, 1700)); // tray
        assert!(!is_icon(&r(0, 1032, 1920, 1080), tb, 1700)); // container
        assert!(!is_icon(&r(900, 500, 940, 540), tb, 1700)); // off the strip
        assert!(!is_icon(&r(900, 1036, 900, 1076), tb, 1700)); // empty
    }
}
