//! Page change transitions of the main window, one per look:
//!
//! * Lime, Boot build-in (550 ms): the old page powers down, a segmented
//!   loading bar runs along the top, each panel draws its frame as an accent
//!   outline and then types in from the left behind a block cursor, in
//!   reading order, with a short lamp-test flash as it lands.
//! * Tactile, Split-flap recall (813 ms): the page is a wall of split-flap
//!   cells; each cell's top flap falls to show the new page, top to bottom
//!   and left to right.
//! * Bento, Curved splash (406 ms): a yellow panel with a bowed leading edge
//!   sweeps over the old page from the navigation side, then lifts off the far
//!   side and the new page settles in. No text on the panel.
//! * Reduced motion (Windows "Animation effects" off): a 120 ms crossfade.
//!
//! Everything is software compositing over two pixel buffers: a snapshot of
//! the old page and the freshly drawn new page (drawn every frame, so live
//! values keep moving under the transition). One pass over the page area per
//! frame; the buffers are kept and reused, nothing is allocated per frame.
//! The direction follows the navigation order: forward when the new page is
//! lower in the pane, mirrored when it is higher.

use crate::look::{Tokens, Transition};
use crate::util::Rect;
use std::time::Instant;

/// Reduced-motion crossfade length (s).
pub const FADE_S: f32 = 0.12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Look(Transition),
    Fade,
}

pub struct PageTrans {
    pub kind: Kind,
    start: Instant,
    pub dur: f32,
    /// +1: from the navigation side (left) to the right; -1 mirrored.
    pub dir: i32,
    /// Page area in client pixels (right of the pane, full height).
    pub region: Rect,
    /// Old page pixels of `region` (premultiplied BGRA, row-major).
    old: Vec<u32>,
    /// Scratch: this frame's new page pixels of `region`.
    new: Vec<u32>,
    /// Panels of the new page (client pixels), in reading order (Boot).
    panels: Vec<Rect>,
    /// Device scale (dpi / 96).
    scale: f32,
}

// ------------------------------------------------------------------ easing

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

fn ease_in_out(x: f32) -> f32 {
    let x = clamp01(x);
    if x < 0.5 {
        4.0 * x * x * x
    } else {
        1.0 - (-2.0 * x + 2.0).powi(3) / 2.0
    }
}

fn ease_out(x: f32) -> f32 {
    1.0 - (1.0 - clamp01(x)).powi(3)
}

fn ease_in(x: f32) -> f32 {
    clamp01(x).powi(2)
}

/// Local progress of a sub-span [a, b] of 0..1.
fn span(t: f32, a: f32, b: f32) -> f32 {
    clamp01((t - a) / (b - a).max(1e-4))
}

// ------------------------------------------------------------------ pixels

/// Premultiplied BGRA of an opaque-ish colour.
pub fn premul(c: crate::render::Rgba) -> u32 {
    let a = c.3 as u32;
    let m = |v: u8| (v as u32 * a + 127) / 255;
    (a << 24) | (m(c.0) << 16) | (m(c.1) << 8) | m(c.2)
}

/// `a` toward `b` by k/256.
#[inline]
pub fn lerp(a: u32, b: u32, k: u32) -> u32 {
    if k == 0 {
        return a;
    }
    if k >= 256 {
        return b;
    }
    let ik = 256 - k;
    let rb = ((a & 0x00FF_00FF) * ik + (b & 0x00FF_00FF) * k) >> 8;
    let ag = (((a >> 8) & 0x00FF_00FF) * ik + ((b >> 8) & 0x00FF_00FF) * k) >> 8;
    (rb & 0x00FF_00FF) | ((ag & 0x00FF_00FF) << 8)
}

