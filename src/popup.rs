//! Hover / click details panel.

use crate::app::{accent, App};
use crate::fmt;
use crate::render::{create_font, ui_face, Canvas, Rgba};
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
    pub chip: Rgba,
    pub border: Rgba,
    pub track: Rgba,
    pub fg: Rgba,
    pub dim: Rgba,
    pub line: Rgba,
}

/// See docs/ui-reference: near-black base, raised cards, hairline borders.
pub fn palette(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: Rgba::rgb(28, 28, 28),
            card: Rgba::rgb(38, 38, 38),
            chip: Rgba::rgb(50, 50, 50),
            border: Rgba::rgb(52, 52, 52),
            track: Rgba::rgb(58, 58, 58),
            fg: Rgba::rgb(242, 242, 242),
            dim: Rgba::rgb(150, 150, 150),
            line: Rgba::rgb(58, 58, 58),
        }
    } else {
        Pal {
            bg: Rgba::rgb(243, 243, 243),
            card: Rgba::rgb(255, 255, 255),
            chip: Rgba::rgb(235, 235, 235),
            border: Rgba::rgb(222, 222, 222),
            track: Rgba::rgb(228, 228, 228),
            fg: Rgba::rgb(26, 26, 26),
            dim: Rgba::rgb(100, 100, 100),
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
    pub mid: HFONT,
    pub big: HFONT,
    pub caps: HFONT,
    pub scale: f32,
}

impl Fonts {
    /// ClearType fonts for opaque GDI painting.
    pub fn new(scale: f32) -> Fonts {
        Self::make(scale, false)
    }
    /// Grayscale-AA fonts for the software canvas.
    pub fn gray(scale: f32) -> Fonts {
        Self::make(scale, true)
    }
    fn make(scale: f32, gray: bool) -> Fonts {
        let px = |v: f32| (v * scale).round() as i32;
        let f = ui_face();
        Fonts {
            small: create_font(px(11.0), 400, f, gray),
            normal: create_font(px(12.5), 400, f, gray),
            bold: create_font(px(12.5), 600, f, gray),
            mid: create_font(px(15.0), 600, f, gray),
            big: create_font(px(20.0), 600, f, gray),
            caps: create_font(px(10.0), 600, f, gray),
            scale,
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in [self.small, self.normal, self.bold, self.mid, self.big, self.caps] {
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

fn scale(a: &App) -> f32 {
    a.anchor().map_or(1.0, |x| x.2)
}

fn sc(a: &App, v: f32) -> i32 {
    (v * scale(a)).round() as i32
}

/// Clamp that never panics when the box is larger than the range.
fn fit(v: i32, lo: i32, hi: i32) -> i32 {
    v.min(hi).max(lo)
}

// Section heights (px at 96 dpi). `size` and `paint` share these.
const PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const W: f32 = 392.0;
const HEADER: f32 = 28.0;
const RINGS: f32 = 134.0;
const NET: f32 = 84.0;
const DISK: f32 = 62.0;
const USAGE: f32 = 70.0;
const CAP: f32 = 24.0;
const PROC_ROW: f32 = 17.0;
const PROCS: f32 = 30.0 + 5.0 * PROC_ROW;
const FOOTER: f32 = 22.0;

fn size(a: &App) -> (i32, i32) {
    let mut h = PAD + HEADER + GAP + RINGS + GAP + NET + GAP + DISK + GAP + USAGE + GAP + PROCS + GAP + FOOTER;
    if a.s.data_cap_gb > 0.0 {
        h += CAP;
    }
    (sc(a, W), sc(a, h))
}

pub fn reposition(a: &mut App) {
    let (w, h) = size(a);
    let Some((anchor, tb, _)) = a.anchor() else { return };
    let g = sc(a, 10.0);
    let center = POINT { x: (anchor.l + anchor.r) / 2, y: (anchor.t + anchor.b) / 2 };
    let work = crate::taskbar::monitor_info(unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST) })
        .map_or(tb.work, |m| m.1);
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
        (fit(x, work.l + g, work.r - w - g), fit(y, work.t + g, work.b - h - g))
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

/// Card with hairline border.
fn card(c: &mut Canvas, r: Rect, rad: f32, pal: &Pal) {
    c.fill_round(r, rad, pal.border);
    c.fill_round(Rect { l: r.l + 1, t: r.t + 1, r: r.r - 1, b: r.b - 1 }, rad - 1.0, pal.card);
}

/// Pill-shaped chip; returns its width.
fn chip(c: &mut Canvas, f: HFONT, text: &str, right: i32, y: i32, h: i32, pal: &Pal) -> i32 {
    let (tw, th) = c.measure(f, text);
    let w = tw + h;
    let r = Rect { l: right - w, t: y, r: right, b: y + h };
    c.fill_round(r, h as f32 / 2.0, pal.chip);
    c.text(f, text, r.l + h / 2, y + (h - th) / 2, pal.dim);
    w
}

fn paint(a: &mut App, hwnd: HWND) {
    // Size can change (GPU appears, cap set…): keep window in sync.
    let (w, h) = size(a);
    if let Some(r) = window_rect(hwnd) {
        if r.w() != w || r.h() != h {
            reposition(a);
        }
    }
    let scale = scale(a);
    let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
    let dc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut rc) };
    let (cw, ch) = (rc.right.max(1), rc.bottom.max(1));

    // Fonts and canvas are cached across the once-per-second repaints.
    let mut cache = a.popup_cache.take().filter(|(f, _)| f.scale == scale);
    let (f, mut c) = cache.take().unwrap_or_else(|| (Fonts::gray(scale), Canvas::new()));
    c.resize(cw, ch);
    draw(a, &mut c, &f, scale);
    c.blit(dc, 0, 0);
    a.popup_cache = Some((f, c));
    unsafe { EndPaint(hwnd, &ps) };
}

fn draw(a: &App, c: &mut Canvas, f: &Fonts, scale: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let pal = palette(a.dark);
    let s = &a.snap;
    let pad = sc(PAD);
    let gap = sc(GAP);
    let right = c.w - pad;
    let rad = 10.0 * scale;
    c.clear(pal.bg);

    // ---- header: title + chips
    let mut y = pad;
    c.text(f.mid, "System", pad, y + sc(2.0), pal.fg);
    let chip_h = sc(22.0);
    let mut x = right;
    x -= chip(c, f.small, &format!("{} processes", s.proc_count), x, y, chip_h, &pal) + sc(6.0);
    x -= chip(c, f.small, &format!("Up {}", fmt::duration(s.uptime_secs)), x, y, chip_h, &pal) + sc(6.0);
    if let Some(b) = s.battery {
        let mut t = format!("Battery {}%", b.percent);
        if b.charging {
            t.push_str(" \u{2191}");
        } else if let (Some(sec), false) = (b.secs_left, b.on_ac) {
            t.push_str(&format!(" \u{00B7} {}", fmt::duration(sec as u64)));
        }
        chip(c, f.small, &t, x, y, chip_h, &pal);
    }
    y += sc(HEADER) + gap;

    // ---- ring gauges
    struct Gauge {
        label: &'static str,
        pct: f32,
        sub: String,
        m: Metric,
    }
    let mut gauges = vec![
        Gauge {
            label: "CPU",
            pct: s.cpu,
            sub: s.top_cpu.first().map_or(String::new(), |p| format!("{} {:.0}%", p.name, p.cpu)),
            m: Metric::Cpu,
        },
        Gauge {
            label: "MEMORY",
            pct: s.ram_pct,
            sub: format!("{} / {}", fmt::bytes(s.ram_used), fmt::bytes(s.ram_total)),
            m: Metric::Ram,
        },
    ];
    if let Some(g) = s.gpu {
        gauges.push(Gauge { label: "GPU", pct: g, sub: s.vram_used.map_or(String::new(), |v| format!("{} VRAM", fmt::bytes(v))), m: Metric::Gpu });
    } else if let Some(d) = s.disk_active {
        gauges.push(Gauge { label: "DISK", pct: d, sub: "active time".into(), m: Metric::DiskActive });
    }
    let n = gauges.len() as i32;
    let gw = (right - pad - gap * (n - 1)) / n;
    let gh = sc(RINGS);
    for (i, g) in gauges.iter().enumerate() {
        let x = pad + i as i32 * (gw + gap);
        let r = Rect { l: x, t: y, r: x + gw, b: y + gh };
        card(c, r, rad, &pal);
        let col = a.heat(accent(g.m, a.dark), g.pct);
        let (cx, cy) = ((x + gw / 2) as f32, (y + sc(48.0)) as f32);
        let rr = 36.0 * scale;
        let th = 7.0 * scale;
        c.ring(cx, cy, rr, th, 1.0, pal.track);
        c.ring(cx, cy, rr, th, g.pct / 100.0, col);
        let v = format!("{:.0}%", g.pct);
        let (_, vh) = c.measure(f.mid, &v);
        c.text_center(f.mid, &v, cx as i32, cy as i32 - vh / 2, a.heat_text(pal.fg, g.pct));
        c.text_center(f.caps, g.label, cx as i32, y + sc(92.0), pal.dim);
        let sub = c.ellipsize(f.small, &g.sub, gw - sc(16.0));
        c.text_center(f.small, &sub, cx as i32, y + sc(108.0), pal.fg);
    }
    y += gh + gap;

    // ---- network
    let chart_w = sc(150.0);
    let nh = sc(NET);
    card(c, Rect { l: pad, t: y, r: right, b: y + nh }, rad, &pal);
    let ix = pad + sc(14.0);
    let adapter = if a.s.adapter.is_empty() { "ALL ADAPTERS".to_string() } else { a.s.adapter.to_uppercase() };
    c.text(f.caps, &format!("NETWORK  \u{00B7}  {}", adapter), ix, y + sc(10.0), pal.dim);
    let down_c = accent(Metric::Down, a.dark);
    let up_c = accent(Metric::Up, a.dark);
    c.text(f.big, &format!("\u{25BC} {}", fmt::speed(s.net_down, a.s.unit)), ix, y + sc(26.0), down_c);
    c.text(f.normal, &format!("\u{25B2} {}", fmt::speed(s.net_up, a.s.unit)), ix, y + sc(56.0), up_c);
    {
        // Shared scale so up/down are comparable.
        let hu = &a.hist[Metric::Up.idx()];
        let hd = &a.hist[Metric::Down.idx()];
        let max = hu.iter().chain(hd.iter()).copied().fold(64.0 * 1024.0, f32::max);
        let down: Vec<f32> = hd.iter().map(|v| v / max).collect();
        let up: Vec<f32> = hu.iter().map(|v| v / max).collect();
        let cr = Rect { l: right - chart_w - sc(12.0), t: y + sc(12.0), r: right - sc(12.0), b: y + nh - sc(12.0) };
        c.spark(cr, &down, down_c);
        c.spark(cr, &up, up_c);
    }
    y += nh + gap;

    // ---- disk
    let dh = sc(DISK);
    card(c, Rect { l: pad, t: y, r: right, b: y + dh }, rad, &pal);
    let dl = match s.disk_active {
        Some(v) => format!("DISK  \u{00B7}  {:.0}% ACTIVE", v),
        None => "DISK".into(),
    };
    c.text(f.caps, &dl, ix, y + sc(10.0), pal.dim);
    c.text(
        f.mid,
        &format!("R {}   W {}", fmt::rate(s.disk_read), fmt::rate(s.disk_write)),
        ix,
        y + sc(28.0),
        pal.fg,
    );
    let dcol = accent(Metric::DiskRead, a.dark);
    let cr = Rect { l: right - chart_w - sc(12.0), t: y + sc(12.0), r: right - sc(12.0), b: y + dh - sc(12.0) };
    let rd = a.series(Metric::DiskRead, 60);
    c.spark(cr, &rd, dcol);
    y += dh + gap;

    // ---- data usage
    let cap = a.cycle_usage();
    let uh = sc(USAGE) + if cap.is_some() { sc(CAP) } else { 0 };
    card(c, Rect { l: pad, t: y, r: right, b: y + uh }, rad, &pal);
    c.text(f.caps, "DATA USAGE", ix, y + sc(10.0), pal.dim);
    let now = usage::today();
    let (yy, mm, _) = usage::ymd(now);
    let today = a.usage.get(now);
    let month = a.usage.month(yy, mm);
    let col2 = pad + (right - pad) / 2 + sc(6.0);
    c.text(f.small, "Today", ix, y + sc(26.0), pal.dim);
    c.text(f.mid, &fmt::bytes(today.total()), ix, y + sc(40.0), pal.fg);
    c.text(f.small, "This month", col2, y + sc(26.0), pal.dim);
    c.text(f.mid, &fmt::bytes(month.total()), col2, y + sc(40.0), pal.fg);
    let split = |u: usage::Usage| format!("\u{25B2} {}  \u{25BC} {}", fmt::bytes(u.sent), fmt::bytes(u.recv));
    let tw = c.measure(f.mid, &fmt::bytes(today.total())).0;
    c.text(f.small, &split(today), ix + tw + sc(8.0), y + sc(44.0), pal.dim);
    let mw = c.measure(f.mid, &fmt::bytes(month.total())).0;
    let ms = c.ellipsize(f.small, &split(month), right - sc(12.0) - (col2 + mw + sc(8.0)));
    c.text(f.small, &ms, col2 + mw + sc(8.0), y + sc(44.0), pal.dim);
    if let Some((used, pct)) = cap {
        let by = y + sc(USAGE);
        let label = format!("{} of {:.0} GB  ({:.0}%)", fmt::bytes(used), a.s.data_cap_gb, pct);
        let lw = c.measure(f.small, &label).0;
        let bar = Rect { l: ix, t: by, r: right - sc(14.0) - lw - sc(10.0), b: by + sc(8.0) };
        c.fill_round(bar, 4.0 * scale, pal.track);
        let fw = (bar.w() as f64 * (pct / 100.0).min(1.0)) as i32;
        let col = a.heat(accent(Metric::Down, a.dark), pct as f32);
        if fw > 0 {
            c.fill_round(Rect { r: bar.l + fw.max(bar.h()), ..bar }, 4.0 * scale, col);
        }
        c.text(f.small, &label, right - sc(14.0) - lw, by - sc(4.0), pal.dim);
    }
    y += uh + gap;

    // ---- processes
    let ph = sc(PROCS);
    card(c, Rect { l: pad, t: y, r: right, b: y + ph }, rad, &pal);
    let inner_l = ix;
    let inner_r = right - sc(14.0);
    let col_w = (inner_r - inner_l) / 3;
    let cols: [(&str, Vec<(String, String)>); 3] = [
        ("TOP CPU", s.top_cpu.iter().map(|p| (p.name.clone(), format!("{:.1}%", p.cpu))).collect()),
        ("TOP MEMORY", s.top_mem.iter().map(|p| (p.name.clone(), fmt::bytes(p.mem))).collect()),
        ("TOP DISK I/O", s.top_io.iter().map(|p| (p.name.clone(), fmt::rate(p.io_bps))).collect()),
    ];
    for (i, (title, list)) in cols.iter().enumerate() {
        let x = inner_l + i as i32 * col_w;
        let xr = x + col_w - sc(12.0);
        c.text(f.caps, title, x, y + sc(10.0), pal.dim);
        if list.is_empty() {
            c.text(f.small, "\u{2013}", x, y + sc(28.0), pal.dim);
        }
        for (j, (name, val)) in list.iter().take(5).enumerate() {
            let ry = y + sc(28.0) + j as i32 * sc(PROC_ROW);
            let vw = c.measure(f.small, val).0;
            let nm = c.ellipsize(f.small, name, xr - x - vw - sc(6.0));
            c.text(f.small, &nm, x, ry, pal.fg);
            c.text_right(f.small, val, xr, ry, pal.dim);
        }
    }
    y += ph + gap;

    // ---- footer
    let hint = if a.popup_pinned {
        "Click to close  \u{00B7}  Double-click widget: Task Manager"
    } else {
        "Click: pin  \u{00B7}  Double-click: Task Manager  \u{00B7}  Right-click: menu"
    };
    c.text_center(f.small, hint, c.w / 2, y + sc(2.0), pal.dim);
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
