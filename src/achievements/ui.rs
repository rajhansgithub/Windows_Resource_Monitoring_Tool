 // remove once the lead wires `register` / `open` / `refresh` / `show_moments` / `close_all`
#![warn(unused_imports, unused_mut, unused_variables)]
//! Achievements UI: the "Achievements" window and the unlock pop-ups.
//!
//! Same approach as `settings_ui`: no child windows. The whole client area is one surface drawn with
//! `render::Canvas` (text, cards, bars) and copied to the window with one `BitBlt`, on the Mica backdrop where
//! available. Medals are premultiplied BGRA tiles from `gpu::render_batch` (one batch per repaint, for the medals
//! that are visible and not cached yet; cached by badge id, tier and pixel size), composited over the canvas by
//! this file. Without a GPU path a flat medal is drawn in code. Icons are tiny vector marks (stroke paths from the
//! reference page) rasterised into 512x512 coverage masks on first use: no image files.
//!
//! Windows:
//! * `ResourceMonitor.Achievements`: the tab (header, racks, Emulsion gate, category cards) and, over it, the
//!   full view (tilt, flip to the spec sheet). Its only timer while idle is the smooth-scroll timer; the full view
//!   adds one animation timer that exists only while it is open.
//! * `ResourceMonitor.AchPop`: layered, topmost, no-activate pop-up in the tray corner, one moment at a time.

use crate::popup::{palette, style_window, Fonts};
use crate::render::{create_font, measure_dc, ui_face, Canvas, Rgba};
use crate::util::*;
use std::collections::{HashMap, VecDeque};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmGetColorizationColor, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::core::*;
use super::gpu::{self, Material};
use super::store::{self, Moment};

const CLASS_MAIN: &str = "ResourceMonitor.Achievements";
const CLASS_POP: &str = "ResourceMonitor.AchPop";

// ------------------------------------------------------------------ small helpers

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn fmt_n(n: u64) -> String {
    let s = n.to_string();
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            o.push(',');
        }
        o.push(c);
    }
    o
}

/// Terabytes from gigabytes: "0.64", "12".
fn fmt_tb(gb: u64) -> String {
    let tb = gb as f64 / 1000.0;
    if tb >= 10.0 {
        format!("{:.0}", tb)
    } else {
        format!("{:.2}", tb)
    }
}

/// Minutes east of UTC, now.
fn tz_offset_min() -> i32 {
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let r = GetTimeZoneInformation(&mut tz);
        let extra = if r == 2 { tz.DaylightBias } else { tz.StandardBias };
        -(tz.Bias + extra)
    }
}

fn fmt_date(ms: u64, tz: i32) -> String {
    if ms == 0 {
        return "earlier".to_string();
    }
    let t = LocalTime::from_unix_ms(ms, tz);
    format!("{} {} {}", t.day, MONTHS[(t.month.clamp(1, 12) - 1) as usize], t.year)
}

/// Seconds, monotonic.
fn now_s() -> f64 {
    unsafe {
        let (mut f, mut c) = (0i64, 0i64);
        QueryPerformanceFrequency(&mut f);
        QueryPerformanceCounter(&mut c);
        c as f64 / f.max(1) as f64
    }
}

/// "Show animations in Windows" is off.
fn reduced_motion() -> bool {
    let mut on: BOOL = 1;
    unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut on as *mut BOOL as _, 0) };
    on == 0
}

fn hash32(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

fn invalidate(hwnd: HWND) {
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
}

fn sc_dpi(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

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

/// Windows 11: Mica behind the whole window (frame extended into the client area, painted with alpha 0).
fn enable_mica(hwnd: HWND) -> bool {
    if build_number() < 22000 {
        return false;
    }
    unsafe {
        let kind: i32 = 2; // DWMSBT_MAINWINDOW
        let mut ok = DwmSetWindowAttribute(hwnd, 38, &kind as *const i32 as _, 4) >= 0;
        if !ok {
            let on: i32 = 1;
            ok = DwmSetWindowAttribute(hwnd, 1029, &on as *const i32 as _, 4) >= 0;
        }
        if !ok {
            return false;
        }
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        DwmExtendFrameIntoClientArea(hwnd, &m) >= 0
    }
}

// ------------------------------------------------------------------ colours

const fn hx(v: u32) -> Rgba {
    Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const TIER_HEX: [u32; 7] = [0xc07f45, 0xc9cfd8, 0xeac35e, 0xa9e4e1, 0xcbe7ff, 0xff4757, 0xb49cff];
const BLACK: Rgba = Rgba::rgb(0, 0, 0);
const WHITE: Rgba = Rgba::rgb(255, 255, 255);

/// Tier colour (tier 1..=7); darker shades of the same hues on the light theme.
fn tier_col(tier: u8, dark: bool) -> Rgba {
    let c = hx(TIER_HEX[(tier.clamp(1, 7) - 1) as usize]);
    if dark {
        c
    } else {
        c.mix(BLACK, 0.32)
    }
}

fn on_color(c: Rgba) -> Rgba {
    let lum = (c.0 as u32 * 299 + c.1 as u32 * 587 + c.2 as u32 * 114) / 1000;
    if lum > 150 {
        hx(0x14181d)
    } else {
        WHITE
    }
}

fn with_alpha(c: Rgba, f: f32) -> Rgba {
    c.with_a((c.3 as f32 * f.clamp(0.0, 1.0)).round() as u8)
}

#[derive(Clone, Copy)]
struct Colors {
    bg: Rgba,
    card: Rgba,
    card_border: Rgba,
    hover: Rgba,
    fg: Rgba,
    dim: Rgba,
    line: Rgba,
    track: Rgba,
    accent: Rgba,
    ok: Rgba,
    dark: bool,
}

fn make_colors(dark: bool, mica: bool) -> Colors {
    let p = palette(dark);
    let accent = {
        let (mut c, mut opaque) = (0u32, 0i32);
        if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) } == 0 {
            let a = hx(c & 0xFF_FFFF);
            if dark {
                a.mix(WHITE, 0.28)
            } else {
                a
            }
        } else if dark {
            hx(0x60CDFF)
        } else {
            hx(0x0078D4)
        }
    };
    let (card, card_border, hover) = if mica {
        if dark {
            (Rgba(255, 255, 255, 0x0D), Rgba(255, 255, 255, 0x0F), Rgba(255, 255, 255, 0x0F))
        } else {
            (Rgba(255, 255, 255, 0xB3), Rgba(0, 0, 0, 0x0F), Rgba(0, 0, 0, 0x09))
        }
    } else {
        (p.card, p.border, p.chip)
    };
    Colors {
        bg: if mica { Rgba(0, 0, 0, 0) } else { p.bg },
        card,
        card_border,
        hover,
        fg: p.fg,
        dim: p.dim,
        line: if dark { Rgba(255, 255, 255, 0x18) } else { Rgba(0, 0, 0, 0x16) },
        track: if dark { Rgba(255, 255, 255, 0x22) } else { Rgba(0, 0, 0, 0x1A) },
        accent,
        ok: if dark { hx(0x59d98e) } else { hx(0x1a8f50) },
        dark,
    }
}

// ------------------------------------------------------------------ icons: stroke paths -> 512x512 masks

/// Marks from the reference page (24x24 view box, stroke only). Elements are separated by `|`:
/// `p:` path data, `r:` x,y,w,h,rx, `c:` cx,cy,r, `e:` cx,cy,rx,ry.
const ICONS: &[(&str, &str)] = &[
    ("clock", "c:12,12,8.5|p:M12 7v5l3.5 2"),
    ("flame", "p:M12 3c1 3.5 5 5.5 5 10a5 5 0 0 1-10 0c0-2 1-3.2 2-4.2.2 1.4 1 2.2 1.8 2.5C10.5 8.5 11 5.5 12 3z"),
    ("gpu", "r:2.5,6.5,19,10,1.5|c:9,11.5,2.6|c:16,11.5,1.4|p:M5 16.5v3M2.5 19.5h6"),
    ("disk", "e:12,6,7.5,3|p:M4.5 6v12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3V6M4.5 12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3"),
    ("globe", "c:12,12,8.5|p:M3.5 12h17M12 3.5c2.6 2.4 4 5.4 4 8.5s-1.4 6.1-4 8.5c-2.6-2.4-4-5.4-4-8.5s1.4-6.1 4-8.5z"),
    ("down", "p:M12 4v11M7 10.5l5 5 5-5M5 20h14"),
    ("up", "p:M12 20V9M7 13.5l5-5 5 5M5 4h14"),
    ("pad", "r:2.5,7.5,19,10,5|p:M8 10.5v4M6 12.5h4M16 11.5h.01M18 13.5h.01"),
    ("bell", "p:M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15zM10 20.5a2 2 0 0 0 4 0"),
    ("xcircle", "c:12,12,8.5|p:M8.5 8.5l7 7M15.5 8.5l-7 7"),
    ("calendar", "r:3.5,5,17,15,2|p:M3.5 10h17M8 3v4M16 3v4"),
    ("repeat", "p:M4 11V9a3 3 0 0 1 3-3h12M16 3l3 3-3 3M20 13v2a3 3 0 0 1-3 3H5M8 21l-3-3 3-3"),
    ("hourglass", "p:M7 3h10M7 21h10M8 3c0 5 8 5 8 9s-8 4-8 9M16 3c0 5-8 5-8 9s8 4 8 9"),
    ("power", "p:M12 3.5v8M7 6.5a7.5 7.5 0 1 0 10 0"),
    ("screens", "r:2.5,5,12,9,1.5|r:9.5,9,12,9,1.5"),
    ("refresh", "p:M20 11a8 8 0 1 0-1.5 5M20 5v6h-6"),
    ("gauge", "p:M4.5 17a8.5 8.5 0 1 1 15 0M12 17l4-5"),
    ("box", "p:M3.5 8l8.5-4.5L20.5 8v8L12 20.5 3.5 16zM3.5 8L12 12.5 20.5 8M12 12.5v8"),
    ("flag", "p:M5.5 21V4M5.5 5h11l-2.5 3.5 2.5 3.5h-11"),
    ("snow", "p:M12 3v18M4.2 7.5l15.6 9M19.8 7.5l-15.6 9"),
    ("stack", "p:M12 3.5l8.5 4.5-8.5 4.5L3.5 8zM3.5 12l8.5 4.5 8.5-4.5M3.5 16l8.5 4.5 8.5-4.5"),
    ("star", "p:M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.9L12 17l-5.2 2.7 1-5.9L3.5 9.7l5.9-.8z"),
    ("thermo", "p:M10 14.5V5a2 2 0 0 1 4 0v9.5a4 4 0 1 1-4 0zM12 9v7"),
    ("ram", "r:3,8,18,8,1|p:M7 11v2M11 11v2M15 11v2M5 16v2M9 16v2M13 16v2M17 16v2"),
    ("nowifi", "p:M3.5 9a13 13 0 0 1 17 0M6.5 12.5a8.5 8.5 0 0 1 11 0M9.5 16a4 4 0 0 1 5 0M12 19.5h.01M4 4l16 16"),
    ("shieldx", "p:M12 3.5l7 2.5v5.5c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z|p:M9.5 9.5l5 5M14.5 9.5l-5 5"),
    ("battery", "r:2.5,8,17,8,1.5|p:M21.5 11v2M6 11v2"),
    ("moon", "p:M19.5 14.5A8 8 0 0 1 9.5 4.5a8 8 0 1 0 10 10z"),
    ("sparkle", "p:M12 3.5l1.8 5.2 5.2 1.8-5.2 1.8L12 17.5l-1.8-5.2L5 10.5l5.2-1.8zM19 16.5v4M17 18.5h4"),
    ("server", "r:3.5,4,17,6,1.5|r:3.5,14,17,6,1.5|p:M7 7h.01M7 17h.01"),
    ("calcheck", "r:3.5,5,17,15,2|p:M3.5 10h17M8 3v4M16 3v4M9 15l2 2 4-4"),
    ("pulse", "p:M4 12h3.6l2-3.7 1.8 7.3 1.8-8.6 1.7 5h5.1"),
];

/// The mark engraved on a badge.
fn icon_of(id: &str) -> &'static str {
    match id {
        "watch" => "clock",
        "heavy" => "flame",
        "render" => "gpu",
        "mover" | "full_house" => "disk",
        "bandwidth" => "globe",
        "down" => "down",
        "up" => "up",
        "gameon" => "pad",
        "responder" => "bell",
        "taskmaster" => "xcircle",
        "regular" => "calendar",
        "ritual" => "repeat",
        "stillhere" => "hourglass",
        "power_on" => "power",
        "second_screen" => "screens",
        "up_to_date" => "refresh",
        "under_cap" => "gauge",
        "first_gb" => "box",
        "marathon" => "flag",
        "cool_week" => "snow",
        "tb_month" => "stack",
        "completionist" => "star",
        "running_hot" => "thermo",
        "red_line" => "ram",
        "gone_dark" => "nowifi",
        "access_denied" => "shieldx",
        "fumes" => "battery",
        "night_owl" => "moon",
        "one_year" => "calcheck",
        "spring_clean" => "sparkle",
        "five_nines" | "rack_watch" | "rack_flow" | "rack_care" => "server",
        _ => "pulse",
    }
}

type Seg = [f32; 4];

fn num(b: &[u8], i: &mut usize) -> Option<f32> {
    while *i < b.len() && (b[*i] == b' ' || b[*i] == b',') {
        *i += 1;
    }
    let s = *i;
    if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
        *i += 1;
    }
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i < b.len() && b[*i] == b'.' {
        *i += 1;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    if *i == s {
        return None;
    }
    std::str::from_utf8(&b[s..*i]).ok()?.parse().ok()
}

fn line_to(out: &mut Vec<Seg>, from: (f32, f32), to: (f32, f32)) {
    out.push([from.0, from.1, to.0, to.1]);
}

fn cubic(out: &mut Vec<Seg>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    let mut prev = p0;
    for k in 1..=12 {
        let t = k as f32 / 12.0;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        line_to(out, prev, (x, y));
        prev = (x, y);
    }
}

