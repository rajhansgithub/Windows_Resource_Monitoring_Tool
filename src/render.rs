//! Tiny software renderer for the per-pixel-alpha overlay, plus GDI helpers
//! for the opaque popup windows. No GPU, no frameworks.

use crate::util::{wide, Rect};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba(r, g, b, 255)
    }
    pub fn with_a(self, a: u8) -> Self {
        Rgba(self.0, self.1, self.2, a)
    }
    pub fn colorref(self) -> COLORREF {
        self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16
    }
    /// Linear mix toward `o` by t (0..1).
    pub fn mix(self, o: Rgba, t: f32) -> Rgba {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgba(f(self.0, o.0), f(self.1, o.1), f(self.2, o.2), f(self.3, o.3))
    }
}

pub fn create_font(px: i32, weight: i32, face: &str, aa_gray: bool) -> HFONT {
    let f = wide(face);
    let q = if aa_gray { ANTIALIASED_QUALITY } else { CLEARTYPE_QUALITY };
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_TT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            q as u32,
            (DEFAULT_PITCH | FF_SWISS) as u32,
            f.as_ptr(),
        )
    }
}

/// Best UI face available: Segoe UI Variable (Win11) else Segoe UI.
pub fn ui_face() -> &'static str {
    use std::sync::OnceLock;
    static FACE: OnceLock<&'static str> = OnceLock::new();
    FACE.get_or_init(|| unsafe {
        let want = "Segoe UI Variable Text";
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let f = create_font(12, 400, want, true);
        let old = SelectObject(dc, f);
        let mut buf = [0u16; 64];
        let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteObject(f);
        DeleteDC(dc);
        let got = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
        if got.eq_ignore_ascii_case(want) {
            want
        } else {
            "Segoe UI"
        }
    })
}

struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Option<Dib> {
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // top-down
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
            Some(Dib { dc, bmp, old, bits: bits as *mut u32 })
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

/// Premultiplied BGRA canvas + a grayscale mask used to rasterise GDI text
/// with correct per-pixel alpha.
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    /// Outline colour drawn behind text (None = plain text).
    pub halo: Option<Rgba>,
    /// Drawing is limited to this rectangle (None = the whole canvas).
    clip: Option<Rect>,
    /// A second, outer limit that every clip is cut to: the part of the
    /// canvas being redrawn (scrolling redraws only the strip that comes in).
    limit: Option<Rect>,
    px: Option<Dib>,
}

impl Canvas {
    pub fn new() -> Self {
        Canvas { w: 0, h: 0, halo: None, clip: None, limit: None, px: None }
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        let (w, h) = (w.max(1), h.max(1));
        if w == self.w && h == self.h && self.px.is_some() {
            return;
        }
        self.px = None;
        self.px = Dib::new(w, h);
        if self.px.is_none() {
            self.w = 0;
            self.h = 0;
            return;
        }
        self.w = w;
        self.h = h;
    }

    /// Limit drawing (fills, outlines, text) to `r`; None = no limit.
    pub fn set_clip(&mut self, r: Option<Rect>) {
        self.clip = r;
    }

    /// Limit all drawing (including `clear`) to `r` on top of any clip; None = no limit.
    pub fn set_limit(&mut self, r: Option<Rect>) {
        self.limit = r;
    }

    /// Move the pixels of `band` vertically by `dy` (negative = up). Rows that
    /// come from outside the band keep their old pixels; redraw them after.
    pub fn shift_rows(&mut self, band: Rect, dy: i32) {
        let (l, t, r, b) = (band.l.max(0), band.t.max(0), band.r.min(self.w), band.b.min(self.h));
        if l >= r || t >= b || dy == 0 || dy.abs() >= b - t {
            return;
        }
        unsafe { GdiFlush() };
        let w = self.w as usize;
        let n = (r - l) as usize;
        let buf = self.buf();
        let mut copy = |y: i32| {
            let (dst, src) = ((y as usize) * w + l as usize, ((y - dy) as usize) * w + l as usize);
            buf.copy_within(src..src + n, dst);
        };
        if dy < 0 {
            for y in t..b + dy {
                copy(y);
            }
        } else {
            for y in (t + dy..b).rev() {
                copy(y);
            }
        }
    }

    /// The current clip rectangle (to restore it after a nested clip).
    pub fn clip(&self) -> Option<Rect> {
        self.clip
    }

    /// `l, t, r, b` limited to the canvas and the clip rectangle.
    fn bounds(&self, l: i32, t: i32, r: i32, b: i32) -> (i32, i32, i32, i32) {
        let (mut l, mut t, mut r, mut b) = (l.max(0), t.max(0), r.min(self.w), b.min(self.h));
        for c in [self.clip, self.limit].into_iter().flatten() {
            l = l.max(c.l);
            t = t.max(c.t);
            r = r.min(c.r);
            b = b.min(c.b);
        }
        (l, t, r, b)
    }

    /// Fade the pixels of `r` toward `to` (straight alpha): `keep` = 1 leaves
    /// them, 0 replaces them with `to`. Used for page transitions; with a
    /// transparent `to` (Mica) it fades to the backdrop.
    #[allow(dead_code)]
    pub fn fade_rect(&mut self, r: Rect, keep: f32, to: Rgba) {
        let k = (keep.clamp(0.0, 1.0) * 256.0) as u32;
        if k >= 256 {
            return;
        }
        let (l, t, rr, b) = self.bounds(r.l, r.t, r.r, r.b);
        if l >= rr || t >= b {
            return;
        }
        let p = premul(to);
        let ik = 256 - k;
        let w = self.w;
        let buf = self.buf();
        for y in t..b {
            for px in &mut buf[(y * w + l) as usize..(y * w + rr) as usize] {
                let d = *px;
                let mut o = 0u32;
                for sh in [0u32, 8, 16, 24] {
                    let dv = (d >> sh) & 0xff;
                    let pv = (p >> sh) & 0xff;
                    o |= (((dv * k + pv * ik) >> 8) & 0xff) << sh;
                }
                *px = o;
            }
        }
    }

    fn buf(&mut self) -> &mut [u32] {
        match &self.px {
            Some(d) => unsafe { std::slice::from_raw_parts_mut(d.bits, (self.w * self.h) as usize) },
            None => &mut [],
        }
    }

    /// The canvas pixels (premultiplied BGRA, row 0 = top), e.g. to read text coverage back.
    pub fn pixels(&mut self) -> &[u32] {
        unsafe { GdiFlush() };
        self.buf()
    }