/// Darken (k > 0) a premultiplied pixel by k/256, keeping its alpha.
#[inline]
fn shade(p: u32, k: u32) -> u32 {
    if k == 0 {
        return p;
    }
    let ik = 256 - k.min(256);
    let rb = ((p & 0x00FF_00FF) * ik) >> 8;
    let g = (((p >> 8) & 0xFF) * ik) >> 8;
    (p & 0xFF00_0000) | (rb & 0x00FF_00FF) | (g << 8)
}

fn k256(f: f32) -> u32 {
    (clamp01(f) * 256.0).round() as u32
}

impl PageTrans {
    /// Start a transition; `old` is copied out of `buf` (the canvas holding the
    /// old page, `stride` pixels per row). `reuse` hands back the buffers of the
    /// previous transition so nothing is allocated after the first one.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: Kind,
        dur: f32,
        dir: i32,
        region: Rect,
        buf: &[u32],
        stride: i32,
        panels: Vec<Rect>,
        scale: f32,
        reuse: Option<PageTrans>,
    ) -> PageTrans {
        let (w, h) = (region.w().max(0) as usize, region.h().max(0) as usize);
        let (mut old, mut new) = match reuse {
            Some(p) => (p.old, p.new),
            None => (Vec::new(), Vec::new()),
        };
        old.clear();
        old.reserve(w * h);
        for y in 0..h {
            let o = (region.t as usize + y) * stride as usize + region.l as usize;
            match buf.get(o..o + w) {
                Some(row) => old.extend_from_slice(row),
                None => old.extend(std::iter::repeat(0).take(w)),
            }
        }
        new.clear();
        new.resize(w * h, 0);
        let mut panels: Vec<Rect> = panels
            .into_iter()
            .filter_map(|r| {
                let c = Rect { l: r.l.max(region.l), t: r.t.max(region.t), r: r.r.min(region.r), b: r.b.min(region.b) };
                (c.w() > 8 && c.h() > 8).then_some(c)
            })
            .collect();
        // Reading order: rows top to bottom (a little slack), then left to right.
        let row_tol = (24.0 * scale) as i32;
        panels.sort_by_key(|r| (r.t / row_tol.max(1), r.l));
        PageTrans { kind, start: Instant::now(), dur: dur.max(0.01), dir: dir.signum().max(-1), region, old, new, panels, scale }
    }

    /// Linear progress 0..1.
    pub fn t(&self) -> f32 {
        clamp01(self.start.elapsed().as_secs_f32() / self.dur)
    }

    /// Jump to progress `t` (tests and the offline frame check).
    #[cfg(any(test, not(windows)))]
    pub fn set_t(&mut self, t: f32) {
        self.start = Instant::now() - std::time::Duration::from_secs_f32(clamp01(t) * self.dur);
    }

    pub fn done(&self) -> bool {
        self.start.elapsed().as_secs_f32() >= self.dur
    }

    /// Vertical offset (px) to draw the new page at this frame: Bento's new
    /// page settles 8 epx down to 0 as the panel lifts off.
    pub fn page_dy(&self) -> i32 {
        match self.kind {
            Kind::Look(Transition::Splash) => {
                let p = span(self.t(), SPLASH_OUT, 1.0);
                if p <= 0.0 {
                    return 0;
                }
                // Small spring: overshoot a touch past 0, then rest.
                let s = 1.0 - (1.0 - p).powi(3) * (1.0 + 2.2 * p);
                ((1.0 - s) * 8.0 * self.scale).round() as i32
            }
            _ => 0,
        }
    }

    /// Compose this frame into `buf` (the canvas, `stride` pixels per row),
    /// which already holds the new page drawn over `region`.
    pub fn compose(&mut self, buf: &mut [u32], stride: i32, tk: &Tokens) {
        let r = self.region;
        let (w, h) = (r.w().max(0) as usize, r.h().max(0) as usize);
        if w == 0 || h == 0 || self.old.len() != w * h || self.new.len() != w * h {
            return;
        }
        let s = stride as usize;
        if buf.len() < (r.b as usize - 1) * s + r.r as usize {
            return;
        }
        // This frame's new page.
        for y in 0..h {
            let o = (r.t as usize + y) * s + r.l as usize;
            self.new[y * w..(y + 1) * w].copy_from_slice(&buf[o..o + w]);
        }
        let t = self.t();
        match self.kind {
            Kind::Fade => {
                let k = k256(t);
                for y in 0..h {
                    let o = (r.t as usize + y) * s + r.l as usize;
                    for x in 0..w {
                        buf[o + x] = lerp(self.old[y * w + x], self.new[y * w + x], k);
                    }
                }
            }
            Kind::Look(Transition::Splash) => self.splash(buf, s, w, h, t, tk),
            Kind::Look(Transition::Flap) => self.flap(buf, s, w, h, t, tk),
            Kind::Look(Transition::Boot) => self.boot(buf, s, w, h, t, tk),
        }
    }

    // -------------------------------------------------------------- Bento

    fn splash(&self, buf: &mut [u32], s: usize, w: usize, h: usize, t: f32, tk: &Tokens) {
        let r = self.region;
        let acc = premul(tk.accent_fill);
        let bow = (w as f32 * 0.28).min(110.0 * self.scale);
        let travel = w as f32 + 2.0 * bow;
        // In: the leading edge crosses; hold; out: the trailing edge crosses.
        // In: emphasized decelerate (fast start, soft landing); out: ease in-out.
        let (pin, pout) = (1.0 - (1.0 - span(t, 0.0, SPLASH_IN)).powi(4), ease_in_out(span(t, SPLASH_OUT, 1.0)));
        let hh = h as f32 / 2.0;
        let (out, edge_t) = if t < SPLASH_OUT { (false, pin) } else { (true, pout) };
        for y in 0..h {
            let v = (y as f32 + 0.5 - hh) / hh.max(1.0);
            let bulge = bow * (1.0 - v * v);
            let o = (r.t as usize + y) * s + r.l as usize;
            // Edge position in "forward" coordinates (0 = navigation side):
            // in, panel before it and the old page after; out, the new page
            // before it and the panel after.
            let e = -2.0 * bow + travel * edge_t + bulge;
            let (before, after): (&dyn Fn(usize) -> u32, &dyn Fn(usize) -> u32) = if out {
                (&|i| self.new[i], &|_| acc)
            } else {
                (&|_| acc, &|i| self.old[i])
            };
            let row = &mut buf[o..o + w];
            let full = (e - 0.5).floor().clamp(0.0, w as f32) as usize; // fully covered pixels
            let edge = (full + 1).min(w);
            let fill_fwd = |row: &mut [u32], fx0: usize, fx1: usize, f: &dyn Fn(usize) -> u32| {
                for fx in fx0..fx1 {
                    let x = if self.dir >= 0 { fx } else { w - 1 - fx };
                    row[x] = f(y * w + x);
                }
            };
            // Bulk spans: the solid accent and plain page parts are straight copies.
            if !out && self.dir >= 0 {
                row[..full].fill(acc);
                row[edge..].copy_from_slice(&self.old[y * w + edge..y * w + w]);
            } else if out && self.dir >= 0 {
                row[..full].copy_from_slice(&self.new[y * w..y * w + full]);
                row[edge..].fill(acc);
            } else {
                fill_fwd(row, 0, full, before);
                fill_fwd(row, edge, w, after);
            }
            // The anti-aliased edge pixel.
            if full < w {
                let fx = full;
                let x = if self.dir >= 0 { fx } else { w - 1 - fx };
                let cov = clamp01(e - (fx as f32 + 0.5) + 0.5);
                row[x] = lerp(after(y * w + x), before(y * w + x), k256(cov));
            }
        }
    }

    // -------------------------------------------------------------- Tactile

    fn flap(&self, buf: &mut [u32], s: usize, w: usize, h: usize, t: f32, tk: &Tokens) {
        let r = self.region;
        let ch = ((56.0 * self.scale).round() as usize).max(8);
        let cw = ((168.0 * self.scale).round() as usize).max(16);
        let rows = h.div_ceil(ch).max(1);
        let cols = w.div_ceil(cw).max(1);
        const FLIP: f32 = 0.36;
        let spread = 1.0 - FLIP;
        let gap = premul(tk.border2);
        let hinge = premul(crate::render::Rgba(0, 0, 0, 255));
        // Thin gaps between the cells while the wall is moving (gone at the end).
        let gap_k = k256(0.55 * span(t, 0.0, 0.08) * (1.0 - span(t, 0.9, 1.0)));
        for row in 0..rows {
            let y0 = row * ch;
            let y1 = (y0 + ch).min(h);
            let mid = (y0 + y1) / 2;
            let half_top = (mid - y0).max(1) as f32;
            let half_bot = (y1 - mid).max(1) as f32;
            for col in 0..cols {
                let cx = if self.dir >= 0 { col } else { cols - 1 - col };
                let x0 = cx * cw;
                let x1 = (x0 + cw).min(w);
                let rf = if rows > 1 { row as f32 / (rows - 1) as f32 } else { 0.0 };
                let cf = if cols > 1 { col as f32 / (cols - 1) as f32 } else { 0.0 };
                let delay = (rf * 0.78 + cf * 0.22) * spread;
                let f = span(t, delay, delay + FLIP);
                for y in y0..y1 {
                    let o = (r.t as usize + y) * s + r.l as usize;
                    // Which source row (and buffer) shows at this row of the cell.
                    let (src, dark): (Option<(bool, usize)>, u32) = if f <= 0.0 {
                        (Some((false, y)), 0)
                    } else if f >= 1.0 {
                        (Some((true, y)), 0)
                    } else if f < 0.5 {
                        // Top flap (old top half) folds down onto the hinge.
                        let g = ease_in(f / 0.5);
                        let fold = (g * std::f32::consts::FRAC_PI_2).cos();
                        if y < mid {
                            let top = mid as f32 - half_top * fold;
                            if (y as f32) < top {
                                (Some((true, y)), 0)
                            } else {
                                let u = ((y as f32 - top) / (half_top * fold).max(0.5)).min(1.0);
                                let sy = y0 + (u * half_top) as usize;
                                (Some((false, sy.min(mid.saturating_sub(1)))), k256(g * 0.55))
                            }
                        } else {
                            (Some((false, y)), 0)
                        }
                    } else {
                        // Bottom flap (new bottom half) falls from the hinge.
                        let g = ease_out((f - 0.5) / 0.5);
                        let open = (g * std::f32::consts::FRAC_PI_2).sin();
                        if y < mid {
                            (Some((true, y)), 0)
                        } else {
                            let bot = mid as f32 + half_bot * open;
                            if (y as f32) < bot {
                                let u = ((y - mid) as f32 / (half_bot * open).max(0.5)).min(1.0);
                                let sy = mid + (u * half_bot) as usize;
                                (Some((true, sy.min(y1 - 1))), k256((1.0 - g) * 0.4))
                            } else {
                                (Some((false, y)), 0)
                            }
                        }
                    };
                    let Some((use_new, sy)) = src else { continue };
                    let b = if use_new { &self.new } else { &self.old };
                    let row_src = &b[sy * w..sy * w + w];
                    for x in x0..x1 {
                        buf[o + x] = shade(row_src[x], dark);
                    }
                    // The hinge line of a moving cell.
                    if f > 0.0 && f < 1.0 && y == mid {
                        for x in x0..x1 {
                            buf[o + x] = lerp(buf[o + x], hinge, 120);
                        }
                    }
                    if gap_k > 0 {
                        if y == y0 {
                            for x in x0..x1 {
                                buf[o + x] = lerp(buf[o + x], gap, gap_k);
                            }
                        }
                        if x0 < x1 {
                            buf[o + x0] = lerp(buf[o + x0], gap, gap_k);
                        }
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------- Lime

    fn boot(&self, buf: &mut [u32], s: usize, w: usize, h: usize, t: f32, tk: &Tokens) {
        let r = self.region;
        let bg = premul(tk.bg);
        let acc = premul(tk.accent);
        let track = premul(tk.raised2);
        let sc = |v: i32| ((v as f32 * self.scale).round() as i32).max(1);
        // 1. The old page powers down; the base shows, then the page chrome
        //    (headings, header) fades up outside the panels.
        let off = k256(span(t, 0.0, 0.16));
        let chrome = k256(ease_out(span(t, 0.14, 0.4)));
        for y in 0..h {
            let o = (r.t as usize + y) * s + r.l as usize;
            for x in 0..w {
                let i = y * w + x;
                let base = lerp(self.old[i], bg, off);
                buf[o + x] = if t < 0.14 { base } else { lerp(bg, self.new[i], chrome) };
            }
        }
        // 2. Panels: outline, then type in behind a block cursor, then a lamp flash.
        let n = self.panels.len().max(1) as f32;
        let (first, last) = (0.16f32, 0.62f32);
        let each = 0.34f32;
        for (k, p) in self.panels.iter().enumerate() {
            let start = first + (last - first) * (k as f32 / (n - 1.0).max(1.0)).min(1.0);
            let lt = span(t, start, start + each);
            let (pl, pt) = ((p.l - r.l) as usize, (p.t - r.t) as usize);
            let (pw, ph) = (p.w() as usize, p.h() as usize);
            // Not powered yet: bare base inside the panel.
            let reveal = ease_out(span(lt, 0.3, 0.9));
            let edge = pl + (pw as f32 * reveal) as usize;
            // While the old page is still powering down, an idle panel leaves it be.
            if lt > 0.0 || t >= 0.14 {
                for y in pt..pt + ph {
                    let o = (r.t as usize + y) * s + r.l as usize;
                    for x in pl..pl + pw {
                        buf[o + x] = if x < edge { self.new[y * w + x] } else { bg };
                    }
                }
            }
            // Outline draws around the panel (top/bottom left to right, sides top down).
            let ol = ease_out(span(lt, 0.0, 0.32));
            let fade_line = 1.0 - span(lt, 0.85, 1.0);
            if ol > 0.0 && fade_line > 0.0 {
                let k = k256(fade_line);
                let len_x = (pw as f32 * ol) as usize;
                let len_y = (ph as f32 * ol) as usize;
                for x in pl..pl + len_x {
                    for y in [pt, pt + ph - 1] {
                        let o = (r.t as usize + y) * s + r.l as usize + x;
                        buf[o] = lerp(buf[o], acc, k);
                    }
                }
                for y in pt..pt + len_y {
                    for x in [pl, pl + pw - 1] {
                        let o = (r.t as usize + y) * s + r.l as usize + x;
                        buf[o] = lerp(buf[o], acc, k);
                    }
                }
            }
            // Block cursor riding the reveal edge, on the first text line.
            if lt > 0.3 && lt < 0.95 {
                let cw = sc(7) as usize;
                let cy0 = pt + (sc(12) as usize).min(ph / 3);
                let cy1 = (cy0 + sc(14) as usize).min(pt + ph);
                let blink = ((t * 18.0) as i32) % 2 == 0 || lt > 0.85;
                if blink {
                    for y in cy0..cy1 {
                        for x in edge.min(pl + pw)..(edge + cw).min(pl + pw) {
                            buf[(r.t as usize + y) * s + r.l as usize + x] = acc;
                        }
                    }
                }
            }
            // Lamp test: the panel's lit parts flash brighter as it lands.
            let lamp = span(lt, 0.86, 1.0);
            if lamp > 0.0 && lamp < 1.0 {
                let k = k256(0.12 * (1.0 - lamp));
                for y in pt..pt + ph {
                    let o = (r.t as usize + y) * s + r.l as usize;
                    for x in pl..pl + pw {
                        buf[o + x] = lerp(buf[o + x], acc, k);
                    }
                }
            }
        }
        // 3. Segmented loading bar along the top of the page.
        let fade_bar = 1.0 - span(t, 0.9, 1.0);
        if fade_bar > 0.0 {
            let segs = 32usize;
            let gap = sc(3) as usize;
            let bh = sc(3) as usize;
            let seg_w = (w.saturating_sub(gap * (segs - 1)) / segs).max(1);
            let lit = (segs as f32 * ease_out(span(t, 0.05, 0.85))).round() as usize;
            let kf = k256(fade_bar);
            for sgi in 0..segs {
                let x0 = sgi * (seg_w + gap);
                let fx = |x: usize| if self.dir >= 0 { x } else { w.saturating_sub(1 + x) };
                let c = if sgi < lit { acc } else { track };
                for y in 0..bh.min(h) {
                    let o = (r.t as usize + y) * s + r.l as usize;
                    for x in x0..(x0 + seg_w).min(w) {
                        let xx = fx(x);
                        buf[o + xx] = lerp(buf[o + xx], c, kf);
                    }
                }
            }
        }
    }
}

/// Bento splash phases: the panel covers by `SPLASH_IN`, holds, lifts from `SPLASH_OUT`.
const SPLASH_IN: f32 = 0.354;
const SPLASH_OUT: f32 = 0.508;

/// The transition the current look plays, or the reduced-motion crossfade.
pub fn pick(animate: bool) -> (Kind, f32) {
    if !animate {
        return (Kind::Fade, FADE_S);
    }
    let (tr, d) = crate::look::current().transition();
    (Kind::Look(tr), d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_ends() {
        let (a, b) = (0xFF10_2030u32, 0xFFF0_E0D0u32);
        assert_eq!(lerp(a, b, 0), a);
        assert_eq!(lerp(a, b, 256), b);
        let m = lerp(a, b, 128);
        assert_eq!((m >> 16) & 0xFF, (0x10 + 0xF0) / 2);
        assert_eq!(m >> 24, 0xFF);
    }

    #[test]
    fn shade_keeps_alpha() {
        assert_eq!(shade(0xFF80_8080, 128) >> 24, 0xFF);
        assert_eq!(shade(0xFF80_8080, 128) & 0xFF, 0x40);
    }

    #[test]
    fn eases_hit_ends() {
        for f in [ease_in_out, ease_out, ease_in] {
            assert!(f(0.0).abs() < 1e-6 && (f(1.0) - 1.0).abs() < 1e-6);
        }
    }

    fn run(kind: Kind, t_override: f32) -> Vec<u32> {
        let (w, h) = (64i32, 48i32);
        let old = vec![0xFF11_1111u32; (w * h) as usize];
        let mut tr = PageTrans::new(kind, 1.0, 1, Rect { l: 0, t: 0, r: w, b: h }, &old, w, vec![Rect { l: 4, t: 8, r: 40, b: 30 }], 1.0, None);
        tr.start = Instant::now() - std::time::Duration::from_secs_f32(t_override);
        let mut buf = vec![0xFF99_9999u32; (w * h) as usize];
        let tk = crate::look::tokens_for(crate::look::Look::Lime, true);
        tr.compose(&mut buf, w, &tk);
        buf
    }

    #[test]
    fn every_transition_starts_old_and_ends_new() {
        for kind in [Kind::Fade, Kind::Look(Transition::Splash), Kind::Look(Transition::Flap), Kind::Look(Transition::Boot)] {
            let end = run(kind, 1.0);
            assert!(end.iter().all(|p| *p == 0xFF99_9999), "{:?} ends on the new page", kind);
            if kind != Kind::Look(Transition::Boot) {
                let start = run(kind, 0.0);
                assert!(start.iter().all(|p| *p == 0xFF11_1111), "{:?} starts on the old page", kind);
            }
        }
    }
}
