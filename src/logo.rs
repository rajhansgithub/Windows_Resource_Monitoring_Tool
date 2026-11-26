//! The Resource Monitor mark ("Pulse Line"): a heartbeat line in the CPU → memory →
//! GPU colours on a rounded tile, dark or eggshell. Drawn live in Settings › About and
//! the tray panel header. res/app.ico and res/app_light.ico come from the same geometry
//! (tools/icon/pulse.js, rendered by tools/icon/make.js).

use crate::render::{Canvas, Rgba};
use crate::util::Rect;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Icon resource ids (build.rs): the dark tile and the eggshell tile.
pub const ICON_DARK: u16 = 1;
pub const ICON_LIGHT: u16 = 2;

struct Look {
    tile: (Rgba, Rgba),
    edge: Rgba,
    grid: Rgba,
    line: [Rgba; 3],
    dot: Rgba,
    /// Alpha of the line outside the moving trace.
    dim: f32,
}

fn look(dark: bool) -> Look {
    if dark {
        Look {
            tile: (Rgba::rgb(0x2C, 0x2C, 0x2E), Rgba::rgb(0x15, 0x15, 0x16)),
            edge: Rgba(255, 255, 255, 23),
            grid: Rgba(255, 255, 255, 13),
            line: [Rgba::rgb(0x4F, 0xC3, 0xF7), Rgba::rgb(0xB3, 0x9D, 0xDB), Rgba::rgb(0xFF, 0x8A, 0x65)],
            dot: Rgba::rgb(255, 255, 255),
            dim: 0.28,
        }
    } else {
        // Light accents follow app::accent: the metric colour mixed 38% toward black.
        Look {
            tile: (Rgba::rgb(0xFB, 0xF8, 0xF1), Rgba::rgb(0xE7, 0xE1, 0xD3)),
            edge: Rgba(42, 38, 32, 41),
            grid: Rgba(42, 38, 32, 18),
            line: [Rgba::rgb(0x31, 0x79, 0x99), Rgba::rgb(0x6F, 0x61, 0x88), Rgba::rgb(0x9E, 0x56, 0x3F)],
            dot: Rgba::rgb(0x2A, 0x26, 0x20),
            dim: 0.36,
        }
    }
}

const PTS: [(f32, f32); 7] = [(0.14, 0.56), (0.36, 0.56), (0.43, 0.38), (0.52, 0.76), (0.61, 0.24), (0.69, 0.56), (0.86, 0.56)];
/// Small sizes: slightly taller peaks, points on pixel centres.
const PTS_S: [(f32, f32); 7] = [(0.12, 0.56), (0.36, 0.56), (0.44, 0.36), (0.53, 0.76), (0.63, 0.22), (0.71, 0.56), (0.88, 0.56)];

/// Hand-set line weight per size (px): thin enough to stay crisp at tray size.
fn weight(s: f32) -> f32 {
    match s as i32 {
        ..=16 => 1.25,
        17..=20 => 1.5,
        21..=24 => 1.6,
        25..=32 => 2.0,
        33..=48 => 2.75,
        49..=64 => 3.25,
        _ => s * 0.04,
    }
}

/// One pass of the trace takes 1 / SPEED s, then it rests for a quarter of that.
const SPEED: f32 = 0.45;

