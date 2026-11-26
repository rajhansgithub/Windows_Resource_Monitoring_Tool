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
    px: Option<Dib>,
    mask: Option<Dib>,
}

impl Canvas {
    pub fn new() -> Self {
        Canvas { w: 0, h: 0, halo: None, clip: None, px: None, mask: None }
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        let (w, h) = (w.max(1), h.max(1));
        if w == self.w && h == self.h && self.px.is_some() {
            return;
        }
        self.px = None;
        self.mask = None;
        self.px = Dib::new(w, h);
        self.mask = Dib::new(w, h);
        if self.px.is_none() || self.mask.is_none() {
            self.px = None;
            self.mask = None;
            self.w = 0;
            self.h = 0;
            return;
        }
        if let Some(m) = &self.mask {
            unsafe {
                SetBkMode(m.dc, TRANSPARENT as i32);
                SetTextColor(m.dc, 0x00FF_FFFF);
            }
        }
        self.w = w;
        self.h = h;
    }

    /// Limit drawing (fills, outlines, text) to `r`; None = no limit.
    pub fn set_clip(&mut self, r: Option<Rect>) {
        self.clip = r;
    }

    /// The current clip rectangle (to restore it after a nested clip).
    pub fn clip(&self) -> Option<Rect> {
        self.clip
    }

    /// `l, t, r, b` limited to the canvas and the clip rectangle.
    fn bounds(&self, l: i32, t: i32, r: i32, b: i32) -> (i32, i32, i32, i32) {
        let (mut l, mut t, mut r, mut b) = (l.max(0), t.max(0), r.min(self.w), b.min(self.h));
        if let Some(c) = self.clip {
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
        self.buf().fill(p);
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
        let buf = self.buf();
        for y in t..b {
            for x in l..rr {
                Self::blend_px(&mut buf[(y * w + x) as usize], c, 255);
            }
        }
    }

    /// Anti-aliased rounded rectangle.
    pub fn fill_round(&mut self, r: Rect, rad: f32, c: Rgba) {
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
                    Self::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Anti-aliased 1px outline just inside `r` (leaves the interior untouched,
    /// so translucent fills drawn over it don't pick up the border colour).
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
        let buf = self.buf();
        for y in bt..bb {
            for x in bl..br {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let outer = cov(px, py, r, rad);
                let inn = if inner.is_empty() { 0.0 } else { cov(px, py, inner, irad) };
                let v = (outer - inn).clamp(0.0, 1.0);
                if v > 0.0 {
                    Self::blend_px(&mut buf[(y * w + x) as usize], c, (v * 255.0) as u32);
                }
            }
        }
    }

    pub fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        measure_dc(self.mask.as_ref().map_or(std::ptr::null_mut(), |m| m.dc), font, s)
    }

    /// Draw text with its top-left at (x, y). Returns advance width.
    pub fn text(&mut self, font: HFONT, s: &str, x: i32, y: i32, c: Rgba) -> i32 {
        let Some(m) = &self.mask else { return 0 };
        let (tw, th) = measure_dc(m.dc, font, s);
        let (w, h) = (self.w, self.h);
        // Cleared area has a margin so the halo pass can read 1px around glyphs.
        let l = (x - 3).max(0);
        let t = (y - 2).max(0);
        let r = (x + tw + 4).min(w);
        let b = (y + th + 2).min(h);
        if l >= r || t >= b {
            return tw;
        }
        let mbits = m.bits;
        let mdc = m.dc;
        unsafe {
            GdiFlush();
            for yy in t..b {
                std::ptr::write_bytes(mbits.add((yy * w + l) as usize), 0, (r - l) as usize);
            }
            let old = SelectObject(mdc, font);
            let ws: Vec<u16> = s.encode_utf16().collect();
            TextOutW(mdc, x, y, ws.as_ptr(), ws.len() as i32);
            SelectObject(mdc, old);
            GdiFlush();
        }
        let cov_at = |xx: i32, yy: i32| -> u32 {
            if xx < l || xx >= r || yy < t || yy >= b {
                return 0;
            }
            let m = unsafe { *mbits.add((yy * w + xx) as usize) };
            ((m >> 16) & 0xff).max((m >> 8) & 0xff).max(m & 0xff)
        };
        let halo = self.halo;
        // Blend only inside the clip (the mask above covers the whole text).
        let (cl, ct, cr, cb) = self.bounds(l, t, r, b);
        let buf = match &self.px {
            Some(d) => unsafe { std::slice::from_raw_parts_mut(d.bits, (w * h) as usize) },
            None => return tw,
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
            for xx in cl..cr {
                let cov = cov_at(xx, yy);
                if cov != 0 {
                    Self::blend_px(&mut buf[(yy * w + xx) as usize], c, lut[cov as usize] as u32);
                }
            }
        }
        tw
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
        let n = data.len();
        let (w, h) = (self.w, self.h);
        let rw = r.w();
        let rh = r.h() as f32;
        let fill = c.with_a((c.3 as u32 * 70 / 255) as u8);
        let buf = self.buf();
        for i in 0..rw {
            // column i maps to sample index (right-aligned, one sample per 2px max)
            let pos = (n as f32 - 1.0) * (i as f32 / (rw - 1).max(1) as f32);
            let a = data[pos.floor() as usize];
            let bb = data[(pos.ceil() as usize).min(n - 1)];
            let v = (a + (bb - a) * pos.fract()).clamp(0.0, 1.0);
            let top = rh * (1.0 - v);
            let x = r.l + i;
            if x < 0 || x >= w {
                continue;
            }
            for yy in 0..r.h() {
                let y = r.t + yy;
                if y < 0 || y >= h {
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

    /// Anti-aliased ring arc from 12 o'clock, clockwise, `frac` 0..1, round caps.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, thick: f32, frac: f32, c: Rgba) {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= 0.0 {
            return;
        }
        let (w, h) = (self.w, self.h);
        let half = thick / 2.0;
        let mid = r - half;
        let tau = std::f32::consts::TAU;
        let end = frac * tau;
        let caps = [(cx, cy - mid), (cx + mid * end.sin(), cy - mid * end.cos())];
        let buf = self.buf();
        let (x0, x1) = (((cx - r - 1.0).floor() as i32).max(0), ((cx + r + 2.0).ceil() as i32).min(w));
        let (y0, y1) = (((cy - r - 1.0).floor() as i32).max(0), ((cy + r + 2.0).ceil() as i32).min(h));
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
                    Self::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
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
                    Self::blend_px(&mut buf[(y * w + x) as usize], c, (cov * 255.0) as u32);
                }
            }
        }
    }

    pub fn text_center(&mut self, font: HFONT, s: &str, cx: i32, y: i32, c: Rgba) {
        let (tw, _) = self.measure(font, s);
        self.text(font, s, cx - tw / 2, y, c);
    }

    /// Shorten with "…" to fit `max_w`.
    pub fn ellipsize(&self, font: HFONT, s: &str, max_w: i32) -> String {
        if self.measure(font, s).0 <= max_w {
            return s.to_string();
        }
        let mut v: String = s.to_string();
        while !v.is_empty() && self.measure(font, &format!("{}\u{2026}", v)).0 > max_w {
            v.pop();
        }
        format!("{}\u{2026}", v)
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