    /// The canvas pixels to write (premultiplied BGRA, row 0 = top), e.g. to put a rendered image on it.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { GdiFlush() };
        self.buf()
    }

    pub fn clear(&mut self, c: Rgba) {
        let p = premul(c);
        match self.limit {
            None => self.buf().fill(p),
            Some(_) => {
                let (l, t, r, b) = self.bounds(0, 0, self.w, self.h);
                let w = self.w;
                let buf = self.buf();
                for y in t..b.max(t) {
                    if l < r {
                        buf[(y * w + l) as usize..(y * w + r) as usize].fill(p);
                    }
                }
            }
        }
    }

    #[inline]
    fn blend_px(dst: &mut u32, c: Rgba, cov: u32) {
        // c is straight alpha; cov 0..255
        let a = (c.3 as u32 * cov + 127) / 255;
        if a == 0 {
            return;
        }
        let ia = 255 - a;
        let d = *dst;
        let ch = |shift: u32, src: u8| -> u32 {
            let dv = (d >> shift) & 0xff;
            ((src as u32 * a + dv * ia + 127) / 255).min(255) << shift
        };
        let da = (d >> 24) & 0xff;
        let na = (a + (da * ia + 127) / 255).min(255);
        *dst = (na << 24) | ch(16, c.0) | ch(8, c.1) | ch(0, c.2);
    }

    pub fn fill_rect(&mut self, r: Rect, c: Rgba) {
        let w = self.w;
        let (l, t, rr, b) = self.bounds(r.l, r.t, r.r, r.b);
        if l >= rr {
            return;
        }
        let buf = self.buf();
        for y in t..b {
            blend_span(&mut buf[(y * w + l) as usize..(y * w + rr) as usize], c, 255);
        }
    }

    /// Anti-aliased rounded rectangle. Only the corner columns are shaded per
    /// pixel; between them a row has one coverage, filled as a span.
    pub fn fill_round(&mut self, r: Rect, rad: f32, c: Rgba) {
        let rad = rad.min(r.w() as f32 / 2.0).min(r.h() as f32 / 2.0).max(0.0);
        let w = self.w;
        let (bl, bt, br, bb) = self.bounds(r.l, r.t, r.r, r.b);
        if bl >= br || bt >= bb {
            return;
        }
        let (m0, m1) = mid_span(r.l as f32 + rad, r.r as f32 - rad, bl, br);
        let buf = self.buf();
        for y in bt..bb {
            let py = y as f32 + 0.5;
            let cy = py.clamp(r.t as f32 + rad, r.b as f32 - rad);
            let row = &mut buf[(y * w) as usize..((y + 1) * w) as usize];
            let mut px_at = |x: i32| {
                let px = x as f32 + 0.5;
                let cx = px.clamp(r.l as f32 + rad, r.r as f32 - rad);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let cov = (rad - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    Self::blend_px(&mut row[x as usize], c, (cov * 255.0) as u32);
                }
            };
            for x in (bl..m0).chain(m1..br) {
                px_at(x);
            }
            if m0 < m1 {
                // Same sum as per pixel (there px - cx is exactly 0).
                let d = (0.0f32.powi(2) + (py - cy).powi(2)).sqrt();
                let cov = (rad - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    blend_span(&mut row[m0 as usize..m1 as usize], c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Anti-aliased 1px outline just inside `r` (leaves the interior untouched,
    /// so translucent fills drawn over it don't pick up the border colour).
    /// Rows away from the top and bottom edges only touch their two ends.
    pub fn stroke_round(&mut self, r: Rect, rad: f32, c: Rgba) {
        let rad = rad.min(r.w() as f32 / 2.0).min(r.h() as f32 / 2.0).max(0.0);
        let inner = Rect { l: r.l + 1, t: r.t + 1, r: r.r - 1, b: r.b - 1 };
        let irad = (rad - 1.0).max(0.0);
        let cov = |px: f32, py: f32, q: Rect, rd: f32| -> f32 {
            let cx = px.clamp(q.l as f32 + rd, q.r as f32 - rd);
            let cy = py.clamp(q.t as f32 + rd, q.b as f32 - rd);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            (rd - d + 0.5).clamp(0.0, 1.0)
        };
        let w = self.w;
        let (bl, bt, br, bb) = self.bounds(r.l, r.t, r.r, r.b);
        if bl >= br || bt >= bb {
            return;
        }
        // Columns whose centres are past both corner radii: there both coverages depend on y only.
        let (m0, m1) = if inner.is_empty() {
            (bl, bl)
        } else {
            let lo = (r.l as f32 + rad).max(inner.l as f32 + irad);
            let hi = (r.r as f32 - rad).min(inner.r as f32 - irad);
            mid_span(lo, hi, bl, br)
        };
        let buf = self.buf();
        for y in bt..bb {
            let py = y as f32 + 0.5;
            let row = &mut buf[(y * w) as usize..((y + 1) * w) as usize];
            for x in (bl..m0).chain(m1..br) {
                let px = x as f32 + 0.5;
                let outer = cov(px, py, r, rad);
                let inn = if inner.is_empty() { 0.0 } else { cov(px, py, inner, irad) };
                let v = (outer - inn).clamp(0.0, 1.0);
                if v > 0.0 {
                    Self::blend_px(&mut row[x as usize], c, (v * 255.0) as u32);
                }
            }
            if m0 < m1 {
                let px = (m0 as f32 + 0.5).max(r.l as f32 + rad);
                let v = (cov(px, py, r, rad) - cov(px, py, inner, irad)).clamp(0.0, 1.0);
                if v > 0.0 {
                    blend_span(&mut row[m0 as usize..m1 as usize], c, (v * 255.0) as u32);
                }
            }
        }
    }

    pub fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        if self.px.is_none() {
            return (0, 0);
        }
        text_cache::measure(font, s)
    }

    /// Draw text with its top-left at (x, y). Returns advance width.
    /// The glyph coverage comes from a cache (GDI draws each string once per font).
    pub fn text(&mut self, font: HFONT, s: &str, x: i32, y: i32, c: Rgba) -> i32 {
        if self.px.is_none() {
            return 0;
        }
        let (w, h) = (self.w, self.h);
        let halo = self.halo;
        let clip = self.clip;
        let limit = self.limit;
        let buf = match &self.px {
            Some(d) => unsafe { std::slice::from_raw_parts_mut(d.bits, (w * h) as usize) },
            None => return 0,
        };
        text_cache::with(font, s, |g| {
            // The bitmap's top-left sits 3px left of and 2px above (x, y).
            let (gl, gt) = (x - text_cache::PAD_L, y - text_cache::PAD_T);
            let (l, t, r, b) = (gl, gt, gl + g.w, gt + g.h);
            let (mut cl, mut ct, mut cr, mut cb) = (l.max(0), t.max(0), r.min(w), b.min(h));
            for q in [clip, limit].into_iter().flatten() {
                cl = cl.max(q.l);
                ct = ct.max(q.t);
                cr = cr.min(q.r);
                cb = cb.min(q.b);
            }
            if cl >= cr || ct >= cb {
                return g.tw;
            }
            let cov_at = |xx: i32, yy: i32| -> u32 {
                let (bx, by) = (xx - gl, yy - gt);
                if bx < 0 || by < 0 || bx >= g.w || by >= g.h {
                    return 0;
                }
                g.cov[(by * g.w + bx) as usize] as u32
            };
            // Crisp 1px drop shadow (down-right): keeps glyphs legible on
            // translucent / Mica taskbars without blurring their edges.
            if let Some(hc) = halo {
                for yy in ct..cb {
                    for xx in cl..cr {
                        let cv = cov_at(xx - 1, yy - 1);
                        if cv != 0 {
                            Self::blend_px(&mut buf[(yy * w + xx) as usize], hc, cv);
                        }
                    }
                }
            }
            let lut = sharpen_lut();
            for yy in ct..cb {
                let src = &g.cov[((yy - gt) * g.w + (cl - gl)) as usize..((yy - gt) * g.w + (cr - gl)) as usize];
                let dst = &mut buf[(yy * w + cl) as usize..(yy * w + cr) as usize];
                for (d, &cv) in dst.iter_mut().zip(src) {
                    if cv != 0 {
                        Self::blend_px(d, c, lut[cv as usize] as u32);
                    }
                }
            }
            g.tw
        })
        .unwrap_or(0)
    }

    pub fn text_right(&mut self, font: HFONT, s: &str, right: i32, y: i32, c: Rgba) {
        let (tw, _) = self.measure(font, s);
        self.text(font, s, right - tw, y, c);
    }

    /// Area sparkline. `data` oldest → newest, values 0..1, right-aligned.
    pub fn spark(&mut self, r: Rect, data: &[f32], c: Rgba) {
        self.spark_dir(r, data, c, false);
    }

    /// Like `spark`; with `down` the area hangs from the top edge instead
    /// (the lower half of a mirror chart).
    pub fn spark_dir(&mut self, r: Rect, data: &[f32], c: Rgba, down: bool) {
        if data.is_empty() || r.is_empty() {
            return;
        }
        // Lime Terminal draws history as dot-matrix columns.
        if crate::look::current() == crate::look::Look::Lime {
            return self.spark_dots(r, data, c, down);
        }
        let n = data.len();
        let (w, h) = (self.w, self.h);
        let rw = r.w();
        let rh = r.h() as f32;
        let fill = c.with_a((c.3 as u32 * 70 / 255) as u8);
        let (cl, ct, cr, cb) = self.bounds(0, 0, w, h);
        let buf = self.buf();
        for i in 0..rw {
            // column i maps to sample index (right-aligned, one sample per 2px max)
            let pos = (n as f32 - 1.0) * (i as f32 / (rw - 1).max(1) as f32);
            let a = data[pos.floor() as usize];
            let bb = data[(pos.ceil() as usize).min(n - 1)];
            let v = (a + (bb - a) * pos.fract()).clamp(0.0, 1.0);
            let top = rh * (1.0 - v);
            let x = r.l + i;
            if x < cl || x >= cr {
                continue;
            }
            for yy in 0..r.h() {
                let y = r.t + yy;
                if y < ct || y >= cb {
                    continue;
                }
                // Mirrored: measure from the bottom edge so the same
                // coverage math hangs the area from the top.
                let yf = if down { (r.h() - 1 - yy) as f32 } else { yy as f32 };
                let px = &mut buf[(y * w + x) as usize];
                if yf + 1.0 <= top {
                    continue;
                }
                let cov = (yf + 1.0 - top).min(1.0);
                // 1px bright edge on top of the area
                if yf < top + 1.0 {
                    Self::blend_px(px, c, (cov * 230.0) as u32);
                } else {
                    Self::blend_px(px, fill, 255);
                }
            }
        }
    }

    /// A big number the look's way: Lime and Tactile light it over dim "8"
    /// ghosts (LED / LCD segments that are off), Bento draws it plain.
    /// Returns the width like `text`.
    pub fn numeral(&mut self, font: HFONT, s: &str, x: i32, y: i32, c: Rgba) -> i32 {
        if crate::look::current() != crate::look::Look::Bento && s.chars().any(|ch| ch.is_ascii_digit()) {
            let ghost: String = s.chars().map(|ch| if ch.is_ascii_digit() { '8' } else { ch }).collect();
            // No outline around the ghosts (the widget outlines text on a clear taskbar).
            let halo = self.halo.take();
            self.text(font, &ghost, x, y, c.with_a((c.3 as u32 * 34 / 255) as u8));
            self.halo = halo;
        }
        self.text(font, s, x, y, c)
    }

    /// `numeral` centred on `cx`.
    pub fn numeral_center(&mut self, font: HFONT, s: &str, cx: i32, y: i32, c: Rgba) {
        let w = self.measure(font, s).0;
        self.numeral(font, s, cx - w / 2, y, c);
    }

    /// A horizontal level bar, drawn the look's way: Lime a row of square dots,
    /// Tactile an LED ladder, Bento a rounded pill. `frac` 0..1.
    pub fn meter(&mut self, r: Rect, frac: f32, c: Rgba, track: Rgba) {
        if r.is_empty() {
            return;
        }
        let frac = frac.clamp(0.0, 1.0);
        match crate::look::current() {
            crate::look::Look::Lime => {
                // One row of dots for thin bars, two for tall ones.
                let rows = if r.h() >= 10 { 2 } else { 1 };
                let gap = (r.h() / 6).max(1);
                let dot = ((r.h() - gap * (rows - 1)) / rows).max(2);
                let step = dot + gap;
                let n = ((r.w() + gap) / step).max(1);
                let lit = (frac * n as f32).round() as i32;
                for k in 0..n {
                    let x = r.l + k * step;
                    for row in 0..rows {
                        let y = r.t + row * step;
                        self.fill_rect(Rect { l: x, t: y, r: x + dot, b: y + dot }, if k < lit { c } else { track });
                    }
                }
            }
            crate::look::Look::Tactile => {
                let gap = (r.h() / 4).clamp(1, 3);
                let seg = (r.h() * 3 / 2).max(4);
                let n = ((r.w() + gap) / (seg + gap)).max(1);
                let sw = (r.w() - gap * (n - 1)) as f32 / n as f32;
                let lit = (frac * n as f32).round() as i32;
                for k in 0..n {
                    let x0 = r.l + (k as f32 * (sw + gap as f32)).round() as i32;
                    let x1 = r.l + (k as f32 * (sw + gap as f32) + sw).round() as i32;
                    self.fill_round(Rect { l: x0, r: x1.max(x0 + 1), ..r }, 1.0, if k < lit { c } else { track });
                }
            }
            crate::look::Look::Bento => {
                let rad = r.h() as f32 / 2.0;
                self.fill_round(r, rad, track);
                let w = (r.w() as f32 * frac).round() as i32;
                if w > 0 {
                    self.fill_round(Rect { r: r.l + w.max(r.h()), ..r }, rad, c);
                }
            }
        }
    }

    /// Dot-matrix history (Lime): square dots in columns, lit from the base up
    /// to the value, unlit dots faint. Same mapping as `spark_dir`.
    fn spark_dots(&mut self, r: Rect, data: &[f32], c: Rgba, down: bool) {
        let n = data.len();
        let dot = (r.h() / 12).clamp(2, 4);
        let gap = (dot / 2).max(1);
        let step = dot + gap;
        let rows = ((r.h() + gap) / step).max(1);
        let cols = ((r.w() + gap) / step).max(1);
        let off = c.with_a((c.3 as u32 * 40 / 255) as u8);
        // Right-aligned grid.
        let x0 = r.r - cols * step + gap;
        for i in 0..cols {
            let pos = (n as f32 - 1.0) * (i as f32 / (cols - 1).max(1) as f32);
            let a = data[pos.floor() as usize];
            let b = data[(pos.ceil() as usize).min(n - 1)];
            let v = (a + (b - a) * pos.fract()).clamp(0.0, 1.0);
            let lit = (v * rows as f32).round() as i32;
            let x = x0 + i * step;
            for k in 0..rows {
                // k = 0 is the base row (bottom, or top when hanging down).
                let y = if down { r.t + k * step } else { r.b - (k + 1) * step + gap };
                let col = if k < lit { c } else { off };
                self.fill_rect(Rect { l: x, t: y, r: x + dot, b: y + dot }, col);
            }
        }
    }

    /// Anti-aliased ring arc from 12 o'clock, clockwise, `frac` 0..1, round caps.
    /// Lime draws it as a ring of dots, Tactile as LED segments, Bento smooth.
    /// Only pixels in the ring's annulus are visited.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, thick: f32, frac: f32, c: Rgba) {
        self.ring_in(cx, cy, r, thick, frac, c, crate::look::current());
    }

    /// `ring` in a given look (`Bento` = the plain smooth ring).
    #[allow(clippy::too_many_arguments)]
    pub fn ring_in(&mut self, cx: f32, cy: f32, r: f32, thick: f32, frac: f32, c: Rgba, look: crate::look::Look) {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= 0.0 {
            return;
        }
        let w = self.w;
        let half = thick / 2.0;
        let mid = r - half;
        let tau = std::f32::consts::TAU;
        // Segments (count, lit share of each) of the look; values snap to whole segments.
        let seg = match look {
            crate::look::Look::Lime => Some(((mid * tau / (thick * 1.9).max(3.0)).round().clamp(12.0, 60.0), 0.55f32)),
            crate::look::Look::Tactile => Some((24.0f32, 0.74f32)),
            crate::look::Look::Bento => None,
        };
        let frac = match seg {
            Some((n, _)) if frac < 1.0 => (frac * n).round() / n,
            _ => frac,
        };
        if frac <= 0.0 {
            return;
        }
        let end = frac * tau;
        let caps = [(cx, cy - mid), (cx + mid * end.sin(), cy - mid * end.cos())];
        let (x0, y0, x1, y1) = self.bounds(
            (cx - r - 1.0).floor() as i32,
            (cy - r - 1.0).floor() as i32,
            (cx + r + 2.0).ceil() as i32,
            (cy + r + 2.0).ceil() as i32,
        );
        let (ri, ro) = (mid - half - 0.5, mid + half + 0.5);
        let buf = self.buf();
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for (sl, sr) in annulus_spans(cx, py - cy, ri, ro, x0, x1) {
                for x in sl..sr {
                    let px = x as f32 + 0.5;
                    let (dx, dy) = (px - cx, py - cy);
                    let d = (dx * dx + dy * dy).sqrt();
                    let radial = (half - (d - mid).abs() + 0.5).clamp(0.0, 1.0);
                    if radial <= 0.0 {
                        continue;
                    }
                    let mut cov = 0.0f32;
                    if let Some((n, duty)) = seg {
                        let mut a = dx.atan2(-dy);
                        if a < 0.0 {
                            a += tau;
                        }
                        if a <= end + 1e-4 {
                            // Distance (px along the arc) to the nearer edge of this segment.
                            let u = (a / (tau / n)).fract();
                            if u < duty {
                                let edge = (u.min(duty - u)) * (tau / n) * d;
                                cov = radial * (edge + 0.5).clamp(0.0, 1.0);
                            }
                        }
                    } else if frac >= 1.0 {
                        cov = radial;
                    } else {
                        let mut a = dx.atan2(-dy);
                        if a < 0.0 {
                            a += tau;
                        }
                        if a <= end {
                            cov = radial;
                        }
                        for (qx, qy) in caps {
                            let cd = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
                            cov = cov.max((half - cd + 0.5).clamp(0.0, 1.0));
                        }
                    }
                    if cov > 0.0 {
                        Self::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                    }
                }
            }
        }
    }

    /// Anti-aliased arc of a circle (stroke centred on radius `r`, `thick` wide, round
    /// caps) from angle `a0` to `a1`, radians clockwise from 3 o'clock (y points down),
    /// `a1 >= a0`, at most a full turn. Respects the clip rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, r: f32, thick: f32, a0: f32, a1: f32, c: Rgba) {
        let tau = std::f32::consts::TAU;
        let span = (a1 - a0).clamp(0.0, tau);
        let half = thick / 2.0;
        let caps = [(cx + r * a0.cos(), cy + r * a0.sin()), (cx + r * (a0 + span).cos(), cy + r * (a0 + span).sin())];
        let ext = r + half + 1.0;
        let w = self.w;
        let (x0, y0, x1, y1) = self.bounds(
            (cx - ext).floor() as i32,
            (cy - ext).floor() as i32,
            (cx + ext).ceil() as i32 + 1,
            (cy + ext).ceil() as i32 + 1,
        );
        let (ri, ro) = (r - half - 0.5, r + half + 0.5);
        let buf = self.buf();
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for (sl, sr) in annulus_spans(cx, py - cy, ri, ro, x0, x1) {
                for x in sl..sr {
                    let px = x as f32 + 0.5;
                    let (dx, dy) = (px - cx, py - cy);
                    let d = (dx * dx + dy * dy).sqrt();
                    let radial = (half - (d - r).abs() + 0.5).clamp(0.0, 1.0);
                    let mut cov = 0.0f32;
                    if radial > 0.0 {
                        let t = (dy.atan2(dx) - a0).rem_euclid(tau);
                        if t <= span || span >= tau - 1e-4 {
                            cov = radial;
                        }
                    }
                    for (qx, qy) in caps {
                        let cd = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
                        cov = cov.max((half - cd + 0.5).clamp(0.0, 1.0));
                    }
                    if cov > 0.0 {
                        Self::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                    }
                }
            }
        }
    }

    /// Anti-aliased polyline with round joins and caps. `shade(x, along)` gives the
    /// colour for a pixel centred at `x`, whose nearest point on the line lies `along`
    /// (0..1) of the way from the first point; its alpha is applied too.
    /// Each segment only visits the pixels near it (not the whole line's box).
    pub fn stroke_poly(&mut self, pts: &[(f32, f32)], thick: f32, shade: impl Fn(f32, f32) -> Rgba) {
        if pts.len() < 2 {
            return;
        }
        let half = thick / 2.0;
        let mut acc = vec![0.0f32; pts.len()];
        for i in 1..pts.len() {
            let (dx, dy) = (pts[i].0 - pts[i - 1].0, pts[i].1 - pts[i - 1].1);
            acc[i] = acc[i - 1] + (dx * dx + dy * dy).sqrt();
        }
        let total = acc[pts.len() - 1].max(1e-3);
        let ext = half + 1.0;
        let lo_x = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min) - ext;
        let hi_x = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) + ext;
        let lo_y = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min) - ext;
        let hi_y = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max) + ext;
        let w = self.w;
        let (x0, y0, x1, y1) = self.bounds(lo_x.floor() as i32, lo_y.floor() as i32, hi_x.ceil() as i32 + 1, hi_y.ceil() as i32 + 1);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let bw = (x1 - x0) as usize;
        let n = bw * (y1 - y0) as usize;
        // Nearest distance and position along the line, per pixel of the box.
        let mut best = vec![f32::MAX; n];
        let mut along = vec![0.0f32; n];
        // A pixel farther than this from a segment gets no coverage from it.
        let reach = half + 0.5;
        for i in 1..pts.len() {
            let (ax, ay) = pts[i - 1];
            let (bx, by) = pts[i];
            let sx0 = ((ax.min(bx) - reach - 0.5).floor() as i32).max(x0);
            let sx1 = ((ax.max(bx) + reach + 0.5).ceil() as i32 + 1).min(x1);
            let sy0 = ((ay.min(by) - reach - 0.5).floor() as i32).max(y0);
            let sy1 = ((ay.max(by) + reach + 0.5).ceil() as i32 + 1).min(y1);
            let (dx, dy) = (bx - ax, by - ay);
            let len2 = (dx * dx + dy * dy).max(1e-6);
            for y in sy0..sy1 {
                let py = y as f32 + 0.5;
                let base = (y - y0) as usize * bw;
                for x in sx0..sx1 {
                    let px = x as f32 + 0.5;
                    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
                    let (qx, qy) = (ax + dx * t - px, ay + dy * t - py);
                    let d = (qx * qx + qy * qy).sqrt();
                    let k = base + (x - x0) as usize;
                    if d < best[k] {
                        best[k] = d;
                        along[k] = (acc[i - 1] + t * (acc[i] - acc[i - 1])) / total;
                    }
                }
            }
        }
        let buf = self.buf();
        for y in y0..y1 {
            let base = (y - y0) as usize * bw;
            for x in x0..x1 {
                let k = base + (x - x0) as usize;
                let cov = (half - best[k] + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    Self::blend_px(&mut buf[(y * w + x) as usize], shade(x as f32 + 0.5, along[k]), (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Anti-aliased filled circle; the fully covered middle of each row is one span.
    pub fn fill_disc(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
        let w = self.w;
        let (x0, y0, x1, y1) =
            self.bounds((cx - r - 1.0).floor() as i32, (cy - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32 + 1, (cy + r + 1.0).ceil() as i32 + 1);
        let buf = self.buf();
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let dy = py - cy;
            let row = &mut buf[(y * w) as usize..((y + 1) * w) as usize];
            // Centres within r - 0.5 of the middle are fully covered.
            let inner = r - 0.5;
            let (m0, m1) = if inner > 0.0 && dy * dy < inner * inner {
                // A hair narrower, so pixels right on the edge take the exact path.
                let hw = (inner * inner - dy * dy).sqrt() - 0.01;
                mid_span(cx - hw, cx + hw, x0, x1)
            } else {
                (x0, x0)
            };
            for x in (x0..m0).chain(m1..x1) {
                let d = ((x as f32 + 0.5 - cx).powi(2) + dy.powi(2)).sqrt();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    Self::blend_px(&mut row[x as usize], c, (cov * 255.0) as u32);
                }
            }
            if m0 < m1 {
                blend_span(&mut row[m0 as usize..m1 as usize], c, 255);
            }
        }
    }

    pub fn text_center(&mut self, font: HFONT, s: &str, cx: i32, y: i32, c: Rgba) {
        let (tw, _) = self.measure(font, s);
        self.text(font, s, cx - tw / 2, y, c);
    }

    /// Shorten with "…" to fit `max_w` (the longest prefix that fits, found by halving).
    pub fn ellipsize(&self, font: HFONT, s: &str, max_w: i32) -> String {
        if self.measure(font, s).0 <= max_w {
            return s.to_string();
        }
        let ends: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
        let fits = |n: usize| self.measure(font, &format!("{}\u{2026}", &s[..ends.get(n).copied().unwrap_or(s.len())])).0 <= max_w;
        // Largest n (chars kept) that fits; 0 always "fits" (just the ellipsis).
        let (mut lo, mut hi) = (0usize, ends.len());
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let cut = ends.get(lo).copied().unwrap_or(s.len());
        format!("{}\u{2026}", &s[..cut])
    }

    /// Copy to an ordinary (opaque) window DC.
    pub fn blit(&self, dc: HDC, x: i32, y: i32) {
        let Some(d) = &self.px else { return };
        unsafe {
            GdiFlush();
            BitBlt(dc, x, y, self.w, self.h, d.dc, 0, 0, SRCCOPY);
        }
    }

    /// Push to a layered window at screen position (x, y).
    /// Push to a layered *child* window: content only; the caller positions
    /// it (child coordinates are relative to the parent).
    pub fn present_child(&self, hwnd: HWND) {
        let Some(d) = &self.px else { return };
        unsafe {
            GdiFlush();
            let sz = SIZE { cx: self.w, cy: self.h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(hwnd, std::ptr::null_mut(), std::ptr::null(), &sz, d.dc, &src, 0, &blend, ULW_ALPHA);
        }
    }

    pub fn present(&self, hwnd: HWND, x: i32, y: i32) {
        let Some(d) = &self.px else { return };
        unsafe {
            GdiFlush();
            let pt = POINT { x, y };
            let sz = SIZE { cx: self.w, cy: self.h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(hwnd, std::ptr::null_mut(), &pt, &sz, d.dc, &src, 0, &blend, ULW_ALPHA);
        }
    }
}

/// Contrast curve for anti-aliased glyph edges: firms up partially covered
/// pixels so strokes look sharper (grayscale AA alone reads soft and thin).
fn sharpen_lut() -> &'static [u8; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[u8; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = ((i as f32 / 255.0).powf(0.72) * 255.0).round() as u8;
        }
        t
    })
}

/// Pixel columns `[x0, x1)` within `[bl, br)` whose centres lie in `[lo, hi]`.
/// What is left, `[bl, x0)` and `[x1, br)`, is shaded per pixel.
fn mid_span(lo: f32, hi: f32, bl: i32, br: i32) -> (i32, i32) {
    let br = br.max(bl);
    let x0 = ((lo - 0.5).ceil() as i32).max(bl).min(br);
    let x1 = ((hi - 0.5).floor() as i32 + 1).max(x0).min(br);
    (x0, x1)
}

/// Columns of a row (`dy` from the centre) that can touch the annulus between
/// radii `ri` and `ro`: up to two spans, limited to `[bl, br)`, with a pixel of slack.
fn annulus_spans(cx: f32, dy: f32, ri: f32, ro: f32, bl: i32, br: i32) -> [(i32, i32); 2] {
    let none = [(bl, bl), (bl, bl)];
    let dy2 = dy * dy;
    if ro <= 0.0 || dy2 >= ro * ro || bl >= br {
        return none;
    }
    let xo = (ro * ro - dy2).sqrt();
    let col = |v: f32| (v - 0.5).floor() as i32;
    let (ol, or) = (col(cx - xo).max(bl), (col(cx + xo) + 2).min(br));
    if ol >= or {
        return none;
    }
    // In [ol, or]; never panics (unlike clamp with crossed bounds).
    let within = |v: i32| v.max(ol).min(or);
    if ri > 1.0 && dy2 < (ri - 1.0) * (ri - 1.0) {
        // Leave out the hole (shrunk a pixel so no edge pixel is skipped).
        let xi = ((ri - 1.0) * (ri - 1.0) - dy2).sqrt();
        let (il, ir) = (col(cx - xi) + 1, col(cx + xi));
        if il < ir {
            return [(ol, within(il)), (within(ir), or)];
        }
    }
    [(ol, or), (bl, bl)]
}

/// Source-over of `c` (straight alpha) at coverage `cov` on every pixel of a row
/// span: the same maths as `Canvas::blend_px`, with the source worked out once.
fn blend_span(row: &mut [u32], c: Rgba, cov: u32) {
    let a = (c.3 as u32 * cov + 127) / 255;
    if a == 0 {
        return;
    }
    if a >= 255 {
        row.fill(0xFF00_0000 | (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32);
        return;
    }
    let ia = 255 - a;
    let (sr, sg, sb) = (c.0 as u32 * a, c.1 as u32 * a, c.2 as u32 * a);
    for px in row {
        let d = *px;
        let ch = |shift: u32, s: u32| ((s + ((d >> shift) & 0xff) * ia + 127) / 255).min(255) << shift;
        let na = (a + (((d >> 24) & 0xff) * ia + 127) / 255).min(255);
        *px = (na << 24) | ch(16, sr) | ch(8, sg) | ch(0, sb);
    }
}

fn premul(c: Rgba) -> u32 {
    let a = c.3 as u32;
    let p = |v: u8| (v as u32 * a + 127) / 255;
    (a << 24) | (p(c.0) << 16) | (p(c.1) << 8) | p(c.2)
}

pub fn measure_dc(dc: HDC, font: HFONT, s: &str) -> (i32, i32) {
    if dc.is_null() {
        return (0, 0);
    }
    let ws: Vec<u16> = s.encode_utf16().collect();
    let mut sz = SIZE { cx: 0, cy: 0 };
    unsafe {
        let old = SelectObject(dc, font);
        GetTextExtentPoint32W(dc, ws.as_ptr(), ws.len() as i32, &mut sz);
        SelectObject(dc, old);
    }
    (sz.cx, sz.cy)
}

/// Double-buffered opaque painting for normal windows (popup, report).
pub struct Painter {
    pub dc: HDC,
    hwnd: HWND,
    ps: PAINTSTRUCT,
    win_dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub w: i32,
    pub h: i32,
}

impl Painter {
    pub fn begin(hwnd: HWND) -> Painter {
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let win_dc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let (w, h) = (rc.right.max(1), rc.bottom.max(1));
            let dc = CreateCompatibleDC(win_dc);
            let bmp = CreateCompatibleBitmap(win_dc, w, h);
            let old = SelectObject(dc, bmp);
            SetBkMode(dc, TRANSPARENT as i32);
            Painter { dc, hwnd, ps, win_dc, bmp, old, w, h }
        }
    }

    pub fn fill(&self, r: Rect, c: Rgba) {
        unsafe {
            let br = CreateSolidBrush(c.colorref());
            let rc: RECT = r.into();
            FillRect(self.dc, &rc, br);
            DeleteObject(br);
        }
    }

    pub fn round(&self, r: Rect, rad: i32, c: Rgba) {
        unsafe {
            let br = CreateSolidBrush(c.colorref());
            let pen = CreatePen(PS_NULL, 0, 0);
            let ob = SelectObject(self.dc, br);
            let op = SelectObject(self.dc, pen);
            RoundRect(self.dc, r.l, r.t, r.r + 1, r.b + 1, rad, rad);
            SelectObject(self.dc, ob);
            SelectObject(self.dc, op);
            DeleteObject(br);
            DeleteObject(pen);
        }
    }

    pub fn text(&self, font: HFONT, s: &str, x: i32, y: i32, c: Rgba) -> i32 {
        let ws: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let old = SelectObject(self.dc, font);
            SetTextColor(self.dc, c.colorref());
            TextOutW(self.dc, x, y, ws.as_ptr(), ws.len() as i32);
            SelectObject(self.dc, old);
        }
        measure_dc(self.dc, font, s).0
    }

    /// Word-wrapped text inside `r`.
    pub fn text_wrap(&self, font: HFONT, s: &str, r: Rect, c: Rgba) {
        let mut ws: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let old = SelectObject(self.dc, font);
            SetTextColor(self.dc, c.colorref());
            let mut rc: RECT = r.into();
            DrawTextW(self.dc, ws.as_mut_ptr(), ws.len() as i32, &mut rc, DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX);
            SelectObject(self.dc, old);
        }
    }

    pub fn text_right(&self, font: HFONT, s: &str, right: i32, y: i32, c: Rgba) {
        let w = measure_dc(self.dc, font, s).0;
        self.text(font, s, right - w, y, c);
    }

    pub fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        measure_dc(self.dc, font, s)
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        unsafe {
            BitBlt(self.win_dc, 0, 0, self.w, self.h, self.dc, 0, 0, SRCCOPY);
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
            EndPaint(self.hwnd, &self.ps);
        }
    }
}