/// Draw the mark in the `s` px square at (`x`, `y`). `t` (seconds) runs the bright
/// trace along the line; `None` draws it still and fully lit.
pub fn draw(cv: &mut Canvas, x: i32, y: i32, s: i32, dark: bool, t: Option<f32>) {
    let lk = look(dark);
    let sf = s as f32;
    let tile = Rect { l: x, t: y, r: x + s, b: y + s };
    let rad = sf * 0.22;
    // Vertical gradient: one clipped row at a time.
    let keep = cv.clip();
    for row in 0..s {
        let band = Rect { l: x, t: y + row, r: x + s, b: y + row + 1 };
        let band = match keep {
            Some(k) => Rect { l: band.l.max(k.l), t: band.t.max(k.t), r: band.r.min(k.r), b: band.b.min(k.b) },
            None => band,
        };
        if band.is_empty() {
            continue;
        }
        cv.set_clip(Some(band));
        cv.fill_round(tile, rad, lk.tile.0.mix(lk.tile.1, row as f32 / (s - 1).max(1) as f32));
    }
    cv.set_clip(keep);
    cv.stroke_round(tile, rad, lk.edge);

    let small = s <= 32;
    if !small {
        for i in 1..4 {
            let gy = y + (sf * (0.2 + i as f32 * 0.18)).round() as i32;
            cv.fill_rect(Rect { l: x + (sf * 0.12) as i32, t: gy, r: x + (sf * 0.88) as i32, b: gy + (sf / 128.0).max(1.0) as i32 }, lk.grid);
        }
    }
    let snap = |v: f32| if small { (v - 0.5).round() + 0.5 } else { v };
    let src: &[(f32, f32)] = if small { &PTS_S } else { &PTS };
    let pts: Vec<(f32, f32)> = src.iter().map(|&(px, py)| (x as f32 + snap(px * sf), y as f32 + snap(py * sf))).collect();
    let (x0, x1) = (pts[0].0, pts[pts.len() - 1].0);
    let colour = |px: f32| {
        let f = ((px - x0) / (x1 - x0).max(1.0)).clamp(0.0, 1.0);
        if f < 0.5 {
            lk.line[0].mix(lk.line[1], f * 2.0)
        } else {
            lk.line[1].mix(lk.line[2], f * 2.0 - 1.0)
        }
    };
    let lw = weight(sf);
    let Some(t) = t.filter(|_| !small) else {
        cv.stroke_poly(&pts, lw, |px, _| colour(px));
        return;
    };
    let p = (t * SPEED) % 1.25;
    let head = p.min(1.0);
    let tail = (head - 0.55).max(0.0);
    let dim = (lk.dim * 255.0) as u8;
    cv.stroke_poly(&pts, lw, |px, along| {
        let c = colour(px);
        if along >= tail && along <= head {
            c
        } else {
            c.with_a(dim)
        }
    });
    if p < 1.0 {
        // The head: a soft glow in the CPU colour under a bright dot.
        let (hx, hy) = point_at(&pts, head);
        cv.fill_disc(hx, hy, lw * 2.2, lk.line[0].with_a(50));
        cv.fill_disc(hx, hy, lw * 0.8, lk.dot);
    }
}

/// The point `f` (0..1) of the way along the polyline.
fn point_at(pts: &[(f32, f32)], f: f32) -> (f32, f32) {
    let seg = |i: usize| ((pts[i].0 - pts[i - 1].0).powi(2) + (pts[i].1 - pts[i - 1].1).powi(2)).sqrt();
    let total: f32 = (1..pts.len()).map(seg).sum();
    let mut left = f * total;
    for i in 1..pts.len() {
        let l = seg(i);
        if left <= l {
            let k = if l > 0.0 { left / l } else { 0.0 };
            return (pts[i - 1].0 + (pts[i].0 - pts[i - 1].0) * k, pts[i - 1].1 + (pts[i].1 - pts[i - 1].1) * k);
        }
        left -= l;
    }
    pts[pts.len() - 1]
}

/// The app icon for a theme, DPI-correct (`big`: title bar large / Alt+Tab size).
pub fn icon(dark: bool, big: bool) -> HICON {
    let id = if dark { ICON_DARK } else { ICON_LIGHT };
    let mut h: HICON = std::ptr::null_mut();
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        if LoadIconMetric(hinst, id as usize as _, if big { LIM_LARGE } else { LIM_SMALL }, &mut h) != 0 || h.is_null() {
            h = LoadIconW(hinst, ICON_DARK as usize as _);
        }
    }
    h
}

/// Give a window the icon matching the app theme (title bar, taskbar button, Alt+Tab).
/// The previous icons are destroyed.
pub fn set_window_icon(hwnd: HWND, dark: bool) {
    unsafe {
        for (kind, big) in [(ICON_SMALL, false), (ICON_BIG, true)] {
            let old = SendMessageW(hwnd, WM_SETICON, kind as WPARAM, icon(dark, big) as LPARAM);
            if old != 0 {
                DestroyIcon(old as HICON);
            }
        }
    }
}
