//! Hover / click details panel.

use crate::app::{accent, App, C_CRIT, C_WARN};
use crate::fmt;
use crate::render::{create_font, ui_face, Painter, Rgba};
use crate::settings::Metric;
use crate::taskbar::Edge;
use crate::usage;
use crate::util::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Popup";

pub struct Pal {
    pub bg: Rgba,
    pub card: Rgba,
    pub fg: Rgba,
    pub dim: Rgba,
    pub line: Rgba,
}

pub fn palette(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: Rgba::rgb(32, 32, 32),
            card: Rgba::rgb(43, 43, 43),
            fg: Rgba::rgb(242, 242, 242),
            dim: Rgba::rgb(160, 160, 160),
            line: Rgba::rgb(58, 58, 58),
        }
    } else {
        Pal {
            bg: Rgba::rgb(249, 249, 249),
            card: Rgba::rgb(255, 255, 255),
            fg: Rgba::rgb(26, 26, 26),
            dim: Rgba::rgb(96, 96, 96),
            line: Rgba::rgb(224, 224, 224),
        }
    }
}

pub fn style_window(hwnd: HWND, dark: bool) {
    unsafe {
        let d: i32 = dark as i32;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &d as *const i32 as _, 4);
        let round: i32 = DWMWCP_ROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &round as *const i32 as _, 4);
    }
}

pub struct Fonts {
    pub small: HFONT,
    pub normal: HFONT,
    pub bold: HFONT,
    pub big: HFONT,
}

impl Fonts {
    pub fn new(scale: f32) -> Fonts {
        let px = |v: f32| (v * scale).round() as i32;
        let f = ui_face();
        Fonts {
            small: create_font(px(11.0), 400, f, false),
            normal: create_font(px(12.5), 400, f, false),
            bold: create_font(px(12.5), 600, f, false),
            big: create_font(px(17.0), 600, f, false),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in [self.small, self.normal, self.bold, self.big] {
                DeleteObject(f);
            }
        }
    }
}

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let cls = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
    }
}

pub fn create(hinst: HINSTANCE, owner: HWND) -> HWND {
    let cls = wide(CLASS);
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            cls.as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            owner,
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        )
    }
}

fn sc(a: &App, v: f32) -> i32 {
    (v * a.scale).round() as i32
}

fn has_gpu(a: &App) -> bool {
    a.snap.gpu.is_some()
}

fn size(a: &App) -> (i32, i32) {
    let mut h = 14.0 + 22.0; // pad + header
    let rows = 4 + has_gpu(a) as i32; // cpu, ram, (gpu), disk, net
    h += rows as f32 * 46.0;
    h += 12.0 + 20.0 + 2.0 * 19.0; // usage header + today + month
    if a.s.data_cap_gb > 0.0 {
        h += 26.0;
    }
    if a.snap.battery.is_some() {
        h += 22.0;
    }
    h += 14.0 + 20.0 + 5.0 * 18.0; // processes
    h += 10.0 + 18.0 + 12.0; // footer + pad
    (sc(a, 380.0), sc(a, h))
}