/// SVG elliptical arc (endpoint form) as line segments.
#[allow(clippy::too_many_arguments)]
fn arc(out: &mut Vec<Seg>, p1: (f32, f32), mut rx: f32, mut ry: f32, rot: f32, large: bool, sweep: bool, p2: (f32, f32)) {
    rx = rx.abs();
    ry = ry.abs();
    if rx < 1e-4 || ry < 1e-4 || p1 == p2 {
        line_to(out, p1, p2);
        return;
    }
    let (sin, cos) = rot.to_radians().sin_cos();
    let (dx, dy) = ((p1.0 - p2.0) / 2.0, (p1.1 - p2.1) / 2.0);
    let (x1, y1) = (cos * dx + sin * dy, -sin * dx + cos * dy);
    let lam = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if lam > 1.0 {
        let s = lam.sqrt();
        rx *= s;
        ry *= s;
    }
    let num_ = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut co = (num_ / den).max(0.0).sqrt();
    if large == sweep {
        co = -co;
    }
    let (cxp, cyp) = (co * rx * y1 / ry, -co * ry * x1 / rx);
    let cx = cos * cxp - sin * cyp + (p1.0 + p2.0) / 2.0;
    let cy = sin * cxp + cos * cyp + (p1.1 + p2.1) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let th1 = ang(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dth = ang((x1 - cxp) / rx, (y1 - cyp) / ry, (-x1 - cxp) / rx, (-y1 - cyp) / ry);
    if !sweep && dth > 0.0 {
        dth -= std::f32::consts::TAU;
    } else if sweep && dth < 0.0 {
        dth += std::f32::consts::TAU;
    }
    let n = ((dth.abs() / (std::f32::consts::PI / 12.0)).ceil() as i32).max(2);
    let mut prev = p1;
    for k in 1..=n {
        let t = th1 + dth * k as f32 / n as f32;
        let (st, ct) = t.sin_cos();
        let p = (cos * rx * ct - sin * ry * st + cx, sin * rx * ct + cos * ry * st + cy);
        line_to(out, prev, p);
        prev = p;
    }
    line_to(out, prev, p2);
}

fn parse_path(d: &str, out: &mut Vec<Seg>) {
    let b = d.as_bytes();
    let mut i = 0;
    let mut cmd = b'M';
    let (mut x, mut y, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut last_c: Option<(f32, f32)> = None;
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
            if cmd == b'z' || cmd == b'Z' {
                line_to(out, (x, y), (sx, sy));
                x = sx;
                y = sy;
                last_c = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        let start = i;
        let mut c_end: Option<(f32, f32)> = None;
        match cmd.to_ascii_uppercase() {
            b'M' | b'L' => {
                let (Some(a), Some(c)) = (num(b, &mut i), num(b, &mut i)) else { break };
                let p = (ox + a, oy + c);
                if cmd.to_ascii_uppercase() == b'M' {
                    sx = p.0;
                    sy = p.1;
                    cmd = if rel { b'l' } else { b'L' };
                } else {
                    line_to(out, (x, y), p);
                }
                x = p.0;
                y = p.1;
            }
            b'H' => {
                let Some(a) = num(b, &mut i) else { break };
                line_to(out, (x, y), (ox + a, y));
                x = ox + a;
            }
            b'V' => {
                let Some(a) = num(b, &mut i) else { break };
                line_to(out, (x, y), (x, oy + a));
                y = oy + a;
            }
            b'C' => {
                let mut v = [0.0f32; 6];
                for s in v.iter_mut() {
                    let Some(a) = num(b, &mut i) else { return };
                    *s = a;
                }
                let (p1, p2, p3) = ((ox + v[0], oy + v[1]), (ox + v[2], oy + v[3]), (ox + v[4], oy + v[5]));
                cubic(out, (x, y), p1, p2, p3);
                c_end = Some(p2);
                x = p3.0;
                y = p3.1;
            }
            b'S' => {
                let mut v = [0.0f32; 4];
                for s in v.iter_mut() {
                    let Some(a) = num(b, &mut i) else { return };
                    *s = a;
                }
                let p1 = last_c.map_or((x, y), |c| (2.0 * x - c.0, 2.0 * y - c.1));
                let (p2, p3) = ((ox + v[0], oy + v[1]), (ox + v[2], oy + v[3]));
                cubic(out, (x, y), p1, p2, p3);
                c_end = Some(p2);
                x = p3.0;
                y = p3.1;
            }
            b'A' => {
                let mut v = [0.0f32; 7];
                for s in v.iter_mut() {
                    let Some(a) = num(b, &mut i) else { return };
                    *s = a;
                }
                let p2 = (ox + v[5], oy + v[6]);
                arc(out, (x, y), v[0], v[1], v[2], v[3] != 0.0, v[4] != 0.0, p2);
                x = p2.0;
                y = p2.1;
            }
            _ => {
                i += 1;
            }
        }
        last_c = c_end;
        if i == start {
            i += 1; // never stall on unexpected text
        }
    }
}

fn icon_segments(name: &str) -> Vec<Seg> {
    let spec = ICONS.iter().find(|(n, _)| *n == name).or_else(|| ICONS.iter().find(|(n, _)| *n == "pulse")).map_or("", |e| e.1);
    let mut segs = Vec::new();
    for el in spec.split('|') {
        if el.len() < 2 {
            continue;
        }
        let (kind, body) = el.split_at(2);
        let v: Vec<f32> = body.split(',').filter_map(|s| s.parse().ok()).collect();
        match kind {
            "p:" => parse_path(body, &mut segs),
            "r:" if v.len() == 5 => {
                let (x, y, w, h, r) = (v[0], v[1], v[2], v[3], v[4].min(v[2] / 2.0).min(v[3] / 2.0));
                if r < 0.05 {
                    let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
                    for k in 0..4 {
                        line_to(&mut segs, pts[k], pts[k + 1]);
                    }
                } else {
                    let d = format!("M{} {}h{}a{} {} 0 0 1 {} {}v{}a{} {} 0 0 1 {} {}h{}a{} {} 0 0 1 {} {}v{}a{} {} 0 0 1 {} {}z", x + r, y, w - 2.0 * r, r, r, r, r, h - 2.0 * r, r, r, -r, r, -(w - 2.0 * r), r, r, -r, -r, -(h - 2.0 * r), r, r, r, -r);
                    parse_path(&d, &mut segs);
                }
            }
            "c:" if v.len() == 3 => ellipse(&mut segs, v[0], v[1], v[2], v[2]),
            "e:" if v.len() == 4 => ellipse(&mut segs, v[0], v[1], v[2], v[3]),
            _ => {}
        }
    }
    segs
}

fn ellipse(out: &mut Vec<Seg>, cx: f32, cy: f32, rx: f32, ry: f32) {
    let n = 40;
    let mut prev = (cx + rx, cy);
    for k in 1..=n {
        let t = std::f32::consts::TAU * k as f32 / n as f32;
        let p = (cx + rx * t.cos(), cy + ry * t.sin());
        line_to(out, prev, p);
        prev = p;
    }
}

/// ICON_N x ICON_N (512) coverage of the mark: round-capped strokes, an SDF per pixel (row 0 = top).
/// The edge ramp is N/128 texels wide, so the engraved walls keep the width they were tuned at with a 128 mask, only smooth.
fn icon_mask(name: &str) -> Vec<u8> {
    const N: usize = gpu::ICON_N as usize;
    let ramp = N as f32 / 128.0;
    let segs = icon_segments(name);
    let k = N as f32 / 24.0;
    let half = 0.95 * k;
    let mut out = vec![0u8; N * N];
    for py in 0..N {
        for px in 0..N {
            let (x, y) = ((px as f32 + 0.5) / k, (py as f32 + 0.5) / k);
            let mut best = f32::MAX;
            for s in &segs {
                let (dx, dy) = (s[2] - s[0], s[3] - s[1]);
                let l2 = dx * dx + dy * dy;
                let t = if l2 < 1e-9 { 0.0 } else { (((x - s[0]) * dx + (y - s[1]) * dy) / l2).clamp(0.0, 1.0) };
                let (qx, qy) = (s[0] + dx * t - x, s[1] + dy * t - y);
                best = best.min(qx * qx + qy * qy);
            }
            let d = best.sqrt() * k;
            out[py * N + px] = ((((half - d) / ramp) + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    out
}

// ------------------------------------------------------------------ pixel surfaces and compositing

/// A top-down 32-bit DIB we own (premultiplied BGRA), used to composite medals over the canvas.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Surface {
    fn new(w: i32, h: i32) -> Option<Surface> {
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB;
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Surface { dc, bmp, old, bits: bits as *mut u32, w, h })
        }
    }

    fn px(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    /// Copy the canvas (same size) into this surface.
    fn load(&mut self, cv: &Canvas) {
        if cv.w == self.w && cv.h == self.h {
            cv.blit(self.dc, 0, 0);
            unsafe { GdiFlush() };
        }
    }

    /// Composite `top` (premultiplied) over this surface.
    fn over_surface(&mut self, top: &mut Surface) {
        if top.w != self.w || top.h != self.h {
            return;
        }
        let t = top.px();
        for (d, s) in self.px().iter_mut().zip(t.iter()) {
            *d = over_px(*d, *s);
        }
    }

    fn blit_to(&self, dc: HDC) {
        unsafe {
            GdiFlush();
            BitBlt(dc, 0, 0, self.w, self.h, self.dc, 0, 0, SRCCOPY);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

/// `s` over `d`, both premultiplied.
#[inline]
fn over_px(d: u32, s: u32) -> u32 {
    let sa = s >> 24;
    if sa == 255 {
        return s;
    }
    if sa == 0 {
        return d;
    }
    let ia = 255 - sa;
    let f = |sh: u32| -> u32 { (((s >> sh) & 255) + (((d >> sh) & 255) * ia + 127) / 255).min(255) << sh };
    f(24) | f(16) | f(8) | f(0)
}

/// Darken everything (a black layer at alpha `a`): the full view's backdrop.
fn dim_pixels(px: &mut [u32], a: u32) {
    let ia = 255 - a;
    for p in px.iter_mut() {
        let v = *p;
        let ch = |sh: u32| (((v >> sh) & 255) * ia + 127) / 255;
        let na = (a + (((v >> 24) & 255) * ia + 127) / 255).min(255);
        *p = (na << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0);
    }
}

/// Draw a square medal tile `src` (n x n, premultiplied) centred on (cx, cy), scaled by (sx, sy) with bilinear
/// sampling, at opacity `op`, only within rows `clip.0..clip.1`.
#[allow(clippy::too_many_arguments)]
fn draw_tile(dst: &mut [u32], dw: i32, dh: i32, src: &[u32], n: i32, cx: f32, cy: f32, sx: f32, sy: f32, op: f32, clip: (i32, i32)) {
    if n <= 0 || src.len() < (n * n) as usize || sx <= 0.001 || sy <= 0.001 || op <= 0.0 {
        return;
    }
    let (wd, hd) = (n as f32 * sx, n as f32 * sy);
    let (left, top) = (cx - wd / 2.0, cy - hd / 2.0);
    let x0 = (left.floor() as i32).max(0);
    let x1 = ((left + wd).ceil() as i32).min(dw);
    let y0 = (top.floor() as i32).max(clip.0.max(0));
    let y1 = ((top + hd).ceil() as i32).min(clip.1.min(dh));
    let opq = op.clamp(0.0, 1.0);
    let get = |xx: i32, yy: i32| -> u32 {
        if xx < 0 || yy < 0 || xx >= n || yy >= n {
            0
        } else {
            src[(yy * n + xx) as usize]
        }
    };
    for y in y0..y1 {
        let v = (y as f32 + 0.5 - top) / sy - 0.5;
        let vf = v.floor();
        let (fv, vi) = (v - vf, vf as i32);
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - left) / sx - 0.5;
            let uf = u.floor();
            let (fu, ui) = (u - uf, uf as i32);
            let (t00, t10, t01, t11) = (get(ui, vi), get(ui + 1, vi), get(ui, vi + 1), get(ui + 1, vi + 1));
            let (w00, w10, w01, w11) = ((1.0 - fu) * (1.0 - fv), fu * (1.0 - fv), (1.0 - fu) * fv, fu * fv);
            let ch = |sh: u32| -> u32 {
                let c = |t: u32| ((t >> sh) & 255) as f32;
                ((c(t00) * w00 + c(t10) * w10 + c(t01) * w01 + c(t11) * w11) * opq + 0.5) as u32
            };
            let s = (ch(24).min(255) << 24) | (ch(16).min(255) << 16) | (ch(8).min(255) << 8) | ch(0).min(255);
            let i = (y * dw + x) as usize;
            dst[i] = over_px(dst[i], s);
        }
    }
}

fn premul(c: Rgba, cov: f32) -> u32 {
    let a = (c.3 as f32 * cov.clamp(0.0, 1.0)).round() as u32;
    let p = |v: u8| (v as u32 * a + 127) / 255;
    (a << 24) | (p(c.0) << 16) | (p(c.1) << 8) | p(c.2)
}

/// The software medal used when the GPU path is not available: rim, face, inner ring, engraved mark.
fn flat_medal(tier: u8, n: u32, mask: Option<&[u8]>) -> Vec<u32> {
    let nf = n as f32;
    let r0 = nf / 2.0;
    let base = hx(TIER_HEX[(tier.clamp(1, 7) - 1) as usize]);
    let rim = base.mix(WHITE, 0.45);
    let deep = base.mix(BLACK, 0.55);
    let rainbow = [hx(0xff9ad5), hx(0x8be9ff), hx(0xffe48a), hx(0xb49cff), hx(0x8dffc7), hx(0xff9ad5)];
    let mut out = vec![0u32; (n * n) as usize];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - r0, y as f32 + 0.5 - r0);
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (r0 - d + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let t = d / r0;
            let face = if tier == 7 {
                let a = (dy.atan2(dx) / std::f32::consts::TAU + 0.5).fract() * 5.0;
                let k = a.floor() as usize;
                rainbow[k.min(4)].mix(rainbow[k.min(4) + 1], a.fract()).mix(WHITE, 0.25)
            } else {
                base.mix(WHITE, (0.28 * (-(dy + dx * 0.5) / r0)).clamp(0.0, 0.28)).mix(BLACK, (0.22 * ((dy + dx * 0.5) / r0)).clamp(0.0, 0.22))
            };
            let mut col = if t > 0.88 {
                rim
            } else if (t - 0.8).abs() < 0.012 {
                deep
            } else {
                face
            };
            if let Some(m) = mask {
                let half = r0 * 0.63;
                let mn = gpu::ICON_N as f32;
                let (ix, iy) = ((dx / half * 0.5 + 0.5) * mn, (dy / half * 0.5 + 0.5) * mn);
                if ix >= 0.0 && iy >= 0.0 && ix < mn && iy < mn {
                    let c = m[iy as usize * gpu::ICON_N as usize + ix as usize] as f32 / 255.0;
                    let ink = if tier == 7 { WHITE } else { deep };
                    col = col.mix(ink, c * 0.9);
                }
            }
            out[(y * n + x) as usize] = premul(col, cov);
        }
    }
    out
}

fn material_of(tier: u8) -> Material {
    match tier {
        0 | 1 => Material::Bronze,
        2 => Material::Silver,
        3 => Material::Gold,
        4 => Material::Platinum,
        5 => Material::Diamond,
        6 => Material::Ruby,
        _ => Material::Emulsion,
    }
}

fn seed_of(id: &str, tier: u8) -> f32 {
    let h = hash32(id);
    if tier == 7 {
        (h % 5) as f32 * 7.3
    } else {
        (h % 7) as f32 * 5.1
    }
}

type MedalKey = (&'static str, u8, u32);

/// Cache of medal tiles by (badge id, tier, pixel size) and of icon masks by name.
#[derive(Default)]
struct Medals {
    masks: HashMap<&'static str, Vec<u8>>,
    cache: HashMap<MedalKey, Vec<u32>>,
}

impl Medals {
    fn get(&self, k: MedalKey) -> Option<&Vec<u32>> {
        self.cache.get(&k)
    }

    fn ensure_mask(&mut self, icon: &'static str) {
        self.masks.entry(icon).or_insert_with(|| icon_mask(icon));
    }

    /// Render everything missing in ONE batch (flat fallback when the GPU path is unavailable or fails).
    fn ensure(&mut self, reqs: &[MedalKey]) {
        let mut need: Vec<MedalKey> = Vec::new();
        for r in reqs {
            if r.1 > 0 && !self.cache.contains_key(r) && !need.contains(r) {
                need.push(*r);
            }
        }
        if need.is_empty() {
            return;
        }
        for k in &need {
            self.ensure_mask(icon_of(k.0));
        }
        let mut out: Vec<Vec<u32>> = Vec::new();
        let mut done = false;
        if gpu::available() {
            let params: Vec<gpu::Params> = need
                .iter()
                .map(|&(id, t, px)| gpu::Params {
                    material: material_of(t),
                    size_px: px,
                    time_s: gpu::still_time(material_of(t)),
                    seed: seed_of(id, t),
                    tilt: (0.0, 0.0),
                    icon_mask: self.masks.get(icon_of(id)).map(|m| &m[..]),
                    detail_mask: None,
                })
                .collect();
            done = gpu::render_batch(&params, &mut out) && out.len() == need.len();
        }
        for (i, &k) in need.iter().enumerate() {
            let tile = if done { std::mem::take(&mut out[i]) } else { flat_medal(k.1, k.2, self.masks.get(icon_of(k.0)).map(|m| &m[..])) };
            self.cache.insert(k, tile);
        }
        // Keep the cache bounded (sizes change with DPI and the full view).
        if self.cache.len() > 160 {
            let keep: Vec<MedalKey> = reqs.to_vec();
            self.cache.retain(|k, _| keep.contains(k));
        }
    }

    /// One live frame (full view): GPU at time `t` and `tilt`; the flat medal when there is no GPU.
    fn frame(&mut self, id: &'static str, tier: u8, px: u32, t: f32, tilt: (f32, f32)) -> Vec<u32> {
        let icon = icon_of(id);
        self.ensure_mask(icon);
        if gpu::available() {
            let params = [gpu::Params {
                material: material_of(tier),
                size_px: px,
                time_s: t,
                seed: seed_of(id, tier),
                tilt,
                icon_mask: self.masks.get(icon).map(|m| &m[..]),
                detail_mask: None,
            }];
            let mut out: Vec<Vec<u32>> = Vec::new();
            if gpu::render_batch(&params, &mut out) && out.len() == 1 {
                return out.swap_remove(0);
            }
        }
        flat_medal(tier, px, self.masks.get(icon).map(|m| &m[..]))
    }
}

// ------------------------------------------------------------------ text helpers

fn ellipsize_dc(dc: HDC, font: HFONT, s: &str, max_w: i32) -> String {
    if measure_dc(dc, font, s).0 <= max_w {
        return s.to_string();
    }
    let mut v: String = s.to_string();
    while !v.is_empty() && measure_dc(dc, font, &format!("{}\u{2026}", v)).0 > max_w {
        v.pop();
    }
    format!("{}\u{2026}", v.trim_end())
}

/// Greedy word wrap into at most `max_lines` lines (the last one is shortened with an ellipsis).
fn wrap_dc(dc: HDC, font: HFONT, s: &str, max_w: i32, max_lines: usize) -> Vec<String> {
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < words.len() {
        let cand = if cur.is_empty() { words[i].to_string() } else { format!("{} {}", cur, words[i]) };
        if cur.is_empty() || measure_dc(dc, font, &cand).0 <= max_w {
            cur = cand;
            i += 1;
        } else if lines.len() + 1 >= max_lines {
            cur = format!("{} {}", cur, words[i..].join(" "));
            i = words.len();
        } else {
            lines.push(ellipsize_dc(dc, font, &cur, max_w));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        lines.push(ellipsize_dc(dc, font, &cur, max_w));
    }
    lines
}

fn text_h(dc: HDC, font: HFONT) -> i32 {
    measure_dc(dc, font, "Ag").1
}

/// Extra fonts the shared `popup::Fonts` set does not have.
struct Extra {
    title: HFONT,
    num: HFONT,
}

impl Extra {
    fn new(dpi: u32) -> Extra {
        let face = ui_face();
        Extra { title: create_font(sc_dpi(24, dpi), 600, face, true), num: create_font(sc_dpi(56, dpi), 600, face, true) }
    }
}

impl Drop for Extra {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.title);
            DeleteObject(self.num);
        }
    }
}

fn text_c(cv: &mut Canvas, font: HFONT, s: &str, cx: i32, cy: i32, c: Rgba) {
    let (w, h) = cv.measure(font, s);
    cv.text(font, s, cx - w / 2, cy - h / 2, c);
}

fn pill(cv: &mut Canvas, r: Rect, c: Rgba) {
    cv.fill_round(r, r.h() as f32 / 2.0, c);
}

fn inflate(r: Rect, d: i32) -> Rect {
    Rect { l: r.l - d, t: r.t - d, r: r.r + d, b: r.b + d }
}

fn shift(r: Rect, dy: i32) -> Rect {
    Rect { t: r.t + dy, b: r.b + dy, ..r }
}

/// Two-pixel focus ring just inside `r`.
fn focus_ring(cv: &mut Canvas, r: Rect, rad: f32, c: Rgba) {
    cv.stroke_round(r, rad, c);
    cv.stroke_round(inflate(r, -1), (rad - 1.0).max(0.0), c);
}

/// Placeholder for a medal that is not earned yet: a dashed ring.
fn dashed_ring(cv: &mut Canvas, cx: i32, cy: i32, radius: i32, dot: i32, c: Rgba) {
    let dashes = 18;
    let dot = dot.max(2);
    for k in 0..dashes {
        for j in 0..3 {
            let a = std::f32::consts::TAU * (k as f32 + j as f32 * 0.16) / dashes as f32;
            let (x, y) = (cx as f32 + radius as f32 * a.cos(), cy as f32 + radius as f32 * a.sin());
            let r = Rect { l: x.round() as i32 - dot / 2, t: y.round() as i32 - dot / 2, r: x.round() as i32 - dot / 2 + dot, b: y.round() as i32 - dot / 2 + dot };
            cv.fill_round(r, dot as f32 / 2.0, c);
        }
    }
}

fn pips(cv: &mut Canvas, x: i32, cy: i32, size: i32, gap: i32, filled: u8, col: &Colors) -> i32 {
    for i in 0..7 {
        let r = Rect { l: x + i * (size + gap), t: cy - size / 2, r: x + i * (size + gap) + size, b: cy - size / 2 + size };
        cv.fill_round(r, size as f32 / 2.0, if (i as u8) < filled { tier_col(i as u8 + 1, col.dark) } else { col.track });
    }
    7 * (size + gap) - gap
}

// ------------------------------------------------------------------ model of the tab

const CLIENT_W: i32 = 880;
const CLIENT_H: i32 = 640;
const WHEEL_STEP: i32 = 48;
const FIRST_LADDERS: usize = 5;
const T_SCROLL: usize = 1;
const T_FV: usize = 2;
const WM_MOUSELEAVE_: u32 = 0x02A3;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;

const CATS: [(&str, Category); 5] = [
    ("Ladders", Category::Ladder),
    ("Getting started", Category::Start),
    ("Mastery", Category::Mastery),
    ("Happy accidents", Category::Accident),
    ("Secrets", Category::Secret),
];

/// Singles wear a fixed metal (they have no ladder).
fn single_tier(c: Category) -> u8 {
    match c {
        Category::Start => 1,
        Category::Accident => 2,
        Category::Secret => 3,
        _ => 4,
    }
}

fn badge_tier(s: &State, b: &Badge) -> u8 {
    if b.is_ladder() {
        s.tier(b.id).min(7)
    } else if s.earned(b.id) {
        single_tier(b.category)
    } else {
        0
    }
}

#[derive(Clone)]
struct RowData {
    id: &'static str,
    title: String,
    tag: String,
    count: String,
    line: String,
    lines: Vec<String>,
    /// Medal tier when earned (0 = not earned).
    tier: u8,
    secret: bool,
    /// Ladders: tiers reached.
    pips: Option<u8>,
    /// Progress fraction and the tier whose colour fills it (0 = accent).
    bar: Option<(f32, u8)>,
    note: String,
    chip: String,
}

struct RowL {
    r: Rect,
    d: RowData,
}

struct CatL {
    r: Rect,
    name: &'static str,
    got: usize,
    total: usize,
    more: Option<Rect>,
    expanded: bool,
}

struct RackL {
    r: Rect,
    set: usize,
    wells: [Rect; 4],
    plate: Rect,
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Row(&'static str),
    More,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Row(usize),
    Well(&'static str),
    More,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Thumb(i32),
}

struct Slot {
    key: MedalKey,
    cx: i32,
    cy: i32,
}

/// Everything the full view and the back card show about one badge.
struct Entry {
    id: &'static str,
    title: String,
    line: String,
    tier: u8,
    earned_ms: u64,
    count: String,
    xp: u32,
    notes: Vec<String>,
}

struct Fv {
    ids: Vec<&'static str>,
    i: usize,
    /// 0 = front, 1 = back (animated).
    flip: f32,
    flip_t: f32,
    rx: f32,
    ry: f32,
    ptr: Option<(i32, i32)>,
    moved: f64,
    t0: f64,
    last: f64,
    /// 0 medal, 1 previous, 2 next, 3 close.
    focus: u8,
    hover: u8,
    reduced: bool,
    frame: Vec<u32>,
    frame_key: Option<MedalKey>,
}

struct FvGeom {
    stage: Rect,
    card_l: i32,
    card_w: i32,
    prev: Rect,
    next: Rect,
    close: Rect,
}

struct Ui {
    hwnd: HWND,
    dpi: u32,
    mica: bool,
    col: Colors,
    f: Fonts,
    x: Extra,
    mdc: HDC,
    cw: i32,
    ch: i32,
    state: State,
    tz: i32,
    expanded: bool,
    // layout (content coordinates)
    content_h: i32,
    head: Rect,
    heads: Vec<(i32, &'static str, &'static str)>,
    racks: Vec<RackL>,
    gate: Rect,
    gate_sub: Vec<String>,
    gate_top_h: i32,
    cond_h: i32,
    cats: Vec<CatL>,
    rows: Vec<RowL>,
    focus_order: Vec<Focus>,
    // interaction
    scroll: f32,
    target: i32,
    hover: Option<Hit>,
    focus: Option<Focus>,
    focus_visible: bool,
    drag: Drag,
    sb_hover: bool,
    tracking: bool,
    // drawing
    cv: Canvas,
    surf: Option<Surface>,
    ov: Option<Surface>,
    snap: Vec<u32>,
    snap_ok: bool,
    medals: Medals,
    fv: Option<Fv>,
}

static mut MAIN: *mut Ui = std::ptr::null_mut();

#[allow(static_mut_refs)]
fn ui() -> Option<&'static mut Ui> {
    unsafe { MAIN.as_mut() }
}

fn free_ui() {
    unsafe {
        let p = std::ptr::replace(std::ptr::addr_of_mut!(MAIN), std::ptr::null_mut());
        if !p.is_null() {
            let u = Box::from_raw(p);
            if !u.mdc.is_null() {
                DeleteDC(u.mdc);
            }
        }
    }
}

impl Ui {
    fn new(dpi: u32, dark: bool, state: State) -> Ui {
        Ui {
            hwnd: std::ptr::null_mut(),
            dpi,
            mica: false,
            col: make_colors(dark, false),
            f: Fonts::gray(dpi as f32 / 96.0),
            x: Extra::new(dpi),
            mdc: unsafe { CreateCompatibleDC(std::ptr::null_mut()) },
            cw: 0,
            ch: 0,
            state,
            tz: tz_offset_min(),
            expanded: false,
            content_h: 0,
            head: Rect::default(),
            heads: Vec::new(),
            racks: Vec::new(),
            gate: Rect::default(),
            gate_sub: Vec::new(),
            gate_top_h: 0,
            cond_h: 0,
            cats: Vec::new(),
            rows: Vec::new(),
            focus_order: Vec::new(),
            scroll: 0.0,
            target: 0,
            hover: None,
            focus: None,
            focus_visible: false,
            drag: Drag::None,
            sb_hover: false,
            tracking: false,
            cv: Canvas::new(),
            surf: None,
            ov: None,
            snap: Vec::new(),
            snap_ok: false,
            medals: Medals::default(),
            fv: None,
        }
    }

    fn sc(&self, v: i32) -> i32 {
        sc_dpi(v, self.dpi)
    }

    fn set_dpi(&mut self, dpi: u32) {
        let old = self.dpi.max(1);
        self.dpi = dpi;
        self.f = Fonts::gray(dpi as f32 / 96.0);
        self.x = Extra::new(dpi);
        self.target = self.target * dpi as i32 / old as i32;
        self.scroll = self.target as f32;
        self.snap_ok = false;
    }

    // -------------------------------------------------------------- data

    fn earned_ids(&self) -> Vec<&'static str> {
        BADGES.iter().filter(|b| badge_tier(&self.state, b) > 0).map(|b| b.id).collect()
    }

    fn current_month_key() -> u32 {
        let st = local_time();
        st.wYear as u32 * 100 + st.wMonth as u32
    }

    /// Progress of an unearned single: fraction and caption.
    fn single_progress(&self, b: &Badge) -> Option<(f32, String)> {
        let s = &self.state;
        match b.id {
            "completionist" => {
                let others = BADGES.len() as u64 - 1;
                let done = BADGES.iter().filter(|x| x.id != "completionist" && badge_tier(s, x) > 0).count() as u64;
                Some((done as f32 / others as f32, format!("{} / {} badges", done, others)))
            }
            "marathon" => {
                let m = if s.r.l > 0 { s.r.l.saturating_sub(s.r.s) + 1 } else { 0 }.min(MARATHON_MINUTES);
                Some((m as f32 / MARATHON_MINUTES as f32, format!("{} / 24 hours", m / 60)))
            }
            "tb_month" => {
                let gb = if s.tm.0 == Self::current_month_key() { s.tm.1 / GB } else { 0 }.min(1000);
                Some((gb as f32 / 1000.0, format!("{} / 1,000 GB this month", fmt_n(gb))))
            }
            "first_gb" => {
                let b_ = s.counter("net_b").min(GB);
                Some((b_ as f32 / GB as f32, format!("{:.1} / 1 GB", b_ as f64 / GB as f64)))
            }
            "power_on" => {
                let m = s.counter("min").min(60);
                Some((m as f32 / 60.0, format!("{} / 60 minutes", m)))
            }
            "cool_week" => {
                let d = (s.cw as u64).min(COOL_DAYS as u64);
                Some((d as f32 / COOL_DAYS as f32, format!("{} / {} days", d, COOL_DAYS)))
            }
            _ => None,
        }
    }

    fn row_data(&self, b: &'static Badge) -> RowData {
        let s = &self.state;
        let tier = badge_tier(s, b);
        let mut d = RowData {
            id: b.id,
            title: b.name.to_string(),
            tag: String::new(),
            count: String::new(),
            line: b.line.to_string(),
            lines: Vec::new(),
            tier,
            secret: false,
            pips: None,
            bar: None,
            note: String::new(),
            chip: String::new(),
        };
        if b.is_ladder() {
            let t = tier;
            let count = count_of(s, b);
            let next = if t >= 7 || (t == 6 && !s.g) { None } else { next_threshold(b, t) };
            let prev = if t > 0 { b.tiers[t as usize - 1] } else { 0 };
            let frac = match next {
                Some(n) if n > prev => (count.saturating_sub(prev) as f32 / (n - prev) as f32).clamp(0.0, 1.0),
                _ => 1.0,
            };
            d.tag = if t > 0 { TIER_NAMES[t as usize - 1].to_uppercase() } else { String::new() };
            d.count = format!("{} {}", fmt_n(count), b.unit);
            d.pips = Some(t);
            d.note = match next {
                Some(n) => format!("Next: {} at {}", TIER_NAMES[t as usize], fmt_n(n)),
                None if t >= 7 => "Emulsion reached".to_string(),
                None => "Emulsion: gate shut".to_string(),
            };
            d.bar = Some((frac, if next.is_some() { t + 1 } else { t.max(1) }));
        } else if tier > 0 {
            d.tag = TIER_NAMES[tier as usize - 1].to_uppercase();
            d.note = format!("Earned {}", fmt_date(s.u.get(b.id).copied().unwrap_or(0), self.tz));
            d.chip = format!("+{} XP", b.xp);
        } else if b.category == Category::Secret {
            d.secret = true;
            d.title = "?".to_string();
            d.line = "A secret. Keep using Resource Monitor.".to_string();
            d.note = "Locked".to_string();
            d.chip = "? XP".to_string();
        } else {
            d.chip = format!("{} XP", b.xp);
            match self.single_progress(b) {
                Some((f, text)) => {
                    d.bar = Some((f.clamp(0.0, 1.0), 0));
                    d.note = text;
                }
                None => d.note = "Not yet".to_string(),
            }
        }
        d
    }

    fn entry(&self, id: &'static str) -> Option<Entry> {
        let b = Catalog::get(id)?;
        let s = &self.state;
        let tier = badge_tier(s, b);
        let first = s.u.get(id).copied().unwrap_or(0);
        if b.is_ladder() {
            let kd = s.kd.get(id);
            let day = |i: usize| kd.and_then(|v| v.get(i).copied()).filter(|&d| d > 0).unwrap_or(first);
            let mut notes: Vec<String> = (0..tier as usize).map(|i| format!("{} \u{b7} {}", TIER_NAMES[i], fmt_date(day(i), self.tz))).collect();
            let count = count_of(s, b);
            notes.push(format!("Your count: {} {}", fmt_n(count), b.unit));
            let next = if tier >= 7 || (tier == 6 && !s.g) { None } else { next_threshold(b, tier) };
            notes.push(match next {
                Some(n) => format!("Next: {} at {}", TIER_NAMES[tier as usize], fmt_n(n)),
                None if tier >= 7 => "Emulsion reached".to_string(),
                None => "Emulsion: gate shut".to_string(),
            });
            let xp = LADDER_XP[..tier.min(7) as usize].iter().sum();
            Some(Entry {
                id,
                title: b.name.to_string(),
                line: b.line.to_string(),
                tier,
                earned_ms: if tier > 0 { day(tier as usize - 1) } else { 0 },
                count: format!("{} {}", fmt_n(count), b.unit),
                xp,
                notes,
            })
        } else {
            Some(Entry {
                id,
                title: b.name.to_string(),
                line: b.line.to_string(),
                tier,
                earned_ms: first,
                count: String::new(),
                xp: b.xp,
                notes: vec![format!("Earned {}", fmt_date(first, self.tz)), format!("+{} XP", b.xp)],
            })
        }
    }

    // -------------------------------------------------------------- layout

    fn row_offsets(&self, d: &RowData, th_t: i32, th_l: i32, th_m: i32) -> (i32, i32, i32, i32, i32) {
        let title_y = self.sc(8);
        let line_y = title_y + th_t + self.sc(1);
        let after_lines = line_y + th_l * d.lines.len() as i32;
        let bar_y = after_lines + self.sc(6);
        let meta_y = if d.bar.is_some() { bar_y + self.sc(5) + self.sc(6) } else { after_lines + self.sc(5) };
        let h = (meta_y + th_m.max(self.sc(8)) + self.sc(9)).max(self.sc(80));
        (title_y, line_y, bar_y, meta_y, h)
    }

    fn layout(&mut self) {
        let dpi = self.dpi;
        let sc = |v: i32| sc_dpi(v, dpi);
        let (pad, wid) = (sc(24), (self.cw - 2 * sc(24)).max(sc(320)));
        let mdc = self.mdc;
        let (th_n, th_s, th_b, th_mid) = (text_h(mdc, self.f.normal), text_h(mdc, self.f.small), text_h(mdc, self.f.bold), text_h(mdc, self.f.mid));
        self.rows.clear();
        self.cats.clear();
        self.racks.clear();
        self.heads.clear();
        self.focus_order.clear();
        let mut y = sc(20);

        // Header.
        self.head = Rect { l: pad, t: y, r: pad + wid, b: y + sc(92) };
        y = self.head.b + sc(22);

        // Racks.
        self.heads.push((y, "1U rack plates", "All four medals of a rack at one tier certify it."));
        y += th_mid + sc(12);
        let ear = sc(26);
        let well_h = sc(8) + sc(72) + sc(4) + th_b + th_s + sc(8);
        let top_h = sc(12) + th_mid + sc(10);
        let rack_h = top_h + well_h + sc(10) + sc(30) + sc(12);
        for si in 0..SETS.len() {
            let r = Rect { l: pad, t: y, r: pad + wid, b: y + rack_h };
            let mid_l = r.l + ear + sc(10);
            let mid_w = wid - 2 * (ear + sc(10));
            let gap = sc(8);
            let ww = (mid_w - 3 * gap) / 4;
            let mut wells = [Rect::default(); 4];
            for (k, w) in wells.iter_mut().enumerate() {
                let l = mid_l + k as i32 * (ww + gap);
                *w = Rect { l, t: r.t + top_h, r: l + ww, b: r.t + top_h + well_h };
            }
            let plate = Rect { l: mid_l, t: r.t + top_h + well_h + sc(10), r: mid_l + mid_w, b: r.t + top_h + well_h + sc(10) + sc(30) };
            self.racks.push(RackL { r, set: si, wells, plate });
            y = r.b + sc(14);
        }
        y += sc(6);

        // Emulsion gate.
        let gate_sub = if self.state.g {
            "Emulsion is open. Ladders climb to it as their counts allow.".to_string()
        } else {
            "Emulsion opens only when all four hold. It is checked after real events, never by a timer, never by idle time.".to_string()
        };
        self.gate_sub = wrap_dc(mdc, self.f.normal, &gate_sub, wid - 2 * sc(16), 2);
        self.gate_top_h = sc(14) + th_mid + sc(6) + th_n * self.gate_sub.len() as i32 + sc(6);
        self.cond_h = sc(8) + th_b + sc(2) + th_s + sc(8);
        self.gate = Rect { l: pad, t: y, r: pad + wid, b: y + self.gate_top_h + 4 * self.cond_h + sc(8) };
        y = self.gate.b + sc(22);

        // Categories.
        self.heads.push((y, "Badges", ""));
        y += th_mid + sc(12);
        let gap = sc(14);
        let col_w = (wid - gap) / 2;
        let h0 = self.layout_cat(0, pad, y, wid);
        y += h0 + gap;
        for pair in [(1usize, 2usize), (3, 4)] {
            let ha = self.layout_cat(pair.0, pad, y, col_w);
            let hb = self.layout_cat(pair.1, pad + col_w + gap, y, col_w);
            y += ha.max(hb) + gap;
        }
        self.content_h = y + sc(10);
        let max = self.max_scroll();
        self.target = self.target.clamp(0, max);
        self.scroll = self.scroll.clamp(0.0, max as f32);
    }

    /// Lay out one category card at (x, y) of width `w`; returns its height.
    fn layout_cat(&mut self, ci: usize, x: i32, y: i32, w: i32) -> i32 {
        let dpi = self.dpi;
        let sc = |v: i32| sc_dpi(v, dpi);
        let mdc = self.mdc;
        let (th_t, th_l, th_m, th_mid) = (text_h(mdc, self.f.bold), text_h(mdc, self.f.normal), text_h(mdc, self.f.small), text_h(mdc, self.f.mid));
        let (name, cat) = CATS[ci];
        let list: Vec<&'static Badge> = BADGES.iter().filter(|b| b.category == cat).collect();
        let got = list.iter().filter(|b| badge_tier(&self.state, b) > 0).count();
        let head_h = sc(12) + th_mid + sc(8);
        let text_w = (w - 2 * sc(12) - sc(6 + 64 + 10) - sc(10)).max(sc(80));
        let show = if cat == Category::Ladder && !self.expanded { FIRST_LADDERS.min(list.len()) } else { list.len() };
        let mut ry = y + head_h;
        for b in list.iter().take(show) {
            let mut d = self.row_data(*b);
            d.lines = wrap_dc(mdc, self.f.normal, &d.line, text_w, 2);
            let (.., h) = self.row_offsets(&d, th_t, th_l, th_m);
            let r = Rect { l: x + sc(6), t: ry, r: x + w - sc(6), b: ry + h };
            if d.tier > 0 {
                self.focus_order.push(Focus::Row(b.id));
            }
            self.rows.push(RowL { r, d });
            ry += h;
        }
        let mut more = None;
        if cat == Category::Ladder && list.len() > FIRST_LADDERS {
            more = Some(Rect { l: x + sc(6), t: ry + sc(2), r: x + w - sc(6), b: ry + sc(2) + sc(34) });
            self.focus_order.push(Focus::More);
            ry += sc(2) + sc(34);
        }
        ry += sc(6);
        self.cats.push(CatL { r: Rect { l: x, t: y, r: x + w, b: ry }, name, got, total: list.len(), more, expanded: self.expanded });
        ry - y
    }

    // -------------------------------------------------------------- scrolling

    fn max_scroll(&self) -> i32 {
        (self.content_h - self.ch).max(0)
    }

    fn scroll_px(&self) -> i32 {
        self.scroll.round() as i32
    }

    fn scroll_to(&mut self, target: i32, animate: bool) {
        self.target = target.clamp(0, self.max_scroll());
        unsafe {
            if animate && (self.target as f32 - self.scroll).abs() > 1.0 {
                SetTimer(self.hwnd, T_SCROLL, 15, None);
            } else {
                self.scroll = self.target as f32;
                KillTimer(self.hwnd, T_SCROLL);
            }
        }
        invalidate(self.hwnd);
    }

    fn scroll_tick(&mut self) {
        let d = self.target as f32 - self.scroll;
        if d.abs() < 0.6 {
            self.scroll = self.target as f32;
            unsafe { KillTimer(self.hwnd, T_SCROLL) };
        } else {
            self.scroll += d * 0.3;
        }
        self.refresh_hover();
        invalidate(self.hwnd);
    }

    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        let max = self.max_scroll();
        if max <= 0 {
            return None;
        }
        let track = Rect { l: self.cw - self.sc(16), t: self.sc(4), r: self.cw - self.sc(2), b: self.ch - self.sc(4) };
        let th = (track.h() * self.ch / self.content_h.max(1)).max(self.sc(32)).min(track.h());
        let ty = track.t + ((track.h() - th) as f32 * (self.scroll / max as f32)).round() as i32;
        let w = if self.sb_hover || matches!(self.drag, Drag::Thumb(_)) { self.sc(6) } else { self.sc(4) };
        let r = track.r - self.sc(2);
        Some((track, Rect { l: r - w, t: ty, r, b: ty + th }))
    }

    fn focus_rect(&self, f: Focus) -> Option<Rect> {
        match f {
            Focus::Row(id) => self.rows.iter().find(|r| r.d.id == id).map(|r| r.r),
            Focus::More => self.cats.first().and_then(|c| c.more),
        }
    }

    fn ensure_visible(&mut self, f: Focus) {
        let Some(r) = self.focus_rect(f) else { return };
        let m = self.sc(16);
        let (t, b) = (r.t - m, r.b + m);
        if t < self.target {
            self.scroll_to(t, true);
        } else if b > self.target + self.ch {
            self.scroll_to(b - self.ch, true);
        }
    }

    fn move_focus(&mut self, dir: i32) {
        if self.focus_order.is_empty() {
            return;
        }
        let n = self.focus_order.len() as i32;
        let next = match self.focus.and_then(|f| self.focus_order.iter().position(|x| *x == f)) {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None if dir > 0 => 0,
            None => n - 1,
        };
        let f = self.focus_order[next as usize];
        self.focus = Some(f);
        self.focus_visible = true;
        self.ensure_visible(f);
        invalidate(self.hwnd);
    }

    // -------------------------------------------------------------- hit testing

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let dy = -self.scroll_px();
        for (i, r) in self.rows.iter().enumerate() {
            if r.d.tier > 0 && shift(r.r, dy).contains(x, y) {
                return Some(Hit::Row(i));
            }
        }
        for rk in &self.racks {
            for (k, w) in rk.wells.iter().enumerate() {
                let id = SETS[rk.set].members[k];
                if self.state.tier(id) > 0 && shift(*w, dy).contains(x, y) {
                    return Some(Hit::Well(id));
                }
            }
        }
        if let Some(m) = self.cats.first().and_then(|c| c.more) {
            if shift(m, dy).contains(x, y) {
                return Some(Hit::More);
            }
        }
        None
    }

    fn refresh_hover(&mut self) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(self.hwnd, &mut pt);
        }
        let inside = pt.x >= 0 && pt.y >= 0 && pt.x < self.cw && pt.y < self.ch;
        self.hover = if inside && self.drag == Drag::None && self.fv.is_none() { self.hit(pt.x, pt.y) } else { None };
    }

    // -------------------------------------------------------------- medals

    /// Medals to draw this frame (client coordinates), visible ones only.
    fn slots(&self) -> Vec<Slot> {
        let dy = -self.scroll_px();
        let mut v = Vec::new();
        let (rp, wp) = (self.sc(64) as u32, self.sc(72) as u32);
        for row in &self.rows {
            let r = shift(row.r, dy);
            if row.d.tier > 0 && r.b > 0 && r.t < self.ch {
                v.push(Slot { key: (row.d.id, row.d.tier, rp), cx: r.l + self.sc(6) + self.sc(32), cy: r.t + r.h() / 2 });
            }
        }
        for rk in &self.racks {
            for (k, w) in rk.wells.iter().enumerate() {
                let id = SETS[rk.set].members[k];
                let tier = self.state.tier(id).min(7);
                let r = shift(*w, dy);
                if tier > 0 && r.b > 0 && r.t < self.ch {
                    v.push(Slot { key: (id, tier, wp), cx: (r.l + r.r) / 2, cy: r.t + self.sc(8) + self.sc(36) });
                }
            }
        }
        v
    }
}