/// Glyph coverage and text sizes, cached per font and string. Text is drawn at
/// whole pixels, so a string's coverage is the same wherever it lands: GDI
/// rasterises it once and every later frame only blends. Fonts are told apart
/// by their LOGFONT, so a font handle that is freed and reused never shows a
/// stale bitmap.
/// Size of `s` in `font` without a canvas (cached; for layout).
pub fn measure_text(font: HFONT, s: &str) -> (i32, i32) {
    text_cache::measure(font, s)
}

mod text_cache {
    use super::{measure_dc, Dib};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use windows_sys::Win32::Graphics::Gdi::*;

    /// Margin around the text box (the halo and overhangs read into it).
    pub const PAD_L: i32 = 3;
    pub const PAD_T: i32 = 2;
    const PAD_R: i32 = 4;
    const PAD_B: i32 = 2;
    /// Drop everything past this many bitmaps or bytes (live numbers keep changing).
    const MAX_ENTRIES: usize = 6000;
    const MAX_BYTES: usize = 48 << 20;

    pub struct Glyphs {
        pub w: i32,
        pub h: i32,
        /// Advance width of the text.
        pub tw: i32,
        pub cov: Vec<u8>,
    }

    #[derive(Default)]
    struct Font {
        sizes: HashMap<String, (i32, i32)>,
        bmps: HashMap<String, Glyphs>,
    }

