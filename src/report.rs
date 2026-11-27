//! Data usage report window.

use crate::app::{accent, App, C_CRIT, C_WARN};
use crate::fmt;
use crate::popup::{palette, style_window, Fonts};
use crate::render::Painter;
use crate::settings::Metric;
use crate::usage::{self, Usage};
use crate::util::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Report";
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

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

fn client_size(scale: f32, cap: bool) -> (i32, i32) {
    // Content: header + cards + chart + 6 table rows.
    let h = if cap { 640.0 } else { 596.0 };
    ((640.0 * scale) as i32, (h * scale) as i32)
}

pub fn open(a: &mut App) {
    unsafe {
        if !a.report.is_null() {
            ShowWindow(a.report, SW_RESTORE);
            SetForegroundWindow(a.report);
            return;
        }
        let tb = a.primary_taskbar();
        let scale = tb.map_or(1.0, |t| t.dpi as f32 / 96.0);
        let (cw, ch) = client_size(scale, a.s.data_cap_gb > 0.0);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT { left: 0, top: 0, right: cw, bottom: ch };
        AdjustWindowRectExForDpi(&mut rc, style, 0, 0, tb.map_or(96, |t| t.dpi));
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let work = tb.map(|t| t.work).unwrap_or(Rect { l: 0, t: 0, r: 1280, b: 720 });
        let x = work.l + (work.w() - w) / 2;
        let y = work.t + ((work.h() - h) / 2).max(0);
        let cls = wide(CLASS);
        let title = wide("Data Usage \u{2014} Resource Monitor");
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        a.report = hwnd;
        style_window(hwnd, a.dark);
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
    }
}

fn win_scale(hwnd: HWND) -> f32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 {
        1.0
    } else {
        d as f32 / 96.0
    }
}
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};