// ------------------------------------------------------------------ drawing the tab

impl Ui {
    fn draw_page(&self, cv: &mut Canvas) {
        let c = self.col;
        cv.clear(c.bg);
        let dy = -self.scroll_px();
        let vis = |r: Rect| r.b + dy > 0 && r.t + dy < self.ch;
        if vis(self.head) {
            self.draw_head(cv, shift(self.head, dy));
        }
        for &(y, title, sub) in &self.heads {
            let y = y + dy;
            if y > -self.sc(40) && y < self.ch {
                let w = cv.text(self.f.mid, title, self.sc(26), y, c.fg);
                if !sub.is_empty() {
                    let ty = y + (cv.measure(self.f.mid, "Ag").1 - cv.measure(self.f.small, "Ag").1) - self.sc(1);
                    cv.text(self.f.small, sub, self.sc(26) + w + self.sc(12), ty, c.dim);
                }
            }
        }
        for rk in &self.racks {
            if vis(rk.r) {
                self.draw_rack(cv, rk, dy);
            }
        }
        if vis(self.gate) {
            self.draw_gate(cv, shift(self.gate, dy));
        }
        for cat in &self.cats {
            if vis(cat.r) {
                self.draw_cat(cv, cat, dy);
            }
        }
        if let Some((_, thumb)) = self.scrollbar() {
            pill(cv, thumb, with_alpha(c.dim, 0.7));
        }
    }

