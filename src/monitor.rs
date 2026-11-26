//! The Monitor page of the main window: a detailed, live view of the whole machine.
//! Opened from the tray System popup. Everything is drawn here; `settings_ui` only
//! places it in the page column, scrolls it and forwards clicks (`Hit`).
//!
//! The layout reflows by width: vitals 2 / 3 / all across, sections in 1 to 4 columns
//! (CPU spans two), so it fills a maximized window instead of leaving a thin column.
//! Per-core, per-engine, per-disk and per-adapter history is kept only while the page
//! is on screen; whole-machine history comes from the app's hour of samples.

use crate::app::{accent, heat, heat_text, App, HIST};
use crate::fmt;
use crate::insights::view::{Colors, Fonts};
use crate::popup::{avatar_col, dial, log_frac};
use crate::render::{Canvas, Rgba};
use crate::sensors::detail::{ghz, Detail, ProcRow};
use crate::sensors::Snapshot;
use crate::settings::Metric;
use crate::usage;
use crate::util::Rect;
use std::collections::VecDeque;

/// Process rows shown.
const TOPN: usize = 14;
/// Chart ranges, seconds.
const RANGES: [(&str, usize); 3] = [("1m", 60), ("5m", 300), ("1h", 3600)];
const SORTS: [&str; 4] = ["CPU", "Memory", "Disk", "GPU"];

/// What a click on the page does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Range(usize),
    Sort(u8),
    Proc(u32),
    TaskMgr,
    ResMon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    Cpu,
    Mem,
    Gpu,
    Disk,
    Net,
    Procs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vit {
    Cpu,
    Mem,
    Gpu,
    Vram,
    Disk,
    Net,
}

/// History of one named device (two traces).
struct Series {
    key: String,
    a: VecDeque<f32>,
    b: VecDeque<f32>,
}

impl Series {
    fn new(key: &str) -> Series {
        Series { key: key.to_string(), a: VecDeque::new(), b: VecDeque::new() }
    }
    fn push(&mut self, a: f32, b: f32) {
        push(&mut self.a, a);
        push(&mut self.b, b);
    }
}

fn push(q: &mut VecDeque<f32>, v: f32) {
    if q.len() >= HIST {
        q.pop_front();
    }
    q.push_back(v);
}

/// Keep `list` in step with `keys` (same order); returns whether the set changed.
fn sync_series(list: &mut Vec<Series>, keys: &[&str]) -> bool {
    let same = list.len() == keys.len() && list.iter().zip(keys).all(|(s, k)| s.key == *k);
    if !same {
        let mut old = std::mem::take(list);
        for k in keys {
            match old.iter().position(|s| s.key == *k) {
                Some(i) => list.push(old.swap_remove(i)),
                None => list.push(Series::new(k)),
            }
        }
    }
    !same
}

pub struct Monitor {
    pub range: usize,
    pub sort: u8,
    dpi: u32,
    warn: f32,
    crit: f32,
    /// Sample interval, ms (history samples per second).
    ms: usize,
    snap: Snapshot,
    det: Detail,
    // Whole-machine history (copied from the app each tick, newest last).
    h_cpu: Vec<f32>,
    h_ram: Vec<f32>,
    h_gpu: Vec<f32>,
    h_vram: Vec<f32>,
    h_dact: Vec<f32>,
    h_down: Vec<f32>,
    h_up: Vec<f32>,
    // Kept here while the page is open.
    cores: Vec<VecDeque<f32>>,
    kern: VecDeque<f32>,
    engines: Vec<Series>,
    disks: Vec<Series>,
    nets: Vec<Series>,
    /// Last 14 days (recv, sent), oldest first; month total.
    days: Vec<(u64, u64)>,
    month: u64,
    top: Vec<ProcRow>,
    has_data: bool,
    // ---- layout (content coordinates)
    l: i32,
    r: i32,
    y0: i32,
    vits: Vec<(Vit, Rect)>,
    boxes: Vec<(Sec, Rect)>,
    pub hits: Vec<(Rect, Hit)>,
}

impl Monitor {
    pub fn new() -> Monitor {
        Monitor {
            range: 60,
            sort: 0,
            dpi: 96,
            warn: 75.0,
            crit: 90.0,
            ms: 1000,
            snap: Snapshot::default(),
            det: Detail::default(),
            h_cpu: Vec::new(),
            h_ram: Vec::new(),
            h_gpu: Vec::new(),
            h_vram: Vec::new(),
            h_dact: Vec::new(),
            h_down: Vec::new(),
            h_up: Vec::new(),
            cores: Vec::new(),
            kern: VecDeque::new(),
            engines: Vec::new(),
            disks: Vec::new(),
            nets: Vec::new(),
            days: Vec::new(),
            month: 0,
            top: Vec::new(),
            has_data: false,
            l: 0,
            r: 0,
            y0: 0,
            vits: Vec::new(),
            boxes: Vec::new(),
            hits: Vec::new(),
        }
    }

    /// Forget per-device history (the page closed).
    pub fn clear(&mut self) {
        self.cores.clear();
        self.kern.clear();
        self.engines.clear();
        self.disks.clear();
        self.nets.clear();
        self.det.procs = Vec::new();
        self.top.clear();
        self.has_data = false;
    }

    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    fn scf(&self, v: f32) -> f32 {
        v * self.dpi as f32 / 96.0
    }

    /// Samples covering the chart range.
    fn n(&self) -> usize {
        (self.range * 1000 / self.ms.max(1)).clamp(2, HIST)
    }

    /// Take the sample the app just made. Returns true when the page must be laid out
    /// again (a device appeared or went away).
    pub fn feed(&mut self, a: &App) -> bool {
        let shape = self.shape();
        self.ms = a.m.s.interval_ms.max(100) as usize;
        self.warn = a.m.s.warn_pct;
        self.crit = a.m.s.crit_pct;
        self.snap = a.m.snap.clone();
        self.det = a.detail().clone();
        let copy = |m: Metric| a.m.hist[m.idx()].iter().copied().collect::<Vec<f32>>();
        self.h_cpu = copy(Metric::Cpu);
        self.h_ram = copy(Metric::Ram);
        self.h_gpu = copy(Metric::Gpu);
        self.h_vram = copy(Metric::Vram);
        self.h_dact = copy(Metric::DiskActive);
        self.h_down = copy(Metric::Down);
        self.h_up = copy(Metric::Up);

        let d = &self.det;
        if self.cores.len() != d.core_pct.len() {
            self.cores = vec![VecDeque::new(); d.core_pct.len()];
        }
        for (q, v) in self.cores.iter_mut().zip(&d.core_pct) {
            push(q, *v);
        }
        push(&mut self.kern, d.kernel.unwrap_or(0.0));
        let keys: Vec<&str> = d.engines.iter().map(|e| e.0.as_str()).collect();
        sync_series(&mut self.engines, &keys);
        for (s, e) in self.engines.iter_mut().zip(&d.engines) {
            s.push(e.1, 0.0);
        }
        let keys: Vec<&str> = d.disks.iter().map(|x| x.name.as_str()).collect();
        sync_series(&mut self.disks, &keys);
        for (s, x) in self.disks.iter_mut().zip(&d.disks) {
            s.push(x.read as f32, x.write as f32);
        }
        let keys: Vec<&str> = d.adapters.iter().map(|x| x.alias.as_str()).collect();
        sync_series(&mut self.nets, &keys);
        for (s, x) in self.nets.iter_mut().zip(&d.adapters) {
            s.push(x.down as f32, x.up as f32);
        }

        // 14 days of data usage, oldest first.
        let mut k = usage::today();
        let (yy, mm, _) = usage::ymd(k);
        self.month = a.m.usage.month(yy, mm).total();
        let mut days = Vec::with_capacity(14);
        for _ in 0..14 {
            let u = a.m.usage.get(k);
            days.push((u.recv, u.sent));
            k = usage::prev_day(k);
        }
        days.reverse();
        self.days = days;

        self.sort_top();
        self.has_data = true;
        shape != self.shape()
    }

    pub fn set_sort(&mut self, s: u8) {
        self.sort = s.min(3);
        self.sort_top();
    }

