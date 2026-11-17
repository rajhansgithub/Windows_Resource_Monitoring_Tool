//! Hover / click details panel.

use crate::app::{accent, App, HIST};
use crate::fmt;
use crate::render::{create_font, ui_face, Canvas, Rgba};
use crate::settings::Metric;
use crate::taskbar::Edge;
use crate::usage;
use crate::util::*;
use std::collections::VecDeque;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
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

/// Semi-transparent variant for the acrylic backdrop: the window background is
/// fully transparent (the backdrop shows through), cards stay mostly opaque so
/// text remains legible.
fn translucent(p: Pal, dark: bool) -> Pal {
    let (card_a, chip_a) = if dark { (191, 170) } else { (217, 190) }; // cards ~0.75 / 0.85
    Pal { bg: Rgba(0, 0, 0, 0), card: p.card.with_a(card_a), chip: p.chip.with_a(chip_a), ..p }
}

pub fn style_window(hwnd: HWND, dark: bool) {
    unsafe {
        let d: i32 = dark as i32;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &d as *const i32 as _, 4);
        let round: i32 = DWMWCP_ROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &round as *const i32 as _, 4);
    }
}

/// Windows build number (0 when unknown); `GetVersionEx` lies without a manifest.
fn build_number() -> u32 {
    static B: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *B.get_or_init(|| unsafe {
        let lib = GetModuleHandleW(wide("ntdll.dll").as_ptr());
        if lib.is_null() {
            return 0;
        }
        let Some(f) = GetProcAddress(lib, b"RtlGetVersion\0".as_ptr()) else { return 0 };
        let f: unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32 = std::mem::transmute(f);
        let mut v: OSVERSIONINFOW = std::mem::zeroed();
        v.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        if f(&mut v) == 0 {
            v.dwBuildNumber
        } else {
            0
        }
    })
}

/// Window styling for the details panel. Returns true when the native acrylic
/// backdrop (Windows 11, build 22000+) is active: the caller then paints a
/// transparent background with correct premultiplied alpha.
pub fn style_popup(hwnd: HWND, dark: bool) -> bool {
    style_window(hwnd, dark);
    if build_number() < 22000 {
        return false;
    }
    unsafe {
        let kind: i32 = DWMSBT_TRANSIENTWINDOW;
        let hr = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE as u32, &kind as *const i32 as _, 4);
        if hr >= 0 {
            let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            if DwmExtendFrameIntoClientArea(hwnd, &m) >= 0 {
                return true;
            }
            // Roll back so the opaque fallback paints on a plain window.
            let auto: i32 = DWMSBT_AUTO;
            DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE as u32, &auto as *const i32 as _, 4);
        }
    }
    false
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

/// What a region of the panel does. Regions are recorded while painting
/// (`App::popup_hits`); later entries sit on top of earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupHit {
    /// Chart time range in seconds.
    Range(usize),
    /// CPU / memory / GPU: open Task Manager.
    TaskMgr,
    /// Disk card: open Resource Monitor.
    ResMon,
    /// Network card: Windows data usage settings.
    NetSettings,
    /// Data usage card: our own usage report.
    UsageReport,
    /// A row of a top-process list (right-click for actions).
    Proc(u32),
    /// A chart (hover tooltip only; clicks fall through to the card below).
    Chart,
}

const RANGES: [(&str, usize); 3] = [("1m", 60), ("5m", 300), ("1h", 3600)];

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
const RINGS: f32 = 166.0;
const NET: f32 = 84.0;
const DISK: f32 = 62.0;
/// Extra disk card height for the "C: 82% used" line and bar.
const DISK_SPACE: f32 = 22.0;
const USAGE: f32 = 70.0;
const CAP: f32 = 24.0;
const PROC_ROW: f32 = 17.0;
const PROCS: f32 = 30.0 + 5.0 * PROC_ROW;
const FOOTER: f32 = 22.0;

fn has_space(a: &App) -> bool {
    a.snap.disk_used_pct.is_some() && a.snap.disk_total > 0
}

fn size(a: &App) -> (i32, i32) {
    let mut h = PAD + HEADER + GAP + RINGS + GAP + NET + GAP + DISK + GAP + USAGE + GAP + PROCS + GAP + FOOTER;
    if has_space(a) {
        h += DISK_SPACE;
    }
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
    a.popup_acrylic = style_popup(a.popup, a.dark);
    reposition(a);
    unsafe { ShowWindow(a.popup, SW_SHOWNOACTIVATE) };
}

