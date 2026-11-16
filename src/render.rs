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
    px: Option<Dib>,
    mask: Option<Dib>,
}

impl Canvas {
    pub fn new() -> Self {
        Canvas { w: 0, h: 0, px: None, mask: None }
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
        if let Some(m) = &self.mask {
            unsafe {
                SetBkMode(m.dc, TRANSPARENT as i32);
                SetTextColor(m.dc, 0x00FF_FFFF);
            }
        }
        self.w = w;
        self.h = h;
    }

    fn buf(&mut self) -> &mut [u32] {
        match &self.px {
            Some(d) => unsafe { std::slice::from_raw_parts_mut(d.bits, (self.w * self.h) as usize) },
            None => &mut [],
        }
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
        let (w, h) = (self.w, self.h);
        let (l, t, rr, b) = (r.l.max(0), r.t.max(0), r.r.min(w), r.b.min(h));
        let buf = self.buf();
        for y in t..b {
            for x in l..rr {
                Self::blend_px(&mut buf[(y * w + x) as usize], c, 255);
            }
        }
    }

    /// Anti-aliased rounded rectangle.
    pub fn fill_round(&mut self, r: Rect, rad: f32, c: Rgba) {
        let (w, h) = (self.w, self.h);
        let buf = self.buf();
        for y in r.t.max(0)..r.b.min(h) {
            for x in r.l.max(0)..r.r.min(w) {
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

    pub fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        measure_dc(self.mask.as_ref().map_or(std::ptr::null_mut(), |m| m.dc), font, s)
    }

    /// Draw text with its top-left at (x, y). Returns advance width.
    pub fn text(&mut self, font: HFONT, s: &str, x: i32, y: i32, c: Rgba) -> i32 {
        let Some(m) = &self.mask else { return 0 };
        let (tw, th) = measure_dc(m.dc, font, s);
        let (w, h) = (self.w, self.h);
        let l = (x - 1).max(0);
        let t = y.max(0);
        let r = (x + tw + 2).min(w);
        let b = (y + th).min(h);
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
        let buf = self.buf();
        for yy in t..b {
            for xx in l..r {
                let i = (yy * w + xx) as usize;
                let m = unsafe { *mbits.add(i) };
                let cov = ((m >> 16) & 0xff).max((m >> 8) & 0xff).max(m & 0xff);
                if cov != 0 {
                    Self::blend_px(&mut buf[i], c, cov);
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
                let yf = yy as f32;
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

    /// Push to a layered window at screen position (x, y).
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

    pub fn text_right(&self, font: HFONT, s: &str, right: i32, y: i32, c: Rgba) {
        let w = measure_dc(self.dc, font, s).0;
        self.text(font, s, right - w, y, c);
    }

    /// Text clipped with ellipsis to max width.
    pub fn text_fit(&self, font: HFONT, s: &str, x: i32, y: i32, max_w: i32, c: Rgba) {
        let mut ws: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let old = SelectObject(self.dc, font);
            SetTextColor(self.dc, c.colorref());
            let mut rc = RECT { left: x, top: y, right: x + max_w, bottom: y + 200 };
            DrawTextW(
                self.dc,
                ws.as_mut_ptr(),
                ws.len() as i32,
                &mut rc,
                DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            SelectObject(self.dc, old);
        }
    }

    pub fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        measure_dc(self.dc, font, s)
    }

    /// Line chart with soft fill. data oldest→newest 0..1.
    pub fn chart(&self, r: Rect, data: &[f32], c: Rgba, bg: Rgba) {
        if data.len() < 2 || r.is_empty() {
            return;
        }
        let fill = bg.mix(c, 0.28);
        let n = data.len();
        unsafe {
            let mut pts: Vec<POINT> = Vec::with_capacity(n + 2);
            for (i, v) in data.iter().enumerate() {
                let x = r.l + (r.w() - 1) * i as i32 / (n - 1) as i32;
                let y = r.b - 1 - ((r.h() - 2) as f32 * v.clamp(0.0, 1.0)).round() as i32;
                pts.push(POINT { x, y });
            }
            let line: Vec<POINT> = pts.clone();
            pts.push(POINT { x: r.r - 1, y: r.b });
            pts.push(POINT { x: r.l, y: r.b });
            let br = CreateSolidBrush(fill.colorref());
            let np = CreatePen(PS_NULL, 0, 0);
            let ob = SelectObject(self.dc, br);
            let op = SelectObject(self.dc, np);
            Polygon(self.dc, pts.as_ptr(), pts.len() as i32);
            SelectObject(self.dc, op);
            DeleteObject(np);
            let pen = CreatePen(PS_SOLID, 1, c.colorref());
            let op = SelectObject(self.dc, pen);
            Polyline(self.dc, line.as_ptr(), line.len() as i32);
            SelectObject(self.dc, op);
            SelectObject(self.dc, ob);
            DeleteObject(pen);
            DeleteObject(br);
        }
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