    struct Cache {
        /// White-on-black scratch for rasterising (grows to the widest string).
        scratch: Option<(Dib, i32, i32)>,
        fonts: HashMap<u64, Font>,
        entries: usize,
        bytes: usize,
    }

    thread_local! {
        static CACHE: RefCell<Cache> = RefCell::new(Cache { scratch: None, fonts: HashMap::new(), entries: 0, bytes: 0 });
    }

    fn font_key(font: HFONT) -> Option<u64> {
        let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
        let n = unsafe { GetObjectW(font, std::mem::size_of::<LOGFONTW>() as i32, &mut lf as *mut _ as *mut core::ffi::c_void) };
        if n <= 0 {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(&lf as *const LOGFONTW as *const u8, n as usize) };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        Some(h.finish())
    }

    impl Cache {
        fn scratch(&mut self, w: i32, h: i32) -> Option<&Dib> {
            let fits = self.scratch.as_ref().is_some_and(|(_, sw, sh)| *sw >= w && *sh >= h);
            if !fits {
                let (ow, oh) = self.scratch.as_ref().map_or((0, 0), |(_, a, b)| (*a, *b));
                let (nw, nh) = (w.max(ow).max(256), h.max(oh).max(48));
                self.scratch = None;
                let d = Dib::new(nw, nh)?;
                unsafe {
                    SetBkMode(d.dc, TRANSPARENT as i32);
                    SetTextColor(d.dc, 0x00FF_FFFF);
                }
                self.scratch = Some((d, nw, nh));
            }
            self.scratch.as_ref().map(|x| &x.0)
        }

