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
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
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

/// Light theme base colours (eggshell), shared by every window and the widget.
pub const EGG_BG: Rgba = Rgba::rgb(0xEF, 0xEA, 0xDF);
pub const EGG_CARD: Rgba = Rgba::rgb(0xF8, 0xF4, 0xEA);
pub const EGG_FG: Rgba = Rgba::rgb(0x2A, 0x26, 0x20);
pub const EGG_DIM: Rgba = Rgba::rgb(0x67, 0x5F, 0x53);

/// See docs/ui-reference: near-black base, raised cards, hairline borders.
/// Light is eggshell rather than white: easier on the eyes.
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
            // Eggshell: warm and soft instead of bright white.
            bg: EGG_BG,
            card: EGG_CARD,
            chip: Rgba::rgb(0xE6, 0xE0, 0xD2),
            border: Rgba::rgb(0xDC, 0xD5, 0xC6),
            track: Rgba::rgb(0xE0, 0xD9, 0xCA),
            fg: EGG_FG,
            dim: EGG_DIM,
            line: Rgba::rgb(0xDC, 0xD5, 0xC6),
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
    // Windows draws the Mica/Acrylic *system backdrop* only for the active
    // window; this panel never takes focus (so you keep yours), which made it
    // fall back to flat grey. The accent-policy acrylic blur — what the
    // taskbar and Start use — stays frosted while inactive. Win10 1803+.
    if build_number() < 17134 {
        return false;
    }
    unsafe { acrylic_accent(hwnd, dark) }
}

/// Acrylic blur behind the whole window via SetWindowCompositionAttribute
/// (user32, resolved at run time). Pixels with alpha 0 show the blur.
unsafe fn acrylic_accent(hwnd: HWND, dark: bool) -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        /// 0xAABBGGRR tint.
        color: u32,
        animation: u32,
    }
    #[repr(C)]
    struct CompositionData {
        attribute: u32,
        data: *mut core::ffi::c_void,
        size: usize,
    }
    type SetWca = unsafe extern "system" fn(HWND, *mut CompositionData) -> i32;
    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
    let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
    if user32.is_null() {
        return false;
    }
    let Some(f) = GetProcAddress(user32, b"SetWindowCompositionAttribute\0".as_ptr()) else { return false };
    let set: SetWca = std::mem::transmute(f);
    // Same tint strength as Windows flyouts: dark #202020 / light #F3F3F3 at ~70%.
    // Light: eggshell #EFEADF, a little stronger so the warm tone carries.
    let tint = if dark { 0xB320_2020u32 } else { 0xC8DF_EAEFu32 };
    let mut policy = AccentPolicy { state: ACCENT_ENABLE_ACRYLICBLURBEHIND, flags: 2, color: tint, animation: 0 };
    let mut data = CompositionData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut policy as *mut AccentPolicy as *mut _,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    set(hwnd, &mut data) != 0
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
    /// Top-process list tab: 0 CPU, 1 memory, 2 disk I/O.
    TopTab(u8),
    /// Panel title: opens the Monitor page.
    Monitor,
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
/// Header row: the logo square, two lines of name beside it, chips on the right.
const HEADER: f32 = 32.0;
const RINGS: f32 = 146.0;
/// Network and disk cards: dial, two values, mirror chart.
const DIAL_CARD: f32 = 92.0;
/// Extra disk card height for the drive-space cells.
const DISK_SPACE: f32 = 20.0;
const USAGE: f32 = 84.0;
const CAP: f32 = 24.0;
/// Days of history in the data usage bars.
const USAGE_DAYS: usize = 14;
const PROC_ROW: f32 = 22.0;
const PROCS: f32 = 36.0 + 5.0 * PROC_ROW + 4.0;
/// Dials sweep 240°, open at the bottom (radians clockwise from 3 o'clock).
const DIAL_A0: f32 = std::f32::consts::PI * 0.75;
const DIAL_SPAN: f32 = std::f32::consts::PI * 1.5;

fn has_space(a: &App) -> bool {
    a.snap.disk_used_pct.is_some() && a.snap.disk_total > 0
}