pub fn hide(a: &mut App) {
    a.popup_pinned = false;
    a.popup_hover = None;
    a.popup_tracking = false;
    if !a.popup.is_null() {
        unsafe { ShowWindow(a.popup, SW_HIDE) };
    }
}

// ================================================================ drawing

/// Everything the drawing helpers read.
struct Env<'a> {
    a: &'a App,
    f: &'a Fonts,
    /// Palette used for painting (translucent with the acrylic backdrop).
    pal: Pal,
    /// Opaque palette (tooltips, markers).
    base: Pal,
    scale: f32,
    hover: Option<(i32, i32)>,
    /// Chart window in samples.
    n: usize,
}

impl Env<'_> {
    fn sc(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }
    fn hot(&self, r: Rect) -> bool {
        self.hover.is_some_and(|(x, y)| r.contains(x, y))
    }
}

#[derive(Default)]
struct Out {
    hits: Vec<(Rect, PopupHit)>,
    tip: Option<Tip>,
}

struct Tip {
    /// Marker column and the chart's hit rect.
    x: i32,
    r: Rect,
    text: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// 0..100 %, averaged when downsampled.
    Pct,
    /// Network rate, peak per bucket so spikes stay visible.
    Net,
    /// Disk rate, peak per bucket.
    Disk,
}

struct Trace {
    m: Metric,
    col: Rgba,
    tag: &'static str,
}

/// Last `n` samples of `h` reduced to at most `cols` points (NaN = no data
/// yet). Point `k` of `m` covers window samples [k*n/m, (k+1)*n/m).
fn reduce(h: &VecDeque<f32>, n: usize, cols: usize, peak: bool) -> Vec<f32> {
    let len = h.len();
    let m = cols.min(n).max(1);
    (0..m)
        .map(|k| {
            let lo = k * n / m;
            let hi = ((k + 1) * n / m).max(lo + 1);
            let (mut sum, mut cnt, mut mx) = (0.0f32, 0u32, f32::MIN);
            for w in lo..hi {
                let Some(i) = (len + w).checked_sub(n) else { continue };
                let v = h[i];
                sum += v;
                cnt += 1;
                mx = mx.max(v);
            }
            match (cnt, peak) {
                (0, _) => f32::NAN,
                (_, true) => mx,
                (c, false) => sum / c as f32,
            }
        })
        .collect()
}

fn ago_text(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0 => "now".into(),
        1..=59 => format!("{} s ago", s),
        60..=599 => format!("{} min {} s ago", s / 60, s % 60),
        _ => format!("{} min ago", s / 60),
    }
}

fn kind_text(e: &Env, k: Kind, v: f32) -> String {
    match k {
        Kind::Pct => format!("{:.0}%", v),
        Kind::Net => fmt::speed(v as f64, e.a.s.unit),
        Kind::Disk => fmt::rate(v as f64),
    }
}