        fn measure(&mut self, key: Option<u64>, font: HFONT, s: &str) -> (i32, i32) {
            if let Some(v) = key.and_then(|k| self.fonts.get(&k)).and_then(|f| f.sizes.get(s)) {
                return *v;
            }
            let dc = self.scratch(1, 1).map_or(std::ptr::null_mut(), |d| d.dc);
            let v = measure_dc(dc, font, s);
            if let Some(k) = key {
                self.room(s.len() + 16);
                self.fonts.entry(k).or_default().sizes.insert(s.to_string(), v);
            }
            v
        }

        /// Make space for `bytes` more: past the limits, start over.
        fn room(&mut self, bytes: usize) {
            self.entries += 1;
            self.bytes += bytes;
            if self.entries > MAX_ENTRIES || self.bytes > MAX_BYTES {
                self.fonts.clear();
                self.entries = 1;
                self.bytes = bytes;
            }
        }

        fn raster(&mut self, key: Option<u64>, font: HFONT, s: &str) -> Option<Glyphs> {
            let (tw, th) = self.measure(key, font, s);
            let (w, h) = (tw + PAD_L + PAD_R, th + PAD_T + PAD_B);
            self.scratch(w, h)?;
            let (d, sw, _) = self.scratch.as_ref()?;
            let stride = *sw as usize;
            let mut cov = vec![0u8; (w * h) as usize];
            unsafe {
                GdiFlush();
                for y in 0..h as usize {
                    std::ptr::write_bytes(d.bits.add(y * stride), 0, w as usize);
                }
                let old = SelectObject(d.dc, font);
                let ws: Vec<u16> = s.encode_utf16().collect();
                TextOutW(d.dc, PAD_L, PAD_T, ws.as_ptr(), ws.len() as i32);
                SelectObject(d.dc, old);
                GdiFlush();
                for y in 0..h as usize {
                    let row = std::slice::from_raw_parts(d.bits.add(y * stride), w as usize);
                    for (o, m) in cov[y * w as usize..(y + 1) * w as usize].iter_mut().zip(row) {
                        *o = ((m >> 16) & 0xff).max((m >> 8) & 0xff).max(m & 0xff) as u8;
                    }
                }
            }
            Some(Glyphs { w, h, tw, cov })
        }
    }