    fn sort_top(&mut self) {
        let key = |p: &ProcRow| -> f64 {
            match self.sort {
                1 => p.mem as f64,
                2 => p.io,
                3 => f64::from(p.gpu),
                _ => f64::from(p.cpu),
            }
        };
        let mut v: Vec<&ProcRow> = self.det.procs.iter().collect();
        v.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.mem.cmp(&a.mem)));
        self.top = v.into_iter().take(TOPN).cloned().collect();
    }

    /// What decides the layout besides the width.
    fn shape(&self) -> (usize, usize, usize, usize, usize, bool, bool) {
        (
            self.det.core_pct.len(),
            self.det.engines.len(),
            self.det.disks.len(),
            self.det.adapters.len(),
            self.det.volumes.len(),
            self.snap.gpu.is_some(),
            self.det.vram_total > 0,
        )
    }

    fn has_gpu(&self) -> bool {
        self.snap.gpu.is_some() || !self.det.engines.is_empty()
    }

    // ------------------------------------------------------------------ layout

    /// Lay the page out between `l` and `r` from `y0` (content coordinates); returns the bottom.
    pub fn layout(&mut self, dpi: u32, l: i32, r: i32, y0: i32) -> i32 {
        self.dpi = dpi;
        self.l = l;
        self.r = r;
        self.y0 = y0;
        self.vits.clear();
        self.boxes.clear();
        self.hits.clear();
        let gap = self.sc(12);
        let w = r - l;
        let mut y = y0;

        // Strip: chips on the left (drawn only), range control on the right.
        let sh = self.sc(28);
        let segw = self.sc(40);
        let mut x = r - segw * RANGES.len() as i32 - self.sc(4);
        for (_, secs) in RANGES {
            let seg = Rect { l: x, t: y + self.sc(2), r: x + segw, b: y + sh - self.sc(2) };
            self.hits.push((seg, Hit::Range(secs)));
            x += segw;
        }
        y += sh + gap;

        // Vitals.
        let mut vk = vec![Vit::Cpu, Vit::Mem];
        if self.has_gpu() {
            vk.push(Vit::Gpu);
        }
        if self.det.vram_total > 0 && self.snap.vram_used.is_some() {
            vk.push(Vit::Vram);
        }
        vk.push(Vit::Disk);
        vk.push(Vit::Net);
        let per = if w >= self.sc(1100) {
            vk.len()
        } else if w >= self.sc(560) {
            3
        } else {
            2
        };
        let th = self.sc(150);
        for (row, chunk) in vk.chunks(per).enumerate() {
            // A short last row stretches its tiles so no gap is left.
            let n = chunk.len() as i32;
            let tw = (w - gap * (n - 1)) / n;
            let t = y + row as i32 * (th + gap);
            for (i, v) in chunk.iter().enumerate() {
                let x = l + i as i32 * (tw + gap);
                let rr = if i as i32 == n - 1 { r } else { x + tw };
                self.vits.push((*v, Rect { l: x, t, r: rr, b: t + th }));
            }
        }
        y += vk.len().div_ceil(per) as i32 * (th + gap);

        // Sections.
        let cols: i32 = if w < self.sc(720) {
            1
        } else if w < self.sc(1240) {
            2
        } else if w < self.sc(1720) {
            3
        } else {
            4
        };
        let mut items: Vec<(Sec, i32)> = vec![(Sec::Cpu, cols.min(2)), (Sec::Mem, 1)];
        if self.has_gpu() {
            items.push((Sec::Gpu, 1));
        }
        let wide = if cols == 4 { 2 } else { 1 };
        items.push((Sec::Disk, wide));
        items.push((Sec::Net, wide));
        items.push((Sec::Procs, cols));
        // Pack into rows; the last card of a short row takes the rest of it.
        let mut rows: Vec<Vec<(Sec, i32)>> = Vec::new();
        let mut used = cols;
        for it in items {
            if used + it.1 > cols {
                rows.push(Vec::new());
                used = 0;
            }
            used += it.1;
            rows.last_mut().unwrap().push(it);
        }
        let colw = (w - gap * (cols - 1)) as f32 / cols as f32;
        for row in rows.iter_mut() {
            let sum: i32 = row.iter().map(|x| x.1).sum();
            if sum < cols {
                row.last_mut().unwrap().1 += cols - sum;
            }
            let mut c0 = 0;
            let mut rects = Vec::new();
            let mut h = 0;
            for (sec, span) in row.iter() {
                let x0 = l + (c0 as f32 * (colw + gap as f32)).round() as i32;
                let x1 = l + ((c0 + span) as f32 * (colw + gap as f32) - gap as f32).round() as i32;
                let x1 = if c0 + span == cols { r } else { x1 };
                h = h.max(self.need_h(*sec, x1 - x0));
                rects.push((*sec, x0, x1));
                c0 += span;
            }
            for (sec, x0, x1) in rects {
                let rc = Rect { l: x0, t: y, r: x1, b: y + h };
                self.boxes.push((sec, rc));
                self.section_hits(sec, rc);
            }
            y += h + gap;
        }
        y
    }

    /// Height a section needs at width `w`.
    fn need_h(&self, sec: Sec, w: i32) -> i32 {
        let s = |v: i32| self.sc(v);
        let head = s(14 + 20 + 12);
        let foot = s(16);
        match sec {
            Sec::Cpu => {
                let left = s(170 + 18 + 14 + 86);
                let right = s(170 + 18 + 14 + 48 * 2 + 8);
                if w >= s(640) {
                    head + left.max(right) + foot
                } else {
                    head + left + s(16) + right + foot
                }
            }
            Sec::Mem => head + s(36 + 26 + 44 + 14) + s(48 * 3 + 8 * 2) + s(12 + 64) + foot,
            Sec::Gpu => {
                let n = self.det.engines.len().max(1) as i32;
                head + n * s(26) + s(24) + s(2 * 26) + s(12 + 64) + foot
            }
            Sec::Disk => {
                let n = self.det.disks.len().max(1) as i32;
                let v = self.det.volumes.len() as i32;
                head + n * s(84) + if v > 0 { s(24) + v * s(24) } else { 0 } + foot
            }
            Sec::Net => {
                let on = self.det.adapters.iter().filter(|a| a.connected).count();
                let off = if on < self.det.adapters.len() { s(26) } else { 0 };
                head + on.max(1) as i32 * s(84) + off + s(24 + 70) + foot
            }
            Sec::Procs => head + s(28) + TOPN as i32 * s(30) + s(8 + 24) + foot,
        }
    }

    fn proc_cols(&self, r: Rect) -> Vec<(usize, i32, i32)> {
        // (column, left, right): 0 avatar, 1 name, 2 cpu, 3 mem, 4 disk, 5 gpu, 6 threads, 7 pid
        let s = |v: i32| self.sc(v);
        let (l, rr) = (r.l + s(16), r.r - s(16));
        let gap = s(12);
        let full = r.w() >= s(900);
        let fixed = s(22) + if full { 2 * s(58) } else { 0 };
        let flex: Vec<f32> = if full { vec![1.4, 1.0, 1.0, 1.0, 0.7] } else { vec![1.4, 1.0, 1.0, 1.0] };
        let ncol = 1 + flex.len() + if full { 2 } else { 0 };
        let room = (rr - l - fixed - gap * (ncol as i32 - 1)).max(0) as f32;
        let tot: f32 = flex.iter().sum();
        let mut out = Vec::new();
        let mut x = l;
        out.push((0, x, x + s(22)));
        x += s(22) + gap;
        for (i, f) in flex.iter().enumerate() {
            let cw = (room * f / tot).round() as i32;
            out.push((i + 1, x, x + cw));
            x += cw + gap;
        }
        if full {
            out.push((6, x, x + s(58)));
            x += s(58) + gap;
            out.push((7, x, rr));
        }
        out
    }

    fn section_hits(&mut self, sec: Sec, r: Rect) {
        if sec != Sec::Procs {
            return;
        }
        let dpi = self.dpi as i32;
        let s = move |v: i32| (v * dpi + 48) / 96;
        // Sort tabs at the top right.
        let tw = s(70);
        let mut x = r.r - s(16) - tw * 4 - s(4);
        let t = r.t + s(14);
        for i in 0..4u8 {
            self.hits.push((Rect { l: x, t, r: x + tw, b: t + s(22) }, Hit::Sort(i)));
            x += tw;
        }
        // Column headers sort too.
        let cols = self.proc_cols(r);
        let ht = r.t + s(46);
        for (c, l, rr) in &cols {
            if (2..=5).contains(c) {
                self.hits.push((Rect { l: *l, t: ht, r: *rr, b: ht + s(28) }, Hit::Sort((*c - 2) as u8)));
            }
        }
        let mut y = ht + s(28) + s(4);
        let pids: Vec<u32> = self.top.iter().map(|p| p.pid).collect();
        for pid in pids {
            self.hits.push((Rect { l: r.l + s(8), t: y, r: r.r - s(8), b: y + s(30) }, Hit::Proc(pid)));
            y += s(30);
        }
        // Footer links.
        let fy = r.b - s(16) - s(22);
        let w2 = s(130);
        self.hits.push((Rect { l: r.r - s(16) - w2, t: fy, r: r.r - s(16), b: fy + s(22) }, Hit::ResMon));
        self.hits.push((Rect { l: r.r - s(16) - 2 * w2 - s(8), t: fy, r: r.r - s(16) - w2 - s(8), b: fy + s(22) }, Hit::TaskMgr));
    }

    // ------------------------------------------------------------------ drawing

    /// Draw the page; `dy` maps content to client y, `vt..vb` is the visible band,
    /// `hot` indexes `hits`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, cv: &mut Canvas, f: &Fonts, col: &Colors, dy: i32, vt: i32, vb: i32, hot: Option<usize>) {
        let d = Draw { m: self, f, col: *col, dy, hot, track: col.line, sub: col.fg.with_a(if col.dark { 10 } else { 14 }) };
        let s = |v: i32| self.sc(v);
        let vis = |r: Rect| r.b + dy >= vt && r.t + dy <= vb;

        // Strip.
        let sy = self.y0 + dy;
        if sy + s(28) >= vt && sy <= vb {
            d.strip(cv, sy);
        }
        for (v, r) in &self.vits {
            if vis(*r) {
                d.vital(cv, *v, d.sh(*r));
            }
        }
        for (sec, r) in &self.boxes {
            if !vis(*r) {
                continue;
            }
            let r = d.sh(*r);
            d.card(cv, r);
            match sec {
                Sec::Cpu => d.cpu(cv, r),
                Sec::Mem => d.mem(cv, r),
                Sec::Gpu => d.gpu(cv, r),
                Sec::Disk => d.disks(cv, r),
                Sec::Net => d.nets(cv, r),
                Sec::Procs => d.procs(cv, r),
            }
        }
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Average (or peak) of `n` newest samples squeezed into `cols` points, oldest first.
/// Fewer samples than `n` give fewer points (the chart is right-aligned).
fn reduce(h: &[f32], n: usize, cols: usize, peak: bool) -> Vec<f32> {
    let have = h.len().min(n);
    if have == 0 || cols == 0 {
        return Vec::new();
    }
    let data = &h[h.len() - have..];
    let m = ((cols as f64 * have as f64 / n as f64).round() as usize).clamp(1, have);
    (0..m)
        .map(|k| {
            let lo = k * have / m;
            let hi = ((k + 1) * have / m).max(lo + 1);
            let w = &data[lo..hi];
            if peak {
                w.iter().copied().fold(0.0, f32::max)
            } else {
                w.iter().sum::<f32>() / w.len() as f32
            }
        })
        .collect()
}