/// Draws one chart (one or more traces sharing a scale), registers its hover
/// region and, while the mouse is over it, the marker and tooltip.
fn chart(c: &mut Canvas, e: &Env, out: &mut Out, r: Rect, kind: Kind, traces: &[Trace]) {
    if r.is_empty() || traces.is_empty() {
        return;
    }
    let rw = r.w() as usize;
    let peak = kind != Kind::Pct;
    let raws: Vec<Vec<f32>> = traces.iter().map(|t| reduce(&e.a.hist[t.m.idx()], e.n, rw, peak)).collect();
    let m = raws[0].len();
    let top = match kind {
        Kind::Pct => 100.0,
        Kind::Net => raws.iter().flatten().copied().filter(|v| !v.is_nan()).fold(64.0 * 1024.0, f32::max),
        Kind::Disk => raws.iter().flatten().copied().filter(|v| !v.is_nan()).fold(1024.0 * 1024.0, f32::max),
    };
    for (t, raw) in traces.iter().zip(&raws) {
        let norm: Vec<f32> = raw.iter().map(|v| if v.is_nan() { 0.0 } else { v / top }).collect();
        c.spark(r, &norm, t.col);
    }
    let hit = Rect { t: r.t - e.sc(4.0), b: r.b + e.sc(4.0), ..r };
    out.hits.push((hit, PopupHit::Chart));
    let Some((hx, hy)) = e.hover else { return };
    if !hit.contains(hx, hy) || raws.iter().all(|v| v.iter().all(|x| x.is_nan())) {
        return;
    }
    // Nearest point to the mouse, and the column the chart draws it in.
    let span = (rw as f32 - 1.0).max(1.0);
    let k = ((((hx - r.l) as f32) / span).clamp(0.0, 1.0) * (m as f32 - 1.0)).round() as usize;
    let k = k.min(m - 1);
    let colx = r.l + (k as f32 * span / (m as f32 - 1.0).max(1.0)).round() as i32;
    if raws.iter().all(|v| v[k].is_nan()) {
        return;
    }
    c.fill_rect(Rect { l: colx, t: r.t, r: colx + (e.scale.round() as i32).max(1), b: r.b }, e.base.fg.with_a(110));
    let d = e.sc(3.0);
    let mut parts = Vec::new();
    for (t, raw) in traces.iter().zip(&raws) {
        let v = raw[k];
        if v.is_nan() {
            continue;
        }
        let y = r.b - ((v / top).clamp(0.0, 1.0) * r.h() as f32).round() as i32;
        c.fill_round(Rect { l: colx - d - 1, t: y - d - 1, r: colx + d + 2, b: y + d + 2 }, d as f32 + 1.5, e.base.card);
        c.fill_round(Rect { l: colx - d, t: y - d, r: colx + d + 1, b: y + d + 1 }, d as f32 + 0.5, t.col);
        let val = kind_text(e, kind, v);
        parts.push(if t.tag.is_empty() { val } else { format!("{} {}", t.tag, val) });
    }
    let n = e.n as f32;
    let center = (k as f32 + 0.5) * n / m as f32 - 0.5;
    let secs = (n - 1.0 - center) * e.a.s.interval_ms as f32 / 1000.0;
    out.tip = Some(Tip { x: colx, r: hit, text: format!("{} \u{00B7} {}", parts.join("   "), ago_text(secs)) });
}

fn draw_tip(c: &mut Canvas, e: &Env, t: &Tip) {
    let font = e.f.small;
    let (tw, th) = c.measure(font, &t.text);
    let (px, py) = (e.sc(8.0), e.sc(5.0));
    let (w, h) = (tw + 2 * px, th + 2 * py);
    let edge = e.sc(4.0);
    let x = fit(t.x - w / 2, edge, c.w - w - edge);
    let mut y = t.r.t - h - e.sc(6.0);
    if y < edge {
        y = t.r.b + e.sc(6.0);
    }
    let r = Rect { l: x, t: y, r: x + w, b: y + h };
    c.fill_round(r, 6.0 * e.scale, e.base.chip);
    c.stroke_round(r, 6.0 * e.scale, e.base.dim.with_a(120));
    c.text(font, &t.text, x + px, y + py, e.base.fg);
}

/// Card with hairline border (brighter while hovered).
fn card(c: &mut Canvas, r: Rect, rad: f32, pal: &Pal, hot: bool) {
    c.fill_round(Rect { l: r.l + 1, t: r.t + 1, r: r.r - 1, b: r.b - 1 }, rad - 1.0, pal.card);
    c.stroke_round(r, rad, if hot { pal.border.mix(pal.dim, 0.55) } else { pal.border });
}

/// Pill-shaped chip right-aligned at `*x`, if it still fits right of `min_x`.
#[allow(clippy::too_many_arguments)]
fn chip(c: &mut Canvas, f: HFONT, text: &str, x: &mut i32, min_x: i32, y: i32, h: i32, gap: i32, pal: &Pal) {
    let (tw, th) = c.measure(f, text);
    let w = tw + h;
    if *x - w < min_x {
        return;
    }
    let r = Rect { l: *x - w, t: y, r: *x, b: y + h };
    c.fill_round(r, h as f32 / 2.0, pal.chip);
    c.text(f, text, r.l + h / 2, y + (h - th) / 2, pal.dim);
    *x -= w + gap;
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
    let mut out = Out { hits: std::mem::take(&mut a.popup_hits), tip: None };
    out.hits.clear();
    draw(a, &mut c, &f, scale, &mut out);
    a.popup_hits = out.hits;
    c.blit(dc, 0, 0);
    a.popup_cache = Some((f, c));
    unsafe { EndPaint(hwnd, &ps) };
}