    pub fn measure(font: HFONT, s: &str) -> (i32, i32) {
        let key = font_key(font);
        CACHE.with(|c| c.borrow_mut().measure(key, font, s))
    }

    /// Run `f` on the coverage of `s` in `font` (rasterised on first use).
    pub fn with<R>(font: HFONT, s: &str, f: impl FnOnce(&Glyphs) -> R) -> Option<R> {
        let key = font_key(font);
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if let Some(g) = key.and_then(|k| c.fonts.get(&k)).and_then(|ft| ft.bmps.get(s)) {
                return Some(f(g));
            }
            let g = c.raster(key, font, s)?;
            let Some(k) = key else { return Some(f(&g)) };
            c.room(g.cov.len() + s.len() + 32);
            let ft = c.fonts.entry(k).or_default();
            let g = ft.bmps.entry(s.to_string()).or_insert(g);
            Some(f(g))
        })
    }
}

#[cfg(test)]
mod tests {
    //! The span / annulus / per-segment fast paths must give the very same
    //! pixels as the plain per-pixel versions they replaced (kept here).
    use super::*;

    impl Canvas {
        fn ref_fill_round(&mut self, r: Rect, rad: f32, c: Rgba) {
            let rad = rad.min(r.w() as f32 / 2.0).min(r.h() as f32 / 2.0).max(0.0);
            let w = self.w;
            let (bl, bt, br, bb) = self.bounds(r.l, r.t, r.r, r.b);
            let buf = self.buf();
            for y in bt..bb {
                for x in bl..br {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let cx = px.clamp(r.l as f32 + rad, r.r as f32 - rad);
                    let cy = py.clamp(r.t as f32 + rad, r.b as f32 - rad);
                    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                    let cov = (rad - d + 0.5).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        Canvas::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                    }
                }
            }
        }