fn size(a: &App) -> (i32, i32) {
    let mut h = PAD + HEADER + GAP + RINGS + GAP + DIAL_CARD + GAP + DIAL_CARD + GAP + USAGE + GAP + PROCS + PAD;
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
    // Half gap on the taskbar side so the popup sits close to the strip it opened from.
    let tg = sc(a, 5.0);
    let center = POINT { x: (anchor.l + anchor.r) / 2, y: (anchor.t + anchor.b) / 2 };
    let work = crate::taskbar::monitor_info(unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST) })
        .map_or(tb.work, |m| m.1);
    let above = (anchor.r - w, anchor.t - h - tg);
    let below = (anchor.r - w, anchor.b + tg);
    let left = (anchor.l - w - tg, anchor.b - h);
    let right = (anchor.r + tg, anchor.b - h);
    // Preferred side first (away from the taskbar), then any side that fits.
    let order = match (a.s.mode, tb.edge) {
        (crate::settings::Mode::Float, _) | (_, Edge::Bottom) => [above, below, left, right],
        (_, Edge::Top) => [below, above, left, right],
        (_, Edge::Left) => [right, above, below, left],
        (_, Edge::Right) => [left, above, below, right],
    };
    // Screen edges keep the full margin; the edge the taskbar sits on gets the half one.
    let docked = a.s.mode != crate::settings::Mode::Float;
    let m = |e: Edge| if docked && tb.edge == e { tg } else { g };
    let clamp = |(x, y): (i32, i32)| {
        (
            fit(x, work.l + m(Edge::Left), work.r - w - m(Edge::Right)),
            fit(y, work.t + m(Edge::Top), work.b - h - m(Edge::Bottom)),
        )
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
    a.popup_dark = a.dark;
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
/// With `mirror`, the first trace grows up from a centre line and the second
/// hangs below it (download / upload, read / write).
#[allow(clippy::too_many_arguments)]
fn chart(c: &mut Canvas, e: &Env, out: &mut Out, r: Rect, kind: Kind, traces: &[Trace], mirror: bool) {
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
    // Each trace's area and direction (down = hangs from its top edge).
    let mid = r.t + r.h() * 56 / 100;
    let area = |i: usize| -> (Rect, bool) {
        match (mirror, i) {
            (false, _) => (r, false),
            (true, 0) => (Rect { b: mid, ..r }, false),
            (true, _) => (Rect { t: mid + 1, ..r }, true),
        }
    };
    if mirror {
        c.fill_rect(Rect { t: mid, b: mid + 1, ..r }, e.pal.track);
    }
    for (i, (t, raw)) in traces.iter().zip(&raws).enumerate() {
        let norm: Vec<f32> = raw.iter().map(|v| if v.is_nan() { 0.0 } else { v / top }).collect();
        let (ar, down) = area(i);
        c.spark_dir(ar, &norm, t.col, down);
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
    for (i, (t, raw)) in traces.iter().zip(&raws).enumerate() {
        let v = raw[k];
        if v.is_nan() {
            continue;
        }
        let (ar, down) = area(i);
        let off = ((v / top).clamp(0.0, 1.0) * ar.h() as f32).round() as i32;
        let y = if down { ar.t + off } else { ar.b - off };
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
    // Theme switched while open: restyle the backdrop (acrylic tint, frame).
    if a.popup_dark != a.dark {
        a.popup_acrylic = style_popup(hwnd, a.dark);
        a.popup_dark = a.dark;
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

/// Avatar colour for a process name: stable hue from the name, soft pastel.
pub(crate) fn avatar_col(name: &str) -> Rgba {
    let h = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    // hsl(h, 55%, 68%)
    let (s, l) = (0.55f32, 0.68f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h as f32 / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let u = |v: f32| ((v + m) * 255.0).round() as u8;
    Rgba::rgb(u(r), u(g), u(b))
}

/// 240° dial track plus the value arc (`frac` 0..1).
#[allow(clippy::too_many_arguments)]
pub(crate) fn dial(c: &mut Canvas, cx: f32, cy: f32, r: f32, thick: f32, frac: f32, col: Rgba, track: Rgba) {
    c.stroke_arc(cx, cy, r, thick, DIAL_A0, DIAL_A0 + DIAL_SPAN, track);
    let f = frac.clamp(0.0, 1.0);
    if f > 0.004 {
        c.stroke_arc(cx, cy, r, thick, DIAL_A0, DIAL_A0 + DIAL_SPAN * f, col);
    }
}

/// Network rate on a log scale, 1 KB/s (empty) to 1 GB/s (full).
pub(crate) fn log_frac(bytes_per_s: f64) -> f32 {
    ((bytes_per_s.max(1024.0) / 1024.0).log10() / 6.0).clamp(0.0, 1.0) as f32
}

/// "64.2 KB/s" → ("64.2", "KB/s").
fn split_unit(s: &str) -> (String, String) {
    match s.rsplit_once(' ') {
        Some((n, u)) => (n.to_string(), u.to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// One "▼ 64.2 KB/s" line of a dial card: marker, value, small unit.
#[allow(clippy::too_many_arguments)]
fn value_row(c: &mut Canvas, f: &Fonts, x: i32, y: i32, mark: &str, text: &str, big: bool, col: Rgba, dim: Rgba, scale: f32) {
    let sc = |v: f32| (v * scale).round() as i32;
    let (n, u) = split_unit(text);
    let font = if big { f.mid } else { f.bold };
    let (_, nh) = c.measure(font, &n);
    let (_, mh) = c.measure(f.caps, mark);
    c.text(f.caps, mark, x, y + nh - mh - sc(1.0), col);
    let nx = x + sc(12.0).max(c.measure(f.caps, mark).0 + sc(4.0));
    let nw = c.measure(font, &n).0;
    c.text(font, &n, nx, y, col);
    let (_, uh) = c.measure(f.small, &u);
    c.text(f.small, &u, nx + nw + sc(4.0), y + nh - uh - sc(1.0), dim);
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

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

    // ---- header: logo + name, range control, uptime (clock icon), battery
    // Two lines beside the logo: "Resource Monitor" over "by Emulsion Labs".
    let chip_h = sc(22.0);
    let logo = sc(HEADER);
    crate::logo::draw(c, pad, pad, logo, a.dark, None);
    let name_x = pad + logo + sc(9.0);
    let (name_w, name_h) = c.measure(f.bold, "Resource Monitor");
    let (by_w, by_h) = c.measure(f.small, "by Emulsion Labs");
    let top = pad + (logo - name_h - by_h) / 2;
    c.text(f.bold, "Resource Monitor", name_x, top, pal.fg);
    c.text(f.small, "by Emulsion Labs", name_x, top + name_h, pal.dim);
    let min_x = name_x + name_w.max(by_w) + sc(14.0);
    // A click on the logo and name opens the Monitor page.
    out.hits.push((Rect { l: pad, t: pad, r: min_x - sc(4.0), b: pad + logo }, PopupHit::Monitor));
    // The chips sit on the logo's centre line.
    let mut y = pad + (logo - chip_h) / 2;
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
    {
        // Uptime chip: clock icon + "2d 3h".
        let t = fmt::duration(s.uptime_secs);
        let (tw, th) = c.measure(f.small, &t);
        let icon = sc(12.0);
        let w = tw + icon + sc(5.0) + chip_h;
        if x - w >= min_x {
            let r = Rect { l: x - w, t: y, r: x, b: y + chip_h };
            c.fill_round(r, chip_h as f32 / 2.0, pal.chip);
            let (icx, icy) = ((r.l + chip_h / 2) as f32 + icon as f32 / 2.0, (y + chip_h / 2) as f32);
            let ir = 4.8 * scale;
            c.stroke_arc(icx, icy, ir, 1.3 * scale, 0.0, std::f32::consts::TAU, pal.dim);
            let lw = (1.3 * scale).round().max(1.0) as i32;
            let (hx, hy) = (icx.round() as i32, icy.round() as i32);
            c.fill_rect(Rect { l: hx - lw / 2, t: hy - sc(3.0), r: hx - lw / 2 + lw, b: hy + 1 }, pal.dim);
            c.fill_rect(Rect { l: hx - lw / 2, t: hy - lw / 2, r: hx + sc(2.5), b: hy - lw / 2 + lw }, pal.dim);
            c.text(f.small, &t, r.l + chip_h / 2 + icon + sc(5.0), y + (chip_h - th) / 2, pal.dim);
            x -= w + sc(6.0);
        }
    }
    if let Some(b) = s.battery {
        let mut t = format!("Battery {}%", b.percent);
        if b.charging {
            t.push_str(" \u{2191}");
        } else if let (Some(sec), false) = (b.secs_left, b.on_ac) {
            t.push_str(&format!(" \u{00B7} {}", fmt::duration(sec as u64)));
        }
        chip(c, f.small, &t, &mut x, min_x, y, chip_h, sc(6.0), pal);
    }
    y = pad + sc(HEADER) + gap;

    // ---- ring gauges: ring, label, one short line, sparkline
    struct Gauge {
        label: &'static str,
        pct: f32,
        sub: String,
        sub_col: Rgba,
        m: Metric,
        hit: PopupHit,
    }
    let mut gauges = vec![
        Gauge {
            label: "CPU",
            pct: s.cpu,
            sub: s.top_cpu.first().map_or(String::new(), |p| format!("{} {:.0}%", p.name, p.cpu)),
            sub_col: pal.dim,
            m: Metric::Cpu,
            hit: PopupHit::TaskMgr,
        },
        Gauge {
            label: "MEMORY",
            pct: s.ram_pct,
            sub: format!("{} / {}", fmt::bytes(s.ram_used), fmt::bytes(s.ram_total)),
            sub_col: pal.dim,
            m: Metric::Ram,
            hit: PopupHit::TaskMgr,
        },
    ];
    if let Some(g) = s.gpu {
        // Temperature if known (tinted when hot), else VRAM in use.
        let (sub, sub_col) = match s.gpu_temp {
            Some(t) => (
                format!("{:.0} \u{00B0}C", t),
                a.pct_of(Metric::GpuTemp).map_or(pal.dim, |p| a.heat_text(pal.dim, p)),
            ),
            None => (s.vram_used.map_or(String::new(), |v| format!("{} VRAM", fmt::bytes(v))), pal.dim),
        };
        gauges.push(Gauge { label: "GPU", pct: g, sub, sub_col, m: Metric::Gpu, hit: PopupHit::TaskMgr });
    } else if let Some(d) = s.disk_active {
        gauges.push(Gauge {
            label: "DISK",
            pct: d,
            sub: "active time".into(),
            sub_col: pal.dim,
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
        let rr = 34.0 * scale;
        let th = 6.5 * scale;
        c.ring(cx, cy, rr, th, 1.0, pal.track);
        c.ring(cx, cy, rr, th, g.pct / 100.0, col);
        let v = format!("{:.0}%", g.pct);
        let (_, vh) = c.measure(f.mid, &v);
        c.text_center(f.mid, &v, cx as i32, cy as i32 - vh / 2, a.heat_text(pal.fg, g.pct));
        c.text_center(f.caps, g.label, cx as i32, y + sc(85.0), pal.dim);
        let sub = c.ellipsize(f.small, &g.sub, gw - sc(16.0));
        c.text_center(f.small, &sub, cx as i32, y + sc(99.0), g.sub_col);
        let cr = Rect { l: x + sc(12.0), t: y + sc(118.0), r: x + gw - sc(12.0), b: y + gh - sc(9.0) };
        chart(c, &e, out, cr, Kind::Pct, &[Trace { m: g.m, col: acc, tag: "" }], false);
    }
    y += gh + gap;

    // Shared geometry of the two dial cards.
    let dial_d = sc(72.0);
    let vals_x = pad + sc(10.0) + dial_d + sc(8.0);
    let chart_l = vals_x + sc(104.0) + sc(8.0);
    let ix = pad + sc(14.0);

    // ---- network: dial (log scale), values, mirror chart
    let nh = sc(DIAL_CARD);
    let r = Rect { l: pad, t: y, r: right, b: y + nh };
    card(c, r, rad, pal, e.hot(r));
    out.hits.push((r, PopupHit::NetSettings));
    let down_c = accent(Metric::Down, a.dark);
    let up_c = accent(Metric::Up, a.dark);
    {
        let (cx, cy) = ((pad + sc(10.0)) as f32 + dial_d as f32 / 2.0, (y + nh / 2) as f32);
        dial(c, cx, cy, 32.0 * scale, 5.5 * scale, log_frac(s.net_down), down_c, pal.track);
        dial(c, cx, cy, 25.5 * scale, 4.0 * scale, log_frac(s.net_up), up_c, pal.track);
        // Decade ticks (KB, MB, GB) between the two arcs.
        for t in [0.0f32, 0.5, 1.0] {
            let ang = DIAL_A0 + DIAL_SPAN * t;
            let (tx, ty) = (cx + 21.0 * scale * ang.cos(), cy + 21.0 * scale * ang.sin());
            c.stroke_arc(tx, ty, 0.6 * scale, 1.2 * scale, 0.0, std::f32::consts::TAU, pal.dim);
        }
        let (dw, dh) = c.measure(f.small, "\u{25BC}");
        let uw = c.measure(f.small, "\u{25B2}").0;
        let gx = cx as i32 - (dw + uw + sc(1.0)) / 2;
        c.text(f.small, "\u{25BC}", gx, cy as i32 - dh / 2 - sc(3.0), down_c);
        c.text(f.small, "\u{25B2}", gx + dw + sc(1.0), cy as i32 - dh / 2 - sc(3.0), up_c);
        c.text_center(f.caps, "NET", cx as i32, cy as i32 + sc(20.0), pal.dim);
    }
    let vy = y + nh / 2 - sc(19.0);
    value_row(c, f, vals_x, vy, "\u{25BC}", &fmt::speed(s.net_down, a.s.unit), true, down_c, pal.dim, scale);
    value_row(c, f, vals_x, vy + sc(22.0), "\u{25B2}", &fmt::speed(s.net_up, a.s.unit), false, up_c, pal.dim, scale);
    {
        let cr = Rect { l: chart_l, t: y + sc(14.0), r: right - sc(12.0), b: y + nh - sc(14.0) };
        let tr = [
            Trace { m: Metric::Down, col: down_c, tag: "\u{25BC}" },
            Trace { m: Metric::Up, col: up_c, tag: "\u{25B2}" },
        ];
        chart(c, &e, out, cr, Kind::Net, &tr, true);
    }
    y += nh + gap;

    // ---- disk: active-time dial, R/W values, mirror chart, drive cells
    let space = has_space(a);
    let dh = sc(DIAL_CARD + if space { DISK_SPACE } else { 0.0 });
    let r = Rect { l: pad, t: y, r: right, b: y + dh };
    card(c, r, rad, pal, e.hot(r));
    out.hits.push((r, PopupHit::ResMon));
    let rd_c = accent(Metric::DiskRead, a.dark);
    let mut wr_c = accent(Metric::DiskWrite, a.dark);
    if wr_c == rd_c {
        wr_c = rd_c.mix(pal.fg, 0.5);
    }
    let nh = sc(DIAL_CARD);
    {
        let (cx, cy) = ((pad + sc(10.0)) as f32 + dial_d as f32 / 2.0, (y + nh / 2) as f32);
        let act = s.disk_active.unwrap_or(0.0);
        let act_c = a.heat(accent(Metric::DiskActive, a.dark), act);
        dial(c, cx, cy, 30.0 * scale, 6.0 * scale, act / 100.0, act_c, pal.track);
        let v = s.disk_active.map_or("\u{2013}".to_string(), |v| format!("{:.0}%", v));
        let (_, vh) = c.measure(f.bold, &v);
        c.text_center(f.bold, &v, cx as i32, cy as i32 - vh / 2 - sc(2.0), a.heat_text(pal.fg, act));
        c.text_center(f.caps, "DISK", cx as i32, cy as i32 + sc(20.0), pal.dim);
    }
    let vy = y + nh / 2 - sc(19.0);
    value_row(c, f, vals_x, vy, "R", &fmt::rate(s.disk_read), true, rd_c, pal.dim, scale);
    value_row(c, f, vals_x, vy + sc(22.0), "W", &fmt::rate(s.disk_write), false, wr_c, pal.dim, scale);
    {
        let cr = Rect { l: chart_l, t: y + sc(14.0), r: right - sc(12.0), b: y + nh - sc(14.0) };
        let tr = [Trace { m: Metric::DiskRead, col: rd_c, tag: "R" }, Trace { m: Metric::DiskWrite, col: wr_c, tag: "W" }];
        chart(c, &e, out, cr, Kind::Disk, &tr, true);
    }
    if space {
        let drive = std::env::var("SystemDrive").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "C:".into());
        let pct = s.disk_used_pct.unwrap_or(0.0);
        let free = format!("{} free", fmt::bytes(s.disk_total.saturating_sub(s.disk_used)));
        let ry = y + nh - sc(6.0);
        let (lw, lh) = c.measure(f.bold, &drive);
        c.text(f.bold, &drive, ix, ry, pal.fg);
        let fw = c.measure(f.small, &free).0;
        c.text_right(f.small, &free, right - sc(12.0), ry + sc(1.0), pal.dim);
        // 24 cells, filled for the used share, heat-coloured.
        let (bl, br) = (ix + lw + sc(8.0), right - sc(12.0) - fw - sc(8.0));
        let cell_gap = sc(2.0);
        let cells = 24;
        let cw = (br - bl - cell_gap * (cells - 1)) as f32 / cells as f32;
        if cw >= 1.0 {
            let filled = ((pct / 100.0).clamp(0.0, 1.0) * cells as f32).round() as i32;
            let col = a.heat(accent(Metric::DiskFree, a.dark), pct);
            let (ct, cb) = (ry + (lh - sc(9.0)) / 2, ry + (lh - sc(9.0)) / 2 + sc(9.0));
            for i in 0..cells {
                let l = bl + (i as f32 * (cw + cell_gap as f32)).round() as i32;
                let rr = bl + (i as f32 * (cw + cell_gap as f32) + cw).round() as i32;
                c.fill_round(Rect { l, t: ct, r: rr, b: cb }, 2.0 * scale, if i < filled { col } else { pal.track });
            }
        }
    }
    y += dh + gap;

    // ---- data usage: today + month figures, 14-day stacked bars
    let cap = a.cycle_usage();
    let uh = sc(USAGE) + if cap.is_some() { sc(CAP) } else { 0 };
    let r = Rect { l: pad, t: y, r: right, b: y + uh };
    card(c, r, rad, pal, e.hot(r));
    out.hits.push((r, PopupHit::UsageReport));
    let now = usage::today();
    let (yy, mm, _) = usage::ymd(now);
    let today = a.usage.get(now);
    let month = a.usage.month(yy, mm);
    c.text(f.caps, "TODAY", ix, y + sc(12.0), pal.dim);
    c.text(f.big, &fmt::bytes(today.total()), ix, y + sc(24.0), pal.fg);
    let mlw = c.measure(f.caps, "MONTH").0;
    let (_, bh) = c.measure(f.bold, "0");
    let (_, chh) = c.measure(f.caps, "M");
    c.text(f.caps, "MONTH", ix, y + sc(58.0) + bh - chh - sc(1.0), pal.dim);
    c.text(f.bold, &fmt::bytes(month.total()), ix + mlw + sc(6.0), y + sc(58.0), pal.fg);
    {
        let br = Rect { l: pad + sc(124.0), t: y + sc(12.0), r: right - sc(12.0), b: y + sc(USAGE) - sc(26.0) };
        let mut days = Vec::with_capacity(USAGE_DAYS);
        let mut d = now;
        for _ in 0..USAGE_DAYS {
            days.push(d);
            d = usage::prev_day(d);
        }
        days.reverse();
        let vals: Vec<usage::Usage> = days.iter().map(|d| a.usage.get(*d)).collect();
        let max = vals.iter().map(|u| u.total()).max().unwrap_or(0).max(1) as f32;
        let bgap = sc(3.0);
        let nd = USAGE_DAYS as i32;
        let bw = (br.w() - bgap * (nd - 1)) as f32 / nd as f32;
        for (i, (day, u)) in days.iter().zip(&vals).enumerate() {
            let l = br.l + (i as f32 * (bw + bgap as f32)).round() as i32;
            let rr = br.l + (i as f32 * (bw + bgap as f32) + bw).round() as i32;
            let col = Rect { l, t: br.t, r: rr, b: br.b };
            let today_bar = i + 1 == USAGE_DAYS;
            let alpha = if today_bar { 255 } else { 160 };
            let th = (u.total() as f32 / max * br.h() as f32).round() as i32;
            let oh = (u.sent as f32 / max * br.h() as f32).round() as i32;
            if u.total() > 0 {
                let top = br.b - th.max(sc(2.0));
                c.fill_round(Rect { t: top, ..col }, 2.0 * scale, down_c.with_a(alpha));
                if oh > 0 {
                    c.fill_rect(Rect { t: br.b - oh, ..col }, up_c.with_a(alpha));
                }
            } else {
                c.fill_rect(Rect { t: br.b - sc(1.0), ..col }, pal.track);
            }
            // Hover: whole column, tooltip with the day's split.
            let hit = Rect { l: l - bgap / 2, r: rr + bgap - bgap / 2, ..col };
            out.hits.push((hit, PopupHit::Chart));
            if e.hot(hit) {
                c.stroke_round(Rect { t: br.b - th.max(sc(2.0)) - 1, b: br.b + 1, l: l - 1, r: rr + 1 }, 3.0 * scale, pal.fg.with_a(150));
                let (_, dm, dd) = usage::ymd(*day);
                let label = if today_bar {
                    "Today".to_string()
                } else {
                    format!("{} {}", MONTHS[(dm.clamp(1, 12) - 1) as usize], dd)
                };
                out.tip = Some(Tip {
                    x: (l + rr) / 2,
                    r: Rect { t: br.t, b: br.b, ..hit },
                    text: format!(
                        "{} \u{00B7} {}   \u{25BC} {}  \u{25B2} {}",
                        label,
                        fmt::bytes(u.total()),
                        fmt::bytes(u.recv),
                        fmt::bytes(u.sent)
                    ),
                });
            }
        }
        // Legend, right-aligned under the bars.
        let ly = br.b + sc(7.0);
        let mut lx = right - sc(12.0);
        let (dw, dh) = c.measure(f.small, "14 days");
        lx -= dw;
        c.text(f.small, "14 days", lx, ly, pal.dim);
        let sq = sc(7.0);
        for (t, col) in [("out", up_c), ("in", down_c)] {
            let tw = c.measure(f.small, t).0;
            lx -= sc(10.0) + tw;
            c.text(f.small, t, lx, ly, pal.dim);
            lx -= sq + sc(4.0);
            c.fill_round(Rect { l: lx, t: ly + (dh - sq) / 2, r: lx + sq, b: ly + (dh - sq) / 2 + sq }, 2.0 * scale, col);
        }
    }
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

    // ---- top processes: one list, CPU / Memory / Disk tabs, bars
    let ph = sc(PROCS);
    card(c, Rect { l: pad, t: y, r: right, b: y + ph }, rad, pal, false);
    c.text(f.caps, "TOP", ix, y + sc(14.0), pal.dim);
    let tabs: [(&str, Rgba); 3] = [
        ("CPU", accent(Metric::Cpu, a.dark)),
        ("Memory", accent(Metric::Ram, a.dark)),
        ("Disk", accent(Metric::DiskRead, a.dark)),
    ];
    let sel = (a.popup_top as usize).min(2);
    {
        let dot = sc(6.0);
        let widths: Vec<i32> = tabs.iter().map(|(t, _)| c.measure(f.small, t).0 + dot + sc(5.0) + sc(18.0)).collect();
        let total = widths.iter().sum::<i32>() + sc(4.0);
        let tr = Rect { l: right - sc(10.0) - total, t: y + sc(8.0), r: right - sc(10.0), b: y + sc(8.0) + chip_h };
        c.fill_round(tr, chip_h as f32 / 2.0, pal.chip);
        let mut sx = tr.l + sc(2.0);
        for (i, ((label, col), w)) in tabs.iter().zip(&widths).enumerate() {
            let seg = Rect { l: sx, t: tr.t + sc(2.0), r: sx + w, b: tr.b - sc(2.0) };
            let on = i == sel;
            if on {
                c.fill_round(seg, seg.h() as f32 / 2.0, pal.fg.with_a(if a.dark { 46 } else { 34 }));
            }
            let (tw, th) = c.measure(f.small, label);
            let cx = seg.l + (seg.w() - tw - dot - sc(5.0)) / 2;
            let my = seg.t + seg.h() / 2;
            c.fill_round(Rect { l: cx, t: my - dot / 2, r: cx + dot, b: my - dot / 2 + dot }, dot as f32 / 2.0, *col);
            let tc = if on || e.hot(seg) { pal.fg } else { pal.dim };
            c.text(f.small, label, cx + dot + sc(5.0), seg.t + (seg.h() - th) / 2, tc);
            out.hits.push((seg, PopupHit::TopTab(i as u8)));
            sx += w;
        }
    }
    {
        // (pid, name, value for the bar, shown text)
        let rows: Vec<(u32, &str, f64, String)> = match sel {
            0 => s.top_cpu.iter().map(|p| (p.pid, p.name.as_str(), p.cpu as f64, format!("{:.1}%", p.cpu))).collect(),
            1 => s.top_mem.iter().map(|p| (p.pid, p.name.as_str(), p.mem as f64, fmt::bytes(p.mem))).collect(),
            _ => s.top_io.iter().map(|p| (p.pid, p.name.as_str(), p.io_bps, fmt::rate(p.io_bps))).collect(),
        };
        let bar_c = tabs[sel].1;
        let max = rows.iter().map(|r| r.2).fold(0.0f64, f64::max);
        let inner_r = right - sc(14.0);
        let av = sc(18.0);
        let name_x = ix + av + sc(8.0);
        let name_w = sc(92.0);
        let val_w = sc(64.0);
        let (bar_l, bar_r) = (name_x + name_w + sc(8.0), inner_r - val_w - sc(8.0));
        if rows.is_empty() {
            c.text(f.small, "\u{2013}", ix, y + sc(40.0), pal.dim);
        }
        for (j, (pid, name, v, text)) in rows.iter().take(5).enumerate() {
            let ry = y + sc(36.0) + j as i32 * sc(PROC_ROW);
            let row = Rect { l: ix - sc(5.0), t: ry, r: inner_r + sc(5.0), b: ry + sc(PROC_ROW) };
            out.hits.push((row, PopupHit::Proc(*pid)));
            if e.hot(row) {
                c.fill_round(row, 5.0 * scale, pal.chip);
            }
            let mid = ry + sc(PROC_ROW) / 2;
            // Avatar: first letter on a colour picked from the name.
            let ar = Rect { l: ix, t: mid - av / 2, r: ix + av, b: mid - av / 2 + av };
            c.fill_round(ar, 5.0 * scale, avatar_col(name));
            let initial: String = name.chars().next().map_or(String::new(), |ch| ch.to_uppercase().collect());
            let (_, ih) = c.measure(f.caps, &initial);
            c.text_center(f.caps, &initial, ar.l + av / 2, mid - ih / 2, Rgba::rgb(20, 20, 20));
            let (_, nh) = c.measure(f.small, "Ag");
            let nm = c.ellipsize(f.small, name, name_w);
            c.text(f.small, &nm, name_x, mid - nh / 2, pal.fg);
            let bh = sc(6.0);
            let track = Rect { l: bar_l, t: mid - bh / 2, r: bar_r, b: mid - bh / 2 + bh };
            c.fill_round(track, bh as f32 / 2.0, pal.track);
            if max > 0.0 && track.w() > 0 {
                let fw = ((v / max).clamp(0.0, 1.0) * track.w() as f64).round() as i32;
                c.fill_round(Rect { r: track.l + fw.max(bh), ..track }, bh as f32 / 2.0, bar_c);
            }
            c.text_right(f.small, text, inner_r, mid - nh / 2, pal.dim);
        }
    }

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
        if !a.popup_pinned {
            a.hide_popup_soon();
        }
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
        Some(PopupHit::Range(_) | PopupHit::TopTab(_) | PopupHit::TaskMgr | PopupHit::ResMon | PopupHit::NetSettings | PopupHit::UsageReport | PopupHit::Monitor)
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
        Some(PopupHit::TopTab(i)) => {
            a.popup_top = i;
            unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
        }
        // CPU, memory, GPU, disk and network open the detailed Monitor page.
        Some(PopupHit::TaskMgr | PopupHit::ResMon | PopupHit::NetSettings | PopupHit::Monitor) => {
            hide(a);
            crate::settings_ui::open(a, crate::nav::Page::Monitor);
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
        .map(|p| p.name.clone())
        // The Monitor page lists every process.
        .or_else(|| a.detail().procs.iter().find(|p| p.pid == pid).map(|p| p.name.clone()))
        .unwrap_or_else(|| format!("PID {}", pid))
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
pub(crate) fn proc_menu(pid: u32) {
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
    if let Some(a) = crate::app::app() {
        a.ach_task_ended(ok);
    }
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
                let _p = crate::perf::span("popup_paint");
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