fn draw(a: &App, c: &mut Canvas, f: &Fonts, scale: f32, out: &mut Out) {
    let sc = |v: f32| (v * scale).round() as i32;
    let base = palette(a.dark);
    let pal = if a.popup_acrylic { translucent(palette(a.dark), a.dark) } else { palette(a.dark) };
    let ms = a.s.interval_ms.max(1) as usize;
    let e = Env { a, f, pal, base, scale, hover: a.popup_hover, n: (a.popup_range * 1000 / ms).clamp(2, HIST) };
    let pal = &e.pal;
    let s = &a.snap;
    let pad = sc(PAD);
    let gap = sc(GAP);
    let right = c.w - pad;
    let rad = 10.0 * scale;
    c.clear(pal.bg);

    // ---- header: title + chips
    let mut y = pad;
    c.text(f.mid, "System", pad, y + sc(2.0), pal.fg);
    let min_x = pad + c.measure(f.mid, "System").0 + sc(14.0);
    let chip_h = sc(22.0);
    let mut x = right;
    {
        // Segmented time-range control.
        let widths: Vec<i32> = RANGES.iter().map(|(t, _)| c.measure(f.small, t).0 + sc(16.0)).collect();
        let total = widths.iter().sum::<i32>() + sc(4.0);
        let r = Rect { l: x - total, t: y, r: x, b: y + chip_h };
        c.fill_round(r, chip_h as f32 / 2.0, pal.chip);
        let mut sx = r.l + sc(2.0);
        for ((label, secs), w) in RANGES.iter().zip(&widths) {
            let seg = Rect { l: sx, t: y + sc(2.0), r: sx + w, b: y + chip_h - sc(2.0) };
            let on = a.popup_range == *secs;
            if on {
                c.fill_round(seg, seg.h() as f32 / 2.0, pal.fg.with_a(if a.dark { 46 } else { 34 }));
            }
            let (tw, th) = c.measure(f.small, label);
            let col = if on || e.hot(seg) { pal.fg } else { pal.dim };
            c.text(f.small, label, seg.l + (seg.w() - tw) / 2, seg.t + (seg.h() - th) / 2, col);
            out.hits.push((seg, PopupHit::Range(*secs)));
            sx += w;
        }
        x = r.l - sc(6.0);
    }
    chip(c, f.small, &format!("Up {}", fmt::duration(s.uptime_secs)), &mut x, min_x, y, chip_h, sc(6.0), pal);
    if let Some(b) = s.battery {
        let mut t = format!("Battery {}%", b.percent);
        if b.charging {
            t.push_str(" \u{2191}");
        } else if let (Some(sec), false) = (b.secs_left, b.on_ac) {
            t.push_str(&format!(" \u{00B7} {}", fmt::duration(sec as u64)));
        }
        chip(c, f.small, &t, &mut x, min_x, y, chip_h, sc(6.0), pal);
    }
    chip(c, f.small, &format!("{} processes", s.proc_count), &mut x, min_x, y, chip_h, sc(6.0), pal);
    y += sc(HEADER) + gap;

    // ---- ring gauges
    struct Gauge {
        label: &'static str,
        pct: f32,
        sub: String,
        sub2: String,
        sub2_col: Rgba,
        m: Metric,
        hit: PopupHit,
    }
    let free = s.ram_total.saturating_sub(s.ram_used);
    let mut gauges = vec![
        Gauge {
            label: "CPU",
            pct: s.cpu,
            sub: s.top_cpu.first().map_or(String::new(), |p| format!("{} {:.0}%", p.name, p.cpu)),
            sub2: format!("{} processes", s.proc_count),
            sub2_col: pal.dim,
            m: Metric::Cpu,
            hit: PopupHit::TaskMgr,
        },
        Gauge {
            label: "MEMORY",
            pct: s.ram_pct,
            sub: format!("{} / {}", fmt::bytes(s.ram_used), fmt::bytes(s.ram_total)),
            sub2: format!("{} free", fmt::bytes(free)),
            sub2_col: pal.dim,
            m: Metric::Ram,
            hit: PopupHit::TaskMgr,
        },
    ];
    if let Some(g) = s.gpu {
        let mut sub2 = String::new();
        if let Some(t) = s.gpu_temp {
            sub2 = format!("{:.0} \u{00B0}C", t);
            // Fan speed if the driver reports it, else board power.
            if let Some(rpm) = s.gpu_fan_rpm {
                sub2.push_str(&format!(" \u{00B7} {} RPM", rpm));
            } else if let Some(w) = s.gpu_power_w {
                sub2.push_str(&format!(" \u{00B7} {:.0} W", w));
            }
        }
        let sub2_col = a.pct_of(Metric::GpuTemp).map_or(pal.dim, |p| a.heat_text(pal.dim, p));
        gauges.push(Gauge {
            label: "GPU",
            pct: g,
            sub: s.vram_used.map_or(String::new(), |v| format!("{} VRAM", fmt::bytes(v))),
            sub2,
            sub2_col,
            m: Metric::Gpu,
            hit: PopupHit::TaskMgr,
        });
    } else if let Some(d) = s.disk_active {
        gauges.push(Gauge {
            label: "DISK",
            pct: d,
            sub: "active time".into(),
            sub2: String::new(),
            sub2_col: pal.dim,
            m: Metric::DiskActive,
            hit: PopupHit::ResMon,
        });
    }
    let n = gauges.len() as i32;
    let gw = (right - pad - gap * (n - 1)) / n;
    let gh = sc(RINGS);
    for (i, g) in gauges.iter().enumerate() {
        let x = pad + i as i32 * (gw + gap);
        let r = Rect { l: x, t: y, r: x + gw, b: y + gh };
        card(c, r, rad, pal, e.hot(r));
        out.hits.push((r, g.hit));
        let acc = accent(g.m, a.dark);
        let col = a.heat(acc, g.pct);
        let (cx, cy) = ((x + gw / 2) as f32, (y + sc(44.0)) as f32);
        let rr = 33.0 * scale;
        let th = 6.5 * scale;
        c.ring(cx, cy, rr, th, 1.0, pal.track);
        c.ring(cx, cy, rr, th, g.pct / 100.0, col);
        let v = format!("{:.0}%", g.pct);
        let (_, vh) = c.measure(f.mid, &v);
        c.text_center(f.mid, &v, cx as i32, cy as i32 - vh / 2, a.heat_text(pal.fg, g.pct));
        c.text_center(f.caps, g.label, cx as i32, y + sc(84.0), pal.dim);
        let sub = c.ellipsize(f.small, &g.sub, gw - sc(16.0));
        c.text_center(f.small, &sub, cx as i32, y + sc(98.0), pal.fg);
        let sub2 = c.ellipsize(f.small, &g.sub2, gw - sc(16.0));
        c.text_center(f.small, &sub2, cx as i32, y + sc(112.0), g.sub2_col);
        let cr = Rect { l: x + sc(12.0), t: y + sc(134.0), r: x + gw - sc(12.0), b: y + gh - sc(10.0) };
        chart(c, &e, out, cr, Kind::Pct, &[Trace { m: g.m, col: acc, tag: "" }]);
    }
    y += gh + gap;

    // ---- network
    let chart_w = sc(150.0);
    let nh = sc(NET);
    let r = Rect { l: pad, t: y, r: right, b: y + nh };
    card(c, r, rad, pal, e.hot(r));
    out.hits.push((r, PopupHit::NetSettings));
    let ix = pad + sc(14.0);
    let adapter = if a.s.adapter.is_empty() { "ALL ADAPTERS".to_string() } else { a.s.adapter.to_uppercase() };
    c.text(f.caps, &format!("NETWORK  \u{00B7}  {}", adapter), ix, y + sc(10.0), pal.dim);
    let down_c = accent(Metric::Down, a.dark);
    let up_c = accent(Metric::Up, a.dark);
    c.text(f.big, &format!("\u{25BC} {}", fmt::speed(s.net_down, a.s.unit)), ix, y + sc(26.0), down_c);
    c.text(f.normal, &format!("\u{25B2} {}", fmt::speed(s.net_up, a.s.unit)), ix, y + sc(56.0), up_c);
    {
        let cr = Rect { l: right - chart_w - sc(12.0), t: y + sc(12.0), r: right - sc(12.0), b: y + nh - sc(12.0) };
        let tr = [
            Trace { m: Metric::Down, col: down_c, tag: "\u{25BC}" },
            Trace { m: Metric::Up, col: up_c, tag: "\u{25B2}" },
        ];
        chart(c, &e, out, cr, Kind::Net, &tr);
    }
    y += nh + gap;

    // ---- disk
    let space = has_space(a);
    let dh = sc(DISK + if space { DISK_SPACE } else { 0.0 });
    let r = Rect { l: pad, t: y, r: right, b: y + dh };
    card(c, r, rad, pal, e.hot(r));
    out.hits.push((r, PopupHit::ResMon));
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
    let rd_c = accent(Metric::DiskRead, a.dark);
    let mut wr_c = accent(Metric::DiskWrite, a.dark);
    if wr_c == rd_c {
        wr_c = rd_c.mix(pal.fg, 0.5);
    }
    let cr = Rect { l: right - chart_w - sc(12.0), t: y + sc(12.0), r: right - sc(12.0), b: y + sc(DISK) - sc(12.0) };
    let tr = [Trace { m: Metric::DiskRead, col: rd_c, tag: "R" }, Trace { m: Metric::DiskWrite, col: wr_c, tag: "W" }];
    chart(c, &e, out, cr, Kind::Disk, &tr);
    if space {
        let drive = std::env::var("SystemDrive").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "C:".into());
        let pct = s.disk_used_pct.unwrap_or(0.0);
        let text = format!(
            "{}  {:.0}% used  \u{00B7}  {} free of {}",
            drive,
            pct,
            fmt::bytes(s.disk_total.saturating_sub(s.disk_used)),
            fmt::bytes(s.disk_total)
        );
        let text = c.ellipsize(f.small, &text, right - sc(14.0) - ix);
        c.text(f.small, &text, ix, y + sc(DISK) - sc(8.0), pal.fg);
        let bar = Rect { l: ix, t: y + dh - sc(14.0), r: right - sc(14.0), b: y + dh - sc(10.0) };
        c.fill_round(bar, 2.0 * scale, pal.track);
        let fw = (bar.w() as f32 * (pct / 100.0).clamp(0.0, 1.0)) as i32;
        if fw > 0 {
            let col = a.heat(accent(Metric::DiskFree, a.dark), pct);
            c.fill_round(Rect { r: bar.l + fw.max(bar.h()), ..bar }, 2.0 * scale, col);
        }
    }
    y += dh + gap;

    // ---- data usage
    let cap = a.cycle_usage();
    let uh = sc(USAGE) + if cap.is_some() { sc(CAP) } else { 0 };
    let r = Rect { l: pad, t: y, r: right, b: y + uh };
    card(c, r, rad, pal, e.hot(r));
    out.hits.push((r, PopupHit::UsageReport));
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
    card(c, Rect { l: pad, t: y, r: right, b: y + ph }, rad, pal, false);
    let inner_l = ix;
    let inner_r = right - sc(14.0);
    let col_w = (inner_r - inner_l) / 3;
    type Row = (u32, String, String);
    let cols: [(&str, Vec<Row>); 3] = [
        ("TOP CPU", s.top_cpu.iter().map(|p| (p.pid, p.name.clone(), format!("{:.1}%", p.cpu))).collect()),
        ("TOP MEMORY", s.top_mem.iter().map(|p| (p.pid, p.name.clone(), fmt::bytes(p.mem))).collect()),
        ("TOP DISK I/O", s.top_io.iter().map(|p| (p.pid, p.name.clone(), fmt::rate(p.io_bps))).collect()),
    ];
    for (i, (title, list)) in cols.iter().enumerate() {
        let x = inner_l + i as i32 * col_w;
        let xr = x + col_w - sc(12.0);
        c.text(f.caps, title, x, y + sc(10.0), pal.dim);
        if list.is_empty() {
            c.text(f.small, "\u{2013}", x, y + sc(28.0), pal.dim);
        }
        for (j, (pid, name, val)) in list.iter().take(5).enumerate() {
            let ry = y + sc(28.0) + j as i32 * sc(PROC_ROW);
            let row = Rect { l: x - sc(5.0), t: ry - sc(1.0), r: xr + sc(5.0), b: ry + sc(PROC_ROW) - sc(1.0) };
            out.hits.push((row, PopupHit::Proc(*pid)));
            if e.hot(row) {
                c.fill_round(row, 4.0 * scale, pal.chip);
            }
            let vw = c.measure(f.small, val).0;
            let nm = c.ellipsize(f.small, name, xr - x - vw - sc(6.0));
            c.text(f.small, &nm, x, ry, pal.fg);
            c.text_right(f.small, val, xr, ry, pal.dim);
        }
    }
    y += ph + gap;

    // ---- footer
    let hint = if a.popup_pinned {
        "Click to close  \u{00B7}  Right-click a process for actions"
    } else {
        "Click: pin  \u{00B7}  Double-click: Task Manager  \u{00B7}  Right-click: menu"
    };
    c.text_center(f.small, hint, c.w / 2, y + sc(2.0), pal.dim);

    if let Some(t) = out.tip.take() {
        draw_tip(c, &e, &t);
    }
}