    fn draw_head(&self, cv: &mut Canvas, r: Rect) {
        let (c, f) = (self.col, &self.f);
        let rad = self.sc(8) as f32;
        cv.fill_round(r, rad, c.card);
        cv.stroke_round(r, rad, c.card_border);
        let s = &self.state;
        let (lv, into, need) = level_progress(total_xp(s));
        let rk = rank_of(lv, s.g);
        let rt = rk as u8 + 1;
        let tc = tier_col(rt, c.dark);
        let cy = (r.t + r.b) / 2;
        let cx = r.l + self.sc(16) + self.sc(32);
        let rr = self.sc(30) as f32;
        cv.ring(cx as f32, cy as f32, rr, self.sc(6) as f32, 1.0, c.track);
        cv.ring(cx as f32, cy as f32, rr, self.sc(6) as f32, (into as f32 / need.max(1) as f32).clamp(0.02, 1.0), tc);
        text_c(cv, f.big, &lv.to_string(), cx, cy, c.fg);
        let (th_b, th_n, th_s) = (cv.measure(f.big, "Ag").1, cv.measure(f.normal, "Ag").1, cv.measure(f.small, "Ag").1);
        let tx = cx + self.sc(30) + self.sc(16);
        let mut y = cy - (th_b + self.sc(2) + th_n + th_s) / 2;
        cv.text(f.big, &format!("{} \u{b7} {}", rk.name(), title_of(lv)), tx, y, tc);
        y += th_b + self.sc(2);
        cv.text(f.normal, &format!("{} / {} XP to level {}", fmt_n(into), fmt_n(need), lv + 1), tx, y, c.dim);
        y += th_n;
        let earned = BADGES.iter().filter(|b| badge_tier(s, b) > 0).count();
        cv.text(f.small, &format!("{} of {} badges earned", earned, BADGES.len()), tx, y, c.dim);
        let label = format!("{} {}", rk.name(), lv);
        let w = cv.measure(f.bold, &label).0 + self.sc(24);
        let pr = Rect { l: r.r - self.sc(16) - w, t: cy - self.sc(12), r: r.r - self.sc(16), b: cy + self.sc(12) };
        pill(cv, pr, tc);
        text_c(cv, f.bold, &label, (pr.l + pr.r) / 2, cy, on_color(tc));
    }