struct Draw<'a> {
    m: &'a Monitor,
    f: &'a Fonts,
    col: Colors,
    dy: i32,
    hot: Option<usize>,
    track: Rgba,
    /// Raised sub-tile fill.
    sub: Rgba,
}

impl Draw<'_> {
    fn s(&self, v: i32) -> i32 {
        self.m.sc(v)
    }
    fn sf(&self, v: f32) -> f32 {
        self.m.scf(v)
    }
    fn sh(&self, r: Rect) -> Rect {
        Rect { t: r.t + self.dy, b: r.b + self.dy, ..r }
    }
    fn dark(&self) -> bool {
        self.col.dark
    }
    fn acc(&self, m: Metric) -> Rgba {
        accent(m, self.dark())
    }
    fn heat(&self, base: Rgba, p: f32) -> Rgba {
        heat(base, p, self.m.warn, self.m.crit)
    }
    fn heat_text(&self, p: f32) -> Rgba {
        heat_text(self.col.fg, p, self.m.warn, self.m.crit)
    }
    fn is_hot(&self, h: Hit) -> bool {
        self.hot.and_then(|i| self.m.hits.get(i)).is_some_and(|x| x.1 == h)
    }
    fn hit_rect(&self, h: Hit) -> Option<Rect> {
        self.m.hits.iter().find(|x| x.1 == h).map(|x| self.sh(x.0))
    }

    fn card(&self, cv: &mut Canvas, r: Rect) {
        let rad = self.sf(8.0);
        cv.fill_round(r, rad, self.col.card);
        cv.stroke_round(r, rad, self.col.card_border);
    }

    /// Small uppercase label.
    fn cap(&self, cv: &mut Canvas, s: &str, x: i32, y: i32) {
        cv.text(self.f.tiny, s, x, y, self.col.dim);
    }

    /// Section title + dim subtitle; returns the y below the header.
    fn header(&self, cv: &mut Canvas, r: Rect, title: &str, sub: &str, right_w: i32) -> i32 {
        let (x, y) = (r.l + self.s(16), r.t + self.s(14));
        let tw = cv.text(self.f.semi, title, x, y, self.col.fg);
        let tw = tw.max(cv.measure(self.f.semi, title).0);
        if !sub.is_empty() {
            let sx = x + tw + self.s(10);
            let max = r.r - self.s(16) - right_w - sx;
            if max > self.s(30) {
                let t = cv.ellipsize(self.f.small, sub, max);
                cv.text(self.f.small, &t, sx, y + self.s(2), self.col.dim);
            }
        }
        r.t + self.s(14 + 20 + 12)
    }

    /// Pill at the top right of a card: returns its width.
    fn pill(&self, cv: &mut Canvas, r: Rect, text: &str, c: Rgba, dot: bool) -> i32 {
        let (tw, th) = cv.measure(self.f.small, text);
        let h = self.s(20);
        let w = tw + self.s(if dot { 26 } else { 16 });
        let pr = Rect { l: r.r - self.s(16) - w, t: r.t + self.s(14), r: r.r - self.s(16), b: r.t + self.s(14) + h };
        cv.fill_round(pr, h as f32 / 2.0, c.with_a(if self.dark() { 40 } else { 32 }));
        let mut x = pr.l + self.s(8);
        if dot {
            let cy = (pr.t + pr.b) / 2;
            cv.fill_round(Rect { l: x, t: cy - self.s(3), r: x + self.s(6), b: cy + self.s(3) }, self.sf(3.0), c);
            x += self.s(10);
        }
        cv.text(self.f.small, text, x, pr.t + (h - th) / 2, c);
        w
    }

    /// Dashed horizontal guide lines at the given fractions of `r`.
    fn guides(&self, cv: &mut Canvas, r: Rect, fr: &[f32]) {
        let (dash, gap) = (self.s(2).max(1), self.s(4));
        let c = self.track;
        for f in fr {
            let y = r.t + (r.h() as f32 * f).round() as i32;
            let mut x = r.l;
            while x < r.r {
                cv.fill_rect(Rect { l: x, t: y, r: (x + dash).min(r.r), b: y + 1 }, c);
                x += dash + gap;
            }
        }
    }

    /// Area chart of `h` (scaled by `max`) right-aligned in `r`.
    fn area(&self, cv: &mut Canvas, r: Rect, h: &[f32], max: f32, c: Rgba, down: bool, peak: bool) {
        if r.is_empty() {
            return;
        }
        let n = self.m.n();
        let cols = (r.w() / self.s(2).max(1)).max(2) as usize;
        let v: Vec<f32> = reduce(h, n, cols, peak).into_iter().map(|x| (x / max.max(1e-6)).clamp(0.0, 1.0)).collect();
        if v.len() < 2 {
            return;
        }
        // Right-align partial history.
        let have = h.len().min(n);
        let w = (r.w() as f64 * have as f64 / n as f64).round() as i32;
        let rr = Rect { l: r.r - w.max(2), ..r };
        cv.spark_dir(rr, &v, c, down);
    }

    /// Two traces mirrored around a mid line (rates), common scale.
    fn mirror(&self, cv: &mut Canvas, r: Rect, a: &[f32], b: &[f32], ca: Rgba, cb: Rgba) {
        let n = self.m.n();
        let peak = |h: &[f32]| h[h.len().saturating_sub(n)..].iter().copied().fold(0.0f32, f32::max);
        let top = peak(a).max(peak(b)).max(64.0 * 1024.0);
        let mid = r.t + (r.h() as f32 * 0.56).round() as i32;
        cv.fill_rect(Rect { l: r.l, t: mid, r: r.r, b: mid + 1 }, self.track);
        self.area(cv, Rect { b: mid, ..r }, a, top, ca, false, true);
        self.area(cv, Rect { t: mid + 1, ..r }, b, top, cb, true, true);
    }

    fn ring(&self, cv: &mut Canvas, cx: f32, cy: f32, rad: f32, thick: f32, frac: f32, c: Rgba) {
        cv.ring(cx, cy, rad, thick, 1.0, self.track);
        cv.ring(cx, cy, rad, thick, frac, c);
    }

    fn text_c(&self, cv: &mut Canvas, font: windows_sys::Win32::Graphics::Gdi::HFONT, s: &str, cx: i32, cy: i32, c: Rgba) {
        let (_, h) = cv.measure(font, s);
        cv.text_center(font, s, cx, cy - h / 2, c);
    }

    /// Key/value tile.
    fn kv(&self, cv: &mut Canvas, r: Rect, k: &str, v: &str) {
        cv.fill_round(r, self.sf(6.0), self.sub);
        self.cap(cv, k, r.l + self.s(10), r.t + self.s(6));
        let v = cv.ellipsize(self.f.semi, v, r.w() - self.s(20));
        cv.text(self.f.semi, &v, r.l + self.s(10), r.t + self.s(22), self.col.fg);
    }

    /// Grid of key/value tiles in `cols` columns starting at (l, t) across width `w`.
    fn kv_grid(&self, cv: &mut Canvas, l: i32, t: i32, w: i32, cols: i32, items: &[(&str, String)]) {
        let g = self.s(8);
        let tw = (w - g * (cols - 1)) / cols;
        let th = self.s(48);
        for (i, (k, v)) in items.iter().enumerate() {
            let (cx, cy) = (i as i32 % cols, i as i32 / cols);
            let x = l + cx * (tw + g);
            let r = Rect { l: x, t: t + cy * (th + g), r: if cx == cols - 1 { l + w } else { x + tw }, b: t + cy * (th + g) + th };
            self.kv(cv, r, k, v);
        }
    }

    fn hbar(&self, cv: &mut Canvas, r: Rect, frac: f32, c: Rgba) {
        let rad = r.h() as f32 / 2.0;
        cv.fill_round(r, rad, self.track);
        let w = (r.w() as f32 * frac.clamp(0.0, 1.0)).round() as i32;
        if w > 0 {
            cv.fill_round(Rect { r: r.l + w.max(r.h()), ..r }, rad, c);
        }
    }

    fn legend(&self, cv: &mut Canvas, x: &mut i32, y: i32, c: Rgba, label: &str, val: &str) {
        let sq = self.s(8);
        let (_, th) = cv.measure(self.f.small, label);
        cv.fill_round(Rect { l: *x, t: y + (th - sq) / 2, r: *x + sq, b: y + (th - sq) / 2 + sq }, self.sf(2.0), c);
        *x += sq + self.s(5);
        *x += cv.text(self.f.small, label, *x, y, self.col.dim).max(cv.measure(self.f.small, label).0) + self.s(4);
        *x += cv.text(self.f.cap, val, *x, y, self.col.fg).max(cv.measure(self.f.cap, val).0) + self.s(14);
    }

    // ---- strip

    fn strip(&self, cv: &mut Canvas, y: i32) {
        let m = self.m;
        let h = self.s(24);
        let mut x = m.l;
        let d = &m.det;
        let chips = [
            (fmt::duration(m.snap.uptime_secs), "up"),
            (format!("{}", if d.processes > 0 { d.processes } else { m.snap.proc_count }), "processes"),
            (group(d.threads as u64), "threads"),
            (format!("{}k", (d.handles + 500) / 1000), "handles"),
        ];
        for (v, label) in chips.iter() {
            let (vw, th) = cv.measure(self.f.cap, v);
            let lw = cv.measure(self.f.small, label).0;
            let w = vw + lw + self.s(26);
            if x + w > m.r - self.s(140) {
                break;
            }
            let r = Rect { l: x, t: y + self.s(2), r: x + w, b: y + self.s(2) + h };
            cv.fill_round(r, h as f32 / 2.0, self.col.card);
            cv.stroke_round(r, h as f32 / 2.0, self.col.card_border);
            cv.text(self.f.cap, v, r.l + self.s(10), r.t + (h - th) / 2, self.col.fg);
            cv.text(self.f.small, label, r.l + self.s(16) + vw, r.t + (h - th) / 2 + self.s(1), self.col.dim);
            x += w + self.s(8);
        }
        // Range control.
        if let (Some(first), Some(last)) = (self.hit_rect(Hit::Range(RANGES[0].1)), self.hit_rect(Hit::Range(RANGES[2].1))) {
            let r = Rect { l: first.l - self.s(2), t: first.t - self.s(2), r: last.r + self.s(2), b: last.b + self.s(2) };
            cv.fill_round(r, r.h() as f32 / 2.0, self.col.card);
            cv.stroke_round(r, r.h() as f32 / 2.0, self.col.card_border);
            for (label, secs) in RANGES {
                let Some(seg) = self.hit_rect(Hit::Range(secs)) else { continue };
                let on = m.range == secs;
                if on {
                    cv.fill_round(seg, seg.h() as f32 / 2.0, self.col.fg.with_a(if self.dark() { 40 } else { 30 }));
                }
                let c = if on || self.is_hot(Hit::Range(secs)) { self.col.fg } else { self.col.dim };
                self.text_c(cv, self.f.small, label, (seg.l + seg.r) / 2, (seg.t + seg.b) / 2, c);
            }
        }
    }

    // ---- vitals

    fn vital(&self, cv: &mut Canvas, v: Vit, r: Rect) {
        self.card(cv, r);
        let m = self.m;
        let s = &m.snap;
        let d = &m.det;
        let cx = (r.l + r.r) / 2;
        let cy = r.t + self.s(48);
        let rad = self.sf(36.0);
        let thick = self.sf(7.0);
        let (label, sub, hist, metric, scale): (&str, String, &[f32], Metric, f32) = match v {
            Vit::Cpu => ("CPU", d.base_mhz.gt(&0.0).then(|| ghz(cur_mhz(s, d))).unwrap_or_default(), &m.h_cpu, Metric::Cpu, 100.0),
            Vit::Mem => ("MEMORY", format!("{} / {}", fmt::bytes(s.ram_used), fmt::bytes(s.ram_total)), &m.h_ram, Metric::Ram, 100.0),
            Vit::Gpu => ("GPU", s.gpu_temp.map_or(String::new(), |t| format!("{:.0} \u{00B0}C", t)), &m.h_gpu, Metric::Gpu, 100.0),
            Vit::Vram => (
                "VRAM",
                format!("{} / {}", fmt::bytes(s.vram_used.unwrap_or(0)), fmt::bytes(d.vram_total)),
                &m.h_vram,
                Metric::Vram,
                d.vram_total.max(1) as f32,
            ),
            Vit::Disk => ("DISK", format!("{} read", fmt::rate(s.disk_read)), &m.h_dact, Metric::DiskActive, 100.0),
            Vit::Net => ("NETWORK", format!("\u{2193} {}", fmt::rate(s.net_down)), &m.h_down, Metric::Down, 0.0),
        };
        let base = self.acc(metric);
        if v == Vit::Net {
            let (fx, fy) = (cx as f32, cy as f32);
            dial(cv, fx, fy, rad - thick / 2.0, thick, log_frac(s.net_down), base, self.track);
            dial(cv, fx, fy, rad - thick * 1.6, thick * 0.7, log_frac(s.net_up), self.acc(Metric::Up), self.track);
            let t = fmt::rate(s.net_down);
            let (n, u) = t.rsplit_once(' ').unwrap_or((&t, ""));
            self.text_c(cv, self.f.cap, n, cx, cy - self.s(5), self.col.fg);
            self.text_c(cv, self.f.tiny, u, cx, cy + self.s(9), self.col.dim);
        } else {
            let pct = match v {
                Vit::Cpu => s.cpu,
                Vit::Mem => s.ram_pct,
                Vit::Gpu => s.gpu.unwrap_or(0.0),
                Vit::Vram => (s.vram_used.unwrap_or(0) as f64 / d.vram_total.max(1) as f64 * 100.0) as f32,
                _ => s.disk_active.unwrap_or(0.0),
            };
            self.ring(cv, cx as f32, cy as f32, rad, thick, pct / 100.0, self.heat(base, pct));
            self.text_c(cv, self.f.semi, &format!("{:.0}%", pct), cx, cy, self.heat_text(pct));
        }
        self.text_c(cv, self.f.tiny, label, cx, r.t + self.s(96), self.col.dim);
        let sub = cv.ellipsize(self.f.small, &sub, r.w() - self.s(16));
        self.text_c(cv, self.f.small, &sub, cx, r.t + self.s(112), self.col.dim);
        let cr = Rect { l: r.l + self.s(12), t: r.b - self.s(30), r: r.r - self.s(12), b: r.b - self.s(8) };
        let max = if scale > 0.0 { scale } else { hist[hist.len().saturating_sub(m.n())..].iter().copied().fold(64.0 * 1024.0, f32::max) };
        self.area(cv, cr, hist, max, base, false, v == Vit::Net || v == Vit::Disk);
    }

    // ---- CPU

    fn cpu(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let s = &m.snap;
        let d = &m.det;
        let base = self.acc(Metric::Cpu);
        // Throttle pill.
        let limit = s.cpu_perf_limit.filter(|l| *l < 99.0).or(s.thermal_passive_limit.filter(|l| *l < 99.0));
        let pw = match limit {
            Some(l) => self.pill(cv, r, &format!("Limited to {:.0}%", l), self.col.warn, true),
            None => self.pill(cv, r, "Full power", self.col.good, true),
        };
        let y = self.header(cv, r, "CPU", &d.cpu_name, pw + self.s(8));
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let two = r.w() >= self.s(640);
        let (lw, rx, ry) = if two {
            let lw = ((rr - l - self.s(16)) as f32 * 0.6) as i32;
            (lw, l + lw + self.s(16), y)
        } else {
            (rr - l, l, y + self.s(170 + 18 + 14 + 86 + 16))
        };
        // Left: history (total + kernel), axis, speed dial + user/kernel/idle split.
        let ch = Rect { l, t: y, r: l + lw, b: y + self.s(170) };
        self.guides(cv, ch, &[0.25, 0.5, 0.75]);
        self.area(cv, ch, &m.h_cpu, 100.0, base, false, false);
        self.area(cv, ch, &m.kern.iter().copied().collect::<Vec<_>>(), 100.0, base.mix(self.col.fg, 0.55), false, false);
        let ay = ch.b + self.s(3);
        cv.text(self.f.tiny, range_label(m.range), l, ay, self.col.dim);
        cv.text_right(self.f.tiny, "now", ch.r, ay, self.col.dim);
        let sy = ch.b + self.s(18 + 14);
        let (dcx, dcy) = ((l + self.s(43)) as f32, (sy + self.s(43)) as f32);
        let mhz = cur_mhz(s, d);
        let maxmhz = (d.base_mhz * 1.35).max(1.0);
        dial(cv, dcx, dcy, self.sf(36.0), self.sf(7.0), mhz / maxmhz, base, self.track);
        if mhz > 0.0 {
            self.text_c(cv, self.f.semi, &format!("{:.2}", mhz / 1000.0), dcx as i32, dcy as i32 - self.s(4), self.col.fg);
            self.text_c(cv, self.f.tiny, "GHz", dcx as i32, dcy as i32 + self.s(12), self.col.dim);
        }
        let bx = l + self.s(86 + 12);
        let bw = l + lw - bx;
        if bw > self.s(60) {
            let kern = d.kernel.unwrap_or(0.0).min(s.cpu);
            let user = (s.cpu - kern).max(0.0);
            let br = Rect { l: bx, t: sy + self.s(30), r: bx + bw, b: sy + self.s(38) };
            cv.fill_round(br, self.sf(4.0), self.track);
            let uw = (bw as f32 * user / 100.0).round() as i32;
            let kw = (bw as f32 * kern / 100.0).round() as i32;
            if uw > 0 {
                cv.fill_rect(Rect { l: bx, r: bx + uw, ..br }, base);
            }
            if kw > 0 {
                cv.fill_rect(Rect { l: bx + uw, r: bx + uw + kw, ..br }, base.mix(self.col.fg, 0.55));
            }
            let mut x = bx;
            let ly = br.b + self.s(8);
            self.legend(cv, &mut x, ly, base, "User", &format!("{:.0}%", user));
            self.legend(cv, &mut x, ly, base.mix(self.col.fg, 0.55), "Kernel", &format!("{:.0}%", kern));
            if x + self.s(70) < bx + bw {
                self.legend(cv, &mut x, ly, self.track, "Idle", &format!("{:.0}%", 100.0 - s.cpu));
            }
        }
        // Right: per-core tiles + facts.
        let tr = Rect { l: rx, t: ry, r: rr, b: ry + self.s(170) };
        let hottest = self.cores(cv, tr);
        let ay = tr.b + self.s(3);
        cv.text(self.f.tiny, &format!("{} logical processors", d.core_pct.len().max(d.logical as usize)), rx, ay, self.col.dim);
        if let Some(h) = hottest {
            cv.text_right(self.f.tiny, &format!("busiest: core {}", h), rr, ay, self.col.dim);
        }
        let facts = [
            ("BASE SPEED", if d.base_mhz > 0.0 { ghz(d.base_mhz) } else { "\u{2013}".into() }),
            ("CORES / THREADS", format!("{} / {}", d.cores, d.logical)),
            ("L3 CACHE", if d.l3 > 0 { fmt::bytes(d.l3) } else { "\u{2013}".into() }),
            ("PERFORMANCE", s.cpu_perf_pct.map_or("\u{2013}".into(), |p| format!("{:.0}%", p))),
        ];
        self.kv_grid(cv, rx, tr.b + self.s(18 + 14), rr - rx, 2, &facts);
    }

    /// Per-core tiles with their own history; returns the busiest core.
    fn cores(&self, cv: &mut Canvas, r: Rect) -> Option<usize> {
        let m = self.m;
        let n = m.det.core_pct.len();
        if n == 0 {
            self.text_c(cv, self.f.small, "Collecting\u{2026}", (r.l + r.r) / 2, (r.t + r.b) / 2, self.col.dim);
            return None;
        }
        let g = self.s(6);
        let (w, h) = (r.w() as f32, r.h() as f32);
        // Columns that make tiles closest to square.
        let mut best = (f32::MAX, 1usize);
        for cols in 1..=n {
            let rows = n.div_ceil(cols);
            let tw = (w - g as f32 * (cols as f32 - 1.0)) / cols as f32;
            let th = (h - g as f32 * (rows as f32 - 1.0)) / rows as f32;
            if tw <= 0.0 || th <= 0.0 {
                continue;
            }
            // A short last row reads as a missing core: avoid it unless nothing else fits.
            let score = (tw / th).ln().abs() + if n % cols != 0 { 1.0 } else { 0.0 };
            if score < best.0 {
                best = (score, cols);
            }
        }
        let cols = best.1 as i32;
        let rows = n.div_ceil(best.1) as i32;
        let tw = (r.w() - g * (cols - 1)) / cols;
        let th = (r.h() - g * (rows - 1)) / rows;
        let base = self.acc(Metric::Cpu);
        let mut hot = 0;
        for (i, v) in m.det.core_pct.iter().enumerate() {
            if *v > m.det.core_pct[hot] {
                hot = i;
            }
            let (cx, cy) = (i as i32 % cols, i as i32 / cols);
            let t = Rect { l: r.l + cx * (tw + g), t: r.t + cy * (th + g), r: r.l + cx * (tw + g) + tw, b: r.t + cy * (th + g) + th };
            cv.fill_round(t, self.sf(5.0), self.sub);
            let c = self.heat(base, *v);
            if th >= self.s(26) {
                let hist: Vec<f32> = m.cores.get(i).map_or(Vec::new(), |q| q.iter().copied().collect());
                let cr = Rect { l: t.l + self.s(2), t: t.t + (th as f32 * 0.14) as i32, r: t.r - self.s(2), b: t.b - self.s(2) };
                let prev = cv.clip();
                cv.set_clip(Some(clip_and(prev, t)));
                self.area_n(cv, cr, &hist, 60.min(m.n()), 100.0, c);
                cv.set_clip(prev);
            } else {
                let fh = (th as f32 * v / 100.0).round() as i32;
                cv.fill_round(Rect { t: t.b - fh.max(1), ..t }, self.sf(3.0), c);
            }
            if tw >= self.s(36) && th >= self.s(22) {
                let p = format!("{:.0}%", v);
                cv.text_right(self.f.tiny, &p, t.r - self.s(6), t.t + self.s(4), self.col.fg);
                if tw >= self.s(64) {
                    cv.text(self.f.tiny, &i.to_string(), t.l + self.s(6), t.t + self.s(4), self.col.dim);
                }
            }
        }
        Some(hot)
    }

    /// Like `area` with an explicit sample count.
    fn area_n(&self, cv: &mut Canvas, r: Rect, h: &[f32], n: usize, max: f32, c: Rgba) {
        let cols = (r.w() / self.s(3).max(1)).max(2) as usize;
        let v: Vec<f32> = reduce(h, n, cols, false).into_iter().map(|x| (x / max).clamp(0.0, 1.0)).collect();
        if v.len() < 2 {
            return;
        }
        let have = h.len().min(n);
        let w = (r.w() as f64 * have as f64 / n as f64).round() as i32;
        cv.spark(Rect { l: r.r - w.max(2), ..r }, &v, c);
    }

    // ---- memory

    fn mem(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let s = &m.snap;
        let d = &m.det;
        let base = self.acc(Metric::Ram);
        let y = self.header(cv, r, "Memory", &fmt::bytes(s.ram_total), 0);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        // Big figure.
        let big = fmt::bytes(s.ram_used);
        let bw = cv.text(self.f.head, &big, l, y, self.col.fg).max(cv.measure(self.f.head, &big).0);
        let bh = cv.measure(self.f.head, &big).1;
        let rest = format!("/ {} in use \u{00B7} {:.0}%", fmt::bytes(s.ram_total), s.ram_pct);
        let sh = cv.measure(self.f.small, &rest).1;
        cv.text(self.f.small, &rest, l + bw + self.s(8), y + bh - sh - self.s(2), self.col.dim);
        // Composition bar: in use, modified, standby, free.
        let total = s.ram_total.max(1) as f64;
        let modified = d.modified.unwrap_or(0).min(s.ram_used);
        let in_use = s.ram_used.saturating_sub(modified);
        let standby = d.standby.unwrap_or(0);
        let free = d.free.unwrap_or_else(|| s.ram_total.saturating_sub(s.ram_used + standby));
        let parts = [
            (in_use, base, "In use"),
            (modified, base.mix(self.col.warn, 0.45), "Modified"),
            (standby, base.with_a(if self.dark() { 110 } else { 120 }), "Standby"),
            (free, self.track, "Free"),
        ];
        let br = Rect { l, t: y + self.s(36), r: rr, b: y + self.s(36 + 26) };
        cv.fill_round(br, self.sf(6.0), self.track);
        let mut x = br.l as f64;
        let prev = cv.clip();
        cv.set_clip(Some(clip_and(prev, br)));
        for (v, c, _) in parts.iter().take(3) {
            let w = br.w() as f64 * *v as f64 / total;
            let x1 = (x + w).round() as i32;
            if x1 > x.round() as i32 {
                cv.fill_round(Rect { l: x.round() as i32, r: (x1 - self.s(1)).max(x.round() as i32 + 1), ..br }, self.sf(2.0), *c);
            }
            x += w;
        }
        cv.set_clip(prev);
        let mut lx = l;
        let mut ly = br.b + self.s(8);
        for (v, c, label) in parts {
            let val = fmt::bytes(v);
            let need = self.s(8 + 5 + 4 + 14) + cv.measure(self.f.small, label).0 + cv.measure(self.f.cap, &val).0;
            if lx + need > rr && lx > l {
                lx = l;
                ly += self.s(18);
            }
            self.legend(cv, &mut lx, ly, c, label, &val);
        }
        // Commit dial + facts.
        let ky = y + self.s(36 + 26 + 44 + 14);
        let (dcx, dcy) = ((l + self.s(43)) as f32, (ky + self.s(50)) as f32);
        let cp = if d.commit_limit > 0 { (d.commit as f64 / d.commit_limit as f64 * 100.0) as f32 } else { 0.0 };
        dial(cv, dcx, dcy, self.sf(36.0), self.sf(7.0), cp / 100.0, self.heat(base, cp), self.track);
        self.text_c(cv, self.f.semi, &format!("{:.0}%", cp), dcx as i32, dcy as i32 - self.s(4), self.heat_text(cp));
        self.text_c(cv, self.f.tiny, "commit", dcx as i32, dcy as i32 + self.s(12), self.col.dim);
        let facts = [
            ("COMMITTED", format!("{} / {}", fmt::bytes(d.commit), fmt::bytes(d.commit_limit))),
            ("CACHED", fmt::bytes(d.cached.max(standby + modified))),
            ("AVAILABLE", fmt::bytes(s.ram_total.saturating_sub(s.ram_used))),
            ("PAGED POOL", fmt::bytes(d.paged)),
            ("NON-PAGED", fmt::bytes(d.nonpaged)),
            ("HARD FAULTS", d.hard_faults.map_or("\u{2013}".into(), |v| format!("{:.0} /s", v))),
        ];
        let kx = l + self.s(86 + 12);
        self.kv_grid(cv, kx, ky, rr - kx, 2, &facts);
        // History fills the rest.
        let ct = ky + self.s(48 * 3 + 16 + 12);
        let cr = Rect { l, t: ct, r: rr, b: r.b - self.s(16) };
        self.guides(cv, cr, &[0.5]);
        self.area(cv, cr, &m.h_ram, 100.0, base, false, false);
    }

    // ---- GPU

    fn gpu(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let s = &m.snap;
        let d = &m.det;
        let base = self.acc(Metric::Gpu);
        let pw = match s.gpu_temp {
            Some(t) => {
                let c = m.snap.gpu_temp.map_or(self.col.dim, |t| self.heat(self.col.dim, ((t - 40.0) / 55.0 * 100.0).clamp(0.0, 100.0)));
                self.pill(cv, r, &format!("{:.0} \u{00B0}C", t), c, false)
            }
            None => 0,
        };
        let name = if d.gpu_name.is_empty() { "Graphics" } else { d.gpu_name.as_str() };
        let mut y = self.header(cv, r, "GPU", name, pw + self.s(8));
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let lab_w = self.s(92);
        let pct_w = self.s(40);
        for (i, (ty, v)) in d.engines.iter().enumerate() {
            let label = engine_label(ty);
            let cy = y + self.s(13);
            text_mid(cv, self.f.small, &label, l, cy, self.col.fg);
            let cr = Rect { l: l + lab_w, t: y + self.s(3), r: rr - pct_w - self.s(8), b: y + self.s(23) };
            if let Some(sr) = m.engines.get(i) {
                let h: Vec<f32> = sr.a.iter().copied().collect();
                self.area(cv, cr, &h, 100.0, base, false, false);
            }
            let p = format!("{:.0}%", v);
            let (pw2, ph) = cv.measure(self.f.cap, &p);
            cv.text(self.f.cap, &p, rr - pw2, cy - ph / 2, self.col.fg);
            y += self.s(26);
        }
        if d.engines.is_empty() {
            self.text_c(cv, self.f.small, "No engine data", (l + rr) / 2, y + self.s(13), self.col.dim);
            y += self.s(26);
        }
        y += self.s(12);
        cv.fill_rect(Rect { l, t: y, r: rr, b: y + 1 }, self.col.card_border);
        y += self.s(12);
        let used = s.vram_used.unwrap_or(0);
        let rows = [
            ("Dedicated", used, d.vram_total, base),
            ("Shared", d.vram_shared.unwrap_or(0), d.shared_total, base.mix(self.col.dim, 0.5)),
        ];
        for (label, v, tot, c) in rows {
            let cy = y + self.s(13);
            text_mid(cv, self.f.small, label, l, cy, self.col.fg);
            let t = if tot > 0 { format!("{} / {}", fmt::bytes(v), fmt::bytes(tot)) } else { fmt::bytes(v) };
            let tw = cv.measure(self.f.small, &t).0;
            text_mid(cv, self.f.small, &t, rr - tw, cy, self.col.dim);
            let br = Rect { l: l + lab_w, t: cy - self.s(4), r: rr - tw - self.s(12), b: cy + self.s(4) };
            if br.w() > self.s(20) {
                self.hbar(cv, br, if tot > 0 { v as f32 / tot as f32 } else { 0.0 }, c);
            }
            y += self.s(26);
        }
        let cr = Rect { l, t: y + self.s(12), r: rr, b: r.b - self.s(16) };
        self.guides(cv, cr, &[0.5]);
        self.area(cv, cr, &m.h_gpu, 100.0, base, false, false);
    }

    // ---- disks

    fn disks(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let d = &m.det;
        let sub = format!("{} physical \u{00B7} {} volumes", d.disks.len(), d.volumes.len());
        let mut y = self.header(cv, r, "Disks", &sub, 0);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let (rd, wr) = (self.acc(Metric::DiskRead), self.acc(Metric::DiskWrite));
        let wr = if wr == rd { rd.mix(self.col.fg, 0.5) } else { wr };
        for (i, x) in d.disks.iter().enumerate() {
            if i > 0 {
                cv.fill_rect(Rect { l, t: y - self.s(6), r: rr, b: y - self.s(5) }, self.col.card_border);
            }
            let h = m.disks.get(i);
            let a: Vec<f32> = h.map_or(Vec::new(), |s| s.a.iter().copied().collect());
            let b: Vec<f32> = h.map_or(Vec::new(), |s| s.b.iter().copied().collect());
            let sub = format!("{}{}{:.1} ms \u{00B7} queue {:.0}", x.letters, if x.letters.is_empty() { "" } else { " \u{00B7} " }, x.latency_ms, x.queue);
            self.device_row(cv, l, rr, y, x.active / 100.0, self.heat(rd, x.active), &format!("{:.0}%", x.active), "ACTIVE", &x.name, [("R", x.read, rd), ("W", x.write, wr)], &sub, &a, &b, false);
            y += self.s(84);
        }
        if d.disks.is_empty() {
            self.text_c(cv, self.f.small, "Collecting\u{2026}", (l + rr) / 2, y + self.s(36), self.col.dim);
            y += self.s(84);
        }
        if d.volumes.is_empty() {
            return;
        }
        y += self.s(6);
        cv.fill_rect(Rect { l, t: y, r: rr, b: y + 1 }, self.col.card_border);
        y += self.s(18);
        let base = self.acc(Metric::DiskFree);
        for v in &d.volumes {
            let cy = y + self.s(12);
            text_mid(cv, self.f.cap, &v.letter, l, cy, self.col.fg);
            let t = format!("{} free of {}", fmt::bytes(v.free), fmt::bytes(v.total));
            let tw = cv.measure(self.f.small, &t).0;
            text_mid(cv, self.f.small, &t, rr - tw, cy, self.col.dim);
            let lab = if v.label.is_empty() { String::new() } else { cv.ellipsize(self.f.small, &v.label, self.s(90)) };
            let lw = if lab.is_empty() { 0 } else { self.s(96) };
            if lw > 0 {
                text_mid(cv, self.f.small, &lab, l + self.s(30), cy, self.col.dim);
            }
            let (bl, br) = (l + self.s(30) + lw, rr - tw - self.s(12));
            let used = 1.0 - v.free as f32 / v.total.max(1) as f32;
            self.cells(cv, bl, br, cy, used, self.heat(base, used * 100.0));
            y += self.s(24);
        }
    }

    /// Row of drive-space cells, `frac` filled.
    fn cells(&self, cv: &mut Canvas, l: i32, r: i32, cy: i32, frac: f32, c: Rgba) {
        let g = self.s(2);
        let n = ((r - l) / self.s(12)).clamp(8, 40);
        let cw = (r - l - g * (n - 1)) as f32 / n as f32;
        if cw < 1.0 {
            return;
        }
        let filled = (frac.clamp(0.0, 1.0) * n as f32).round() as i32;
        let (t, b) = (cy - self.s(5), cy + self.s(5));
        for i in 0..n {
            let x0 = l + (i as f32 * (cw + g as f32)).round() as i32;
            let x1 = l + (i as f32 * (cw + g as f32) + cw).round() as i32;
            cv.fill_round(Rect { l: x0, t, r: x1, b }, self.sf(2.0), if i < filled { c } else { self.track });
        }
    }

    /// Dial + two values + mirror chart (one disk or one adapter).
    #[allow(clippy::too_many_arguments)]
    fn device_row(
        &self,
        cv: &mut Canvas,
        l: i32,
        rr: i32,
        y: i32,
        frac: f32,
        c: Rgba,
        center: &str,
        cap: &str,
        name: &str,
        vals: [(&str, f64, Rgba); 2],
        sub: &str,
        a: &[f32],
        b: &[f32],
        net: bool,
    ) {
        let (cx, cy) = ((l + self.s(36)) as f32, (y + self.s(36)) as f32);
        if net {
            dial(cv, cx, cy, self.sf(30.0), self.sf(6.0), log_frac(vals[0].1), vals[0].2, self.track);
            dial(cv, cx, cy, self.sf(22.5), self.sf(4.5), log_frac(vals[1].1), vals[1].2, self.track);
        } else {
            dial(cv, cx, cy, self.sf(30.0), self.sf(6.0), frac, c, self.track);
        }
        if net {
            // Down and up marks in their ring colours.
            let ty = cy as i32 - self.s(1);
            self.text_c(cv, self.f.tiny, vals[0].0, cx as i32 - self.s(5), ty, vals[0].2);
            self.text_c(cv, self.f.tiny, vals[1].0, cx as i32 + self.s(5), ty, vals[1].2);
        } else {
            self.text_c(cv, self.f.cap, center, cx as i32, cy as i32 - self.s(2), self.col.fg);
        }
        self.text_c(cv, self.f.tiny, cap, cx as i32, cy as i32 + self.s(26), self.col.dim);
        let vx = l + self.s(84);
        let vw = self.s(132);
        let nm = cv.ellipsize(self.f.cap, name, vw);
        cv.text(self.f.cap, &nm, vx, y, self.col.fg);
        for (i, (mark, v, col)) in vals.iter().enumerate() {
            let ty = y + self.s(18 + i as i32 * 18);
            cv.text(self.f.tiny, mark, vx, ty + self.s(2), *col);
            let t = fmt::rate(*v);
            let (n, u) = t.rsplit_once(' ').unwrap_or((&t, ""));
            let nw = cv.text(if i == 0 { self.f.semi } else { self.f.cap }, n, vx + self.s(14), ty, self.col.fg).max(cv.measure(self.f.cap, n).0);
            cv.text(self.f.tiny, u, vx + self.s(18) + nw, ty + self.s(3), self.col.dim);
        }
        let st = cv.ellipsize(self.f.tiny, sub, vw);
        cv.text(self.f.tiny, &st, vx, y + self.s(56), self.col.dim);
        let cr = Rect { l: vx + vw + self.s(12), t: y + self.s(4), r: rr, b: y + self.s(68) };
        if cr.w() > self.s(40) {
            self.mirror(cv, cr, a, b, vals[0].2, vals[1].2);
        }
    }

    // ---- network

    fn nets(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let d = &m.det;
        let first = d.adapters.iter().find(|a| a.connected);
        let sub = first.map_or(String::new(), |a| if a.ipv4.is_empty() { a.alias.clone() } else { format!("{} \u{00B7} {}", a.alias, a.ipv4) });
        let mut y = self.header(cv, r, "Network", &sub, 0);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let (dn, up) = (self.acc(Metric::Down), self.acc(Metric::Up));
        // Connected adapters come first; the rest collapse to one line.
        for (i, x) in d.adapters.iter().enumerate().filter(|(_, x)| x.connected) {
            if i > 0 {
                cv.fill_rect(Rect { l, t: y - self.s(6), r: rr, b: y - self.s(5) }, self.col.card_border);
            }
            let h = m.nets.get(i);
            let a: Vec<f32> = h.map_or(Vec::new(), |s| s.a.iter().copied().collect());
            let b: Vec<f32> = h.map_or(Vec::new(), |s| s.b.iter().copied().collect());
            let mut sub = Vec::new();
            if !x.description.is_empty() {
                sub.push(x.description.clone());
            }
            if x.link_bps > 0 {
                sub.push(link(x.link_bps));
            }
            if !x.ipv4.is_empty() {
                sub.push(x.ipv4.clone());
            }
            let cap = if x.wifi { "WI-FI" } else { "" };
            self.device_row(cv, l, rr, y, 0.0, dn, "", cap, &x.alias, [("\u{25BC}", x.down, dn), ("\u{25B2}", x.up, up)], &sub.join(" \u{00B7} "), &a, &b, true);
            y += self.s(84);
        }
        let off: Vec<&str> = d.adapters.iter().filter(|a| !a.connected).map(|a| a.alias.as_str()).collect();
        if first.is_none() {
            let msg = if d.adapters.is_empty() { "Collecting\u{2026}" } else { "No network connected" };
            self.text_c(cv, self.f.small, msg, (l + rr) / 2, y + self.s(36), self.col.dim);
            y += self.s(84);
        }
        if !off.is_empty() {
            let dot = self.s(4);
            cv.fill_round(Rect { l, t: y + self.s(5), r: l + dot * 2, b: y + self.s(5) + dot * 2 }, dot as f32, self.col.off);
            let line = format!("{} disconnected \u{00B7} {}", off.len(), off.join(", "));
            let t = cv.ellipsize(self.f.small, &line, rr - l - self.s(16));
            cv.text(self.f.small, &t, l + self.s(16), y, self.col.dim);
            y += self.s(26);
        }
        y += self.s(6);
        cv.fill_rect(Rect { l, t: y, r: rr, b: y + 1 }, self.col.card_border);
        y += self.s(18);
        // Data usage: today + month, 14-day bars.
        let (today_r, today_s) = m.days.last().copied().unwrap_or((0, 0));
        self.cap(cv, "TODAY", l, y);
        cv.text(self.f.head, &fmt::bytes(today_r + today_s), l, y + self.s(14), self.col.fg);
        self.cap(cv, "MONTH", l + self.s(120), y);
        cv.text(self.f.head, &fmt::bytes(m.month), l + self.s(120), y + self.s(14), self.col.fg);
        let br = Rect { l: l + self.s(230), t: y, r: rr, b: y + self.s(58) };
        if br.w() > self.s(80) && !m.days.is_empty() {
            let n = m.days.len() as i32;
            let g = self.s(4);
            let bw = (br.w() - g * (n - 1)) as f32 / n as f32;
            let max = m.days.iter().map(|d| d.0 + d.1).max().unwrap_or(1).max(1) as f32;
            for (i, (rcv, snt)) in m.days.iter().enumerate() {
                let x0 = br.l + (i as f32 * (bw + g as f32)).round() as i32;
                let x1 = br.l + (i as f32 * (bw + g as f32) + bw).round() as i32;
                let tot = ((rcv + snt) as f32 / max * br.h() as f32).round() as i32;
                let so = (*snt as f32 / max * br.h() as f32).round() as i32;
                let a = if i as i32 == n - 1 { 255 } else { 160 };
                if tot > 0 {
                    cv.fill_round(Rect { l: x0, t: br.b - tot.max(2), r: x1, b: br.b }, self.sf(2.5), dn.with_a(a));
                }
                if so > 0 {
                    cv.fill_rect(Rect { l: x0, t: br.b - so, r: x1, b: br.b }, up.with_a(a));
                }
            }
        }
    }

    // ---- processes

    fn procs(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let d = &m.det;
        let count = if d.processes > 0 { d.processes as usize } else { d.procs.len() };
        // Sort tabs.
        let mut tabs_l = r.r;
        for i in 0..4u8 {
            if let Some(t) = self.hit_rect(Hit::Sort(i)).filter(|_| true) {
                if t.t < r.t + self.s(40) {
                    tabs_l = tabs_l.min(t.l);
                }
            }
        }
        let y = self.header(cv, r, "Processes", &format!("Top {} of {}", m.top.len(), count), r.r - tabs_l);
        let tab_cols = [Metric::Cpu, Metric::Ram, Metric::DiskRead, Metric::Gpu];
        let tabs: Vec<Rect> = m.hits.iter().filter(|h| matches!(h.1, Hit::Sort(_)) && h.0.t < m.boxes.iter().find(|b| b.0 == Sec::Procs).map_or(0, |b| b.1.t) + self.s(40)).map(|h| self.sh(h.0)).collect();
        if let (Some(f), Some(l)) = (tabs.first(), tabs.last()) {
            let tr = Rect { l: f.l - self.s(2), t: f.t - self.s(2), r: l.r + self.s(2), b: l.b + self.s(2) };
            cv.fill_round(tr, tr.h() as f32 / 2.0, self.sub);
            for (i, t) in tabs.iter().enumerate() {
                let on = m.sort == i as u8;
                if on {
                    cv.fill_round(*t, t.h() as f32 / 2.0, self.col.fg.with_a(if self.dark() { 40 } else { 30 }));
                }
                let label = SORTS[i];
                let (lw, _) = cv.measure(self.f.small, label);
                let dot = self.s(6);
                let x = (t.l + t.r - lw - dot - self.s(5)) / 2;
                let cy = (t.t + t.b) / 2;
                cv.fill_round(Rect { l: x, t: cy - dot / 2, r: x + dot, b: cy - dot / 2 + dot }, dot as f32 / 2.0, self.acc(tab_cols[i]));
                let c = if on || self.is_hot(Hit::Sort(i as u8)) { self.col.fg } else { self.col.dim };
                text_mid(cv, self.f.small, label, x + dot + self.s(5), cy, c);
            }
        }
        let cols = m.proc_cols(r);
        let ht = y;
        let names = ["", "NAME", "CPU", "MEMORY", "DISK", "GPU", "THREADS", "PID"];
        for (c, l, rr) in &cols {
            let on = (2..=5).contains(c) && m.sort as usize == c - 2;
            let col = if on { self.col.fg } else { self.col.dim };
            if *c >= 6 {
                cv.text_right(self.f.tiny, names[*c], *rr, ht + self.s(8), col);
            } else {
                cv.text(self.f.tiny, names[*c], *l, ht + self.s(8), col);
            }
        }
        cv.fill_rect(Rect { l: r.l + self.s(16), t: ht + self.s(27), r: r.r - self.s(16), b: ht + self.s(28) }, self.col.card_border);
        let mx = |f: &dyn Fn(&ProcRow) -> f64, floor: f64| m.top.iter().map(f).fold(floor, f64::max);
        let max_cpu = mx(&|p| f64::from(p.cpu), 10.0);
        let max_mem = mx(&|p| p.mem as f64, 1.0);
        let max_io = mx(&|p| p.io, 64.0 * 1024.0);
        let max_gpu = mx(&|p| f64::from(p.gpu), 10.0);
        let mut y = ht + self.s(32);
        for p in &m.top {
            let row = Rect { l: r.l + self.s(8), t: y, r: r.r - self.s(8), b: y + self.s(30) };
            if self.is_hot(Hit::Proc(p.pid)) {
                cv.fill_round(row, self.sf(5.0), self.sub);
            }
            let cy = y + self.s(15);
            for (c, l, rr) in &cols {
                match c {
                    0 => {
                        let a = Rect { l: *l, t: cy - self.s(11), r: *l + self.s(22), b: cy + self.s(11) };
                        cv.fill_round(a, self.sf(6.0), avatar_col(&p.name));
                        let ch: String = p.name.chars().next().map_or(String::from("?"), |c| c.to_uppercase().collect());
                        self.text_c(cv, self.f.cap, &ch, (a.l + a.r) / 2, cy, Rgba::rgb(17, 17, 17));
                    }
                    1 => {
                        let n = cv.ellipsize(self.f.small, &p.name, rr - l);
                        text_mid(cv, self.f.small, &n, *l, cy, self.col.fg);
                    }
                    2..=5 => {
                        let (f, t, c2) = match c {
                            2 => ((f64::from(p.cpu) / max_cpu) as f32, format!("{:.1}%", p.cpu), self.acc(Metric::Cpu)),
                            3 => ((p.mem as f64 / max_mem) as f32, fmt::bytes(p.mem), self.acc(Metric::Ram)),
                            4 => ((p.io / max_io) as f32, if p.io > 0.0 { fmt::rate(p.io) } else { "0".into() }, self.acc(Metric::DiskRead)),
                            _ => ((f64::from(p.gpu) / max_gpu) as f32, format!("{:.0}%", p.gpu), self.acc(Metric::Gpu)),
                        };
                        let tw = self.s(64).min((rr - l) / 2);
                        cv.text_right(self.f.small, &t, *rr, cy - cv.measure(self.f.small, &t).1 / 2, self.col.dim);
                        let br = Rect { l: *l, t: cy - self.s(3), r: rr - tw - self.s(6), b: cy + self.s(3) };
                        if br.w() > self.s(12) {
                            self.hbar(cv, br, f, c2);
                        }
                    }
                    6 => {
                        let t = p.threads.to_string();
                        cv.text_right(self.f.small, &t, *rr, cy - cv.measure(self.f.small, &t).1 / 2, self.col.dim);
                    }
                    _ => {
                        let t = p.pid.to_string();
                        cv.text_right(self.f.small, &t, *rr, cy - cv.measure(self.f.small, &t).1 / 2, self.col.dim);
                    }
                }
            }
            y += self.s(30);
        }
        if m.top.is_empty() {
            self.text_c(cv, self.f.small, "Collecting\u{2026}", (r.l + r.r) / 2, y + self.s(40), self.col.dim);
        }
        for (h, label) in [(Hit::TaskMgr, "Task Manager"), (Hit::ResMon, "Resource Monitor")] {
            if let Some(t) = self.hit_rect(h) {
                let c = if self.is_hot(h) { self.col.accent.mix(self.col.fg, 0.25) } else { self.col.accent };
                let tw = cv.measure(self.f.small, label).0;
                text_mid(cv, self.f.small, label, t.r - tw, (t.t + t.b) / 2, c);
            }
        }
    }
}