// ================================================================ input

fn hit_index(a: &App, x: i32, y: i32) -> Option<usize> {
    a.popup_hits.iter().rposition(|(r, _)| r.contains(x, y))
}

/// Topmost region at (x, y); with `clickable`, charts let clicks fall through
/// to their card.
fn hit_at(a: &App, x: i32, y: i32, clickable: bool) -> Option<PopupHit> {
    a.popup_hits
        .iter()
        .rev()
        .find(|(r, h)| r.contains(x, y) && !(clickable && *h == PopupHit::Chart))
        .map(|(_, h)| *h)
}

fn on_mouse_move(hwnd: HWND, l: LPARAM) {
    let Some(a) = crate::app::app() else { return };
    if !a.popup_tracking {
        let mut t = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        a.popup_tracking = unsafe { TrackMouseEvent(&mut t) } != 0;
    }
    let (x, y) = (get_x(l), get_y(l));
    // Repaint only when the hovered region (or the position inside a chart) changed.
    let key = |a: &App, p: Option<(i32, i32)>| {
        p.map(|(x, y)| {
            let i = hit_index(a, x, y);
            let in_chart = i.is_some_and(|i| a.popup_hits[i].1 == PopupHit::Chart);
            (i, if in_chart { x } else { 0 })
        })
    };
    let old = key(a, a.popup_hover);
    a.popup_hover = Some((x, y));
    if key(a, a.popup_hover) != old {
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
    }
}