    fn draw_rack(&self, cv: &mut Canvas, rk: &RackL, dy: i32) {
        let f = &self.f;
        let r = shift(rk.r, dy);
        let s = &self.state;
        let set = &SETS[rk.set];
        let (ink, soft) = (hx(0xe8ebf0), hx(0x9aa3b0));
        let ear = self.sc(26);
        cv.fill_round(r, self.sc(6) as f32, hx(0x2b3037));
        cv.stroke_round(r, self.sc(6) as f32, hx(0x4b525c));
        for left in [true, false] {
            let e = if left { Rect { l: r.l + 1, t: r.t + 1, r: r.l + ear, b: r.b - 1 } } else { Rect { l: r.r - ear, t: r.t + 1, r: r.r - 1, b: r.b - 1 } };
            cv.fill_round(e, self.sc(5) as f32, hx(0x22262c));
            for q in [1, 3] {
                let (hw, hh) = (self.sc(11), self.sc(22));
                let (hx_, hy) = ((e.l + e.r) / 2 - hw / 2, r.t + r.h() * q / 4 - hh / 2);
                cv.fill_round(Rect { l: hx_, t: hy, r: hx_ + hw, b: hy + hh }, hw as f32 / 2.0, hx(0x05070a));
            }
        }
        let mid_l = rk.plate.l;
        let rec = s.ps.get(set.id).copied().unwrap_or(0).min(7);
        let ty = r.t + self.sc(12);
        let nw = cv.text(f.mid, set.name, mid_l + self.sc(4), ty, hx(0xcfd5dd));
        let led = self.sc(8);
        let ly = ty + cv.measure(f.mid, "Ag").1 / 2;
        let leds = [hx(0x59d98e), if rec > 0 { hx(0x59d98e) } else { hx(0x3a2a2e) }, hx(0xeac35e)];
        for (i, col) in leds.iter().enumerate() {
            let lx = mid_l + self.sc(4) + nw + self.sc(14) + i as i32 * (led + self.sc(6));
            cv.fill_round(Rect { l: lx, t: ly - led / 2, r: lx + led, b: ly - led / 2 + led }, led as f32 / 2.0, *col);
        }
        let (vw, vh, vg) = (self.sc(5), self.sc(14), self.sc(5));
        for i in 0..16 {
            let vx = rk.plate.r - self.sc(4) - (i + 1) * (vw + vg) + vg;
            cv.fill_round(Rect { l: vx, t: ly - vh / 2, r: vx + vw, b: ly - vh / 2 + vh }, vw as f32 / 2.0, hx(0x06080a));
        }
        for (k, w) in rk.wells.iter().enumerate() {
            let w = shift(*w, dy);
            let id = set.members[k];
            let tier = s.tier(id).min(7);
            cv.fill_round(w, self.sc(8) as f32, hx(0x12151a));
            let cx = (w.l + w.r) / 2;
            let my = w.t + self.sc(8) + self.sc(36);
            if tier == 0 {
                dashed_ring(cv, cx, my, self.sc(28), self.sc(2), hx(0x3a414b));
                text_c(cv, f.small, "0", cx, my, hx(0x5b6571));
            }
            let b = Catalog::get(id);
            let name = cv.ellipsize(f.bold, b.map_or(id, |b| b.name), w.w() - self.sc(6));
            let ny = w.t + self.sc(8) + self.sc(72) + self.sc(4);
            text_c(cv, f.bold, &name, cx, ny + cv.measure(f.bold, "Ag").1 / 2, ink);
            let sub = if tier > 0 { TIER_NAMES[tier as usize - 1] } else { "Not started" };
            text_c(cv, f.small, sub, cx, ny + cv.measure(f.bold, "Ag").1 + cv.measure(f.small, "Ag").1 / 2, soft);
        }
        // Name plate.
        let p = shift(rk.plate, dy);
        cv.fill_round(p, self.sc(5) as f32, hx(0xb2b8c1));
        cv.fill_round(Rect { b: p.t + p.h() / 2, ..p }, self.sc(5) as f32, hx(0xc6cbd3));
        let lead = if rec > 0 { format!("{} {}", TIER_NAMES[rec as usize - 1], RECORD_NOUN) } else { "No rack certified yet".to_string() };
        let plate_ink = hx(0x14181d);
        let pcy = (p.t + p.b) / 2;
        let label = cv.ellipsize(f.bold, &format!("{} \u{b7} {} \u{b7} Resource Monitor", lead, set.name), p.w() / 2);
        cv.text(f.bold, &label, p.l + self.sc(12), pcy - cv.measure(f.bold, "Ag").1 / 2, plate_ink);
        let tiers: Vec<u8> = set.members.iter().map(|m| s.tier(m)).collect();
        let cap = if s.g { 7 } else { 6 };
        let mut rx = p.r - self.sc(12);
        if rec >= cap {
            cv.text_right(f.small, "Complete", rx, pcy - cv.measure(f.small, "Ag").1 / 2, plate_ink);
        } else {
            let next = rec + 1;
            let cnt = tiers.iter().filter(|&&t| t >= next).count();
            let dot = self.sc(9);
            for t in tiers.iter().rev() {
                cv.fill_round(Rect { l: rx - dot, t: pcy - dot / 2, r: rx, b: pcy - dot / 2 + dot }, dot as f32 / 2.0, if *t >= next { plate_ink } else { hx(0x6c737d) });
                rx -= dot + self.sc(3);
            }
            rx -= self.sc(6);
            cv.text_right(f.small, &format!("Next: {} {}/4", TIER_NAMES[next as usize - 1], cnt), rx, pcy - cv.measure(f.small, "Ag").1 / 2, plate_ink);
        }
    }

    fn draw_gate(&self, cv: &mut Canvas, r: Rect) {
        let (c, f) = (self.col, &self.f);
        let rad = self.sc(8) as f32;
        cv.fill_round(r, rad, c.card);
        cv.stroke_round(r, rad, c.card_border);
        let pad = self.sc(16);
        let th_mid = cv.measure(f.mid, "Ag").1;
        let (dot, dy0) = (self.sc(14), r.t + self.sc(14));
        let dr = Rect { l: r.l + pad, t: dy0 + (th_mid - dot) / 2, r: r.l + pad + dot, b: dy0 + (th_mid - dot) / 2 + dot };
        cv.fill_round(dr, dot as f32 / 2.0, hx(0xb49cff));
        cv.fill_round(inflate(dr, -self.sc(3)), (dot / 2 - self.sc(3)).max(1) as f32, hx(0x8be9ff));
        cv.text(f.mid, "Emulsion gate", r.l + pad + dot + self.sc(10), dy0, c.fg);
        let th_n = cv.measure(f.normal, "Ag").1;
        for (i, l) in self.gate_sub.iter().enumerate() {
            cv.text(f.normal, l, r.l + pad, dy0 + th_mid + self.sc(6) + i as i32 * th_n, c.dim);
        }
        let g = gate_progress(&self.state);
        let fm = |p: (u64, u64), unit: &str| (format!("{} / {}{}", fmt_n(p.0), fmt_n(p.1), unit), p.0 >= p.1);
        let conds: [(&str, Vec<(String, bool)>); 4] = [
            ("Level 30 or higher", vec![(format!("Lv {} / {}", g.level.0, g.level.1), g.level.0 >= g.level.1)]),
            ("Time in the tool: 180 days of real use, in 6 different months", vec![fm(g.days, " days"), fm(g.months, " months")]),
            ("Depth: 3 ladders at Ruby", vec![fm(g.ruby_ladders, " ladders")]),
            (
                "Tool factor: 2,000 active hours watched and 5 TB of traffic tracked",
                vec![fm(g.active_hours, " hours"), (format!("{} / {} TB", fmt_tb(g.traffic_gb.0), fmt_tb(g.traffic_gb.1)), g.traffic_gb.0 >= g.traffic_gb.1)],
            ),
        ];
        let (th_b, th_s) = (cv.measure(f.bold, "Ag").1, cv.measure(f.small, "Ag").1);
        for (i, (name, vals)) in conds.iter().enumerate() {
            let top = r.t + self.gate_top_h + i as i32 * self.cond_h;
            cv.fill_rect(Rect { l: r.l + pad, t: top, r: r.r - pad, b: top + 1 }, c.line);
            let met = vals.iter().all(|v| v.1);
            let tk = self.sc(20);
            let tr = Rect { l: r.l + pad, t: top + self.sc(8), r: r.l + pad + tk, b: top + self.sc(8) + tk };
            if met {
                cv.fill_round(tr, tk as f32 / 2.0, with_alpha(c.ok, 0.25));
                cv.stroke_round(tr, tk as f32 / 2.0, c.ok);
                text_c(cv, f.bold, "\u{2713}", (tr.l + tr.r) / 2, (tr.t + tr.b) / 2, c.ok);
            } else {
                cv.stroke_round(tr, tk as f32 / 2.0, c.track);
                cv.stroke_round(inflate(tr, -1), tk as f32 / 2.0 - 1.0, c.track);
            }
            let tx = r.l + pad + tk + self.sc(10);
            cv.text(f.bold, name, tx, top + self.sc(8), c.fg);
            let mut vx = tx;
            for (txt, ok) in vals {
                let s = if *ok { format!("\u{2713} {}", txt) } else { txt.clone() };
                vx += cv.text(f.small, &s, vx, top + self.sc(8) + th_b + self.sc(2), if *ok { c.ok } else { c.dim }) + self.sc(16);
            }
            let _ = th_s;
        }
    }

    fn draw_cat(&self, cv: &mut Canvas, cat: &CatL, dy: i32) {
        let (c, f) = (self.col, &self.f);
        let r = shift(cat.r, dy);
        let rad = self.sc(8) as f32;
        cv.fill_round(r, rad, c.card);
        cv.stroke_round(r, rad, c.card_border);
        let th_mid = cv.measure(f.mid, "Ag").1;
        let ty = r.t + self.sc(12);
        cv.text(f.mid, cat.name, r.l + self.sc(14), ty, c.fg);
        cv.text_right(f.small, &format!("{} / {}", cat.got, cat.total), r.r - self.sc(14), ty + (th_mid - cv.measure(f.small, "Ag").1) - self.sc(2), c.dim);
        let (th_t, th_l, th_m) = (cv.measure(f.bold, "Ag").1, cv.measure(f.normal, "Ag").1, cv.measure(f.small, "Ag").1);
        let mut first = true;
        for (i, row) in self.rows.iter().enumerate() {
            if !cat.r.contains(row.r.l + 1, row.r.t + 1) {
                continue;
            }
            let rr = shift(row.r, dy);
            let was_first = first;
            first = false;
            if rr.b < 0 || rr.t > self.ch {
                continue;
            }
            let d = &row.d;
            if !was_first {
                cv.fill_rect(Rect { l: rr.l + self.sc(4), t: rr.t, r: rr.r - self.sc(4), b: rr.t + 1 }, c.line);
            }
            let hovered = self.hover == Some(Hit::Row(i));
            if hovered {
                cv.fill_round(rr, self.sc(8) as f32, c.hover);
            }
            if self.focus == Some(Focus::Row(d.id)) && self.focus_visible {
                focus_ring(cv, rr, self.sc(8) as f32, c.accent);
            }
            self.draw_row(cv, d, rr, th_t, th_l, th_m);
        }
        if let Some(m) = cat.more {
            let m = shift(m, dy);
            cv.fill_rect(Rect { l: m.l + self.sc(4), t: m.t - self.sc(2), r: m.r - self.sc(4), b: m.t - self.sc(2) + 1 }, c.line);
            if self.hover == Some(Hit::More) {
                cv.fill_round(m, self.sc(8) as f32, c.hover);
            }
            if self.focus == Some(Focus::More) && self.focus_visible {
                focus_ring(cv, m, self.sc(8) as f32, c.accent);
            }
            let label = if cat.expanded { "Show fewer".to_string() } else { format!("Show all {}", cat.total) };
            text_c(cv, f.bold, &label, (m.l + m.r) / 2, (m.t + m.b) / 2, c.accent);
        }
    }

    fn draw_row(&self, cv: &mut Canvas, d: &RowData, r: Rect, th_t: i32, th_l: i32, th_m: i32) {
        let (c, f) = (self.col, &self.f);
        let (title_y, line_y, bar_y, meta_y, _) = self.row_offsets(d, th_t, th_l, th_m);
        let mcx = r.l + self.sc(6) + self.sc(32);
        let mcy = r.t + r.h() / 2;
        if d.tier == 0 {
            dashed_ring(cv, mcx, mcy, self.sc(25), self.sc(2), if c.dark { hx(0x4a525e) } else { hx(0xb0b6bf) });
            if d.secret {
                text_c(cv, f.big, "?", mcx, mcy, c.dim);
            }
        }
        let tx = r.l + self.sc(6 + 64 + 10);
        let right = r.r - self.sc(10);
        let title_col = if d.tier > 0 { c.fg } else { c.dim };
        let tw = cv.text(f.bold, &d.title, tx, r.t + title_y, title_col);
        if !d.tag.is_empty() {
            cv.text(f.caps, &d.tag, tx + tw + self.sc(8), r.t + title_y + (th_t - cv.measure(f.caps, "Ag").1) - self.sc(2), c.dim);
        }
        if !d.count.is_empty() && d.tier > 0 {
            cv.text_right(f.small, &d.count, right, r.t + title_y + (th_t - th_m) - self.sc(1), c.dim);
        }
        for (i, l) in d.lines.iter().enumerate() {
            cv.text(f.normal, l, tx, r.t + line_y + i as i32 * th_l, c.dim);
        }
        let bw = right - tx;
        if let Some((frac, ct)) = d.bar {
            let bt = Rect { l: tx, t: r.t + bar_y, r: tx + bw, b: r.t + bar_y + self.sc(5) };
            cv.fill_round(bt, self.sc(3) as f32, c.track);
            let fw = ((bw as f32 * frac).round() as i32).max(if frac > 0.0 { self.sc(5) } else { 0 });
            if fw > 0 {
                let col = if ct == 0 { c.accent } else { tier_col(ct, c.dark) };
                cv.fill_round(Rect { r: tx + fw, ..bt }, self.sc(3) as f32, col);
            }
        }
        let my = r.t + meta_y;
        let mut nx = tx;
        if let Some(t) = d.pips {
            nx += pips(cv, tx, my + th_m / 2, self.sc(8), self.sc(4), t, &c) + self.sc(10);
        }
        if !d.chip.is_empty() {
            let w = cv.measure(f.small, &d.chip).0 + self.sc(14);
            let cr = Rect { l: right - w, t: my - self.sc(1), r: right, b: my + th_m + self.sc(1) };
            cv.stroke_round(cr, cr.h() as f32 / 2.0, c.line);
            text_c(cv, f.small, &d.chip, (cr.l + cr.r) / 2, (cr.t + cr.b) / 2, c.dim);
        }
        let max_w = (right - nx - if d.chip.is_empty() { 0 } else { cv.measure(f.small, &d.chip).0 + self.sc(24) }).max(self.sc(40));
        let note = cv.ellipsize(f.small, &d.note, max_w);
        cv.text(f.small, &note, nx, my, c.dim);
    }

    /// Composite the visible medals over the canvas copy in `surf`.
    fn draw_medals(&self, surf: &mut Surface) {
        let (w, h) = (surf.w, surf.h);
        let px = surf.px();
        for s in self.slots() {
            if let Some(tile) = self.medals.get(s.key) {
                let n = s.key.2 as i32;
                draw_tile(px, w, h, tile, n, (s.cx - n / 2) as f32 + n as f32 / 2.0, (s.cy - n / 2) as f32 + n as f32 / 2.0, 1.0, 1.0, 1.0, (0, h));
            }
        }
    }

    // -------------------------------------------------------------- painting

    fn paint(&mut self, dc: HDC) {
        let (w, h) = (self.cw.max(1), self.ch.max(1));
        let mut surf = match self.surf.take() {
            Some(s) if s.w == w && s.h == h => s,
            _ => match Surface::new(w, h) {
                Some(s) => {
                    self.snap_ok = false;
                    s
                }
                None => return,
            },
        };
        let reqs: Vec<MedalKey> = self.slots().iter().map(|s| s.key).collect();
        self.medals.ensure(&reqs);
        let mut cv = std::mem::replace(&mut self.cv, Canvas::new());
        cv.resize(w, h);
        if self.fv.is_some() {
            if !self.snap_ok || self.snap.len() != (w * h) as usize {
                self.draw_page(&mut cv);
                surf.load(&cv);
                self.draw_medals(&mut surf);
                dim_pixels(surf.px(), 150);
                self.snap = surf.px().to_vec();
                self.snap_ok = true;
            } else {
                surf.px().copy_from_slice(&self.snap);
            }
            if self.ov.as_ref().map_or(true, |o| o.w != w || o.h != h) {
                self.ov = Surface::new(w, h);
            }
            if let Some(mut ov) = self.ov.take() {
                self.draw_fv(&mut cv, &mut surf, &mut ov);
                self.ov = Some(ov);
            }
        } else {
            self.draw_page(&mut cv);
            surf.load(&cv);
            self.draw_medals(&mut surf);
        }
        surf.blit_to(dc);
        self.cv = cv;
        self.surf = Some(surf);
    }
}

// ------------------------------------------------------------------ full view

/// Back of the medal (spec sheet): face, rim, ink.
fn back_colors(tier: u8) -> (Rgba, Rgba, Rgba, Rgba) {
    let (a, b, ink, rim) = match tier {
        1 => (0xdca06a, 0xa8662f, 0x1c1209, 0x000000),
        2 => (0xeef1f5, 0xb4bcc7, 0x14181d, 0x000000),
        3 => (0xf5d884, 0xc4963a, 0x1d1507, 0x000000),
        4 => (0xd6f1f0, 0x8cc6c3, 0x0f1d1d, 0x000000),
        5 => (0x20232a, 0x101215, 0xe2f0ff, 0x9fd7ff),
        6 => (0x26151a, 0x12090b, 0xffe3e6, 0xff4757),
        _ => (0x17161e, 0x0d0c12, 0xf4f1ff, 0xb49cff),
    };
    let rim_c = if rim == 0 { Rgba(0, 0, 0, 70) } else { hx(rim) };
    (hx(a).mix(hx(b), 0.3), hx(b), hx(ink), rim_c)
}