        fn ref_stroke_round(&mut self, r: Rect, rad: f32, c: Rgba) {
            let rad = rad.min(r.w() as f32 / 2.0).min(r.h() as f32 / 2.0).max(0.0);
            let inner = Rect { l: r.l + 1, t: r.t + 1, r: r.r - 1, b: r.b - 1 };
            let irad = (rad - 1.0).max(0.0);
            let cov = |px: f32, py: f32, q: Rect, rd: f32| -> f32 {
                let cx = px.clamp(q.l as f32 + rd, q.r as f32 - rd);
                let cy = py.clamp(q.t as f32 + rd, q.b as f32 - rd);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                (rd - d + 0.5).clamp(0.0, 1.0)
            };
            let w = self.w;
            let (bl, bt, br, bb) = self.bounds(r.l, r.t, r.r, r.b);
            let buf = self.buf();
            for y in bt..bb {
                for x in bl..br {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let outer = cov(px, py, r, rad);
                    let inn = if inner.is_empty() { 0.0 } else { cov(px, py, inner, irad) };
                    let v = (outer - inn).clamp(0.0, 1.0);
                    if v > 0.0 {
                        Canvas::blend_px(&mut buf[(y * w + x) as usize], c, (v * 255.0) as u32);
                    }
                }
            }
        }