fn on_mouse_leave(hwnd: HWND) {
    if let Some(a) = crate::app::app() {
        a.popup_tracking = false;
        if a.popup_hover.take().is_some() {
            unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
        }
    }
}

fn on_set_cursor(hwnd: HWND) -> bool {
    let Some(a) = crate::app::app() else { return false };
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
        ScreenToClient(hwnd, &mut pt);
    }
    let hand = matches!(
        hit_at(a, pt.x, pt.y, true),
        Some(PopupHit::Range(_) | PopupHit::TaskMgr | PopupHit::ResMon | PopupHit::NetSettings | PopupHit::UsageReport)
    );
    unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), if hand { IDC_HAND } else { IDC_ARROW })) };
    true
}

fn on_click(hwnd: HWND, l: LPARAM) {
    let Some(a) = crate::app::app() else { return };
    match hit_at(a, get_x(l), get_y(l), true) {
        Some(PopupHit::Range(secs)) => {
            a.popup_range = secs;
            unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
        }
        Some(PopupHit::TaskMgr) => {
            hide(a);
            shell_open("taskmgr.exe", None);
        }
        Some(PopupHit::ResMon) => {
            hide(a);
            shell_open("resmon.exe", None);
        }
        Some(PopupHit::NetSettings) => {
            hide(a);
            shell_open("ms-settings:datausage", None);
        }
        Some(PopupHit::UsageReport) => {
            hide(a);
            crate::report::open(a);
        }
        _ => hide(a),
    }
}