fn rm_number(e: &Entry) -> String {
    format!("RM-{:04X}", hash32(&format!("{}:{}:{}", e.id, e.tier, e.earned_ms)) % 65536)
}

enum Cl {
    T(String, HFONT, Rgba, i32),
    Pips(u8, i32),
    Rule(i32),
}

impl Ui {
    fn fv_geom(&self, tier: u8) -> FvGeom {
        let sc = |v: i32| self.sc(v);
        let mut s = ((self.cw as f32 * 0.44).min(self.ch as f32 * 0.64) as i32).min(sc(520));
        if tier == 7 {
            s = s.min(sc(360));
        }
        s = s.max(sc(200));
        let (gap, mut card_w) = (sc(28), sc(320));
        let x0 = ((self.cw - (s + gap + card_w)) / 2).max(sc(64));
        let top = (self.ch - s) / 2;
        let card_l = x0 + s + gap;
        card_w = card_w.min(self.cw - card_l - sc(16)).max(sc(180));
        FvGeom {
            stage: Rect { l: x0, t: top, r: x0 + s, b: top + s },
            card_l,
            card_w,
            prev: Rect { l: sc(12), t: self.ch / 2 - sc(32), r: sc(12) + sc(46), b: self.ch / 2 + sc(32) },
            next: Rect { l: self.cw - sc(12) - sc(46), t: self.ch / 2 - sc(32), r: self.cw - sc(12), b: self.ch / 2 + sc(32) },
            close: Rect { l: self.cw - sc(16) - sc(42), t: sc(14), r: self.cw - sc(16), b: sc(14) + sc(42) },
        }
    }

    fn fv_tier(&self) -> u8 {
        self.fv.as_ref().and_then(|f| Catalog::get(f.ids[f.i])).map_or(1, |b| badge_tier(&self.state, b).max(1))
    }

    fn open_fv(&mut self, id: &'static str) {
        let ids = self.earned_ids();
        let Some(i) = ids.iter().position(|x| *x == id) else { return };
        let (reduced, now) = (reduced_motion(), now_s());
        self.fv = Some(Fv {
            ids,
            i,
            flip: 0.0,
            flip_t: 0.0,
            rx: 0.0,
            ry: 0.0,
            ptr: None,
            moved: 0.0,
            t0: now,
            last: now,
            focus: 0,
            hover: 0,
            reduced,
            frame: Vec::new(),
            frame_key: None,
        });
        self.hover = None;
        self.snap_ok = false;
        self.fv_render();
        if !reduced {
            unsafe { SetTimer(self.hwnd, T_FV, 16, None) };
        }
        invalidate(self.hwnd);
    }

    fn close_fv(&mut self) {
        if self.fv.take().is_some() {
            unsafe { KillTimer(self.hwnd, T_FV) };
            self.snap = Vec::new();
            self.snap_ok = false;
            self.refresh_hover();
            invalidate(self.hwnd);
        }
    }

    /// Render the live medal for the current tilt and time.
    fn fv_render(&mut self) {
        let Some(fv) = self.fv.as_ref() else { return };
        let (id, reduced, rx, ry, t0, key_old) = (fv.ids[fv.i], fv.reduced, fv.rx, fv.ry, fv.t0, fv.frame_key);
        let tier = self.fv_tier();
        let px = self.fv_geom(tier).stage.w() as u32;
        let key = (id, tier, px);
        if !gpu::available() && key_old == Some(key) {
            return;
        }
        // metals and stones move at a third of real time (calm); the bubble keeps its own pace
        let pace = if tier >= 7 { 1.0 } else { 1.0 / 3.0 };
        let t = if reduced { gpu::still_time(material_of(tier)) } else { (now_s() - t0) as f32 * pace };
        let frame = self.medals.frame(id, tier, px, t, (ry / 22.0 * 0.5, rx / 16.0 * 0.5));
        if let Some(fv) = self.fv.as_mut() {
            fv.frame = frame;
            fv.frame_key = Some(key);
        }
    }

    /// Where the pointer (or the idle sway) wants the medal to tilt: (x degrees, y degrees).
    fn fv_target(&self) -> (f32, f32) {
        let Some(fv) = self.fv.as_ref() else { return (0.0, 0.0) };
        let g = self.fv_geom(self.fv_tier());
        let now = now_s();
        // metals and stones: a third of the hover tilt and idle sway (calm); the bubble keeps its own
        let amp = if self.fv_tier() >= 7 { 1.0 } else { 1.0 / 3.0 };
        match fv.ptr {
            Some((px, py)) if now - fv.moved < 4.0 => {
                let (cx, cy) = ((g.stage.l + g.stage.r) as f32 / 2.0, (g.stage.t + g.stage.b) as f32 / 2.0);
                let ty = ((px as f32 - cx) / (self.cw as f32 / 2.0)).clamp(-1.0, 1.0) * 22.0 * amp;
                let tx = -((py as f32 - cy) / (self.ch as f32 / 2.0)).clamp(-1.0, 1.0) * 16.0 * amp;
                (tx, ty)
            }
            _ if fv.reduced => (0.0, 0.0),
            _ => {
                let t = (now - fv.t0) as f32;
                (3.5 * amp * (0.7 * t).sin(), 9.0 * amp * (0.5 * t).sin())
            }
        }
    }

    fn fv_tick(&mut self) {
        if self.fv.is_none() {
            return;
        }
        let (tx, ty) = self.fv_target();
        let now = now_s();
        if let Some(fv) = self.fv.as_mut() {
            let dt = ((now - fv.last) as f32).clamp(0.0, 0.05);
            fv.last = now;
            let k = 1.0 - (-dt * 16.0).exp();
            fv.rx += (tx - fv.rx) * k;
            fv.ry += (ty - fv.ry) * k;
            fv.flip += (fv.flip_t - fv.flip) * (1.0 - (-dt * 6.5).exp());
            if (fv.flip_t - fv.flip).abs() < 0.004 {
                fv.flip = fv.flip_t;
            }
        }
        self.fv_render();
        invalidate(self.hwnd);
    }

    fn fv_flip(&mut self) {
        if let Some(fv) = self.fv.as_mut() {
            fv.flip_t = 1.0 - fv.flip_t;
            if fv.reduced {
                fv.flip = fv.flip_t;
            }
        }
        invalidate(self.hwnd);
    }

    fn fv_step(&mut self, d: i32) {
        if let Some(fv) = self.fv.as_mut() {
            let n = fv.ids.len() as i32;
            if n < 2 {
                return;
            }
            fv.i = (fv.i as i32 + d).rem_euclid(n) as usize;
            fv.flip = 0.0;
            fv.flip_t = 0.0;
            fv.frame_key = None;
        }
        self.fv_render();
        invalidate(self.hwnd);
    }

    fn fv_region(&self, x: i32, y: i32) -> u8 {
        let g = self.fv_geom(self.fv_tier());
        let n = self.fv.as_ref().map_or(0, |f| f.ids.len());
        if g.close.contains(x, y) {
            return 4;
        }
        if n > 1 && g.prev.contains(x, y) {
            return 2;
        }
        if n > 1 && g.next.contains(x, y) {
            return 3;
        }
        let (cx, cy) = ((g.stage.l + g.stage.r) / 2, (g.stage.t + g.stage.b) / 2);
        let r = g.stage.w() / 2;
        if (x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r {
            return 1;
        }
        if x >= g.card_l && x < g.card_l + g.card_w {
            return 5; // the info card: inert
        }
        0
    }

    fn draw_fv(&self, cv: &mut Canvas, surf: &mut Surface, ov: &mut Surface) {
        let Some(fv) = self.fv.as_ref() else { return };
        let Some(e) = self.entry(fv.ids[fv.i]).filter(|e| e.tier > 0) else { return };
        let g = self.fv_geom(e.tier);
        let f = &self.f;
        let sc = |v: i32| self.sc(v);
        let (ink, soft) = (Rgba::rgb(240, 242, 245), Rgba::rgb(176, 184, 196));
        let accent = self.col.accent;
        cv.clear(Rgba(0, 0, 0, 0));

        // Buttons.
        let n = fv.ids.len();
        let btn = |cv: &mut Canvas, r: Rect, label: &str, font: HFONT, which: u8, id: u8| {
            let rad = if id == 4 { r.h() as f32 / 2.0 } else { sc(12) as f32 };
            cv.fill_round(r, rad, if fv.hover == id { Rgba(48, 54, 64, 235) } else { Rgba(21, 24, 29, 217) });
            cv.stroke_round(r, rad, Rgba(0x4a, 0x52, 0x5e, 255));
            text_c(cv, font, label, (r.l + r.r) / 2, (r.t + r.b) / 2, ink);
            if fv.focus == which && self.focus_visible {
                focus_ring(cv, inflate(r, sc(2)), rad + 2.0, accent);
            }
        };
        btn(cv, g.close, "\u{2715}", f.mid, 3, 4);
        if n > 1 {
            btn(cv, g.prev, "\u{2039}", self.x.title, 1, 2);
            btn(cv, g.next, "\u{203a}", self.x.title, 2, 3);
        }

        // Medal: tilt squashes it a little, the flip squashes it to a line and swaps to the back.
        let s = g.stage.w();
        let (cx, cy) = (((g.stage.l + g.stage.r) / 2) as f32, ((g.stage.t + g.stage.b) / 2) as f32);
        let ang = fv.flip * std::f32::consts::PI;
        let c = ang.cos();
        let sxm = (c.abs() * fv.ry.to_radians().cos()).max(0.02);
        let sym = fv.rx.to_radians().cos();
        let sh = Rect {
            l: (cx - s as f32 * 0.35 * (1.0 - 0.28 * ang.sin().abs())) as i32,
            t: (cy + s as f32 * 0.5 + s as f32 * 0.02) as i32,
            r: (cx + s as f32 * 0.35 * (1.0 - 0.28 * ang.sin().abs())) as i32,
            b: (cy + s as f32 * 0.5 + s as f32 * 0.02) as i32 + (s as f32 * 0.05) as i32,
        };
        cv.fill_round(sh, sh.h() as f32 / 2.0, Rgba(0, 0, 0, 100));
        if c < 0.0 {
            self.draw_back(cv, &e, cx, cy, s, sxm, sym);
        }
        if fv.focus == 0 && self.focus_visible {
            cv.ring(cx, cy, s as f32 / 2.0 + sc(8) as f32, sc(3) as f32, 1.0, accent);
        }

        // Info card.
        let pad = sc(18);
        let inner = g.card_w - 2 * pad;
        let mdc = self.mdc;
        let tcol = tier_col(e.tier, true);
        let mut items: Vec<Cl> = vec![
            Cl::T(format!("{} of {}", fv.i + 1, n), f.small, soft, 0),
            Cl::T(format!("UPTIME \u{b7} {}", TIER_NAMES[e.tier as usize - 1].to_uppercase()), f.caps, tcol, sc(4)),
            Cl::T(cv.ellipsize(self.x.title, &e.title, inner), self.x.title, ink, sc(2)),
        ];
        for (i, l) in wrap_dc(mdc, f.normal, &e.line, inner, 3).into_iter().enumerate() {
            items.push(Cl::T(l, f.normal, soft, if i == 0 { sc(4) } else { 0 }));
        }
        items.push(Cl::Pips(e.tier, sc(10)));
        for (i, nline) in e.notes.iter().enumerate() {
            items.push(Cl::T(cv.ellipsize(f.normal, nline, inner), f.normal, if i + 1 == e.notes.len() { soft } else { ink }, if i == 0 { sc(10) } else { sc(2) }));
        }
        items.push(Cl::Rule(sc(12)));
        let hint = "Tilt with the pointer. Click, Enter or Space turns the medal to its spec sheet. \u{2190} \u{2192} go through the list, Esc closes.";
        for (i, l) in wrap_dc(mdc, f.small, hint, inner, 4).into_iter().enumerate() {
            items.push(Cl::T(l, f.small, soft, if i == 0 { sc(8) } else { 0 }));
        }
        let th = |font: HFONT| text_h(mdc, font);
        let total: i32 = items
            .iter()
            .map(|it| match it {
                Cl::T(_, font, _, gap) => gap + th(*font),
                Cl::Pips(_, gap) => gap + sc(8),
                Cl::Rule(gap) => gap + 1,
            })
            .sum::<i32>()
            + 2 * pad;
        let top = ((self.ch - total) / 2).max(sc(8));
        let card = Rect { l: g.card_l, t: top, r: g.card_l + g.card_w, b: top + total };
        cv.fill_round(card, sc(12) as f32, Rgba(21, 24, 29, 224));
        cv.stroke_round(card, sc(12) as f32, Rgba(0x3a, 0x42, 0x4e, 255));
        let mut y = card.t + pad;
        let pc = Colors { dark: true, track: Rgba(255, 255, 255, 0x2a), ..self.col };
        for it in &items {
            match it {
                Cl::T(s, font, col, gap) => {
                    y += gap;
                    cv.text(*font, s, card.l + pad, y, *col);
                    y += th(*font);
                }
                Cl::Pips(t, gap) => {
                    y += gap;
                    pips(cv, card.l + pad, y + sc(4), sc(8), sc(4), *t, &pc);
                    y += sc(8);
                }
                Cl::Rule(gap) => {
                    y += gap;
                    cv.fill_rect(Rect { l: card.l + pad, t: y, r: card.r - pad, b: y + 1 }, Rgba(255, 255, 255, 0x22));
                    y += 1;
                }
            }
        }

        ov.load(cv);
        surf.over_surface(ov);
        if c >= 0.0 && !fv.frame.is_empty() {
            let (w, h) = (surf.w, surf.h);
            let px = fv.frame_key.map_or(s, |k| k.2 as i32);
            draw_tile(surf.px(), w, h, &fv.frame, px, cx, cy, sxm, sym, 1.0, (0, h));
        }
    }

    /// The spec sheet on the back of the medal: a round card squashed like the medal.
    fn draw_back(&self, cv: &mut Canvas, e: &Entry, cx: f32, cy: f32, s: i32, sxm: f32, sym: f32) {
        let f = &self.f;
        let sc = |v: i32| self.sc(v);
        let (face, edge, ink, rim) = back_colors(e.tier);
        let (w, h) = ((s as f32 * sxm) as i32, (s as f32 * sym) as i32);
        let r = Rect { l: (cx - w as f32 / 2.0) as i32, t: (cy - h as f32 / 2.0) as i32, r: (cx + w as f32 / 2.0) as i32, b: (cy + h as f32 / 2.0) as i32 };
        let rad = (w.min(h) / 2) as f32;
        cv.fill_round(r, rad, edge);
        cv.fill_round(inflate(r, -sc(3)), (rad - sc(3) as f32).max(0.0), face);
        cv.stroke_round(inflate(r, -sc(3)), (rad - sc(3) as f32).max(0.0), rim);
        if sxm < 0.9 || sym < 0.9 {
            return;
        }
        let dim = ink.with_a(190);
        let inner = (s as f32 * 0.6) as i32;
        let mdc = self.mdc;
        let big = s >= sc(300);
        let mut lines: Vec<(String, HFONT, Rgba)> = vec![
            ("Resource Monitor".to_string(), f.bold, ink),
            ("CERTIFICATE OF ACHIEVEMENT".to_string(), f.caps, dim),
            (e.title.clone(), self.x.title, ink),
        ];
        if big {
            for l in wrap_dc(mdc, f.normal, &e.line, inner, 2) {
                lines.push((l, f.normal, dim));
            }
        }
        let th = |font: HFONT| text_h(mdc, font);
        let total_h = lines.iter().map(|l| th(l.1) + sc(2)).sum::<i32>();
        let dl_h = th(f.caps) + th(f.normal) + sc(14);
        let mut note: Vec<String> = Vec::new();
        if big {
            note = wrap_dc(mdc, f.small, "Earned by using Resource Monitor, never bought. Only confirmed changes count.", inner, 3);
        }
        let extra = th(f.small) + sc(6) + note.len() as i32 * th(f.small) + if big { th(f.caps) + sc(6) } else { 0 };
        let all = total_h + dl_h + extra + sc(10);
        let mut y = cy as i32 - all / 2;
        let ccx = cx as i32;
        for (t, font, col) in &lines {
            text_c(cv, *font, &cv.ellipsize(*font, t, inner), ccx, y + th(*font) / 2, *col);
            y += th(*font) + sc(2);
        }
        y += sc(6);
        cv.fill_rect(Rect { l: ccx - inner / 2, t: y, r: ccx + inner / 2, b: y + 1 }, dim);
        y += sc(4);
        let earned = fmt_date(e.earned_ms, self.tz);
        let cols = [("TIER", TIER_NAMES[e.tier as usize - 1].to_string()), ("EARNED", earned), ("No.", rm_number(e))];
        for (i, (k, v)) in cols.iter().enumerate() {
            let x = ccx + (i as i32 - 1) * inner / 3;
            text_c(cv, f.caps, k, x, y + th(f.caps) / 2, dim);
            text_c(cv, f.normal, v, x, y + th(f.caps) + th(f.normal) / 2, ink);
        }
        y += th(f.caps) + th(f.normal) + sc(4);
        cv.fill_rect(Rect { l: ccx - inner / 2, t: y, r: ccx + inner / 2, b: y + 1 }, dim);
        y += sc(6);
        let stat = if e.count.is_empty() { format!("{} XP", e.xp) } else { format!("Count {} \u{b7} {} XP", e.count, e.xp) };
        text_c(cv, f.small, &stat, ccx, y + th(f.small) / 2, ink);
        y += th(f.small) + sc(6);
        for l in &note {
            text_c(cv, f.small, l, ccx, y + th(f.small) / 2, dim);
            y += th(f.small);
        }
        if big {
            y += sc(6);
            text_c(cv, f.caps, "RESOURCE MONITOR BY RAJHANS", ccx, y + th(f.caps) / 2, dim);
        }
    }
}

// ------------------------------------------------------------------ input

fn open_badge(id: &'static str) {
    if let Some(u) = ui() {
        u.open_fv(id);
    }
}

fn toggle_more() {
    if let Some(u) = ui() {
        u.expanded = !u.expanded;
        u.layout();
        u.snap_ok = false;
        u.refresh_hover();
        invalidate(u.hwnd);
    }
}

fn activate_focus(f: Focus) {
    match f {
        Focus::Row(id) => open_badge(id),
        Focus::More => toggle_more(),
    }
}

fn on_key(hwnd: HWND, key: u16) {
    let shift_down = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
    let Some(u) = ui() else { return };
    if u.fv.is_some() {
        match key {
            VK_ESCAPE => u.close_fv(),
            VK_LEFT => u.fv_step(-1),
            VK_RIGHT => u.fv_step(1),
            VK_TAB => {
                let n = u.fv.as_ref().map_or(0, |f| f.ids.len());
                let order: Vec<u8> = if n > 1 { vec![0, 1, 2, 3] } else { vec![0, 3] };
                let cur = u.fv.as_ref().map_or(0, |f| f.focus);
                let i = order.iter().position(|x| *x == cur).unwrap_or(0) as i32;
                let next = order[(i + if shift_down { -1 } else { 1 }).rem_euclid(order.len() as i32) as usize];
                u.focus_visible = true;
                if let Some(fv) = u.fv.as_mut() {
                    fv.focus = next;
                }
            }
            VK_RETURN | VK_SPACE => {
                u.focus_visible = true;
                match u.fv.as_ref().map_or(0, |f| f.focus) {
                    0 => u.fv_flip(),
                    1 => u.fv_step(-1),
                    2 => u.fv_step(1),
                    _ => u.close_fv(),
                }
            }
            _ => {}
        }
        invalidate(hwnd);
        return;
    }
    match key {
        VK_ESCAPE => unsafe {
            DestroyWindow(hwnd);
        },
        VK_TAB => u.move_focus(if shift_down { -1 } else { 1 }),
        VK_RETURN | VK_SPACE => {
            if let Some(f) = u.focus {
                u.focus_visible = true;
                activate_focus(f);
            }
        }
        VK_UP | VK_LEFT | VK_DOWN | VK_RIGHT | VK_HOME | VK_END | VK_PRIOR | VK_NEXT => {
            let dir = if matches!(key, VK_UP | VK_LEFT) { -1 } else { 1 };
            match key {
                VK_UP | VK_LEFT | VK_DOWN | VK_RIGHT if !u.focus_order.is_empty() => u.move_focus(dir),
                _ => {
                    let page = (u.ch - u.sc(WHEEL_STEP)).max(u.sc(WHEEL_STEP));
                    let t = match key {
                        VK_UP => u.target - u.sc(WHEEL_STEP),
                        VK_DOWN => u.target + u.sc(WHEEL_STEP),
                        VK_PRIOR => u.target - page,
                        VK_NEXT => u.target + page,
                        VK_HOME => 0,
                        VK_END => u.max_scroll(),
                        _ => u.target,
                    };
                    u.scroll_to(t, true);
                }
            }
        }
        _ => {}
    }
    invalidate(hwnd);
}

fn on_wheel(delta: i32) {
    let Some(u) = ui() else { return };
    if u.fv.is_some() {
        return;
    }
    let t = u.target - delta * u.sc(WHEEL_STEP) / 120;
    u.scroll_to(t, true);
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    if !u.tracking {
        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        u.tracking = unsafe { TrackMouseEvent(&mut t) } != 0;
    }
    if u.fv.is_some() {
        let region = u.fv_region(x, y);
        let hover = if region == 5 || region == 0 { 0 } else { region };
        let reduced = u.fv.as_ref().is_some_and(|f| f.reduced);
        let now = now_s();
        if let Some(fv) = u.fv.as_mut() {
            fv.ptr = Some((x, y));
            fv.moved = now;
            fv.hover = hover;
        }
        if reduced {
            let (tx, ty) = u.fv_target();
            if let Some(fv) = u.fv.as_mut() {
                fv.rx = tx;
                fv.ry = ty;
            }
            u.fv_render();
        }
        if reduced || hover != 0 {
            invalidate(hwnd);
        }
        return;
    }
    if let Drag::Thumb(grab) = u.drag {
        if let Some((track, thumb)) = u.scrollbar() {
            let span = (track.h() - thumb.h()).max(1);
            let frac = ((y - grab - track.t) as f32 / span as f32).clamp(0.0, 1.0);
            let t = (frac * u.max_scroll() as f32).round() as i32;
            u.scroll_to(t, false);
        }
        return;
    }
    let hover = u.hit(x, y);
    let sb = u.scrollbar().is_some_and(|(t, _)| t.contains(x, y));
    if hover != u.hover || sb != u.sb_hover {
        u.hover = hover;
        u.sb_hover = sb;
        invalidate(hwnd);
    }
}

fn on_button_down(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    u.focus_visible = false;
    if u.fv.is_some() {
        match u.fv_region(x, y) {
            4 => u.close_fv(),
            2 => u.fv_step(-1),
            3 => u.fv_step(1),
            1 => {
                if let Some(fv) = u.fv.as_mut() {
                    fv.focus = 0;
                }
                u.fv_flip();
            }
            0 => u.close_fv(),
            _ => {}
        }
        invalidate(hwnd);
        return;
    }
    if let Some((track, thumb)) = u.scrollbar() {
        if track.contains(x, y) {
            if y >= thumb.t && y < thumb.b {
                u.drag = Drag::Thumb(y - thumb.t);
            } else {
                let page = (u.ch - u.sc(WHEEL_STEP)).max(u.sc(WHEEL_STEP));
                let t = if y < thumb.t { u.target - page } else { u.target + page };
                u.scroll_to(t, true);
            }
            unsafe { SetCapture(hwnd) };
            invalidate(hwnd);
            return;
        }
    }
    match u.hit(x, y) {
        Some(Hit::Row(i)) => {
            let id = u.rows[i].d.id;
            u.focus = Some(Focus::Row(id));
            open_badge(id);
        }
        Some(Hit::Well(id)) => open_badge(id),
        Some(Hit::More) => {
            u.focus = Some(Focus::More);
            toggle_more();
        }
        None => u.focus = None,
    }
    invalidate(hwnd);
}

fn on_button_up(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    if u.drag != Drag::None {
        u.drag = Drag::None;
        unsafe { ReleaseCapture() };
    }
    u.sb_hover = u.scrollbar().is_some_and(|(t, _)| t.contains(x, y));
    invalidate(hwnd);
}

fn apply_theme(hwnd: HWND) {
    let Some(u) = ui() else { return };
    u.col = make_colors(u.col.dark, u.mica);
    style_window(hwnd, u.col.dark);
    u.snap_ok = false;
    unsafe { RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_FRAME) };
}