        fn ref_ring(&mut self, cx: f32, cy: f32, r: f32, thick: f32, frac: f32, c: Rgba) {
            let frac = frac.clamp(0.0, 1.0);
            if frac <= 0.0 {
                return;
            }
            let w = self.w;
            let half = thick / 2.0;
            let mid = r - half;
            let tau = std::f32::consts::TAU;
            let end = frac * tau;
            let caps = [(cx, cy - mid), (cx + mid * end.sin(), cy - mid * end.cos())];
            let (x0, y0, x1, y1) = self.bounds(
                (cx - r - 1.0).floor() as i32,
                (cy - r - 1.0).floor() as i32,
                (cx + r + 2.0).ceil() as i32,
                (cy + r + 2.0).ceil() as i32,
            );
            let buf = self.buf();
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let (dx, dy) = (px - cx, py - cy);
                    let d = (dx * dx + dy * dy).sqrt();
                    let radial = (half - (d - mid).abs() + 0.5).clamp(0.0, 1.0);
                    if radial <= 0.0 {
                        continue;
                    }
                    let mut cov = 0.0f32;
                    if frac >= 1.0 {
                        cov = radial;
                    } else {
                        let mut a = dx.atan2(-dy);
                        if a < 0.0 {
                            a += tau;
                        }
                        if a <= end {
                            cov = radial;
                        }
                        for (qx, qy) in caps {
                            let cd = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
                            cov = cov.max((half - cd + 0.5).clamp(0.0, 1.0));
                        }
                    }
                    if cov > 0.0 {
                        Canvas::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                    }
                }
            }
        }

        fn ref_stroke_arc(&mut self, cx: f32, cy: f32, r: f32, thick: f32, a0: f32, a1: f32, c: Rgba) {
            let tau = std::f32::consts::TAU;
            let span = (a1 - a0).clamp(0.0, tau);
            let half = thick / 2.0;
            let caps = [(cx + r * a0.cos(), cy + r * a0.sin()), (cx + r * (a0 + span).cos(), cy + r * (a0 + span).sin())];
            let ext = r + half + 1.0;
            let w = self.w;
            let (x0, y0, x1, y1) = self.bounds(
                (cx - ext).floor() as i32,
                (cy - ext).floor() as i32,
                (cx + ext).ceil() as i32 + 1,
                (cy + ext).ceil() as i32 + 1,
            );
            let buf = self.buf();
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let (dx, dy) = (px - cx, py - cy);
                    let d = (dx * dx + dy * dy).sqrt();
                    let radial = (half - (d - r).abs() + 0.5).clamp(0.0, 1.0);
                    let mut cov = 0.0f32;
                    if radial > 0.0 {
                        let t = (dy.atan2(dx) - a0).rem_euclid(tau);
                        if t <= span || span >= tau - 1e-4 {
                            cov = radial;
                        }
                    }
                    for (qx, qy) in caps {
                        let cd = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
                        cov = cov.max((half - cd + 0.5).clamp(0.0, 1.0));
                    }
                    if cov > 0.0 {
                        Canvas::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                    }
                }
            }
        }

        fn ref_stroke_poly(&mut self, pts: &[(f32, f32)], thick: f32, shade: impl Fn(f32, f32) -> Rgba) {
            if pts.len() < 2 {
                return;
            }
            let half = thick / 2.0;
            let mut acc = vec![0.0f32; pts.len()];
            for i in 1..pts.len() {
                let (dx, dy) = (pts[i].0 - pts[i - 1].0, pts[i].1 - pts[i - 1].1);
                acc[i] = acc[i - 1] + (dx * dx + dy * dy).sqrt();
            }
            let total = acc[pts.len() - 1].max(1e-3);
            let ext = half + 1.0;
            let lo_x = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min) - ext;
            let hi_x = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) + ext;
            let lo_y = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min) - ext;
            let hi_y = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max) + ext;
            let w = self.w;
            let (x0, y0, x1, y1) = self.bounds(lo_x.floor() as i32, lo_y.floor() as i32, hi_x.ceil() as i32 + 1, hi_y.ceil() as i32 + 1);
            let buf = self.buf();
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let (mut best, mut along) = (f32::MAX, 0.0f32);
                    for i in 1..pts.len() {
                        let (ax, ay) = pts[i - 1];
                        let (dx, dy) = (pts[i].0 - ax, pts[i].1 - ay);
                        let len2 = (dx * dx + dy * dy).max(1e-6);
                        let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
                        let (qx, qy) = (ax + dx * t - px, ay + dy * t - py);
                        let d = (qx * qx + qy * qy).sqrt();
                        if d < best {
                            best = d;
                            along = (acc[i - 1] + t * (acc[i] - acc[i - 1])) / total;
                        }
                    }
                    let cov = (half - best + 0.5).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        Canvas::blend_px(&mut buf[(y * w + x) as usize], shade(px, along), (cov * 255.0) as u32);
                    }
                }
            }
        }

        fn ref_fill_disc(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
            let w = self.w;
            let (x0, y0, x1, y1) =
                self.bounds((cx - r - 1.0).floor() as i32, (cy - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32 + 1, (cy + r + 1.0).ceil() as i32 + 1);
            let buf = self.buf();
            for y in y0..y1 {
                for x in x0..x1 {
                    let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                    let cov = (r - d + 0.5).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        Canvas::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                    }
                }
            }
        }

        fn ref_fill_rect(&mut self, r: Rect, c: Rgba) {
            let w = self.w;
            let (l, t, rr, b) = self.bounds(r.l, r.t, r.r, r.b);
            let buf = self.buf();
            for y in t..b {
                for x in l..rr {
                    Canvas::blend_px(&mut buf[(y * w + x) as usize], c, 255);
                }
            }
        }


    }

    fn pair(w: i32, h: i32) -> (Canvas, Canvas) {
        let (mut a, mut b) = (Canvas::new(), Canvas::new());
        a.resize(w, h);
        b.resize(w, h);
        a.clear(Rgba(30, 40, 50, 200));
        b.clear(Rgba(30, 40, 50, 200));
        (a, b)
    }

    fn same(a: &mut Canvas, b: &mut Canvas, what: &str) {
        let (pa, pb) = (a.pixels().to_vec(), b.pixels().to_vec());
        let diff = pa.iter().zip(&pb).filter(|(x, y)| x != y).count();
        assert_eq!(diff, 0, "{what}: {diff} pixels differ");
    }

    const COLS: [Rgba; 3] = [Rgba(255, 255, 255, 255), Rgba(200, 80, 20, 128), Rgba(10, 220, 90, 17)];

    #[test]
    fn fast_shapes_match_per_pixel_reference() {
        let rects = [
            Rect { l: 10, t: 12, r: 210, b: 140 },
            Rect { l: -20, t: -5, r: 60, b: 30 },
            Rect { l: 100, t: 100, r: 104, b: 160 },
            Rect { l: 5, t: 150, r: 250, b: 171 },
            Rect { l: 180, t: 20, r: 181, b: 21 },
        ];
        for c in COLS {
            for r in rects {
                for rad in [0.0f32, 0.4, 1.0, 3.0, 6.0, 8.5, 20.0, 200.0] {
                    for clip in [None, Some(Rect { l: 33, t: 17, r: 150, b: 120 })] {
                        let (mut a, mut b) = pair(240, 180);
                        a.set_clip(clip);
                        b.set_clip(clip);
                        a.fill_round(r, rad, c);
                        b.ref_fill_round(r, rad, c);
                        same(&mut a, &mut b, &format!("fill_round {r:?} {rad} {clip:?}"));
                        a.stroke_round(r, rad, c);
                        b.ref_stroke_round(r, rad, c);
                        same(&mut a, &mut b, &format!("stroke_round {r:?} {rad} {clip:?}"));
                        a.fill_rect(r, c);
                        b.ref_fill_rect(r, c);
                        same(&mut a, &mut b, &format!("fill_rect {r:?} {clip:?}"));
                    }
                }
            }
        }
    }

    #[test]
    fn fast_circles_match_per_pixel_reference() {
        for c in COLS {
            for (cx, cy, r, th) in [(-40.0f32, 50.0f32, 30.0f32, 6.0f32), (260.0, 50.0, 30.0, 6.0), (100.0, -60.0, 40.0, 5.0), (195.0, 155.0, 30.0, 8.0), (60.0f32, 50.0f32, 30.0f32, 6.0f32), (100.3, 80.7, 44.5, 9.0), (10.0, 10.0, 25.0, 4.0), (120.0, 90.0, 3.0, 3.0), (50.5, 60.5, 0.6, 1.0)] {
                for (frac, clip) in [(0.0f32, None), (0.1, None), (0.5, Some(Rect { l: 70, t: 0, r: 120, b: 160 })), (0.77, Some(Rect { l: 150, t: 40, r: 200, b: 60 })), (1.0, None), (1.0, Some(Rect { l: 0, t: 0, r: 30, b: 160 }))] {
                    let (mut a, mut b) = pair(200, 160);
                    a.set_clip(clip);
                    b.set_clip(clip);
                    a.ring_in(cx, cy, r, th, frac, c, crate::look::Look::Bento);
                    b.ref_ring(cx, cy, r, th, frac, c);
                    same(&mut a, &mut b, &format!("ring {cx} {cy} {r} {th} {frac}"));
                    let (a0, a1) = (frac * 2.0 - 1.0, frac * 6.0 + 0.3);
                    a.stroke_arc(cx, cy, r, th, a0, a1, c);
                    b.ref_stroke_arc(cx, cy, r, th, a0, a1, c);
                    same(&mut a, &mut b, &format!("arc {cx} {cy} {r} {th} {a0} {a1}"));
                }
                let (mut a, mut b) = pair(200, 160);
                a.set_clip(Some(Rect { l: 50, t: 10, r: 90, b: 150 }));
                b.set_clip(Some(Rect { l: 50, t: 10, r: 90, b: 150 }));
                a.fill_disc(cx, cy, r, c);
                b.ref_fill_disc(cx, cy, r, c);
                same(&mut a, &mut b, &format!("disc {cx} {cy} {r}"));
            }
        }
    }

    #[test]
    fn fast_polyline_matches_per_pixel_reference() {
        let zig: Vec<(f32, f32)> = (0..40).map(|i| (5.0 + i as f32 * 6.3, 60.0 + ((i * 37) % 23) as f32 * 3.1 - 30.0)).collect();
        let shade = |x: f32, t: f32| Rgba((x as u32 % 255) as u8, (t * 255.0) as u8, 90, 220);
        for th in [1.0f32, 1.5, 2.0, 5.0] {
            for pts in [zig.clone(), vec![(10.0, 10.0), (10.0, 10.0)], vec![(-20.0, 5.0), (300.0, 150.0), (40.0, 140.0)]] {
                let (mut a, mut b) = pair(260, 160);
                a.stroke_poly(&pts, th, shade);
                b.ref_stroke_poly(&pts, th, shade);
                same(&mut a, &mut b, &format!("poly {th} {}", pts.len()));
            }
        }
    }

    #[test]
    fn shifted_rows_equal_a_fresh_draw() {
        let draw = |cv: &mut Canvas, dy: i32| {
            cv.clear(Rgba(20, 20, 20, 255));
            for i in 0..12 {
                cv.fill_round(Rect { l: 10, t: 15 + i * 40 + dy, r: 190, b: 45 + i * 40 + dy }, 6.0, Rgba(200, 120, 40, 255));
            }
        };
        let band = Rect { l: 0, t: 20, r: 200, b: 180 };
        for d in [-37, -1, 1, 23] {
            let (mut a, mut b) = pair(200, 200);
            draw(&mut a, 0);
            a.shift_rows(band, -d);
            let strip = if d > 0 { Rect { t: band.b - d, ..band } } else { Rect { b: band.t - d, ..band } };
            a.set_limit(Some(strip));
            draw(&mut a, -d);
            a.set_limit(None);
            draw(&mut b, -d);
            let (pa, pb) = (a.pixels().to_vec(), b.pixels().to_vec());
            for y in band.t..band.b {
                let row = |p: &Vec<u32>| p[(y * 200) as usize..(y * 200 + 200) as usize].to_vec();
                assert_eq!(row(&pa), row(&pb), "row {y} after a shift of {d}");
            }
        }
    }
}