pub fn reposition(a: &mut App) {
    let (w, h) = size(a);
    let Some(tb) = a.tb else { return };
    let anchor = if unsafe { IsWindowVisible(a.hwnd) } != 0 { a.pos } else { tb.tray };
    let g = sc(a, 10.0);
    let work = tb.work;
    let above = (anchor.r - w, anchor.t - h - g);
    let below = (anchor.r - w, anchor.b + g);
    let left = (anchor.l - w - g, anchor.b - h);
    let right = (anchor.r + g, anchor.b - h);
    // Preferred side first (away from the taskbar), then any side that fits.
    let order = match (a.s.mode, tb.edge) {
        (crate::settings::Mode::Float, _) | (_, Edge::Bottom) => [above, below, left, right],
        (_, Edge::Top) => [below, above, left, right],
        (_, Edge::Left) => [right, above, below, left],
        (_, Edge::Right) => [left, above, below, right],
    };
    let clamp = |(x, y): (i32, i32)| {
        (x.clamp(work.l + g, (work.r - w - g).max(work.l)), y.clamp(work.t + g, (work.b - h - g).max(work.t)))
    };
    let clear = |(x, y): (i32, i32)| !(x < anchor.r && x + w > anchor.l && y < anchor.b && y + h > anchor.t);
    let (x, y) = order.iter().map(|p| clamp(*p)).find(|p| clear(*p)).unwrap_or_else(|| clamp(order[0]));
    unsafe {
        SetWindowPos(a.popup, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        InvalidateRect(a.popup, std::ptr::null(), 0);
    }
}

pub fn show(a: &mut App, pinned: bool) {
    if a.popup.is_null() {
        return;
    }
    a.popup_pinned = pinned;
    style_window(a.popup, a.dark);
    reposition(a);
    unsafe { ShowWindow(a.popup, SW_SHOWNOACTIVATE) };
}

pub fn hide(a: &mut App) {
    a.popup_pinned = false;
    if !a.popup.is_null() {
        unsafe { ShowWindow(a.popup, SW_HIDE) };
    }
}

fn paint(a: &mut App, hwnd: HWND) {
    // Size can change (GPU appears, battery…): keep window in sync.
    let (w, h) = size(a);
    if let Some(r) = window_rect(hwnd) {
        if r.w() != w || r.h() != h {
            reposition(a);
        }
    }
    let p = Painter::begin(hwnd);
    let pal = palette(a.dark);
    let f = Fonts::new(a.scale);
    let s = &a.snap;
    let pad = sc(a, 14.0);
    let right = p.w - pad;
    p.fill(Rect { l: 0, t: 0, r: p.w, b: p.h }, pal.bg);

    let mut y = pad;
    p.text(f.bold, "System", pad, y, pal.fg);
    p.text_right(
        f.small,
        &format!("Up {}  \u{00B7}  {} processes", fmt::duration(s.uptime_secs), s.proc_count),
        right,
        y + sc(a, 2.0),
        pal.dim,
    );
    y += sc(a, 22.0);

    let level_color = |v: f32| {
        if v >= a.s.crit_pct {
            C_CRIT
        } else if v >= a.s.warn_pct {
            C_WARN
        } else {
            pal.fg
        }
    };
    let chart_w = sc(a, 120.0);
    let row_h = sc(a, 46.0);
    let row = |label: &str, value: &str, vc: Rgba, series: &[(Metric, Vec<f32>)], y: &mut i32| {
        p.round(Rect { l: pad, t: *y, r: right, b: *y + row_h - sc(a, 6.0) }, sc(a, 8.0), pal.card);
        let ix = pad + sc(a, 10.0);
        p.text(f.small, label, ix, *y + sc(a, 4.0), pal.dim);
        p.text_fit(f.big, value, ix, *y + sc(a, 17.0), right - chart_w - ix - sc(a, 16.0), vc);
        let cr = Rect {
            l: right - chart_w - sc(a, 8.0),
            t: *y + sc(a, 6.0),
            r: right - sc(a, 8.0),
            b: *y + row_h - sc(a, 12.0),
        };
        for (m, d) in series {
            p.chart(cr, d, accent(*m, a.dark), pal.card);
        }
        *y += row_h;
    };

    row("CPU", &format!("{:.0}%", s.cpu), level_color(s.cpu), &[(Metric::Cpu, a.series(Metric::Cpu, 60))], &mut y);
    row(
        &format!("Memory  \u{00B7}  {:.0}%", s.ram_pct),
        &format!("{} / {}", fmt::bytes(s.ram_used), fmt::bytes(s.ram_total)),
        level_color(s.ram_pct),
        &[(Metric::Ram, a.series(Metric::Ram, 60))],
        &mut y,
    );
    if let Some(g) = s.gpu {
        let v = match s.vram_used {
            Some(m) => format!("{:.0}%  \u{00B7}  {} VRAM", g, fmt::bytes(m)),
            None => format!("{:.0}%", g),
        };
        row("GPU", &v, level_color(g), &[(Metric::Gpu, a.series(Metric::Gpu, 60))], &mut y);
    }
    let disk_label = match s.disk_active {
        Some(act) => format!("Disk  \u{00B7}  {:.0}% active", act),
        None => "Disk".into(),
    };
    row(
        &disk_label,
        &format!("R {}   W {}", fmt::rate(s.disk_read), fmt::rate(s.disk_write)),
        pal.fg,
        &[(Metric::DiskRead, a.series(Metric::DiskRead, 60))],
        &mut y,
    );
    let adapter = if a.s.adapter.is_empty() { "all adapters".to_string() } else { a.s.adapter.clone() };
    // Shared scale so up/down are comparable.
    let (up, down) = {
        let hu = &a.hist[Metric::Up.idx()];
        let hd = &a.hist[Metric::Down.idx()];
        let max = hu.iter().chain(hd.iter()).copied().fold(64.0 * 1024.0, f32::max);
        (hu.iter().map(|v| v / max).collect::<Vec<_>>(), hd.iter().map(|v| v / max).collect::<Vec<_>>())
    };
    row(
        &format!("Network  \u{00B7}  {}", adapter),
        &format!("\u{25B2} {}   \u{25BC} {}", fmt::speed(s.net_up, a.s.unit), fmt::speed(s.net_down, a.s.unit)),
        pal.fg,
        &[(Metric::Down, down), (Metric::Up, up)],
        &mut y,
    );

    // ---- data usage
    y += sc(a, 4.0);
    p.text(f.bold, "Data usage", pad, y, pal.fg);
    y += sc(a, 20.0);
    let now = usage::today();
    let (yy, mm, _) = usage::ymd(now);
    let line = |label: &str, u: usage::Usage, y: i32| {
        p.text(f.normal, label, pad, y, pal.dim);
        let x0 = pad + sc(a, 100.0);
        p.text(f.normal, &format!("\u{25B2} {}", fmt::bytes(u.sent)), x0, y, accent(Metric::Up, a.dark));
        p.text(f.normal, &format!("\u{25BC} {}", fmt::bytes(u.recv)), x0 + sc(a, 95.0), y, accent(Metric::Down, a.dark));
        p.text_right(f.bold, &fmt::bytes(u.total()), right, y, pal.fg);
    };
    line("Today", a.usage.get(now), y);
    y += sc(a, 19.0);
    line("This month", a.usage.month(yy, mm), y);
    y += sc(a, 19.0);
    if let Some((used, pct)) = a.cycle_usage() {
        y += sc(a, 4.0);
        let bar = Rect { l: pad, t: y + sc(a, 4.0), r: right - sc(a, 120.0), b: y + sc(a, 10.0) };
        p.round(bar, sc(a, 6.0), pal.line);
        let fw = ((bar.w() as f64) * (pct / 100.0).min(1.0)) as i32;
        let c = if pct >= a.s.crit_pct as f64 {
            C_CRIT
        } else if pct >= a.s.warn_pct as f64 {
            C_WARN
        } else {
            accent(Metric::Down, a.dark)
        };
        if fw > 0 {
            p.round(Rect { r: bar.l + fw, ..bar }, sc(a, 6.0), c);
        }
        p.text_right(
            f.small,
            &format!("{} of {:.0} GB ({:.0}%)", fmt::bytes(used), a.s.data_cap_gb, pct),
            right,
            y,
            pal.dim,
        );
        y += sc(a, 22.0);
    }
    if let Some(b) = s.battery {
        y += sc(a, 3.0);
        let mut t = format!("{}%", b.percent);
        if b.charging {
            t.push_str("  \u{00B7}  charging");
        } else if b.on_ac {
            t.push_str("  \u{00B7}  plugged in");
        }
        if let (Some(sec), false) = (b.secs_left, b.on_ac) {
            t.push_str(&format!("  \u{00B7}  {} left", fmt::duration(sec as u64)));
        }
        p.text(f.normal, "Battery", pad, y, pal.dim);
        p.text(f.normal, &t, pad + sc(a, 100.0), y, pal.fg);
        y += sc(a, 19.0);
    }

    // ---- processes
    y += sc(a, 10.0);
    let col_w = (right - pad) / 3;
    let cols: [(&str, Vec<(String, String)>); 3] = [
        ("Top CPU", s.top_cpu.iter().map(|p| (p.name.clone(), format!("{:.1}%", p.cpu))).collect()),
        ("Top memory", s.top_mem.iter().map(|p| (p.name.clone(), fmt::bytes(p.mem))).collect()),
        ("Top disk I/O", s.top_io.iter().map(|p| (p.name.clone(), fmt::rate(p.io_bps))).collect()),
    ];
    for (i, (title, list)) in cols.iter().enumerate() {
        let x = pad + i as i32 * col_w;
        let xr = x + col_w - sc(a, 10.0);
        p.text(f.bold, title, x, y, pal.fg);
        if list.is_empty() {
            p.text(f.small, "\u{2013}", x, y + sc(a, 20.0), pal.dim);
        }
        for (j, (name, val)) in list.iter().take(5).enumerate() {
            let ry = y + sc(a, 20.0) + j as i32 * sc(a, 18.0);
            let vw = p.measure(f.small, val).0;
            p.text_fit(f.small, name, x, ry, xr - x - vw - sc(a, 6.0), pal.dim);
            p.text_right(f.small, val, xr, ry, pal.fg);
        }
    }
    y += sc(a, 20.0) + 5 * sc(a, 18.0) + sc(a, 10.0);
    p.fill(Rect { l: pad, t: y - sc(a, 4.0), r: right, b: y - sc(a, 3.0) }, pal.line);
    p.text(
        f.small,
        if a.popup_pinned {
            "Click to close  \u{00B7}  Double-click widget: Task Manager"
        } else {
            "Click: pin  \u{00B7}  Double-click: Task Manager  \u{00B7}  Right-click: menu"
        },
        pad,
        y + sc(a, 2.0),
        pal.dim,
    );
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            if let Some(a) = crate::app::app() {
                paint(a, hwnd);
            } else {
                let mut ps = std::mem::zeroed();
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_LBUTTONUP => {
            if let Some(a) = crate::app::app() {
                hide(a);
            }
            0
        }
        WM_RBUTTONUP => {
            if let Some(a) = crate::app::app() {
                hide(a);
                let owner = a.hwnd;
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                PostMessageW(owner, crate::tray::WM_TRAY, ((pt.y as u16 as usize) << 16) | pt.x as u16 as usize, WM_CONTEXTMENU as isize);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