const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;

/// Size the window for `dpi` (never larger than the work area) and centre it.
fn fit(hwnd: HWND, dpi: u32) {
    unsafe {
        let sc = |v: i32| sc_dpi(v, dpi);
        let mut rc = RECT { left: 0, top: 0, right: sc(CLIENT_W), bottom: sc(CLIENT_H) };
        AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi);
        let mut work: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0);
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let (w, h) = ((rc.right - rc.left).min(ww), (rc.bottom - rc.top).min(wh));
        let x = work.left + (ww - w) / 2;
        let y = work.top + ((wh - h) / 2).max(0);
        SetWindowPos(hwnd, std::ptr::null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

unsafe fn setting_name(l: LPARAM) -> String {
    let p = l as *const u16;
    if p.is_null() {
        return String::new();
    }
    let mut v = Vec::new();
    for i in 0..63 {
        let c = *p.add(i);
        if c == 0 {
            break;
        }
        v.push(c);
    }
    String::from_utf16_lossy(&v)
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let Some(u) = ui() else { return -1 };
            u.hwnd = hwnd;
            let dpi = GetDpiForWindow(hwnd);
            if dpi != 0 && dpi != u.dpi {
                u.set_dpi(dpi);
            }
            u.mica = enable_mica(hwnd);
            apply_theme(hwnd);
            if let Some(u) = ui() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                u.cw = rc.right;
                u.ch = rc.bottom;
                u.layout();
            }
            0
        }
        WM_SIZE => {
            if let Some(u) = ui() {
                u.cw = (l & 0xFFFF) as i32;
                u.ch = ((l >> 16) & 0xFFFF) as i32;
                u.layout();
                u.snap_ok = false;
            }
            invalidate(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            if let Some(u) = ui() {
                u.paint(dc);
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_SETCURSOR if (l & 0xFFFF) as u32 == HTCLIENT => {
            let hand = ui().is_some_and(|u| match &u.fv {
                Some(f) => f.hover != 0,
                None => u.hover.is_some(),
            });
            SetCursor(LoadCursorW(std::ptr::null_mut(), if hand { IDC_HAND } else { IDC_ARROW }));
            1
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, get_x(l), get_y(l));
            0
        }
        WM_MOUSELEAVE_ => {
            if let Some(u) = ui() {
                u.tracking = false;
                if u.drag == Drag::None && (u.hover.is_some() || u.sb_hover || u.fv.as_ref().is_some_and(|f| f.hover != 0)) {
                    u.hover = None;
                    u.sb_hover = false;
                    if let Some(f) = u.fv.as_mut() {
                        f.hover = 0;
                    }
                    invalidate(hwnd);
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            on_button_down(hwnd, get_x(l), get_y(l));
            0
        }
        WM_LBUTTONUP => {
            on_button_up(hwnd, get_x(l), get_y(l));
            0
        }
        WM_CAPTURECHANGED => {
            if let Some(u) = ui() {
                u.drag = Drag::None;
            }
            0
        }
        WM_MOUSEWHEEL => {
            on_wheel(((w >> 16) & 0xFFFF) as u16 as i16 as i32);
            0
        }
        WM_KEYDOWN => {
            on_key(hwnd, w as u16);
            0
        }
        WM_TIMER => {
            match w {
                T_SCROLL => {
                    if let Some(u) = ui() {
                        u.scroll_tick();
                    }
                }
                T_FV => {
                    if let Some(u) = ui() {
                        u.fv_tick();
                    }
                }
                _ => {}
            }
            0
        }
        WM_SETTINGCHANGE => {
            if setting_name(l) == "ImmersiveColorSet" {
                apply_theme(hwnd);
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            apply_theme(hwnd);
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_DPICHANGED => {
            let dpi = ((w >> 16) & 0xFFFF) as u32;
            let r = &*(l as *const RECT);
            if let Some(u) = ui() {
                u.set_dpi(dpi);
            }
            SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(u) = ui() {
                u.layout();
                u.fv_render();
            }
            invalidate(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, T_SCROLL);
            KillTimer(hwnd, T_FV);
            0
        }
        WM_NCDESTROY => {
            free_ui();
            DefWindowProcW(hwnd, msg, w, l)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

// ------------------------------------------------------------------ public API (window)

/// Register the window classes of the Achievements window and its pop-ups.
pub fn register(hinst: HINSTANCE) {
    unsafe {
        let cls = wide(CLASS_MAIN);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: hinst,
            hIcon: LoadIconW(hinst, 1 as _),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let cls = wide(CLASS_POP);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(pop_proc),
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_HAND),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
    }
}

/// Open the Achievements window, or focus the open one (it keeps its own copy of the state).
pub fn open(_owner: HWND, state: &State, dark: bool) {
    unsafe {
        if let Some(u) = ui() {
            let hwnd = u.hwnd;
            u.state = state.clone();
            u.layout();
            u.snap_ok = false;
            let changed_theme = u.col.dark != dark;
            if changed_theme {
                u.col = make_colors(dark, u.mica);
                style_window(hwnd, dark);
            }
            invalidate(hwnd);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
            return;
        }
        let hinst = GetModuleHandleW(std::ptr::null());
        let dpi = GetDpiForSystem().max(96);
        free_ui();
        MAIN = Box::into_raw(Box::new(Ui::new(dpi, dark, state.clone())));
        let cls = wide(CLASS_MAIN);
        let title = wide("Achievements");
        let hwnd = CreateWindowExW(0, cls.as_ptr(), title.as_ptr(), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, std::ptr::null_mut(), std::ptr::null_mut(), hinst, std::ptr::null());
        if hwnd.is_null() {
            free_ui();
            return;
        }
        fit(hwnd, ui().map_or(96, |u| u.dpi));
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
    }
}

/// Update an open Achievements window with a new state (nothing happens when it is closed).
pub fn refresh(state: &State) {
    let Some(u) = ui() else { return };
    u.state = state.clone();
    u.layout();
    u.snap_ok = false;
    if let Some(fv) = u.fv.as_ref() {
        let cur = fv.ids[fv.i];
        let ids = u.earned_ids();
        match ids.iter().position(|x| *x == cur) {
            Some(i) => {
                if let Some(fv) = u.fv.as_mut() {
                    fv.ids = ids;
                    fv.i = i;
                    fv.frame_key = None;
                }
                u.fv_render();
            }
            None => u.close_fv(),
        }
    }
    u.refresh_hover();
    invalidate(u.hwnd);
}

/// Follow a theme change (dark / light) of the app.
pub fn set_dark(dark: bool) {
    if let Some(u) = ui() {
        if u.col.dark != dark {
            let hwnd = u.hwnd;
            u.col = make_colors(dark, u.mica);
            u.snap_ok = false;
            style_window(hwnd, dark);
            invalidate(hwnd);
        }
    }
}

// ------------------------------------------------------------------ unlock pop-ups

const T_ANIM: usize = 1;
const T_CLOSE: usize = 2;
const POP_W: i32 = 400;
const POP_H: i32 = 176;
const RISE_S: f64 = 1.1;

struct Cur {
    key: String,
    /// Medal: cache id, tier (1..=7).
    medal_id: &'static str,
    tier: u8,
    mpx: u32,
    eyebrow: String,
    title: String,
    tier_name: String,
    line: String,
    xp: u32,
    /// Level-up: (level, title, "New rank: X" or "").
    level: Option<(u32, String, String)>,
    t0: f64,
    dur: f64,
}

struct Pop {
    hwnd: HWND,
    dpi: u32,
    col: Colors,
    f: Fonts,
    x: Extra,
    mdc: HDC,
    cv: Canvas,
    surf: Option<Surface>,
    ov: Option<Surface>,
    state: State,
    queue: VecDeque<Moment>,
    cur: Option<Cur>,
    medals: Medals,
    reduced: bool,
}

static mut POP: *mut Pop = std::ptr::null_mut();

#[allow(static_mut_refs)]
fn pop() -> Option<&'static mut Pop> {
    unsafe { POP.as_mut() }
}

fn free_pop() {
    unsafe {
        let p = std::ptr::replace(std::ptr::addr_of_mut!(POP), std::ptr::null_mut());
        if !p.is_null() {
            let b = Box::from_raw(p);
            if !b.mdc.is_null() {
                DeleteDC(b.mdc);
            }
        }
    }
}

fn ease_back(t: f64) -> f32 {
    // cubic-bezier(.34, 1.45, .64, 1) look: ease-out with a small overshoot.
    let t = t.clamp(0.0, 1.0) as f32 - 1.0;
    let (c1, c3) = (1.3f32, 2.3f32);
    1.0 + c3 * t * t * t + c1 * t * t
}

fn moment_rank(m: &Moment) -> u8 {
    match m.kind.as_str() {
        "gate" => 0,
        "record" => 1,
        "badge" => 2,
        _ => 3,
    }
}

impl Pop {
    fn sc(&self, v: i32) -> i32 {
        sc_dpi(v, self.dpi)
    }

    /// What a moment shows; `None` when it names something this build does not know.
    fn describe(&self, m: &Moment) -> Option<Cur> {
        let s = &self.state;
        let mpx = self.sc(104) as u32;
        let mk = |medal_id: &'static str, tier: u8, eyebrow: &str, title: String, line: String, xp: u32, level: Option<(u32, String, String)>| Cur {
            key: m.key(),
            medal_id,
            tier,
            mpx,
            eyebrow: eyebrow.to_string(),
            title,
            tier_name: TIER_NAMES[tier as usize - 1].to_string(),
            line,
            xp,
            level,
            t0: 0.0,
            dur: 0.0,
        };
        match m.kind.as_str() {
            "badge" => {
                let b = Catalog::get(&m.id)?;
                let (tier, line) = if b.is_ladder() {
                    let t = (m.to as u8).clamp(1, 7);
                    (t, format!("{} {} reached", fmt_n(b.tiers[t as usize - 1]), b.unit))
                } else {
                    (single_tier(b.category), b.line.to_string())
                };
                let eyebrow = if b.is_ladder() && m.from > 0 { "Badge upgraded" } else { "Unlocked" };
                Some(mk(b.id, tier, eyebrow, b.name.to_string(), line, m.xp, None))
            }
            "record" => {
                let set = Catalog::set(&m.id)?;
                let tier = (m.to as u8).clamp(1, 7);
                Some(mk(set.id, tier, "Rack certified", set.name.to_string(), format!("All four medals at {}.", TIER_NAMES[tier as usize - 1]), m.xp, None))
            }
            "gate" => Some(mk("gate", 7, "Emulsion unlocked", "Emulsion is open".to_string(), "You met every condition. Ladders can now climb to Emulsion.".to_string(), 0, None)),
            "level" => {
                let lv = m.to.max(1);
                let rank = rank_of(lv, s.g);
                let new_rank = if m.id.is_empty() { String::new() } else { format!("New rank: {}", m.id) };
                Some(mk("rank", rank as u8 + 1, "Level up", String::new(), String::new(), 0, Some((lv, title_of(lv).to_string(), new_rank))))
            }
            _ => None,
        }
    }

    /// Show the next queued moment (quiet ones are only removed from the pending file).
    fn next(&mut self) {
        while let Some(m) = self.queue.pop_front() {
            if m.quiet {
                let _ = store::pop_pending(&m.key());
                continue;
            }
            let Some(mut cur) = self.describe(&m) else {
                let _ = store::pop_pending(&m.key());
                continue;
            };
            self.reduced = reduced_motion();
            cur.t0 = now_s();
            cur.dur = if cur.level.is_some() {
                if self.reduced {
                    1.6
                } else {
                    3.2
                }
            } else {
                6.0
            };
            self.medals.ensure(&[(cur.medal_id, cur.tier, cur.mpx)]);
            let (dur, hwnd) = (cur.dur, self.hwnd);
            self.cur = Some(cur);
            self.place();
            self.draw();
            unsafe {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                SetTimer(hwnd, T_CLOSE, (dur * 1000.0) as u32 + 40, None);
                if !self.reduced {
                    SetTimer(hwnd, T_ANIM, 16, None);
                }
            }
            return;
        }
    }

    /// Bottom-right corner of the primary work area (next to the tray).
    fn place(&self) {
        unsafe {
            let mut work: RECT = std::mem::zeroed();
            SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0);
            let (w, h, m) = (self.sc(POP_W), self.sc(POP_H), self.sc(16));
            SetWindowPos(self.hwnd, HWND_TOPMOST, work.right - w - m, work.bottom - h - m, w, h, SWP_NOACTIVATE);
        }
    }

    fn dismiss(&mut self) {
        let hwnd = self.hwnd;
        if let Some(c) = self.cur.take() {
            let _ = store::pop_pending(&c.key);
        }
        unsafe {
            KillTimer(hwnd, T_ANIM);
            KillTimer(hwnd, T_CLOSE);
        }
        self.next();
        if self.cur.is_none() {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
    }

    fn draw(&mut self) {
        let Some(cur) = self.cur.as_ref() else { return };
        let (w, h) = (self.sc(POP_W), self.sc(POP_H));
        let mut surf = match self.surf.take() {
            Some(s) if s.w == w && s.h == h => s,
            _ => match Surface::new(w, h) {
                Some(s) => s,
                None => return,
            },
        };
        if self.ov.as_ref().map_or(true, |o| o.w != w || o.h != h) {
            self.ov = Surface::new(w, h);
        }
        let Some(mut ov) = self.ov.take() else { return };
        let mut cv = std::mem::replace(&mut self.cv, Canvas::new());
        cv.resize(w, h);
        let sc = |v: i32| self.sc(v);
        let (c, f) = (self.col, &self.f);
        let t = now_s() - cur.t0;
        let tc = tier_col(cur.tier, c.dark);
        let rise = if self.reduced { 1.0 } else { ease_back(t / RISE_S) };
        let fade_in = if self.reduced { 1.0 } else { (t / 0.25).clamp(0.0, 1.0) as f32 };
        let is_level = cur.level.is_some();
        let fade_out = if is_level && !self.reduced { (((cur.dur - t) / (cur.dur * 0.14)).clamp(0.0, 1.0)) as f32 } else { 1.0 };
        let all = fade_in * fade_out;

        // Card.
        let r = Rect { l: 0, t: 0, r: w, b: h };
        let rad = sc(14) as f32;
        let bg = if c.dark { Rgba(32, 32, 36, 246) } else { Rgba(250, 250, 250, 246) };
        cv.clear(Rgba(0, 0, 0, 0));
        cv.fill_round(Rect { l: 1, t: 1, r: w - 1, b: h - 1 }, rad, with_alpha(bg, all));
        cv.stroke_round(Rect { l: 1, t: 1, r: w - 1, b: h - 1 }, rad, with_alpha(if c.dark { Rgba(255, 255, 255, 40) } else { Rgba(0, 0, 0, 40) }, all));
        let _ = r;
        surf.load(&cv);

        // Level-up burst: 18 rays behind the medal, in the pixels the card covers.
        let (mcx, mcy) = (sc(16) + sc(52), h / 2);
        if is_level && !self.reduced {
            let ray_col = hx(TIER_HEX[cur.tier as usize - 1]);
            let hw = sc(2) as f32 + 0.5;
            let len_max = sc(300) as f32;
            let px = surf.px();
            for y in 0..h {
                for x in 0..w {
                    let (dx, dy) = ((x - mcx) as f32, (y - mcy) as f32);
                    let rr = (dx * dx + dy * dy).sqrt();
                    if rr < sc(30) as f32 {
                        continue;
                    }
                    let step = std::f32::consts::TAU / 18.0;
                    let a = dy.atan2(dx) - std::f32::consts::FRAC_PI_2;
                    let idx = (a / step).round();
                    let d = a - idx * step;
                    let perp = rr * d.sin().abs();
                    if perp > hw + 1.0 {
                        continue;
                    }
                    let i = idx.rem_euclid(18.0);
                    let p = ((t - i as f64 * 0.022) / cur.dur).clamp(0.0, 1.0) as f32;
                    let op = if p < 0.14 { p / 0.14 * 0.85 } else { 0.85 * (1.0 - (p - 0.14) / 0.86) };
                    let len = len_max * (0.7 + 0.4 * p);
                    let along = (1.0 - rr / (0.85 * len)).clamp(0.0, 1.0);
                    let cov = (hw + 0.5 - perp).clamp(0.0, 1.0) * along * op * all;
                    if cov <= 0.0 {
                        continue;
                    }
                    let idx_px = (y * w + x) as usize;
                    let dst = px[idx_px];
                    let card_a = (dst >> 24) as f32 / 255.0;
                    px[idx_px] = over_px(dst, premul(ray_col, cov * card_a));
                }
            }
        }

        // Text and bar on a clear layer.
        cv.clear(Rgba(0, 0, 0, 0));
        let ta = if self.reduced { 1.0 } else { ((t - 0.35) / 0.45).clamp(0.0, 1.0) as f32 } * fade_out;
        let tx = sc(136);
        let tw = w - tx - sc(16);
        let th = |font: HFONT| cv.measure(font, "Ag").1;
        if let Some((lv, title, new_rank)) = &cur.level {
            let (h_eb, h_num, h_t, h_nr) = (th(f.caps), th(self.x.num), th(f.mid), th(f.normal));
            let total = h_eb + h_num + h_t + if new_rank.is_empty() { 0 } else { h_nr };
            let mut y = (h - total) / 2;
            cv.text(f.caps, "LEVEL UP", tx, y, with_alpha(c.dim, ta));
            y += h_eb;
            cv.text(self.x.num, &lv.to_string(), tx, y, with_alpha(tc, ta));
            y += h_num;
            cv.text(f.mid, title, tx, y, with_alpha(c.fg, ta));
            y += h_t;
            if !new_rank.is_empty() {
                cv.text(f.normal, new_rank, tx, y, with_alpha(c.dim, ta));
            }
        } else {
            let lines = wrap_dc(self.mdc, f.normal, &cur.line, tw, 2);
            let (h_eb, h_t, h_tn, h_l) = (th(f.caps), th(f.big), th(f.mid), th(f.normal));
            let pill_h = sc(22);
            let total = h_eb + h_t + h_tn + lines.len() as i32 * h_l + if cur.xp > 0 { pill_h + sc(6) } else { 0 };
            let mut y = ((h - sc(10)) - total) / 2;
            cv.text(f.caps, &cur.eyebrow.to_uppercase(), tx, y, with_alpha(c.dim, ta));
            y += h_eb;
            let title = cv.ellipsize(f.big, &cur.title, tw);
            cv.text(f.big, &title, tx, y, with_alpha(c.fg, ta));
            y += h_t;
            cv.text(f.mid, &cur.tier_name, tx, y, with_alpha(tc, ta));
            y += h_tn;
            for l in &lines {
                cv.text(f.normal, l, tx, y, with_alpha(c.dim, ta));
                y += h_l;
            }
            if cur.xp > 0 {
                y += sc(6);
                let label = format!("+{} XP", fmt_n(cur.xp as u64));
                let pw = cv.measure(f.bold, &label).0 + sc(24);
                let pr = Rect { l: tx, t: y, r: tx + pw, b: y + pill_h };
                pill(&mut cv, pr, with_alpha(tc, ta));
                text_c(&mut cv, f.bold, &label, (pr.l + pr.r) / 2, (pr.t + pr.b) / 2, with_alpha(on_color(tc), ta));
            }
            if !self.reduced {
                let frac = (1.0 - t / cur.dur).clamp(0.0, 1.0) as f32;
                let bar = Rect { l: sc(14), t: h - sc(10), r: w - sc(14), b: h - sc(6) };
                cv.fill_round(bar, sc(2) as f32, with_alpha(c.track, all));
                let fw = (bar.w() as f32 * frac) as i32;
                if fw > 0 {
                    cv.fill_round(Rect { r: bar.l + fw, ..bar }, sc(2) as f32, with_alpha(tc, all));
                }
            }
        }
        ov.load(&cv);
        surf.over_surface(&mut ov);

        // Medal (rise-in: from small and low to its place, with a little overshoot).
        let scale = if self.reduced { 1.0 } else { 0.35 + 0.65 * rise };
        let scale = if is_level && !self.reduced { scale * (1.0 + 0.06 * ((t / 0.45).clamp(0.0, 1.0) as f32 * std::f32::consts::PI).sin()) } else { scale };
        let off_y = if self.reduced { 0.0 } else { 40.0 * dpi_scale(self.dpi) * (1.0 - rise) };
        if let Some(tile) = self.medals.get((cur.medal_id, cur.tier, cur.mpx)) {
            let (sw, sh) = (surf.w, surf.h);
            let n = cur.mpx as i32;
            draw_tile(surf.px(), sw, sh, tile, n, (mcx - n / 2) as f32 + n as f32 / 2.0, (mcy - n / 2) as f32 + n as f32 / 2.0 + off_y, scale, scale, all, (0, sh));
        }

        // Present.
        unsafe {
            GdiFlush();
            let sz = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            UpdateLayeredWindow(self.hwnd, std::ptr::null_mut(), std::ptr::null(), &sz, surf.dc, &src, 0, &blend, ULW_ALPHA);
        }
        self.cv = cv;
        self.surf = Some(surf);
        self.ov = Some(ov);
    }
}

fn dpi_scale(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}

unsafe extern "system" fn pop_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            // Layered window: content comes from UpdateLayeredWindow; just validate.
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            if let Some(p) = pop() {
                p.dismiss();
            }
            0
        }
        WM_KEYDOWN => {
            if matches!(w as u16, VK_ESCAPE | VK_RETURN | VK_SPACE) {
                if let Some(p) = pop() {
                    p.dismiss();
                }
            }
            0
        }
        WM_TIMER => {
            match w {
                T_ANIM => {
                    if let Some(p) = pop() {
                        let done = p.cur.as_ref().is_some_and(|c| now_s() - c.t0 >= c.dur);
                        if !done {
                            p.draw();
                        }
                    }
                }
                T_CLOSE => {
                    if let Some(p) = pop() {
                        p.dismiss();
                    }
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, T_ANIM);
            KillTimer(hwnd, T_CLOSE);
            0
        }
        WM_NCDESTROY => {
            free_pop();
            DefWindowProcW(hwnd, msg, w, l)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// Queue unlock pop-ups and show them one at a time near the tray corner. Quiet moments are not shown; each
/// shown moment is removed from the pending file when it closes.
pub fn show_moments(owner: HWND, moments: &[Moment], state: &State, dark: bool) {
    unsafe {
        if pop().is_none() {
            let hinst = GetModuleHandleW(std::ptr::null());
            let dpi = GetDpiForSystem().max(96);
            let cls = wide(CLASS_POP);
            let title = wide("Achievements");
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                cls.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                sc_dpi(POP_W, dpi),
                sc_dpi(POP_H, dpi),
                owner,
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return;
            }
            free_pop();
            POP = Box::into_raw(Box::new(Pop {
                hwnd,
                dpi,
                col: make_colors(dark, false),
                f: Fonts::gray(dpi as f32 / 96.0),
                x: Extra::new(dpi),
                mdc: CreateCompatibleDC(std::ptr::null_mut()),
                cv: Canvas::new(),
                surf: None,
                ov: None,
                state: state.clone(),
                queue: VecDeque::new(),
                cur: None,
                medals: Medals::default(),
                reduced: false,
            }));
        }
        let Some(p) = pop() else { return };
        p.state = state.clone();
        p.col = make_colors(dark, false);
        for m in moments {
            let key = m.key();
            if p.cur.as_ref().is_some_and(|c| c.key == key) || p.queue.iter().any(|q| q.key() == key) {
                continue;
            }
            p.queue.push_back(m.clone());
        }
        // Who goes first: Emulsion gate, rack record, badges (highest tier, then XP), level-up last.
        let mut v: Vec<Moment> = p.queue.drain(..).collect();
        v.sort_by_key(|m| (moment_rank(m), std::cmp::Reverse(m.to), std::cmp::Reverse(m.xp)));
        p.queue = v.into();
        if p.cur.is_none() {
            p.next();
        }
    }
}

/// Close the Achievements window and any pop-up (app exit).
pub fn close_all() {
    unsafe {
        if let Some(u) = ui() {
            let h = u.hwnd;
            DestroyWindow(h);
        }
        if let Some(p) = pop() {
            p.queue.clear();
            p.cur = None;
            let h = p.hwnd;
            DestroyWindow(h);
        }
    }
}