fn on_right_click(l: LPARAM) {
    let row = crate::app::app().and_then(|a| match hit_at(a, get_x(l), get_y(l), true) {
        Some(PopupHit::Proc(pid)) => Some(pid),
        _ => None,
    });
    if let Some(pid) = row {
        proc_menu(pid);
        return;
    }
    if let Some(a) = crate::app::app() {
        hide(a);
        let owner = a.hwnd;
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            PostMessageW(owner, crate::tray::WM_TRAY, ((pt.y as u16 as usize) << 16) | pt.x as u16 as usize, WM_CONTEXTMENU as isize);
        }
    }
}

// ================================================================ process actions

const ID_END: usize = 1;
const ID_LOCATION: usize = 2;

fn proc_name(a: &App, pid: u32) -> String {
    let s = &a.snap;
    s.top_cpu
        .iter()
        .chain(&s.top_mem)
        .chain(&s.top_io)
        .find(|p| p.pid == pid)
        .map_or_else(|| format!("PID {}", pid), |p| p.name.clone())
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut n);
        CloseHandle(h);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]))
    }
}

fn message(owner: HWND, text: &str, title: &str, flags: u32) -> i32 {
    let (t, c) = (wide(text), wide(title));
    unsafe { MessageBoxW(owner, t.as_ptr(), c.as_ptr(), flags | MB_TOPMOST | MB_SETFOREGROUND) }
}