fn text_mid(cv: &mut Canvas, font: windows_sys::Win32::Graphics::Gdi::HFONT, s: &str, x: i32, cy: i32, c: Rgba) {
    let (_, h) = cv.measure(font, s);
    cv.text(font, s, x, cy - h / 2, c);
}

fn clip_and(prev: Option<Rect>, r: Rect) -> Rect {
    match prev {
        Some(p) => Rect { l: p.l.max(r.l), t: p.t.max(r.t), r: p.r.min(r.r), b: p.b.min(r.b) },
        None => r,
    }
}

/// Current clock: nominal × `% Processor Performance`.
fn cur_mhz(s: &Snapshot, d: &Detail) -> f32 {
    match s.cpu_perf_pct {
        Some(p) if d.base_mhz > 0.0 => d.base_mhz * p / 100.0,
        _ => d.base_mhz,
    }
}

fn range_label(secs: usize) -> &'static str {
    match secs {
        s if s >= 3600 => "1 h",
        s if s >= 300 => "5 min",
        _ => "60 s",
    }
}

/// "VideoDecode" -> "Video Decode", "Compute_0" -> "Compute 0".
fn engine_label(t: &str) -> String {
    let mut out = String::new();
    for (i, c) in t.char_indices() {
        if c == '_' {
            out.push(' ');
            continue;
        }
        if i > 0 && c.is_ascii_uppercase() && t[..i].ends_with(|p: char| p.is_ascii_lowercase()) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// "1 Gbps" style link speed.
fn link(bps: u64) -> String {
    let b = bps as f64;
    if b >= 1e9 {
        let g = b / 1e9;
        if (g - g.round()).abs() < 0.05 { format!("{:.0} Gbps", g) } else { format!("{:.1} Gbps", g) }
    } else if b >= 1e6 {
        format!("{:.0} Mbps", b / 1e6)
    } else {
        format!("{:.0} Kbps", b / 1e3)
    }
}

/// 5212 -> "5,212".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_labels() {
        assert_eq!(engine_label("3D"), "3D");
        assert_eq!(engine_label("VideoDecode"), "Video Decode");
        assert_eq!(engine_label("Compute_0"), "Compute 0");
    }

    #[test]
    fn formats() {
        assert_eq!(group(5212), "5,212");
        assert_eq!(group(999), "999");
        assert_eq!(link(1_000_000_000), "1 Gbps");
        assert_eq!(link(2_500_000_000), "2.5 Gbps");
        assert_eq!(link(866_700_000), "867 Mbps");
    }

    #[test]
    fn reduce_right_aligns_partial_history() {
        let h: Vec<f32> = (0..30).map(|i| i as f32).collect();
        // 30 samples of a 60-sample range into 60 columns: 30 points.
        assert_eq!(reduce(&h, 60, 60, false).len(), 30);
        let p = reduce(&h, 30, 3, true);
        assert_eq!(p, vec![9.0, 19.0, 29.0]);
    }

    fn mon(w: i32) -> (Monitor, i32) {
        let mut m = Monitor::new();
        m.det.core_pct = vec![1.0; 16];
        m.det.disks = vec![Default::default(); 2];
        m.det.adapters = vec![Default::default(); 2];
        m.det.engines = vec![("3D".into(), 1.0)];
        m.snap.gpu = Some(1.0);
        let b = m.layout(96, 0, w, 0);
        (m, b)
    }

    #[test]
    fn layout_fills_every_width_without_gaps() {
        for w in [400, 700, 1000, 1500, 2400, 3400] {
            let (m, _) = mon(w);
            // Every grid row ends at the right edge.
            let mut rows: Vec<i32> = m.boxes.iter().map(|b| b.1.t).collect();
            rows.dedup();
            for t in rows {
                let right = m.boxes.iter().filter(|b| b.1.t == t).map(|b| b.1.r).max().unwrap();
                assert_eq!(right, w, "row at {} ends at {} (width {})", t, right, w);
            }
            for (_, r) in &m.vits {
                assert!(r.r <= w && r.l >= 0);
            }
        }
    }

    #[test]
    fn columns_grow_with_width() {
        let count = |w| {
            let (m, _) = mon(w);
            let t = m.boxes.iter().find(|b| b.0 == Sec::Disk).unwrap().1.t;
            m.boxes.iter().filter(|b| b.1.t == t).count()
        };
        assert_eq!(count(600), 1);
        assert!(count(1500) >= 2);
    }
}
