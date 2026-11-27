//! Small "Software update" window: status line + progress bar, so the user
//! can always see what the updater is doing.

use crate::app::{accent, App};
use crate::popup::{palette, style_window, Fonts};
use crate::render::Painter;
use crate::settings::Metric;
use crate::util::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Update";

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let cls = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: crate::logo::icon(true, true),
            hIconSm: crate::logo::icon(true, false),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
    }
}

/// Show (creating if needed) with this status. `pct`: progress 0..100 or None
/// for no bar. `activate`: bring to front (user clicked) vs. stay quiet.
pub fn show(a: &mut App, text: &str, pct: Option<f32>, activate: bool) {
    a.upd_text = text.to_string();
    a.upd_pct = pct;
    unsafe {
        if a.upd.is_null() {
            let tb = a.primary_taskbar();
            let dpi = tb.map_or(96, |t| t.dpi);
            let sc = |v: i32| v * dpi as i32 / 96;
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
            let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
            let mut rc = RECT { left: 0, top: 0, right: sc(400), bottom: sc(128) };
            AdjustWindowRectExForDpi(&mut rc, style, 0, ex, dpi);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let work = tb.map(|t| t.work).unwrap_or(Rect { l: 0, t: 0, r: 1280, b: 720 });
            let cls = wide(CLASS);
            let title = wide("Resource Monitor \u{2014} Update");
            a.upd = CreateWindowExW(
                ex,
                cls.as_ptr(),
                title.as_ptr(),
                style,
                work.r - w - sc(16),
                work.b - h - sc(16),
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            if a.upd.is_null() {
                return;
            }
            style_window(a.upd, a.dark);
            ShowWindow(a.upd, if activate { SW_SHOWNORMAL } else { SW_SHOWNOACTIVATE });
        } else {
            InvalidateRect(a.upd, std::ptr::null(), 0);
            if activate {
                ShowWindow(a.upd, SW_SHOWNORMAL);
            }
        }
        if activate {
            SetForegroundWindow(a.upd);
        }
    }
}

fn paint(a: &App, hwnd: HWND) {
    let p = Painter::begin(hwnd);
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let scale = dpi as f32 / 96.0;
    let sc = |v: f32| (v * scale).round() as i32;
    let pal = palette(a.dark);
    let f = Fonts::new(scale);
    p.fill(Rect { l: 0, t: 0, r: p.w, b: p.h }, pal.bg);
    let pad = sc(18.0);
    p.text(f.mid, "Software update", pad, pad, pal.fg);
    p.text_right(f.small, concat!("current v", env!("CARGO_PKG_VERSION")), p.w - pad, pad + sc(4.0), pal.dim);
    let ty = pad + sc(28.0);
    let bar_y = p.h - pad - sc(8.0);
    p.text_wrap(f.normal, &a.upd_text, Rect { l: pad, t: ty, r: p.w - pad, b: bar_y - sc(6.0) }, pal.fg);
    if let Some(pct) = a.upd_pct {
        let bar = Rect { l: pad, t: bar_y, r: p.w - pad, b: bar_y + sc(8.0) };
        p.round(bar, sc(8.0), pal.line);
        let fw = (bar.w() as f32 * (pct / 100.0).clamp(0.0, 1.0)) as i32;
        if fw > 0 {
            p.round(Rect { r: bar.l + fw.max(bar.h()), ..bar }, sc(8.0), accent(Metric::Down, a.dark));
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            match crate::app::app() {
                Some(a) => paint(a, hwnd),
                None => {
                    let mut ps = std::mem::zeroed();
                    BeginPaint(hwnd, &mut ps);
                    EndPaint(hwnd, &ps);
                }
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_DESTROY => {
            if let Some(a) = crate::app::app() {
                a.upd = std::ptr::null_mut();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