fn paint(a: &App, hwnd: HWND) {
    let p = Painter::begin(hwnd);
    let scale = win_scale(hwnd);
    let sc = |v: f32| (v * scale).round() as i32;
    let pal = palette(a.dark);
    let f = Fonts::new(scale);
    let pad = sc(20.0);
    let right = p.w - pad;
    p.fill(Rect { l: 0, t: 0, r: p.w, b: p.h }, pal.bg);

    let now = usage::today();
    let (y0, m0, _) = usage::ymd(now);
    let st = &a.usage;
    let mut y = pad;

    let since = st.days.keys().next().map(|k| {
        let (yy, mm, dd) = usage::ymd(*k);
        format!("Tracking since {} {} {}", dd, MONTHS[(mm as usize).clamp(1, 12) - 1], yy)
    });
    let src = if a.s.adapter.is_empty() { "All connected adapters".to_string() } else { a.s.adapter.clone() };
    p.text(f.big, "Data usage", pad, y, pal.fg);
    p.text_right(
        f.small,
        &format!("{}  \u{00B7}  {}", src, since.unwrap_or_else(|| "No data yet".into())),
        right,
        y + sc(5.0),
        pal.dim,
    );
    y += sc(34.0);

    // ---- summary cards
    let (lm_y, lm_m) = if m0 == 1 { (y0 - 1, 12) } else { (y0, m0 - 1) };
    let cards: [(&str, Usage); 6] = [
        ("Today", st.get(now)),
        ("Yesterday", st.get(usage::prev_day(now))),
        ("This month", st.month(y0, m0)),
        ("Last month", st.month(lm_y, lm_m)),
        ("This year", st.year(y0)),
        ("All time", st.all()),
    ];
    let gap = sc(10.0);
    let cw = (right - pad - 2 * gap) / 3;
    let ch = sc(68.0);
    for (i, (title, u)) in cards.iter().enumerate() {
        let x = pad + (i as i32 % 3) * (cw + gap);
        let cy = y + (i as i32 / 3) * (ch + gap);
        p.round(Rect { l: x, t: cy, r: x + cw, b: cy + ch }, sc(10.0), pal.card);
        let ix = x + sc(12.0);
        p.text(f.small, title, ix, cy + sc(8.0), pal.dim);
        p.text(f.big, &fmt::bytes(u.total()), ix, cy + sc(22.0), pal.fg);
        let ux = ix;
        let w1 = p.text(f.small, &format!("\u{25B2} {}", fmt::bytes(u.sent)), ux, cy + sc(46.0), accent(Metric::Up, a.dark));
        p.text(
            f.small,
            &format!("\u{25BC} {}", fmt::bytes(u.recv)),
            ux + w1 + sc(10.0),
            cy + sc(46.0),
            accent(Metric::Down, a.dark),
        );
    }
    y += 2 * ch + gap + sc(16.0);

    // ---- billing cycle
    if let Some((used, pct)) = a.cycle_usage() {
        p.text(f.bold, "Data cap", pad, y, pal.fg);
        let t = format!(
            "{} of {:.0} GB  ({:.0}%)  \u{00B7}  resets on day {}",
            fmt::bytes(used),
            a.s.data_cap_gb,
            pct,
            a.s.cap_reset_day
        );
        p.text_right(f.small, &t, right, y + sc(2.0), pal.dim);
        y += sc(22.0);
        let bar = Rect { l: pad, t: y, r: right, b: y + sc(8.0) };
        p.round(bar, sc(8.0), pal.line);
        let fw = (bar.w() as f64 * (pct / 100.0).min(1.0)) as i32;
        let c = if pct >= a.s.crit_pct as f64 {
            C_CRIT
        } else if pct >= a.s.warn_pct as f64 {
            C_WARN
        } else {
            accent(Metric::Down, a.dark)
        };
        if fw > 0 {
            p.round(Rect { r: bar.l + fw, ..bar }, sc(8.0), c);
        }
        y += sc(22.0);
    }

    // ---- last 30 days chart
    p.text(f.bold, "Last 30 days", pad, y, pal.fg);
    let wu = p.measure(f.small, "\u{25A0} Upload").0;
    p.text_right(f.small, "\u{25A0} Upload", right, y + sc(2.0), accent(Metric::Up, a.dark));
    p.text_right(f.small, "\u{25A0} Download", right - wu - sc(14.0), y + sc(2.0), accent(Metric::Down, a.dark));
    y += sc(24.0);
    let mut days = Vec::with_capacity(30);
    let mut d = now;
    for _ in 0..30 {
        days.push(d);
        d = usage::prev_day(d);
    }
    days.reverse();
    let vals: Vec<Usage> = days.iter().map(|k| st.get(*k)).collect();
    let max = vals.iter().map(|u| u.total()).max().unwrap_or(0).max(1);
    let chart = Rect { l: pad + sc(48.0), t: y, r: right, b: y + sc(130.0) };
    p.fill(Rect { l: chart.l, t: chart.b, r: chart.r, b: chart.b + 1 }, pal.line);
    p.fill(Rect { l: chart.l, t: chart.t, r: chart.r, b: chart.t + 1 }, pal.line);
    p.text_right(f.small, &fmt::bytes(max), chart.l - sc(6.0), chart.t - sc(6.0), pal.dim);
    p.text_right(f.small, "0", chart.l - sc(6.0), chart.b - sc(8.0), pal.dim);
    let slot = chart.w() as f32 / 30.0;
    let bw = (slot * 0.7).max(2.0) as i32;
    for (i, u) in vals.iter().enumerate() {
        let x = chart.l + (i as f32 * slot + (slot - bw as f32) / 2.0) as i32;
        let hr = (chart.h() as f64 * u.recv as f64 / max as f64) as i32;
        let hs = (chart.h() as f64 * u.sent as f64 / max as f64) as i32;
        let today = i == 29;
        let mut dc = accent(Metric::Down, a.dark);
        let mut uc = accent(Metric::Up, a.dark);
        if !today {
            dc = pal.card.mix(dc, 0.75);
            uc = pal.card.mix(uc, 0.75);
        }
        if hr > 0 {
            p.fill(Rect { l: x, t: chart.b - hr, r: x + bw, b: chart.b }, dc);
        }
        if hs > 0 {
            p.fill(Rect { l: x, t: chart.b - hr - hs, r: x + bw, b: chart.b - hr }, uc);
        }
        if i % 7 == 1 || today {
            let (_, mm, dd) = usage::ymd(days[i]);
            let lbl = if today { "Today".to_string() } else { format!("{} {}", dd, MONTHS[(mm as usize).clamp(1, 12) - 1]) };
            let lw = p.measure(f.small, &lbl).0;
            let cx = (x + bw / 2 - lw / 2).min(right - lw);
            p.text(f.small, &lbl, cx, chart.b + sc(4.0), pal.dim);
        }
    }
    y = chart.b + sc(30.0);

    // ---- months & years tables
    let half = (right - pad - gap) / 2;
    let table = |x: i32, title: &str, rows: &[(String, Usage)], y: i32| {
        let r = x + half;
        p.text(f.bold, title, x, y, pal.fg);
        let mut yy = y + sc(22.0);
        let c1 = r - sc(150.0);
        let c2 = r - sc(75.0);
        p.text_right(f.small, "\u{25B2} Up", c1, yy, pal.dim);
        p.text_right(f.small, "\u{25BC} Down", c2, yy, pal.dim);
        p.text_right(f.small, "Total", r, yy, pal.dim);
        yy += sc(18.0);
        if rows.is_empty() {
            p.text(f.small, "No data yet", x, yy, pal.dim);
        }
        for (label, u) in rows {
            p.fill(Rect { l: x, t: yy - sc(2.0), r, b: yy - sc(1.0) }, pal.line);
            p.text(f.normal, label, x, yy, pal.fg);
            p.text_right(f.small, &fmt::bytes(u.sent), c1, yy + sc(1.0), pal.dim);
            p.text_right(f.small, &fmt::bytes(u.recv), c2, yy + sc(1.0), pal.dim);
            p.text_right(f.bold, &fmt::bytes(u.total()), r, yy, pal.fg);
            yy += sc(19.0);
        }
    };
    let mut months = Vec::new();
    let (mut yy, mut mm) = (y0, m0);
    for _ in 0..12 {
        if months.len() == 6 {
            break;
        }
        let u = st.month(yy, mm);
        if u.total() > 0 || (yy, mm) == (y0, m0) {
            months.push((format!("{} {}", MONTHS[(mm as usize).clamp(1, 12) - 1], yy), u));
        }
        if mm == 1 {
            mm = 12;
            yy -= 1;
        } else {
            mm -= 1;
        }
    }
    let mut years: Vec<(String, Usage)> = Vec::new();
    let first_year = st.days.keys().next().map_or(y0, |k| usage::ymd(*k).0);
    for yr in (first_year..=y0).rev().take(6) {
        years.push((yr.to_string(), st.year(yr)));
    }
    table(pad, "Monthly", &months, y);
    table(pad + half + gap, "Yearly", &years, y);
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
        WM_DPICHANGED => {
            let r = &*(l as *const RECT);
            SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            InvalidateRect(hwnd, std::ptr::null(), 0);
            0
        }
        WM_KEYDOWN if w == 0x1B => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            if let Some(a) = crate::app::app() {
                a.report = std::ptr::null_mut();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