/// Right-click menu for a process row. Runs nested message loops (menu,
/// message boxes): no `&mut App` is held across them.
fn proc_menu(pid: u32) {
    let (owner, name) = match crate::app::app() {
        Some(a) => (a.hwnd, proc_name(a, pid)),
        None => return,
    };
    let cmd = unsafe {
        let hm = CreatePopupMenu();
        let title = wide(&format!("{} ({})", name.replace('&', "&&"), pid));
        AppendMenuW(hm, MF_STRING | MF_GRAYED, 0, title.as_ptr());
        AppendMenuW(hm, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(hm, MF_STRING, ID_END, wide("End task").as_ptr());
        AppendMenuW(hm, MF_STRING, ID_LOCATION, wide("Open file location").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        if let Some(a) = crate::app::app() {
            a.menu_open = true;
        }
        SetForegroundWindow(owner);
        let cmd = TrackPopupMenuEx(hm, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, owner, std::ptr::null());
        DestroyMenu(hm);
        PostMessageW(owner, WM_NULL, 0, 0);
        if let Some(a) = crate::app::app() {
            a.menu_open = false;
        }
        cmd as usize
    };
    match cmd {
        ID_END => end_task(owner, pid, &name),
        ID_LOCATION => match image_path(pid) {
            Some(p) => shell_open("explorer.exe", Some(&format!("/select,\"{}\"", p))),
            None => {
                message(
                    owner,
                    &format!("Couldn't find the location of \"{}\".\n\nThe process may have exited or may be protected.", name),
                    "Open file location",
                    MB_OK | MB_ICONINFORMATION,
                );
            }
        },
        _ => {}
    }
}

fn end_task(owner: HWND, pid: u32, name: &str) {
    // The list may be a moment stale: make sure the PID still is that program.
    if let Some(p) = image_path(pid) {
        let stem = std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if !stem.eq_ignore_ascii_case(name) {
            message(owner, &format!("\"{}\" is no longer running.", name), "End task", MB_OK | MB_ICONINFORMATION);
            return;
        }
    }
    let ask = format!("End \"{}\" (PID {})?\n\nAny unsaved data in this program will be lost.", name, pid);
    if message(owner, &ask, "End task", MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != IDYES {
        return;
    }
    let ok = unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        !h.is_null() && {
            let r = TerminateProcess(h, 1);
            CloseHandle(h);
            r != 0
        }
    };
    if !ok {
        message(
            owner,
            &format!("Couldn't end \"{}\".\n\nIt is protected, or ending it requires administrator rights.", name),
            "End task",
            MB_OK | MB_ICONERROR,
        );
    }
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
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, l);
            0
        }
        WM_MOUSELEAVE => {
            on_mouse_leave(hwnd);
            0
        }
        WM_SETCURSOR if loword(l as usize) as u32 == HTCLIENT => {
            if on_set_cursor(hwnd) {
                1
            } else {
                DefWindowProcW(hwnd, msg, w, l)
            }
        }
        WM_LBUTTONUP => {
            on_click(hwnd, l);
            0
        }
        WM_RBUTTONUP => {
            on_right_click(l);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
