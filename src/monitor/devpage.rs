//! Device detail pages: click a tile on the Monitor page (CPU, memory, GPU, a disk, a
//! display, network, battery) and the page turns into everything about that one device.
//!
//! Every page has the same shape, because that is what people asked for: a row of device
//! tabs, a one-line verdict in plain words, a big chart with a real scale and min / average
//! / max, where hovering shows the app that was busiest at that second (click it for the
//! app's menu), then cards of facts that reflow into 1 to 4 columns.
//!
//! A child module of `monitor`, so it draws with the same `Draw` helpers and palette.
//! Extra history is kept only for the open device; the sensors read the device's deep
//! facts only while its page is on screen.

use super::*;
use crate::insights::Tone;
use crate::sensors::deep::{battery, display, net, space, AppNet, DeepLive, DeepStatic, Dev};

/// Samples of per-second app history kept for chart hover (one hour).
const TOPS: usize = HIST;
/// Chart height, unscaled.
const CHART_H: i32 = 250;
/// Folder rows in the space card.
const SPACE_ROWS: usize = 8;
/// Rows in an apps card.
const APP_ROWS: usize = 6;

/// The busiest app of one resource at one second: (pid, title, value).
type Top = (u32, String, f64);

#[derive(Clone, Default)]
pub(super) struct Tops {
    cpu: Option<Top>,
    mem: Option<Top>,
    io: Option<Top>,
    gpu: Option<Top>,
}

/// A card on a device page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Card {
    CoreMap,
    Chip,
    Power,
    /// Top apps by a sort key (0 CPU, 1 memory, 2 disk, 3 GPU).
    Apps(u8),
    Clock,
    MemMap,
    Commit,
    Sticks,
    VramApps,
    Engines,
    GpuHealth,
    Adapters,
    Health,
    DriveFacts,
    Volumes,
    /// What fills the drive: biggest folders, on demand.
    Space,
    Signal,
    Link,
    Hdr,
    Panel,
    Path,
    Outages,
    AdapterFacts,
    Wifi,
    Conns,
    /// Bytes per app (ETW; admin only, else an unlock button).
    AppTraffic,
    Traffic,
    Capacity,
    BatLive,
    /// Admin-only details are hidden: explain and offer to unlock.
    Admin,
}

#[derive(Default)]
pub(super) struct DevPage {
    /// Pointer over the page, content coordinates.
    pub hover: Option<(i32, i32)>,
    /// Extra history of the open device, newest last.
    hist: HashMap<String, VecDeque<f32>>,
    tops: VecDeque<Tops>,
    tabs: Vec<(Dev, Rect)>,
    /// Bluetooth page pieces.
    pub(super) bt: btpage::BtLayout,
    verdict: Rect,
    chart: Rect,
    cards: Vec<(Card, Rect)>,
    /// "What fills this drive" scan of the open disk (runs on its own thread).
    scan: Option<space::Scan>,
    scan_root: String,
    scan_tree: Option<std::sync::Arc<space::Tree>>,
    scan_dir: space::NodeId,
    scan_rows: Vec<space::Entry>,
}

impl DevPage {
    pub fn clear(&mut self) {
        self.hist.clear();
        self.tops.clear();
        self.hover = None;
        self.scan = None;
        self.scan_tree = None;
        self.scan_rows.clear();
        self.scan_dir = 0;
    }

    fn push(&mut self, key: &str, v: f32) {
        let q = self.hist.entry(key.to_string()).or_default();
        push(q, v);
    }

    fn get(&self, key: &str) -> Vec<f32> {
        self.hist.get(key).map_or(Vec::new(), |q| q.iter().copied().collect())
    }
}

/// Unit of a chart's value axis.
#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Pct,
    Rate,
    Ms,
    Hz,
    Watts,
}

fn axis_text(a: Axis, v: f32) -> String {
    match a {
        Axis::Pct => format!("{:.0}%", v),
        Axis::Rate => fmt::rate(v as f64),
        Axis::Ms => format!("{:.0} ms", v),
        Axis::Hz => format!("{:.0} Hz", v),
        Axis::Watts => format!("{:.0} W", v),
    }
}

/// A rounded-up axis maximum: 1, 2, 2.5 or 5 times a power of ten.
fn nice_max(v: f32) -> f32 {
    if v.is_nan() || v <= 0.0 {
        return 1.0;
    }
    let p = 10f32.powf(v.log10().floor());
    for k in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if v <= k * p {
            return k * p;
        }
    }
    10.0 * p
}

/// One trace of a chart. `data` is newest last; NaN = no sample.
struct Ser {
    label: String,
    c: Rgba,
    data: Vec<f32>,
    fill: bool,
}

/// Min, average and max of the newest `n` finite samples.
fn stats(d: &[f32], n: usize) -> Option<(f32, f32, f32)> {
    let w = &d[d.len().saturating_sub(n)..];
    let mut it = w.iter().copied().filter(|v| v.is_finite());
    let first = it.next()?;
    let (mut mn, mut mx, mut sum, mut k) = (first, first, first as f64, 1usize);
    for v in it {
        mn = mn.min(v);
        mx = mx.max(v);
        sum += v as f64;
        k += 1;
    }
    Some((mn, (sum / k as f64) as f32, mx))
}

/// Short device tab label.
fn dev_label(m: &Monitor, d: Dev) -> String {
    match d {
        Dev::Cpu => "CPU".into(),
        Dev::Mem => "Memory".into(),
        Dev::Gpu => "GPU".into(),
        Dev::Disk(n) => {
            let letters = m.det.disks.iter().find(|x| x.num == Some(n)).map(|x| x.letters.clone()).unwrap_or_default();
            if letters.is_empty() {
                format!("Disk {n}")
            } else {
                format!("Disk {n} \u{00B7} {}", letters)
            }
        }
        Dev::Display(i) => format!("Display {}", i + 1),
        Dev::Net => "Network".into(),
        Dev::Battery => "Battery".into(),
        Dev::Bluetooth => "Bluetooth".into(),
    }
}

/// Kit speed printed in a memory part number (G.Skill F5-6000J…, Corsair …B6000C36,
/// Kingston KF560C36 = 6000, Crucial CP16G4DFRA32A = 3200), MT/s.
pub(super) fn kit_speed(part: &str) -> Option<u32> {
    let p = part.to_ascii_uppercase();
    let b = p.as_bytes();
    // Kingston: KF / HX + generation digit + two digits (hundreds of MT/s).
    for pre in ["KF", "HX"] {
        if let Some(i) = p.find(pre) {
            let d = &b[i + 2..];
            if d.len() >= 3 && d[..3].iter().all(u8::is_ascii_digit) {
                let v = u32::from(d[1] - b'0') * 10 + u32::from(d[2] - b'0');
                if (16..=96).contains(&v) {
                    return Some(v * 100);
                }
            }
        }
    }
    // Any 4-digit run that is a plausible memory speed.
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let j = (i..b.len()).find(|&k| !b[k].is_ascii_digit()).unwrap_or(b.len());
            if j - i == 4 {
                if let Ok(v) = p[i..j].parse::<u32>() {
                    if (2133..=9600).contains(&v) {
                        return Some(v);
                    }
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    // Crucial: …4DFRA32A style ("32" = 3200 after the DDR generation letters).
    if p.starts_with("CT") || p.starts_with("CP") {
        if let Some(i) = p.find("FRA").or_else(|| p.find("SFRA")) {
            let d = &b[i + 3..];
            if d.len() >= 2 && d[..2].iter().all(u8::is_ascii_digit) {
                let v = u32::from(d[0] - b'0') * 10 + u32::from(d[1] - b'0');
                if (21..=96).contains(&v) {
                    return Some(v * 100);
                }
            }
        }
    }
    None
}

impl Monitor {
    /// Open a device page (from a tile or a tab).
    pub fn open_dev(&mut self, d: Dev) {
        if self.view != Some(d) {
            self.view = Some(d);
            let keep = std::mem::take(&mut self.dp.tops);
            self.dp.clear();
            self.dp.tops = keep;
        }
    }

    /// Back to the overview.
    pub fn close_dev(&mut self) -> bool {
        let was = self.view.is_some();
        self.view = None;
        self.dp.hist.clear();
        self.dp.hover = None;
        was
    }

    fn st(&self) -> &DeepStatic {
        &self.det.deep.st
    }

    fn live(&self) -> &DeepLive {
        &self.det.deep
    }

    /// A click on the space card.
    pub fn scan_click(&mut self, h: Hit) {
        match h {
            Hit::Scan => {
                if self.dp.scan.as_ref().is_some_and(|s| !s.progress().done) {
                    return;
                }
                let Some(root) = self.dev_vols().first().map(|v| format!("{}\\", v.letter)) else { return };
                self.dp.scan_tree = None;
                self.dp.scan_rows.clear();
                self.dp.scan_dir = 0;
                self.dp.scan = Some(space::start(&root, true));
                self.dp.scan_root = root;
            }
            Hit::Folder(id) => self.scan_go(id),
            Hit::FolderUp => {
                if let Some(p) = self.dp.scan_tree.as_ref().and_then(|t| t.parent(self.dp.scan_dir)) {
                    self.scan_go(p);
                }
            }
            _ => {}
        }
    }

    fn scan_go(&mut self, dir: space::NodeId) {
        if let Some(t) = &self.dp.scan_tree {
            self.dp.scan_dir = dir;
            self.dp.scan_rows = t.top(Some(dir), SPACE_ROWS);
        }
    }

    /// Pick the finished scan up (once a second, from `feed`).
    fn scan_poll(&mut self) {
        if self.dp.scan_tree.is_none() {
            if let Some(t) = self.dp.scan.as_ref().and_then(space::Scan::result) {
                self.dp.scan_tree = Some(t);
                self.scan_go(0);
            }
        }
    }

    /// Pointer moved over the page (content coordinates, None = left). True = redraw.
    pub fn pointer(&mut self, p: Option<(i32, i32)>) -> bool {
        if self.view.is_none() {
            return false;
        }
        let was = self.dp.hover.filter(|(x, y)| self.dp.chart.contains(*x, *y));
        self.dp.hover = p;
        let now = p.filter(|(x, y)| self.dp.chart.contains(*x, *y));
        was.is_some() || now.is_some()
    }

    /// Record the open device's history (called from `feed`).
    pub(super) fn feed_dev(&mut self) {
        // Busiest app per resource, every second while the Monitor page is open.
        let mut t = Tops::default();
        let pick = |v: f64, cur: &Option<Top>| cur.as_ref().is_none_or(|c| v > c.2);
        for p in &self.det.procs {
            let title = || self.title_of(&p.name, &p.name.to_lowercase());
            if pick(f64::from(p.cpu), &t.cpu) {
                t.cpu = Some((p.pid, title(), f64::from(p.cpu)));
            }
            if pick(p.mem as f64, &t.mem) {
                t.mem = Some((p.pid, title(), p.mem as f64));
            }
            if pick(p.io, &t.io) {
                t.io = Some((p.pid, title(), p.io));
            }
            if pick(f64::from(p.gpu), &t.gpu) {
                t.gpu = Some((p.pid, title(), f64::from(p.gpu)));
            }
        }
        if self.dp.tops.len() >= TOPS {
            self.dp.tops.pop_front();
        }
        self.dp.tops.push_back(t);

        self.scan_poll();
        let Some(dev) = self.view else { return };
        let lv = self.det.deep.clone();
        match dev {
            Dev::Cpu => {
                let mhz = if lv.core_mhz.is_empty() { cur_mhz(&self.snap, &self.det) } else { lv.core_mhz.iter().sum::<f32>() / lv.core_mhz.len() as f32 };
                self.dp.push("mhz", mhz);
                let top = lv.core_mhz.iter().copied().fold(0.0, f32::max);
                self.dp.push("mhz_max", if top > 0.0 { top } else { mhz });
            }
            Dev::Mem => {
                let d = &self.det;
                let c = if d.commit_limit > 0 { (d.commit as f64 / d.commit_limit as f64 * 100.0) as f32 } else { f32::NAN };
                self.dp.push("commit", c);
            }
            Dev::Gpu => {
                let sh = self.det.vram_shared.map_or(f32::NAN, |v| v as f32);
                self.dp.push("shared", sh);
            }
            Dev::Disk(n) => {
                let a = self.det.disks.iter().find(|x| x.num == Some(n)).map_or(f32::NAN, |x| x.active);
                self.dp.push("active", a);
            }
            Dev::Display(_) => {
                let hz = if lv.measured_hz > 0.0 { lv.measured_hz } else { f32::NAN };
                self.dp.push("hz", hz);
            }
            Dev::Net | Dev::Bluetooth => {}
            Dev::Battery => {
                let b = lv.batteries.first();
                let w = b.and_then(|b| b.rate_mw).map_or(f32::NAN, |r| r as f32 / 1000.0);
                self.dp.push("watts", w.abs());
                let pct = self.snap.battery.as_ref().map_or(f32::NAN, |b| b.percent as f32);
                self.dp.push("pct", pct);
            }
        }
    }

    /// Devices that have a page, in tab order.
    pub(super) fn devices(&self) -> Vec<Dev> {
        let mut v = vec![Dev::Cpu, Dev::Mem];
        if self.has_gpu() {
            v.push(Dev::Gpu);
        }
        for x in &self.det.disks {
            if let Some(n) = x.num {
                v.push(Dev::Disk(n));
            }
        }
        let screens = self.st().screens.len().max(self.det.hw.displays.len());
        for i in 0..screens {
            v.push(Dev::Display(i as u32));
        }
        v.push(Dev::Net);
        if self.snap.battery.is_some() {
            v.push(Dev::Battery);
        }
        if self.has_bt() || self.view == Some(Dev::Bluetooth) {
            v.push(Dev::Bluetooth);
        }
        v
    }

    fn cards_for(&self, d: Dev) -> Vec<Card> {
        match d {
            Dev::Cpu => vec![Card::CoreMap, Card::Power, Card::Apps(0), Card::Clock, Card::Chip],
            Dev::Mem => vec![Card::MemMap, Card::Apps(1), Card::Commit, Card::Sticks],
            Dev::Gpu => {
                let mut v = vec![Card::VramApps, Card::GpuHealth, Card::Engines, Card::Apps(3)];
                if self.st().gpus.len() > 1 {
                    v.push(Card::Adapters);
                }
                v
            }
            Dev::Disk(n) => {
                let mut v = vec![Card::Health, Card::DriveFacts, Card::Apps(2), Card::Volumes, Card::Space];
                let nvme = self.st().disk(n).is_some_and(|x| x.extra.nvme);
                if !nvme && !self.st().admin && self.st().disk(n).is_none_or(|x| x.health.is_none()) {
                    v.push(Card::Admin);
                }
                v
            }
            Dev::Display(_) => vec![Card::Signal, Card::Link, Card::Hdr, Card::Panel],
            Dev::Net => vec![Card::Path, Card::Outages, Card::AdapterFacts, Card::Wifi, Card::AppTraffic, Card::Conns, Card::Traffic],
            Dev::Battery => vec![Card::Capacity, Card::BatLive],
            Dev::Bluetooth => Vec::new(),
        }
    }

    /// Height a card needs at width `w`.
    fn card_h(&self, c: Card, w: i32) -> i32 {
        let s = |v: i32| self.sc(v);
        let head = s(46);
        let foot = s(16);
        let kv_rows = |n: usize, w: i32| {
            let cols = ((w - s(32)) / s(150)).clamp(1, 3) as usize;
            n.div_ceil(cols) as i32 * s(56)
        };
        let body = match c {
            Card::CoreMap => {
                let n = self.det.core_pct.len().max(1) as i32;
                let cols = ((w - s(32)) / s(58)).clamp(1, 16);
                (n + cols - 1) / cols * s(58) + s(30)
            }
            Card::Chip => kv_rows(self.chip_facts().len(), w),
            Card::Power => kv_rows(self.power_facts().len(), w),
            Card::Apps(_) => APP_ROWS as i32 * s(30),
            Card::Clock => s(150),
            Card::MemMap => s(36 + 18) + 4 * s(24),
            Card::Commit => kv_rows(self.commit_facts().len(), w) + s(12 + 80),
            Card::Sticks => s(64 + 28 + 20) + if self.xmp_note().is_some() { s(40) } else { 0 },
            Card::VramApps => APP_ROWS as i32 * s(30) + s(40),
            Card::Engines => self.det.engines.len().max(1) as i32 * s(26),
            Card::GpuHealth => kv_rows(self.gpu_facts().len(), w),
            Card::Adapters => self.st().gpus.len().max(1) as i32 * s(44),
            Card::Health => s(120) + s(8),
            Card::DriveFacts => kv_rows(self.drive_facts().len(), w),
            Card::Volumes => self.dev_vols().len().max(1) as i32 * s(28),
            Card::Space => s(34) + SPACE_ROWS as i32 * s(30),
            Card::Signal => s(72),
            Card::Link => kv_rows(self.link_facts().len(), w),
            Card::Hdr => s(150),
            Card::Panel => kv_rows(self.panel_facts().len(), w),
            Card::Path => s(56 + 8) + 5 * s(22),
            Card::Outages => self.outages().len().clamp(1, 8) as i32 * s(26) + s(40),
            Card::AdapterFacts => kv_rows(self.net_facts().len(), w),
            Card::Wifi => kv_rows(self.wifi_facts().len().max(1), w) + s(24),
            Card::Conns => APP_ROWS as i32 * s(30),
            Card::AppTraffic => APP_ROWS as i32 * s(30),
            Card::Traffic => s(130),
            Card::Capacity => s(40 + 28) + kv_rows(self.bat_facts().len(), w),
            Card::BatLive => kv_rows(self.bat_live().len(), w),
            Card::Admin => s(96),
        };
        head + body + foot
    }

    // ------------------------------------------------------------------ layout

    /// Lay a device page out; returns the bottom.
    pub(super) fn layout_dev(&mut self, l: i32, r: i32, y0: i32) -> i32 {
        let Some(dev) = self.view else { return y0 };
        let gap = self.sc(12);
        let w = r - l;
        let mut y = y0;
        // Tabs row: Overview, then every device; the range control on the right.
        let th = self.sc(30);
        let bt = dev == Dev::Bluetooth;
        // The Bluetooth page has no chart: its layout switch takes the range control's place.
        let segw = self.sc(if bt { 54 } else { 40 });
        let nseg = if bt { btpage::BT_LAYOUTS.len() } else { RANGES.len() } as i32;
        let rx = r - segw * nseg - self.sc(4);
        let mut x = rx;
        let segs: Vec<Hit> = if bt { (0..nseg as u8).map(Hit::BtLayout).collect() } else { RANGES.iter().map(|x| Hit::Range(x.1)).collect() };
        for h in segs {
            self.hits.push((Rect { l: x, t: y + self.sc(3), r: x + segw, b: y + th - self.sc(3) }, h));
            x += segw;
        }
        // Not elevated: a standing way to unlock the admin-only details, left of the range control.
        let first_r = if is_admin() {
            rx - self.sc(12)
        } else {
            let aw = self.sc(172);
            let b = Rect { l: rx - self.sc(12) - aw, t: y + self.sc(2), r: rx - self.sc(12), b: y + th - self.sc(2) };
            self.hits.push((b, Hit::RunAdmin(0)));
            b.l - self.sc(12)
        };
        self.dp.tabs.clear();
        let mut tx = l;
        let mut ty = y;
        let back_w = self.sc(104);
        self.hits.push((Rect { l: tx, t: ty, r: tx + back_w, b: ty + th }, Hit::Overview));
        tx += back_w + self.sc(8);
        let devs = self.devices();
        for d in devs {
            let label = dev_label(self, d);
            let tw = crate::render::measure_text(self.tab_font, &label).0 + self.sc(24);
            let limit = if ty == y0 { first_r } else { r };
            if tx + tw > limit && tx > l {
                tx = l;
                ty += th + self.sc(6);
            }
            let rc = Rect { l: tx, t: ty, r: tx + tw, b: ty + th };
            self.dp.tabs.push((d, rc));
            self.hits.push((rc, Hit::Open(d)));
            tx += tw + self.sc(6);
        }
        y = ty + th + gap;
        // Verdict.
        self.dp.verdict = Rect { l, t: y, r, b: y + self.sc(78) };
        y += self.dp.verdict.h() + gap;
        self.dp.bt = btpage::BtLayout::default();
        if bt {
            self.dp.chart = Rect::default();
            self.dp.cards.clear();
            let v = self.dp.verdict;
            let bw = self.sc(150);
            let bs = Rect { l: v.r - self.sc(16) - bw, t: v.t + self.sc(24), r: v.r - self.sc(16), b: v.t + self.sc(54) };
            self.hits.push((bs, Hit::BtSettings));
            // Debug report button left of it, when the card is wide enough for both.
            let dw = self.sc(120);
            if bs.l - self.sc(8) - dw > v.l + self.sc(260) {
                self.hits.push((Rect { l: bs.l - self.sc(8) - dw, r: bs.l - self.sc(8), ..bs }, Hit::BtDebug));
            }
            return self.layout_bt(l, r, y);
        }
        // Chart.
        let ch = if w < self.sc(560) { self.sc(200) } else { self.sc(CHART_H) };
        self.dp.chart = Rect { l, t: y, r, b: y + ch };
        self.hits.push((self.dp.chart, Hit::Spike));
        y += ch + gap;
        // Cards: masonry into 1-4 columns (each card goes to the shortest column).
        let cols: i32 = if w < self.sc(720) {
            1
        } else if w < self.sc(1240) {
            2
        } else if w < self.sc(1720) {
            3
        } else {
            4
        };
        let colw = (w - gap * (cols - 1)) / cols;
        let mut tops = vec![y; cols as usize];
        self.dp.cards.clear();
        let cards = self.cards_for(dev);
        // Fewer cards than columns: they share the width.
        let cols_used = cols.min(cards.len().max(1) as i32);
        let colw = if cols_used < cols { (w - gap * (cols_used - 1)) / cols_used } else { colw };
        for c in cards {
            let (ci, _) = tops.iter().enumerate().take(cols_used as usize).min_by_key(|(_, t)| **t).unwrap_or((0, &y));
            let x0 = l + ci as i32 * (colw + gap);
            let x1 = if ci as i32 == cols_used - 1 { r } else { x0 + colw };
            let h = self.card_h(c, x1 - x0);
            let rc = Rect { l: x0, t: tops[ci], r: x1, b: tops[ci] + h };
            tops[ci] = rc.b + gap;
            self.dp.cards.push((c, rc));
        }
        // Even out the column bottoms: the last card of each column grows to the tallest.
        let bottom = tops.iter().copied().max().unwrap_or(y);
        for ci in 0..cols_used as usize {
            let x0 = l + ci as i32 * (colw + gap);
            if let Some(last) = self.dp.cards.iter_mut().filter(|(_, rc)| rc.l == x0).last() {
                last.1.b = bottom - gap;
            }
        }
        let cards = self.dp.cards.clone();
        for (c, rc) in cards {
            self.card_hits(c, rc);
        }
        bottom
    }

    fn card_hits(&mut self, c: Card, r: Rect) {
        let dpi = self.dpi as i32;
        let s = move |v: i32| (v * dpi + 48) / 96;
        match c {
            Card::Apps(k) => {
                let rows = self.app_rows(k);
                for (i, row) in rows.iter().enumerate() {
                    let t = r.t + s(46) + i as i32 * s(30);
                    self.hits.push((Rect { l: r.l + s(8), t, r: r.r - s(8), b: t + s(30) }, Hit::Proc(row.0)));
                }
            }
            Card::VramApps => {
                for (i, row) in self.vram_rows().iter().enumerate() {
                    let t = r.t + s(46) + i as i32 * s(30);
                    self.hits.push((Rect { l: r.l + s(8), t, r: r.r - s(8), b: t + s(30) }, Hit::Proc(row.0)));
                }
            }
            Card::Conns => {
                for (i, row) in self.conn_rows().iter().enumerate() {
                    let t = r.t + s(46) + i as i32 * s(30);
                    self.hits.push((Rect { l: r.l + s(8), t, r: r.r - s(8), b: t + s(30) }, Hit::Proc(row.0)));
                }
            }
            Card::Outages => {
                let b = Rect { l: r.r - s(16) - s(120), t: r.t + s(12), r: r.r - s(16), b: r.t + s(36) };
                self.hits.push((b, Hit::CopyOutages));
            }
            Card::Admin => {
                let b = Rect { l: r.l + s(16), t: r.b - s(16) - s(32), r: r.l + s(16) + s(200), b: r.b - s(16) };
                self.hits.push((b, Hit::Elevate));
            }
            Card::Space => {
                // Scan / rescan button in the header, an "up" row, then the folders.
                self.hits.push((Rect { l: r.r - s(16) - s(96), t: r.t + s(12), r: r.r - s(16), b: r.t + s(36) }, Hit::Scan));
                if self.dp.scan.is_none() && !is_admin() {
                    self.hits.push((Rect { l: r.l + s(16), t: r.t + s(46 + 30), r: r.l + s(16) + s(200), b: r.t + s(46 + 30 + 32) }, Hit::RunAdmin(1)));
                }
                if self.dp.scan_tree.is_some() {
                    if self.dp.scan_dir != 0 {
                        self.hits.push((Rect { l: r.l + s(8), t: r.t + s(46), r: r.r - s(8), b: r.t + s(46 + 28) }, Hit::FolderUp));
                    }
                    let rows: Vec<(u32, bool)> = self.dp.scan_rows.iter().map(|e| (e.id, e.is_files_here)).collect();
                    for (i, (id, files)) in rows.into_iter().enumerate() {
                        if !files {
                            let t = r.t + s(46 + 34) + i as i32 * s(30);
                            self.hits.push((Rect { l: r.l + s(8), t, r: r.r - s(8), b: t + s(30) }, Hit::Folder(id)));
                        }
                    }
                }
            }
            Card::AppTraffic => {
                if self.live().app_net_state == AppNet::NeedsAdmin {
                    let b = Rect { l: r.l + s(16), t: r.b - s(16) - s(32), r: r.l + s(16) + s(200), b: r.b - s(16) };
                    self.hits.push((b, Hit::Elevate));
                } else {
                    for (i, row) in self.traffic_rows().iter().enumerate() {
                        let t = r.t + s(46) + i as i32 * s(30);
                        self.hits.push((Rect { l: r.l + s(8), t, r: r.r - s(8), b: t + s(30) }, Hit::Proc(row.0)));
                    }
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ data helpers

    /// Top apps by sort key (0 CPU, 1 memory, 2 disk, 3 GPU): (pid, title, value text, fraction).
    fn app_rows(&self, k: u8) -> Vec<(u32, String, String, f32)> {
        let mut by: HashMap<String, (u32, f64, u32)> = HashMap::new();
        for p in &self.det.procs {
            let v = match k {
                0 => f64::from(p.cpu),
                1 => p.mem as f64,
                2 => p.io,
                _ => f64::from(p.gpu),
            };
            let e = by.entry(p.name.to_lowercase()).or_insert((p.pid, 0.0, 0));
            e.1 += v;
            e.2 += 1;
            if e.2 == 1 {
                e.0 = p.pid;
            }
        }
        let mut v: Vec<(String, (u32, f64, u32))> = by.into_iter().filter(|(_, x)| x.1 > 0.0).collect();
        v.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(APP_ROWS);
        let max = match k {
            0 | 3 => 100.0,
            _ => v.first().map_or(1.0, |x| x.1 .1).max(1.0),
        };
        v.into_iter()
            .map(|(key, (pid, val, n))| {
                let name = self.names.get(&key).cloned().unwrap_or_else(|| appname::bare(&key).to_string());
                let name = if n > 1 { format!("{name} ({n})") } else { name };
                let t = match k {
                    0 | 3 => format!("{:.1}%", val.min(100.0)),
                    1 => fmt::bytes(val as u64),
                    _ => fmt::rate(val),
                };
                (pid, name, t, (val / max) as f32)
            })
            .collect()
    }

    fn proc_title(&self, pid: u32) -> String {
        self.det.procs.iter().find(|p| p.pid == pid).map_or(format!("PID {pid}"), |p| self.title_of(&p.name, &p.name.to_lowercase()))
    }

    /// Apps by dedicated video memory: (pid, title, dedicated, shared).
    fn vram_rows(&self) -> Vec<(u32, String, u64, u64)> {
        let mut v: Vec<(u32, u64, u64)> = Vec::new();
        for (pid, _, ded, sh) in &self.live().gpu_mem {
            match v.iter_mut().find(|x| x.0 == *pid) {
                Some(x) => {
                    x.1 += ded;
                    x.2 += sh;
                }
                None => v.push((*pid, *ded, *sh)),
            }
        }
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v.truncate(APP_ROWS);
        v.into_iter().filter(|x| x.1 + x.2 > 0).map(|(pid, d, s)| (pid, self.proc_title(pid), d, s)).collect()
    }

    /// Apps by network bytes: (pid, title, "down / up" text, fraction of the busiest).
    fn traffic_rows(&self) -> Vec<(u32, String, String, f32)> {
        let v = &self.live().app_net;
        let top = v.first().map_or(0.0, |a| a.down_bps + a.up_bps).max(1.0);
        v.iter()
            .filter(|a| a.down_bps + a.up_bps > 0.0)
            .take(APP_ROWS)
            .map(|a| (a.pid, self.proc_title(a.pid), format!("\u{25BC} {}  \u{25B2} {}", fmt::rate(a.down_bps), fmt::rate(a.up_bps)), ((a.down_bps + a.up_bps) / top) as f32))
            .collect()
    }

    /// Apps with network connections: (pid, title, established, remote hosts).
    fn conn_rows(&self) -> Vec<(u32, String, usize, String)> {
        let mut v = net::by_app(&self.live().conns);
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v.truncate(APP_ROWS);
        v.into_iter().map(|(pid, n, hosts)| (pid, self.proc_title(pid), n, hosts.join(", "))).collect()
    }

    fn outages(&self) -> Vec<net::Outage> {
        self.live().ping.as_ref().map_or(Vec::new(), |p| p.outages.clone())
    }

    /// Text for the "copy for my provider" button.
    pub fn outage_text(&self) -> String {
        net::outage_text(&self.outages())
    }

    fn screen(&self) -> Option<&display::Screen> {
        match self.view {
            Some(Dev::Display(i)) => self.st().screens.get(i as usize),
            _ => None,
        }
    }

    fn dev_vols(&self) -> Vec<&crate::sensors::hw::Vol> {
        match self.view {
            Some(Dev::Disk(n)) => self.det.hw.vols.iter().filter(|v| v.disk == Some(n)).collect(),
            _ => Vec::new(),
        }
    }

    fn chip_facts(&self) -> Vec<(&'static str, String)> {
        let d = &self.det;
        let mut v: Vec<(&'static str, String)> = vec![("CORES / THREADS", format!("{} / {}", d.cores, d.logical))];
        if d.base_mhz > 0.0 {
            v.push(("BASE SPEED", ghz(d.base_mhz)));
        }
        if let Some(t) = &self.st().topo {
            for c in &t.caches {
                let k = match (c.level, c.kind) {
                    (1, "Data") => "L1 DATA",
                    (1, "Instruction") => "L1 CODE",
                    (2, _) => "L2 PER CORE",
                    (3, _) => "L3",
                    _ => continue,
                };
                v.push((k, fmt::bytes(c.size)));
            }
            if t.sockets > 1 {
                v.push(("SOCKETS", t.sockets.to_string()));
            }
            if t.hybrid {
                let p = t.cores.iter().filter(|c| c.efficiency_class == t.cores.iter().map(|x| x.efficiency_class).max().unwrap_or(0)).count();
                v.push(("CORE TYPES", format!("{} P + {} E", p, t.cores.len() - p)));
            }
            if let Some(i) = t.vcache_group {
                let mb = t.l3.get(i).map_or(0, |x| x.0);
                v.push(("3D V-CACHE", format!("CCD {} \u{00B7} {}", i, fmt::bytes(mb))));
            }
            if let Some(vt) = t.virtualization {
                v.push(("VIRTUALIZATION", if vt { "On".into() } else { "Off".into() }));
            }
        } else if d.l3 > 0 {
            v.push(("L3", fmt::bytes(d.l3)));
        }
        v
    }

    fn power_facts(&self) -> Vec<(&'static str, String)> {
        let s = &self.snap;
        let lv = self.live();
        let mut v: Vec<(&'static str, String)> = Vec::new();
        if let Some(p) = &self.st().power {
            v.push(("POWER MODE", p.overlay.to_string()));
            if !p.plan.is_empty() {
                v.push(("POWER PLAN", p.plan.clone()));
            }
        }
        v.push(("SPEED LIMIT", s.cpu_perf_limit.map_or("\u{2013}".into(), |l| if l >= 99.0 { "None".into() } else { format!("{:.0}%", l) })));
        if let Some(t) = s.thermal_passive_limit.filter(|t| *t < 99.0) {
            v.push(("HEAT LIMIT", format!("{:.0}%", t)));
        }
        v.push(("PERFORMANCE", s.cpu_perf_pct.map_or("\u{2013}".into(), |p| format!("{:.0}%", p))));
        if let Some(i) = lv.interrupt_pct {
            v.push(("INTERRUPTS", format!("{:.1}%", i)));
        }
        if let Some(i) = lv.dpc_pct {
            v.push(("DPC", format!("{:.1}%", i)));
        }
        if let Some(c) = lv.ctx_switches {
            v.push(("SWITCHES", format!("{} /s", group(c as u64))));
        }
        if let Some(q) = lv.queue {
            v.push(("WAITING", format!("{:.0} threads", q)));
        }
        v
    }

    fn commit_facts(&self) -> Vec<(&'static str, String)> {
        let d = &self.det;
        let s = &self.snap;
        let mut v = vec![
            ("COMMITTED", of(&fmt::bytes(d.commit), &fmt::bytes(d.commit_limit))),
            ("AVAILABLE", fmt::bytes(s.ram_total.saturating_sub(s.ram_used))),
            ("HARD FAULTS", d.hard_faults.map_or("\u{2013}".into(), |v| format!("{:.0} /s", v))),
            ("PAGED POOL", fmt::bytes(d.paged)),
            ("NON-PAGED", fmt::bytes(d.nonpaged)),
        ];
        let installed = self.det.hw.dimms.iter().map(|x| x.size).sum::<u64>();
        if installed > s.ram_total {
            v.push(("RESERVED", fmt::bytes(installed - s.ram_total)));
        }
        v
    }

    /// "Running 4800, kit rated 6000" when the fast memory profile looks off.
    fn xmp_note(&self) -> Option<(u32, u32)> {
        let hw = &self.det.hw;
        let cur = hw.dimms.iter().filter(|d| d.size > 0).map(|d| d.speed).max()?;
        if cur == 0 {
            return None;
        }
        let rated = hw.dimms.iter().filter(|d| d.size > 0).map(|d| d.rated.max(kit_speed(&d.part).unwrap_or(0))).max()?;
        (rated > cur + 100).then_some((cur, rated))
    }

    fn gpu_facts(&self) -> Vec<(&'static str, String)> {
        let mut v: Vec<(&'static str, String)> = Vec::new();
        let d = &self.det;
        if d.vram_total > 0 {
            v.push(("VIDEO MEMORY", fmt::bytes(d.vram_total)));
        }
        if let Some(a) = self.main_gpu() {
            if let Some(p) = a.pcie {
                let ok = p.gen >= p.max_gen && p.width >= p.max_width;
                let t = format!("Gen{} x{} of Gen{} x{}", p.gen, p.width, p.max_gen, p.max_width);
                v.push(("SLOT LINK", if ok { t } else { format!("{t} (idle?)") }));
            }
            if let Some(h) = a.hags {
                v.push(("GPU SCHEDULING", if h { "On".into() } else { "Off".into() }));
            }
            if !a.driver_version.is_empty() {
                v.push(("DRIVER", a.driver_version.clone()));
            }
            if !a.driver_date.is_empty() {
                v.push(("DRIVER DATE", a.driver_date.clone()));
            }
            if let Some(p) = self.live().gpu_perf.iter().find(|x| x.0 == a.luid).map(|x| &x.1) {
                if let Some(t) = p.temp_c {
                    v.push(("TEMPERATURE", format!("{:.0} \u{00B0}C", t)));
                }
                if let Some(f) = p.fan_rpm.filter(|f| *f > 0) {
                    v.push(("FAN", format!("{} rpm", group(u64::from(f)))));
                }
                if let Some(pw) = p.power_pct {
                    v.push(("POWER", format!("{:.0}% of limit", pw)));
                }
                if let Some(mh) = p.mem_mhz.filter(|m| *m > 0.0) {
                    v.push(("MEMORY CLOCK", format!("{:.0} MHz", mh)));
                }
            }
        }
        if let Some(t) = self.snap.gpu_temp.filter(|_| !v.iter().any(|x| x.0 == "TEMPERATURE")) {
            v.push(("TEMPERATURE", format!("{:.0} \u{00B0}C", t)));
        }
        v
    }

    /// The adapter the GPU page is about: the busiest discrete one, else the first.
    fn main_gpu(&self) -> Option<&crate::sensors::deep::gpu::Adapter> {
        let g = &self.st().gpus;
        g.iter().find(|a| !a.integrated).or_else(|| g.first())
    }

    fn wrong_gpu(&self) -> Vec<(u32, u64)> {
        crate::sensors::deep::gpu::wrong_gpu(&self.live().gpu_3d, &self.st().gpus)
    }

    fn drive_facts(&self) -> Vec<(&'static str, String)> {
        let Some(Dev::Disk(n)) = self.view else { return Vec::new() };
        let mut v: Vec<(&'static str, String)> = Vec::new();
        if let Some(d) = self.det.hw.drive(n) {
            if !d.model.is_empty() {
                v.push(("MODEL", d.model.clone()));
            }
            let kind = match d.ssd {
                Some(true) => " SSD",
                Some(false) => " HDD",
                None => "",
            };
            v.push(("TYPE", format!("{}{}", d.bus, kind).trim().to_string()));
            if d.size > 0 {
                v.push(("SIZE", fmt::bytes(d.size)));
            }
        }
        if let Some(x) = self.st().disk(n) {
            let e = &x.extra;
            if let Some((g, w, mg, mw)) = e.pcie {
                let busy = self.det.disks.iter().find(|d| d.num == Some(n)).is_some_and(|d| d.active > 30.0);
                let full = g >= mg && w >= mw;
                let t = format!("Gen{g} x{w} of Gen{mg} x{mw}");
                v.push(("LINK", if full { format!("{t} \u{2713}") } else if busy { format!("{t} \u{26A0}") } else { t }));
            }
            if !e.firmware.is_empty() {
                v.push(("FIRMWARE", e.firmware.clone()));
            }
            if let Some(t) = e.trim {
                v.push(("TRIM", if t { "On".into() } else { "Off".into() }));
            }
            if !e.serial.is_empty() {
                v.push(("SERIAL", e.serial.clone()));
            }
        }
        if let Some(x) = self.det.disks.iter().find(|d| d.num == Some(n)) {
            v.push(("RESPONSE", format!("{:.1} ms", x.latency_ms)));
            v.push(("QUEUE", format!("{:.1}", x.queue)));
        }
        v
    }

    fn link_facts(&self) -> Vec<(&'static str, String)> {
        let Some(s) = self.screen() else { return self.basic_display_facts() };
        let mut v: Vec<(&'static str, String)> = vec![("RESOLUTION", format!("{} \u{00D7} {}", s.w, s.h)), ("REFRESH, SET", format!("{:.0} Hz", s.hz))];
        let mh = self.live().measured_hz;
        if mh > 0.0 {
            v.push(("REFRESH, MEASURED", format!("{:.1} Hz", mh)));
        }
        if let Some(b) = s.bits {
            v.push(("BIT DEPTH", format!("{b}-bit")));
        }
        if !s.encoding.is_empty() {
            v.push(("COLOUR FORMAT", s.encoding.to_string()));
        }
        v.push(("HDR", if s.hdr_on { "On".into() } else if s.hdr_supported { "Off (supported)".into() } else { "Not supported".into() }));
        if let Some(n) = s.sdr_white_nits.filter(|_| s.hdr_on) {
            v.push(("SDR WHITE", format!("{:.0} nits", n)));
        }
        if let Some(dsc) = display::dsc_guess(s) {
            v.push(("COMPRESSION (DSC)", if dsc { "Probably on".into() } else { "Not needed".into() }));
        }
        if let Some(p) = s.dpi_scale {
            v.push(("SCALING", format!("{p}%")));
        }
        v
    }

    fn basic_display_facts(&self) -> Vec<(&'static str, String)> {
        let i = match self.view {
            Some(Dev::Display(i)) => i as usize,
            _ => return Vec::new(),
        };
        let Some(d) = self.det.hw.displays.get(i) else { return Vec::new() };
        vec![("RESOLUTION", format!("{} \u{00D7} {}", d.w, d.h)), ("REFRESH", format!("{:.0} Hz", d.hz)), ("CONNECTION", d.link.clone())]
    }

    fn panel_facts(&self) -> Vec<(&'static str, String)> {
        let Some(s) = self.screen() else { return Vec::new() };
        let mut v: Vec<(&'static str, String)> = Vec::new();
        if let Some(e) = &s.edid {
            if !e.model.is_empty() {
                v.push(("MODEL", e.model.clone()));
            }
            if !e.maker.is_empty() {
                v.push(("MAKER", e.maker.clone()));
            }
            if e.year > 0 {
                v.push(("BUILT", if e.week > 0 && e.week <= 53 { format!("{}, week {}", e.year, e.week) } else { e.year.to_string() }));
            }
            if e.diag_in > 0.0 {
                v.push(("SIZE", format!("{:.1} in", e.diag_in)));
                if e.width_cm > 0 && s.w > 0 {
                    let ppi = s.w as f32 / (f32::from(e.width_cm) / 2.54);
                    v.push(("SHARPNESS", format!("{:.0} ppi", ppi)));
                }
            }
            if let Some((lo, hi)) = e.range {
                v.push(("REFRESH RANGE", format!("{lo}\u{2013}{hi} Hz")));
            }
        }
        v.push(("CONNECTOR", s.connector.to_string()));
        if let Some(g) = self.st().gpus.iter().find(|g| g.luid == s.adapter_luid) {
            v.push(("DRIVEN BY", g.name.clone()));
        }
        v
    }

    fn net_facts(&self) -> Vec<(&'static str, String)> {
        let Some(a) = self.st().adapters.iter().find(|a| a.up && !a.gateway.is_empty()).or_else(|| self.st().adapters.iter().find(|a| a.up)) else {
            return Vec::new();
        };
        let mut v: Vec<(&'static str, String)> = vec![("ADAPTER", a.alias.clone()), ("KIND", a.kind.to_string())];
        if a.link_down_bps > 0 {
            v.push(("LINK", link(a.link_down_bps)));
        }
        if let Some(ip) = a.ipv4.first() {
            v.push(("ADDRESS", ip.clone()));
        }
        if let Some(g) = a.gateway.first() {
            v.push(("ROUTER", g.clone()));
        }
        if !a.dns.is_empty() {
            v.push(("DNS", a.dns.iter().take(2).cloned().collect::<Vec<_>>().join(", ")));
        }
        if let Some(ip6) = a.ipv6.first() {
            v.push(("IPV6", ip6.clone()));
        }
        if !a.mac.is_empty() {
            v.push(("MAC", a.mac.clone()));
        }
        v.push(("SETUP", if a.dhcp { "Automatic (DHCP)".into() } else { "Manual".into() }));
        if self.st().adapters.iter().any(|x| x.up && x.kind == "VPN") {
            v.push(("VPN", "Connected".into()));
        }
        v
    }

    fn wifi_facts(&self) -> Vec<(&'static str, String)> {
        let Some(w) = &self.live().wifi else { return Vec::new() };
        let mut v: Vec<(&'static str, String)> = Vec::new();
        if let Some(s) = &w.ssid {
            v.push(("NETWORK", s.clone()));
        }
        if let Some(p) = w.signal_pct {
            v.push(("SIGNAL", match w.rssi_dbm {
                Some(d) => format!("{p}% \u{00B7} {d} dBm"),
                None => format!("{p}%"),
            }));
        }
        if let Some(b) = w.band {
            v.push(("BAND", b.to_string()));
        }
        if let Some(c) = w.channel {
            v.push(("CHANNEL", c.to_string()));
        }
        if let Some(p) = w.phy {
            v.push(("STANDARD", p.to_string()));
        }
        if let Some(r) = w.rx_mbps {
            v.push(("LINK RATE", format!("{r} Mbps")));
        }
        v
    }

    fn bat_facts(&self) -> Vec<(&'static str, String)> {
        let Some(b) = self.live().batteries.first() else { return Vec::new() };
        let mut v: Vec<(&'static str, String)> = Vec::new();
        v.push(("HOLDS NOW", format!("{:.1} Wh", b.full_mwh as f32 / 1000.0)));
        v.push(("WHEN NEW", format!("{:.1} Wh", b.design_mwh as f32 / 1000.0)));
        if let Some(c) = b.cycles {
            v.push(("CYCLES", c.to_string()));
        }
        if !b.chemistry.is_empty() {
            v.push(("CHEMISTRY", b.chemistry.clone()));
        }
        if !b.maker.is_empty() {
            v.push(("MAKER", b.maker.clone()));
        }
        if !b.name.is_empty() {
            v.push(("MODEL", b.name.clone()));
        }
        v
    }

    fn bat_live(&self) -> Vec<(&'static str, String)> {
        let Some(b) = self.live().batteries.first() else { return Vec::new() };
        let mut v: Vec<(&'static str, String)> = Vec::new();
        if let Some(r) = b.rate_mw {
            let w = r.unsigned_abs() as f32 / 1000.0;
            v.push((if r > 0 { "CHARGING AT" } else if r < 0 { "DRAINING AT" } else { "POWER" }, format!("{:.1} W", w)));
        }
        if let Some(mn) = b.minutes_left {
            v.push((if b.charging { "FULL IN" } else { "TIME LEFT" }, fmt::duration(u64::from(mn) * 60)));
        }
        if let Some(n) = b.now_mwh {
            v.push(("CHARGE", format!("{:.1} Wh", n as f32 / 1000.0)));
        }
        if let Some(mv) = b.voltage_mv {
            v.push(("VOLTAGE", format!("{:.2} V", mv as f32 / 1000.0)));
        }
        if let Some(t) = b.temp_c {
            v.push(("TEMPERATURE", format!("{:.0} \u{00B0}C", t)));
        }
        v.push(("POWER SOURCE", if b.on_ac { "Plugged in".into() } else { "Battery".into() }));
        v
    }

    // ------------------------------------------------------------------ verdicts

    /// Tone, headline and detail line for the open page.
    fn verdict(&self) -> (Tone, String, String) {
        let s = &self.snap;
        let d = &self.det;
        match self.view {
            Some(Dev::Bluetooth) => self.bt_verdict(),
            Some(Dev::Cpu) => {
                let mhz = cur_mhz(s, d);
                let parked = self.live().parked.len();
                let mut sub = Vec::new();
                if mhz > 0.0 {
                    sub.push(format!("{} now", ghz(mhz)));
                }
                if d.base_mhz > 0.0 {
                    sub.push(format!("base {}", ghz(d.base_mhz)));
                }
                if parked > 0 {
                    sub.push(format!("{parked} cores parked"));
                }
                let limit = s.cpu_perf_limit.filter(|l| *l < 95.0);
                let heat = s.thermal_passive_limit.filter(|l| *l < 95.0);
                if let Some(h) = heat {
                    return (Tone::Bad, format!("Held back to {:.0}% by heat.", h), "Windows is slowing the CPU to cool it. Check fans and airflow.".into());
                }
                if let Some(l) = limit {
                    let why = if s.battery.as_ref().is_some_and(|b| !b.on_ac) { "on battery" } else { "by the power settings" };
                    return (Tone::Warn, format!("Held back to {:.0}% {why}.", l), sub.join(" \u{00B7} "));
                }
                if s.cpu >= 90.0 {
                    let top = self.app_rows(0).into_iter().next().map_or(String::new(), |r| format!(" {} is using {}.", r.1, r.2));
                    return (Tone::Warn, format!("Busy at {:.0}%.{top}", s.cpu), sub.join(" \u{00B7} "));
                }
                (Tone::Good, "Running at full speed.".into(), sub.join(" \u{00B7} "))
            }
            Some(Dev::Mem) => {
                let avail = s.ram_total.saturating_sub(s.ram_used);
                let cache = d.standby.unwrap_or(0);
                let commit = if d.commit_limit > 0 { d.commit as f64 / d.commit_limit as f64 * 100.0 } else { 0.0 };
                let faults = d.hard_faults.unwrap_or(0.0);
                if commit >= 90.0 || (avail < s.ram_total / 20 && faults > 300.0) {
                    let top = self.app_rows(1).into_iter().next().map_or(String::new(), |r| format!(" {} holds {}.", r.1, r.2));
                    return (Tone::Bad, "Real memory pressure.".into(), format!("Commit {:.0}%, {:.0} page reads a second.{top}", commit, faults));
                }
                if s.ram_pct >= 70.0 && cache > s.ram_total / 10 {
                    return (Tone::Good, format!("Normal. {:.0}% looks high, {} of it is cache.", s.ram_pct, fmt::bytes(cache)), "Windows hands cache back the moment an app needs it.".into());
                }
                if s.ram_pct >= 85.0 {
                    return (Tone::Warn, format!("Getting full: {} free.", fmt::bytes(avail)), "Closing the top app below frees the most.".into());
                }
                (Tone::Good, format!("Plenty free: {} available.", fmt::bytes(avail)), format!("Commit {:.0}% of the limit.", commit))
            }
            Some(Dev::Gpu) => {
                if let Some((pid, luid)) = self.wrong_gpu().first().copied() {
                    let on = self.st().gpus.iter().find(|g| g.luid == luid).map_or("the built-in GPU".to_string(), |g| g.name.clone());
                    let other = self.st().gpus.iter().find(|g| !g.integrated).map_or(String::new(), |g| format!(", not {}", g.name));
                    return (Tone::Warn, format!("{} is on {on}{other}.", self.proc_title(pid)), "Pick the fast GPU for it in Settings > Display > Graphics.".into());
                }
                let shared = d.vram_shared.unwrap_or(0);
                let vram_pct = if d.vram_total > 0 { s.vram_used.unwrap_or(0) as f64 / d.vram_total as f64 * 100.0 } else { 0.0 };
                if vram_pct >= 90.0 && shared > 512 << 20 {
                    return (Tone::Warn, "Video memory is full and spilling.".into(), format!("{} moved to system RAM, which is much slower.", fmt::bytes(shared)));
                }
                match self.vram_rows().first() {
                    Some((_, name, ded, _)) if *ded > 0 => (Tone::Good, format!("{name} holds the most video memory: {}.", fmt::bytes(*ded)), format!("{} of {} in use.", fmt::bytes(s.vram_used.unwrap_or(0)), fmt::bytes(d.vram_total))),
                    _ => (Tone::Good, format!("GPU at {:.0}%.", s.gpu.unwrap_or(0.0)), "Nothing is spilling into shared memory.".into()),
                }
            }
            Some(Dev::Disk(n)) => {
                let x = self.st().disk(n);
                let act = d.disks.iter().find(|x| x.num == Some(n)).map_or(0.0, |x| x.active);
                if let Some(h) = x.and_then(|x| x.health.as_ref()) {
                    if h.critical_warning != 0 || h.media_errors > 0 && h.life_left < 50 {
                        return (Tone::Bad, "This drive reports a problem.".into(), "Back up what matters on it now.".into());
                    }
                    if h.life_left < 10 {
                        return (Tone::Warn, format!("{}% life left.", h.life_left), "Plan a replacement and keep backups.".into());
                    }
                    let days = (h.power_on_hours as f64 / 24.0).max(1.0);
                    let daily = h.written_bytes as f64 / days;
                    let yrs = crate::sensors::deep::disk::forecast(h, daily);
                    let pace = format!("{} written a day on average.", fmt::bytes(daily as u64));
                    let slow = x.and_then(|x| x.extra.pcie).filter(|(g, w, mg, mw)| (g < mg || w < mw) && act > 30.0);
                    if let Some((g, w, mg, mw)) = slow {
                        return (Tone::Warn, format!("Running at Gen{g} x{w}, made for Gen{mg} x{mw}."), "The slot or a BIOS setting holds it back.".into());
                    }
                    return match yrs {
                        Some(y) if y >= 1.0 => (Tone::Good, format!("Healthy. About {:.0} years left at your pace.", y.min(99.0)), pace),
                        _ => (Tone::Good, format!("Healthy. {}% life left.", h.life_left), pace),
                    };
                }
                if act >= 90.0 {
                    let top = self.app_rows(2).into_iter().next().map_or(String::new(), |r| format!("{} is the busiest app at {}.", r.1, r.2));
                    return (Tone::Warn, format!("Busy: {:.0}% of the time.", act), top);
                }
                (Tone::Info, format!("Active {:.0}% of the time.", act), if self.st().admin { "This drive does not share health data.".to_string() } else { "Health needs admin rights on this drive. Unlock it below.".to_string() })
            }
            Some(Dev::Display(_)) => match self.screen() {
                Some(sc) => {
                    let bits = sc.bits.map_or(String::new(), |b| format!("{b}-bit "));
                    let hdr = if sc.hdr_on { ", HDR on" } else { "" };
                    let head = format!("Sending {bits}{} at {:.0} Hz{hdr}.", if sc.encoding.is_empty() { "video" } else { sc.encoding }, sc.hz);
                    let mh = self.live().measured_hz;
                    // A virtual or remote display reports no real mode (1 Hz): only the measurement counts.
                    if sc.hz < 20.0 {
                        let sub = if mh > 0.0 { format!("Measured {:.0} Hz. Windows reports no fixed refresh for this screen.", mh) } else { "Windows reports no fixed refresh for this screen.".to_string() };
                        return (Tone::Info, format!("{} \u{00D7} {} on a virtual or remote screen.", sc.w, sc.h), sub);
                    }
                    if mh > 0.0 && (mh - sc.hz).abs() > sc.hz * 0.15 {
                        return (Tone::Info, "Refresh rate is following the content.".into(), format!("Measured {:.0} Hz against {:.0} Hz set. Variable refresh is working.", mh, sc.hz));
                    }
                    if sc.hdr_on && sc.bits.is_some_and(|b| b < 10) {
                        return (Tone::Warn, head, "HDR at 8-bit can band. A lower refresh may free 10-bit.".into());
                    }
                    if !sc.encoding.is_empty() && sc.encoding.contains("4:2") {
                        return (Tone::Warn, head, "Colour detail is cut down on the cable. Text may look soft.".into());
                    }
                    (Tone::Good, head, "Nothing is being cut down on the way to the panel.".into())
                }
                None => (Tone::Info, "Reading the display\u{2026}".into(), String::new()),
            },
            Some(Dev::Net) => {
                let p = self.live().ping.as_ref();
                let (gl, nl) = p.map_or((0.0, 0.0), |p| (net::loss(&p.gw[p.gw.len().saturating_sub(20)..]), net::loss(&p.net[p.net.len().saturating_sub(20)..])));
                let ms = p.and_then(|p| p.net.iter().rev().flatten().next().copied());
                match p.map(|p| p.verdict) {
                    Some(net::Verdict::Good) => (Tone::Good, "Connection is clean.".into(), ms.map_or(String::new(), |m| format!("Internet {:.0} ms, no loss in the last 20 seconds.", m))),
                    Some(net::Verdict::IspLoss) => (Tone::Bad, "Router is fine. Your provider is losing packets.".into(), format!("Router loss {:.0}%, internet loss {:.0}%. This is not your Wi-Fi.", gl, nl)),
                    Some(net::Verdict::LocalLoss) => (Tone::Bad, "Your local link is dropping packets.".into(), format!("Router loss {:.0}%. Move closer to the router or try a cable.", gl)),
                    // Data still flows: the network just drops pings (offices, VPNs, cloud PCs).
                    Some(net::Verdict::Offline) if self.snap.net_down + self.snap.net_up > 0.0 && !self.live().conns.is_empty() => {
                        (Tone::Info, "Online. This network blocks ping.".into(), "Data is flowing, so the connection works. Latency and drops can't be measured here.".into())
                    }
                    Some(net::Verdict::Offline) => (Tone::Bad, "No internet.".into(), "Neither the router nor the internet answers.".into()),
                    Some(net::Verdict::NoGateway) => (Tone::Warn, "No router found.".into(), "This PC has no default gateway.".into()),
                    _ => (Tone::Info, "Checking the connection\u{2026}".into(), "Pinging the router and the internet once a second.".into()),
                }
            }
            Some(Dev::Battery) => match self.live().batteries.first() {
                Some(b) => {
                    let h = b.health_pct.map_or(String::new(), |h| format!("Battery health {:.0}%.", h.min(100.0)));
                    let cyc = b.cycles.map_or(String::new(), |c| format!(" after {c} cycles"));
                    let rate = b.rate_mw.map_or(String::new(), |r| if r < 0 { format!(" Draining at {:.1} W.", -r as f32 / 1000.0) } else if r > 0 { format!(" Charging at {:.1} W.", r as f32 / 1000.0) } else { String::new() });
                    let pct = s.battery.as_ref().map_or(0, |b| b.percent);
                    if let Some(l) = battery::limit_hint(pct, b.on_ac, b.charging, b.rate_mw, self.dp.hist.get("pct").map_or(0, |q| q.len() as u32 / 60)) {
                        return (Tone::Info, format!("Charge limit looks active at {l}%."), format!("{h}{rate}"));
                    }
                    let tone = match b.health_pct {
                        Some(x) if x < 60.0 => Tone::Warn,
                        _ => Tone::Good,
                    };
                    (tone, if h.is_empty() { "Battery".into() } else { h }, format!("Normal wear{cyc}.{rate}"))
                }
                None => (Tone::Info, "Reading the battery\u{2026}".into(), String::new()),
            },
            None => (Tone::Info, String::new(), String::new()),
        }
    }

    // ------------------------------------------------------------------ hover

    /// Sample index from the newest (0) under the chart pointer.
    fn hover_back(&self) -> Option<usize> {
        let (x, y) = self.dp.hover?;
        let r = self.chart_plot();
        if !self.dp.chart.contains(x, y) || x < r.l || x > r.r {
            return None;
        }
        let n = self.n();
        let f = (r.r - x) as f32 / r.w().max(1) as f32;
        Some(((f * (n - 1) as f32).round() as usize).min(n - 1))
    }

    /// The app that was busiest at the hovered second (for the chart click).
    pub fn spike_pid(&self) -> Option<u32> {
        let back = self.hover_back()?;
        self.top_at(back).map(|t| t.0)
    }

    fn top_at(&self, back: usize) -> Option<&Top> {
        let i = self.dp.tops.len().checked_sub(1 + back)?;
        let t = self.dp.tops.get(i)?;
        match self.view {
            Some(Dev::Mem) => t.mem.as_ref(),
            Some(Dev::Disk(_)) => t.io.as_ref(),
            Some(Dev::Gpu) => t.gpu.as_ref(),
            _ => t.cpu.as_ref(),
        }
    }

    /// Plot area inside the chart card (right of the value axis).
    fn chart_plot(&self) -> Rect {
        let r = self.dp.chart;
        Rect { l: r.l + self.sc(16 + 56), t: r.t + self.sc(46), r: r.r - self.sc(16), b: r.b - self.sc(36) }
    }

    /// Series of the open page's chart: traces, axis maximum, unit, title.
    fn chart_series(&self, dark: bool) -> (Vec<Ser>, f32, Axis, &'static str) {
        let ser = |label: &str, c: Rgba, data: Vec<f32>, fill: bool| Ser { label: label.into(), c, data, fill };
        let acc = |m: Metric| accent(m, dark);
        let n = self.n();
        match self.view {
            Some(Dev::Cpu) => (
                vec![ser("Total", acc(Metric::Cpu), self.h_cpu.clone(), true), ser("Kernel", acc(Metric::Cpu).mix(Rgba::rgb(128, 128, 128), 0.5), self.kern.iter().copied().collect(), false)],
                100.0,
                Axis::Pct,
                "CPU use",
            ),
            Some(Dev::Mem) => (
                vec![ser("In use", acc(Metric::Ram), self.h_ram.clone(), true), ser("Commit", acc(Metric::Ram).mix(Rgba::rgb(255, 170, 60), 0.5), self.dp.get("commit"), false)],
                100.0,
                Axis::Pct,
                "Memory",
            ),
            Some(Dev::Gpu) => {
                let vram: Vec<f32> = self.h_vram.iter().map(|v| v / self.det.vram_total.max(1) as f32 * 100.0).collect();
                let mut v = vec![ser("GPU", acc(Metric::Gpu), self.h_gpu.clone(), true)];
                if self.det.vram_total > 0 {
                    v.push(ser("Video memory", acc(Metric::Vram), vram, false));
                }
                (v, 100.0, Axis::Pct, "GPU use")
            }
            Some(Dev::Disk(n0)) => {
                let key = format!("Disk {n0}");
                let s = self.disks.iter().find(|s| s.key == key).or_else(|| self.det.disks.iter().position(|x| x.num == Some(n0)).and_then(|i| self.disks.get(i)));
                let a: Vec<f32> = s.map_or(Vec::new(), |s| s.a.iter().copied().collect());
                let b: Vec<f32> = s.map_or(Vec::new(), |s| s.b.iter().copied().collect());
                let peak = a[a.len().saturating_sub(n)..].iter().chain(b[b.len().saturating_sub(n)..].iter()).copied().fold(1024.0 * 1024.0, f32::max);
                (vec![ser("Read", acc(Metric::DiskRead), a, true), ser("Write", acc(Metric::DiskWrite).mix(Rgba::rgb(255, 120, 60), 0.4), b, false)], nice_max(peak), Axis::Rate, "Read and write")
            }
            Some(Dev::Display(_)) => {
                let hz = self.dp.get("hz");
                let set = self.screen().map_or(60.0, |s| s.hz);
                let peak = hz.iter().copied().filter(|v| v.is_finite()).fold(set, f32::max);
                (vec![ser("Measured", accent(Metric::Cpu, dark), hz, true)], nice_max(peak * 1.05), Axis::Hz, "Measured refresh")
            }
            Some(Dev::Net) => {
                let (gw, nt) = self.live().ping.as_ref().map_or((Vec::new(), Vec::new()), |p| {
                    let f = |v: &Vec<Option<f32>>| v.iter().map(|x| x.unwrap_or(f32::NAN)).collect::<Vec<f32>>();
                    (f(&p.gw), f(&p.net))
                });
                let peak = gw.iter().chain(nt.iter()).copied().filter(|v| v.is_finite()).fold(20.0, f32::max);
                (vec![ser("Internet", acc(Metric::Up), nt, true), ser("Router", acc(Metric::Down), gw, false)], nice_max(peak), Axis::Ms, "Ping")
            }
            Some(Dev::Battery) => {
                let w = self.dp.get("watts");
                let peak = w.iter().copied().filter(|v| v.is_finite()).fold(10.0, f32::max);
                (vec![ser("Power", Rgba::rgb(108, 203, 95), w, true)], nice_max(peak), Axis::Watts, "Battery power")
            }
            None | Some(Dev::Bluetooth) => (Vec::new(), 1.0, Axis::Pct, ""),
        }
    }

    // ------------------------------------------------------------------ drawing

    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_dev(&self, cv: &mut Canvas, f: &Fonts, col: &Colors, dy: i32, vt: i32, vb: i32, hot: Option<usize>) {
        let d = Draw { m: self, f, col: *col, dx: 0, dy, hot, track: col.line, sub: col.fg.with_a(if col.dark { 10 } else { 14 }) };
        let vis = |r: Rect| r.b + dy >= vt && r.t + dy <= vb;
        let sh = |r: Rect| Rect { t: r.t + dy, b: r.b + dy, ..r };
        d.dev_tabs(cv);
        if vis(self.dp.verdict) {
            d.verdict_card(cv, sh(self.dp.verdict));
        }
        if self.view == Some(Dev::Bluetooth) {
            d.draw_bt(cv, &vis);
            return;
        }
        if vis(self.dp.chart) {
            d.hero_chart(cv, sh(self.dp.chart));
        }
        for (c, r) in &self.dp.cards {
            if vis(*r) {
                let r = sh(*r);
                d.card(cv, r);
                d.dev_card(cv, *c, r);
            }
        }
    }
}

impl Draw<'_> {
    fn dev_tabs(&self, cv: &mut Canvas) {
        let m = self.m;
        // Back to the overview.
        if let Some(b) = self.hit_rect(Hit::Overview) {
            let hot = self.is_hot(Hit::Overview);
            cv.fill_round(b, b.h() as f32 / 2.0, if hot { self.col.fg.with_a(if self.dark() { 30 } else { 24 }) } else { self.sub });
            let cy = (b.t + b.b) / 2;
            chevron_left(cv, b.l + self.s(14), cy, self.s(8), self.col.fg);
            text_mid(cv, self.f.small, "Overview", b.l + self.s(24), cy, self.col.fg);
        }
        for (dv, _) in &m.dp.tabs {
            let Some(t) = self.hit_rect(Hit::Open(*dv)) else { continue };
            let on = m.view == Some(*dv);
            let hot = self.is_hot(Hit::Open(*dv));
            let bg = if on { self.col.fg.with_a(if self.dark() { 44 } else { 34 }) } else if hot { self.col.fg.with_a(if self.dark() { 24 } else { 18 }) } else { self.col.card };
            cv.fill_round(t, t.h() as f32 / 2.0, bg);
            if !on {
                cv.stroke_round(t, t.h() as f32 / 2.0, self.col.card_border);
            }
            let label = dev_label(m, *dv);
            self.text_c(cv, self.f.small, &label, (t.l + t.r) / 2, (t.t + t.b) / 2, if on || hot { self.col.fg } else { self.col.dim });
        }
        if let Some(b) = self.hit_rect(Hit::RunAdmin(0)) {
            self.admin_button(cv, b, Hit::RunAdmin(0));
        }
        // Bluetooth layout switch (in the range control's place).
        if let (Some(first), Some(last)) = (self.hit_rect(Hit::BtLayout(0)), self.hit_rect(Hit::BtLayout(2))) {
            let r = Rect { l: first.l - self.s(2), t: first.t - self.s(2), r: last.r + self.s(2), b: last.b + self.s(2) };
            cv.fill_round(r, r.h() as f32 / 2.0, self.col.card);
            cv.stroke_round(r, r.h() as f32 / 2.0, self.col.card_border);
            for (i, label) in btpage::BT_LAYOUTS.iter().enumerate() {
                let h = Hit::BtLayout(i as u8);
                let Some(seg) = self.hit_rect(h) else { continue };
                let on = m.bt_layout as usize == i;
                if on {
                    cv.fill_round(seg, seg.h() as f32 / 2.0, self.col.fg.with_a(if self.dark() { 40 } else { 30 }));
                }
                let c = if on || self.is_hot(h) { self.col.fg } else { self.col.dim };
                self.text_c(cv, self.f.small, label, (seg.l + seg.r) / 2, (seg.t + seg.b) / 2, c);
            }
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

    fn verdict_card(&self, cv: &mut Canvas, r: Rect) {
        let (tone, head, sub) = self.m.verdict();
        let c = self.col.tone(tone);
        cv.fill_round(r, self.sf(8.0), c.with_a(if self.dark() { 30 } else { 22 }));
        cv.stroke_round(r, self.sf(8.0), c.with_a(110));
        let x = r.l + self.s(20);
        let dot = self.s(10);
        let cy = r.t + self.s(26);
        cv.fill_round(Rect { l: x, t: cy - dot / 2, r: x + dot, b: cy - dot / 2 + dot }, dot as f32 / 2.0, c);
        let tx = x + dot + self.s(12);
        // Bluetooth page: a button to Windows' own Bluetooth settings on the right.
        let mut r = r;
        for (hit, label) in [(Hit::BtSettings, "Bluetooth settings"), (Hit::BtDebug, "Debug report")] {
            let Some(b) = self.hit_rect(hit) else { continue };
            let hot = self.is_hot(hit);
            cv.fill_round(b, b.h() as f32 / 2.0, if hot { self.col.fg.with_a(if self.dark() { 40 } else { 30 }) } else { self.col.card });
            cv.stroke_round(b, b.h() as f32 / 2.0, self.col.card_border);
            self.text_c(cv, self.f.small, label, (b.l + b.r) / 2, (b.t + b.b) / 2, self.col.fg);
            r.r = r.r.min(b.l - self.s(4));
        }
        let h = cv.ellipsize(self.f.head, &head, r.r - tx - self.s(16));
        let hh = cv.measure(self.f.head, &h).1;
        cv.text(self.f.head, &h, tx, cy - hh / 2, self.col.fg);
        if !sub.is_empty() {
            let st = cv.ellipsize(self.f.small, &sub, r.r - tx - self.s(16));
            cv.text(self.f.small, &st, tx, r.t + self.s(48), self.col.dim);
        }
    }

    fn hero_chart(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        self.card(cv, r);
        let (sers, max, axis, title) = m.chart_series(self.dark());
        let n = m.n();
        cv.text(self.f.semi, title, r.l + self.s(16), r.t + self.s(14), self.col.fg);
        // Legend with min / average / max on the title line.
        let mut lx = r.l + self.s(16) + cv.measure(self.f.semi, title).0 + self.s(20);
        let ly = r.t + self.s(17);
        for s in &sers {
            let val = stats(&s.data, n).map_or("\u{2013}".into(), |(a, b, c)| format!("{} \u{00B7} {} \u{00B7} {}", axis_text(axis, a), axis_text(axis, b), axis_text(axis, c)));
            if lx + self.s(200) > r.r {
                break;
            }
            self.legend(cv, &mut lx, ly, s.c, &s.label, &val);
        }
        cv.text_right(self.f.tiny, "min \u{00B7} avg \u{00B7} max", r.r - self.s(16), ly + self.s(1), self.col.dim);
        let p = m.chart_plot();
        let p = Rect { t: p.t - m.dp.chart.t + r.t, b: p.b - m.dp.chart.t + r.t, ..p };
        // Grid + value axis.
        for k in 0..=4 {
            let y = p.b - (p.h() as f32 * k as f32 / 4.0).round() as i32;
            if k > 0 {
                self.guides(cv, Rect { l: p.l, t: y, r: p.r, b: y + 1 }, &[0.0]);
            } else {
                cv.fill_rect(Rect { l: p.l, t: y, r: p.r, b: y + 1 }, self.track);
            }
            let t = axis_text(axis, max * k as f32 / 4.0);
            let th = cv.measure(self.f.tiny, &t).1;
            cv.text_right(self.f.tiny, &t, p.l - self.s(8), y - th / 2, self.col.dim);
        }
        cv.text(self.f.tiny, range_label(m.range), p.l, p.b + self.s(6), self.col.dim);
        cv.text_right(self.f.tiny, "now", p.r, p.b + self.s(6), self.col.dim);
        // Traces.
        let cols = (p.w() / self.s(2).max(1)).max(2) as usize;
        for s in &sers {
            let have = s.data.len().min(n);
            if have < 2 {
                continue;
            }
            let pts_v = reduce_nan(&s.data, n, cols);
            if pts_v.len() < 2 {
                continue;
            }
            let w = (p.w() as f64 * have as f64 / n as f64).round() as i32;
            let x0 = p.r - w.max(2);
            let k = pts_v.len();
            if s.fill {
                let v: Vec<f32> = pts_v.iter().map(|v| if v.is_finite() { (v / max).clamp(0.0, 1.0) } else { 0.0 }).collect();
                cv.spark(Rect { l: x0, ..p }, &v, s.c);
            }
            // Line, split at gaps.
            let mut run: Vec<(f32, f32)> = Vec::new();
            for (i, v) in pts_v.iter().enumerate() {
                let x = x0 as f32 + (p.r - x0) as f32 * i as f32 / (k - 1) as f32;
                if v.is_finite() {
                    run.push((x, p.b as f32 - p.h() as f32 * (v / max).clamp(0.0, 1.0)));
                } else if !run.is_empty() {
                    let c = s.c;
                    cv.stroke_poly(&run, self.sf(1.6), |_, _| c);
                    run.clear();
                }
            }
            if run.len() >= 2 {
                let c = s.c;
                cv.stroke_poly(&run, self.sf(1.6), |_, _| c);
            }
        }
        if sers.iter().all(|s| s.data.iter().filter(|v| v.is_finite()).count() < 2) {
            self.text_c(cv, self.f.small, "Collecting\u{2026}", (p.l + p.r) / 2, (p.t + p.b) / 2, self.col.dim);
        }
        // Hover: crosshair, values, and the busiest app at that second.
        let Some(back) = m.hover_back() else { return };
        let x = p.r - (p.w() as f32 * back as f32 / (n - 1).max(1) as f32).round() as i32;
        cv.fill_rect(Rect { l: x, t: p.t, r: x + 1, b: p.b }, self.col.fg.with_a(120));
        let mut lines: Vec<(Rgba, String)> = vec![(self.col.dim, if back == 0 { "now".into() } else { format!("{} ago", fmt::duration((back * m.ms / 1000) as u64)) })];
        for s in &sers {
            let i = s.data.len().checked_sub(1 + back);
            if let Some(v) = i.and_then(|i| s.data.get(i)).filter(|v| v.is_finite()) {
                lines.push((s.c, format!("{}  {}", s.label, axis_text(axis, *v))));
                let y = p.b - (p.h() as f32 * (v / max).clamp(0.0, 1.0)).round() as i32;
                cv.fill_disc(x as f32 + 0.5, y as f32, self.sf(3.5), s.c);
            }
        }
        let app = match m.view {
            Some(Dev::Cpu | Dev::Mem | Dev::Gpu | Dev::Disk(_)) => m.top_at(back).map(|t| {
                let v = match m.view {
                    Some(Dev::Mem) => fmt::bytes(t.2 as u64),
                    Some(Dev::Disk(_)) => fmt::rate(t.2),
                    _ => format!("{:.0}%", t.2),
                };
                format!("Busiest: {} {}", t.1, v)
            }),
            _ => None,
        };
        if let Some(a) = &app {
            lines.push((self.col.fg, a.clone()));
            lines.push((self.col.dim, "Click for this app's menu".into()));
        }
        let lh = self.s(18);
        let bw = lines.iter().map(|(_, t)| cv.measure(self.f.small, t).0).max().unwrap_or(0) + self.s(20);
        let bh = lines.len() as i32 * lh + self.s(12);
        let bx = if x + self.s(12) + bw < p.r { x + self.s(12) } else { x - self.s(12) - bw };
        let b = Rect { l: bx, t: p.t + self.s(4), r: bx + bw, b: p.t + self.s(4) + bh };
        cv.fill_round(b, self.sf(6.0), self.col.bubble);
        cv.stroke_round(b, self.sf(6.0), self.col.card_border);
        for (i, (c, t)) in lines.iter().enumerate() {
            cv.text(self.f.small, t, b.l + self.s(10), b.t + self.s(6) + i as i32 * lh, *c);
        }
    }

    /// Card title + subtitle; returns the y below.
    fn card_head(&self, cv: &mut Canvas, r: Rect, title: &str, sub: &str) -> i32 {
        self.header(cv, r, title, sub, 0)
    }

    /// Key/value tiles in up to 3 columns.
    fn facts(&self, cv: &mut Canvas, r: Rect, y: i32, items: &[(&str, String)]) {
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        if items.is_empty() {
            text_mid(cv, self.f.small, "Not reported on this PC", l, y + self.s(14), self.col.dim);
            return;
        }
        let cols = ((rr - l) / self.s(150)).clamp(1, 3);
        self.kv_grid(cv, l, y, rr - l, cols, items);
    }

    fn dev_card(&self, cv: &mut Canvas, c: Card, r: Rect) {
        let m = self.m;
        match c {
            Card::CoreMap => self.core_map(cv, r),
            Card::Chip => {
                let y = self.card_head(cv, r, "Chip", &m.det.cpu_name);
                self.facts(cv, r, y, &m.chip_facts());
            }
            Card::Power => {
                let y = self.card_head(cv, r, "Why it runs at this speed", "");
                self.facts(cv, r, y, &m.power_facts());
            }
            Card::Apps(k) => self.apps_card(cv, r, k),
            Card::Clock => {
                let y = self.card_head(cv, r, "Clock", "average and fastest core");
                let a = m.dp.get("mhz");
                let b = m.dp.get("mhz_max");
                let cr = Rect { l: r.l + self.s(16), t: y, r: r.r - self.s(16), b: r.b - self.s(16) };
                let peak = b.iter().copied().filter(|v| v.is_finite()).fold(m.det.base_mhz.max(1000.0), f32::max) * 1.1;
                self.guides(cv, cr, &[0.25, 0.5, 0.75]);
                self.area(cv, cr, &b, peak, self.acc(Metric::Cpu).with_a(140), false, false);
                self.area(cv, cr, &a, peak, self.acc(Metric::Cpu), false, false);
                if let Some(v) = a.last() {
                    cv.text_right(self.f.cap, &ghz(*v), cr.r, cr.t, self.col.fg);
                }
            }
            Card::MemMap => self.mem_map(cv, r),
            Card::Commit => {
                let y = self.card_head(cv, r, "Commit and paging", "");
                self.facts(cv, r, y, &m.commit_facts());
                let rows = ((r.w() - self.s(32)) / self.s(150)).clamp(1, 3) as usize;
                let cy = y + m.commit_facts().len().div_ceil(rows) as i32 * self.s(56) + self.s(12);
                let cr = Rect { l: r.l + self.s(16), t: cy, r: r.r - self.s(16), b: r.b - self.s(16) };
                self.guides(cv, cr, &[0.5]);
                self.area(cv, cr, &m.dp.get("commit"), 100.0, self.acc(Metric::Ram), false, false);
            }
            Card::Sticks => self.sticks(cv, r),
            Card::VramApps => self.vram_card(cv, r),
            Card::Engines => {
                let y = self.card_head(cv, r, "Engines", "busiest one per type");
                let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
                let base = self.acc(Metric::Gpu);
                let mut y = y;
                for (i, (ty, v)) in m.det.engines.iter().enumerate() {
                    let cy = y + self.s(13);
                    text_mid(cv, self.f.small, &engine_label(ty), l, cy, self.col.fg);
                    let cr = Rect { l: l + self.s(92), t: y + self.s(3), r: rr - self.s(48), b: y + self.s(23) };
                    if let Some(sr) = m.engines.get(i) {
                        self.area(cv, cr, &sr.a.iter().copied().collect::<Vec<_>>(), 100.0, base, false, false);
                    }
                    cv.text_right(self.f.cap, &format!("{:.0}%", v), rr, cy - self.s(8), self.col.fg);
                    y += self.s(26);
                }
            }
            Card::GpuHealth => {
                let name = m.main_gpu().map_or(m.det.gpu_name.clone(), |g| g.name.clone());
                let y = self.card_head(cv, r, "Health", &name);
                self.facts(cv, r, y, &m.gpu_facts());
            }
            Card::Adapters => {
                let mut y = self.card_head(cv, r, "Graphics adapters", "");
                let l = r.l + self.s(16);
                for g in &m.st().gpus {
                    cv.text(self.f.cap, &g.name, l, y, self.col.fg);
                    let kind = if g.integrated { "Built-in" } else { "Dedicated" };
                    let t = format!("{kind} \u{00B7} {} video memory \u{00B7} {} shared", fmt::bytes(g.dedicated), fmt::bytes(g.shared));
                    cv.text(self.f.small, &t, l, y + self.s(18), self.col.dim);
                    y += self.s(44);
                }
            }
            Card::Health => self.health_card(cv, r),
            Card::DriveFacts => {
                let y = self.card_head(cv, r, "Drive", "");
                self.facts(cv, r, y, &m.drive_facts());
            }
            Card::Volumes => {
                let mut y = self.card_head(cv, r, "Volumes", "");
                let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
                let base = self.acc(Metric::DiskFree);
                let vols = m.dev_vols();
                if vols.is_empty() {
                    text_mid(cv, self.f.small, "No drive letters on this disk", l, y + self.s(12), self.col.dim);
                }
                for v in vols {
                    let cy = y + self.s(12);
                    text_mid(cv, self.f.cap, &v.letter, l, cy, self.col.fg);
                    let t = format!("{} free of {}", fmt::bytes(v.free), fmt::bytes(v.total));
                    let tw = cv.measure(self.f.small, &t).0;
                    text_mid(cv, self.f.small, &t, rr - tw, cy, self.col.dim);
                    let used = 1.0 - v.free as f32 / v.total.max(1) as f32;
                    if rr - tw - self.s(12) - (l + self.s(36)) > self.s(40) {
                        self.cells(cv, l + self.s(36), rr - tw - self.s(12), cy, used, self.heat(base, used * 100.0));
                    }
                    y += self.s(28);
                }
            }
            Card::Signal => self.signal_card(cv, r),
            Card::Link => {
                let y = self.card_head(cv, r, "What the cable carries", "");
                self.facts(cv, r, y, &m.link_facts());
            }
            Card::Hdr => self.hdr_card(cv, r),
            Card::Panel => {
                let y = self.card_head(cv, r, "Panel", m.screen().map_or("", |s| s.name.as_str()));
                self.facts(cv, r, y, &m.panel_facts());
            }
            Card::Path => self.path_card(cv, r),
            Card::Outages => self.outage_card(cv, r),
            Card::AdapterFacts => {
                let y = self.card_head(cv, r, "Connection", "");
                self.facts(cv, r, y, &m.net_facts());
            }
            Card::Wifi => {
                let mut y = self.card_head(cv, r, "Wi-Fi", "");
                let w = m.live().wifi.as_ref();
                if w.is_some_and(|w| w.location_blocked) {
                    cv.text(self.f.small, "Windows hides the network name and band until location is on.", r.l + self.s(16), y, self.col.dim);
                    y += self.s(24);
                } else if w.is_none() {
                    cv.text(self.f.small, "Not on Wi-Fi.", r.l + self.s(16), y, self.col.dim);
                    y += self.s(24);
                }
                self.facts(cv, r, y, &m.wifi_facts());
            }
            Card::Conns => self.conns_card(cv, r),
            Card::AppTraffic => self.traffic_card(cv, r),
            Card::Space => self.space_card(cv, r),
            Card::Traffic => {
                let y = self.card_head(cv, r, "Traffic", "all adapters");
                let cr = Rect { l: r.l + self.s(16), t: y, r: r.r - self.s(16), b: r.b - self.s(16) };
                self.mirror(cv, cr, &m.h_down, &m.h_up, self.acc(Metric::Down), self.acc(Metric::Up));
                cv.text_right(self.f.cap, &format!("\u{25BC} {}  \u{25B2} {}", fmt::rate(m.snap.net_down), fmt::rate(m.snap.net_up)), cr.r, cr.t, self.col.fg);
            }
            Card::Capacity => self.capacity_card(cv, r),
            Card::BatLive => {
                let y = self.card_head(cv, r, "Right now", "");
                self.facts(cv, r, y, &m.bat_live());
            }
            Card::Admin => self.admin_card(cv, r),
        }
    }

    fn apps_card(&self, cv: &mut Canvas, r: Rect, k: u8) {
        let title = ["Busiest apps", "Biggest apps", "Busiest apps, all disks", "Busiest apps"][k as usize % 4];
        let y = self.card_head(cv, r, title, "click for its menu");
        let rows = self.m.app_rows(k);
        let c = [self.acc(Metric::Cpu), self.acc(Metric::Ram), self.acc(Metric::DiskRead), self.acc(Metric::Gpu)][k as usize % 4];
        self.bar_rows(cv, r, y, rows.iter().map(|x| (x.0, x.1.clone(), x.2.clone(), x.3, c)).collect());
    }

    /// Rows of name, bar, value; hover highlights (hit = Proc(pid)).
    fn bar_rows(&self, cv: &mut Canvas, r: Rect, y: i32, rows: Vec<(u32, String, String, f32, Rgba)>) {
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        if rows.is_empty() {
            text_mid(cv, self.f.small, "Nothing busy right now", l, y + self.s(14), self.col.dim);
            return;
        }
        let nw = ((rr - l) as f32 * 0.42) as i32;
        let widest = rows.iter().map(|x| cv.measure(self.f.cap, &x.2).0).max().unwrap_or(0);
        let vw = (widest + self.s(4)).clamp(self.s(76), (rr - l) / 2);
        for (i, (pid, name, val, frac, c)) in rows.into_iter().enumerate() {
            let t = y + i as i32 * self.s(30);
            let cy = t + self.s(15);
            if self.is_hot(Hit::Proc(pid)) {
                cv.fill_round(Rect { l: r.l + self.s(8), t, r: r.r - self.s(8), b: t + self.s(30) }, self.sf(5.0), self.sub);
            }
            let ac = avatar_col(&name);
            cv.fill_round(Rect { l, t: cy - self.s(8), r: l + self.s(16), b: cy + self.s(8) }, self.sf(4.0), ac);
            let initial: String = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            self.text_c(cv, self.f.tiny, &initial, l + self.s(8), cy, Rgba::rgb(255, 255, 255));
            let nm = cv.ellipsize(self.f.small, &name, nw - self.s(24));
            text_mid(cv, self.f.small, &nm, l + self.s(24), cy, self.col.fg);
            let br = Rect { l: l + nw, t: cy - self.s(4), r: rr - vw - self.s(8), b: cy + self.s(4) };
            if br.w() > self.s(20) {
                self.hbar(cv, br, frac, c);
            }
            let vwid = cv.measure(self.f.cap, &val).0;
            text_mid(cv, self.f.cap, &val, rr - vwid, cy, self.col.fg);
        }
    }

    fn core_map(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let topo = m.st().topo.as_ref();
        let sub = match topo {
            Some(t) if t.hybrid => "performance and efficiency cores",
            Some(t) if t.vcache_group.is_some() => "cache groups",
            _ => "every logical processor",
        };
        let y = self.card_head(cv, r, "Core map", sub);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let n = m.det.core_pct.len();
        if n == 0 {
            text_mid(cv, self.f.small, "Collecting\u{2026}", l, y + self.s(14), self.col.dim);
            return;
        }
        let cols = ((rr - l) / self.s(58)).clamp(1, 16);
        let g = self.s(6);
        let tw = (rr - l - g * (cols - 1)) / cols;
        let th = self.s(52);
        let base = self.acc(Metric::Cpu);
        let eff = self.acc(Metric::Ram);
        let lv = m.live();
        for (i, v) in m.det.core_pct.iter().enumerate() {
            let (cx, cy) = (i as i32 % cols, i as i32 / cols);
            let t = Rect { l: l + cx * (tw + g), t: y + cy * (th + g), r: l + cx * (tw + g) + tw, b: y + cy * (th + g) + th };
            let kind = topo.map(|tp| crate::sensors::deep::cpu::core_kind(tp, i as u32));
            let e = matches!(kind, Some(crate::sensors::deep::cpu::Kind::Efficiency));
            let parked = lv.parked.contains(&(i as u32));
            cv.fill_round(t, self.sf(5.0), self.sub);
            let c = if e { eff } else { base };
            if !parked {
                let fh = (th as f32 * v / 100.0).round() as i32;
                if fh > 0 {
                    cv.fill_round(Rect { t: t.b - fh.max(2), ..t }, self.sf(4.0), self.heat(c, *v).with_a(200));
                }
            }
            cv.text(self.f.tiny, &i.to_string(), t.l + self.s(5), t.t + self.s(3), self.col.dim);
            let lab = if parked {
                "zz".to_string()
            } else {
                lv.core_mhz.get(i).filter(|f| **f > 0.0).map_or(format!("{:.0}%", v), |f| format!("{:.1}", f / 1000.0))
            };
            self.text_c(cv, self.f.cap, &lab, (t.l + t.r) / 2, t.b - self.s(14), if parked { self.col.dim } else { self.col.fg });
            if e {
                cv.text_right(self.f.tiny, "E", t.r - self.s(5), t.t + self.s(3), eff);
            }
        }
        let rows = (n as i32 + cols - 1) / cols;
        let ly = y + rows * (th + g) + self.s(4);
        let mut lx = l;
        let hybrid = topo.is_some_and(|t| t.hybrid);
        self.legend(cv, &mut lx, ly, base, if hybrid { "Performance" } else { "Core" }, "");
        if hybrid {
            self.legend(cv, &mut lx, ly, eff, "Efficiency", "");
        }
        let tail = if lv.core_mhz.is_empty() { "% busy" } else { "GHz now \u{00B7} dim = parked" };
        cv.text(self.f.tiny, tail, lx, ly + self.s(1), self.col.dim);
    }

    fn mem_map(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let s = &m.snap;
        let d = &m.det;
        let y = self.card_head(cv, r, "Memory map", &fmt::bytes(s.ram_total));
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let base = self.acc(Metric::Ram);
        let modified = d.modified.unwrap_or(0).min(s.ram_used);
        let in_use = s.ram_used.saturating_sub(modified);
        let standby = d.standby.unwrap_or(0);
        let free = d.free.unwrap_or_else(|| s.ram_total.saturating_sub(s.ram_used + standby));
        let parts = [
            (in_use, base, "In use", "apps and Windows"),
            (modified, base.mix(self.col.warn, 0.45), "Modified", "waiting to be written to disk"),
            (standby, base.with_a(if self.dark() { 110 } else { 120 }), "Standby", "cache, handed back on demand"),
            (free, self.track, "Free", "not used at all"),
        ];
        let br = Rect { l, t: y, r: rr, b: y + self.s(36) };
        cv.fill_round(br, self.sf(6.0), self.track);
        let total = s.ram_total.max(1) as f64;
        let mut x = br.l as f64;
        let prev = cv.clip();
        cv.set_clip(Some(clip_and(prev, br)));
        for (v, c, _, _) in parts.iter().take(3) {
            let w = br.w() as f64 * *v as f64 / total;
            let x1 = (x + w).round() as i32;
            if x1 > x.round() as i32 {
                cv.fill_round(Rect { l: x.round() as i32, r: (x1 - self.s(1)).max(x.round() as i32 + 1), ..br }, self.sf(2.0), *c);
            }
            x += w;
        }
        cv.set_clip(prev);
        let mut ty = br.b + self.s(18);
        for (v, c, label, what) in parts {
            let cy = ty + self.s(8);
            cv.fill_round(Rect { l, t: cy - self.s(5), r: l + self.s(10), b: cy + self.s(5) }, self.sf(2.0), c);
            text_mid(cv, self.f.cap, label, l + self.s(18), cy, self.col.fg);
            text_mid(cv, self.f.small, &fmt::bytes(v), l + self.s(110), cy, self.col.fg);
            if rr - (l + self.s(190)) > self.s(80) {
                let t = cv.ellipsize(self.f.small, what, rr - l - self.s(190));
                text_mid(cv, self.f.small, &t, l + self.s(190), cy, self.col.dim);
            }
            ty += self.s(24);
        }
    }

    fn sticks(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let mut y = self.card_head(cv, r, "Memory sticks", "");
        if let Some((cur, rated)) = m.xmp_note() {
            let b = Rect { l: r.l + self.s(16), t: y, r: r.r - self.s(16), b: y + self.s(32) };
            cv.fill_round(b, self.sf(6.0), self.col.warn.with_a(if self.dark() { 40 } else { 30 }));
            let t = format!("Running {cur} MT/s, kit rated {rated}. The fast profile (XMP / EXPO) looks off.");
            let t = cv.ellipsize(self.f.small, &t, b.w() - self.s(20));
            text_mid(cv, self.f.small, &t, b.l + self.s(10), (b.t + b.b) / 2, self.col.fg);
            y += self.s(40);
        }
        self.dimms(cv, r.l + self.s(16), r.r - self.s(16), y);
    }

    fn vram_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let d = &m.det;
        let y = self.card_head(cv, r, "Who is using video memory", &format!("{} card", fmt::bytes(d.vram_total)));
        let rows = m.vram_rows();
        let max = rows.first().map_or(1, |x| x.2.max(1)) as f32;
        let c = self.acc(Metric::Vram);
        if rows.is_empty() && m.live().gpu_mem.is_empty() {
            text_mid(cv, self.f.small, "Collecting\u{2026}", r.l + self.s(16), y + self.s(14), self.col.dim);
        } else {
            self.bar_rows(cv, r, y, rows.iter().map(|x| (x.0, x.1.clone(), fmt::bytes(x.2), x.2 as f32 / max, c)).collect());
        }
        // Dedicated vs shared at the bottom.
        let by = r.b - self.s(16) - self.s(24);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let used = m.snap.vram_used.unwrap_or(0);
        let t = format!("Dedicated {} of {} \u{00B7} shared {}", fmt::bytes(used), fmt::bytes(d.vram_total), fmt::bytes(d.vram_shared.unwrap_or(0)));
        cv.text(self.f.small, &t, l, by, self.col.dim);
        self.hbar(cv, Rect { l, t: by + self.s(18), r: rr, b: by + self.s(24) }, used as f32 / d.vram_total.max(1) as f32, c);
    }

    fn health_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let Some(Dev::Disk(n)) = m.view else { return };
        let x = m.st().disk(n);
        let h = x.and_then(|x| x.health.as_ref());
        let y = self.card_head(cv, r, "Health", h.map_or("", |h| h.source));
        let l = r.l + self.s(16);
        let Some(h) = h else {
            let msg = if m.st().disks.is_empty() { "Reading the drive\u{2026}" } else { if m.st().admin { "This drive does not share health data (virtual, RAID or USB bridge)." } else { "This drive does not report health without admin rights." } };
            text_mid(cv, self.f.small, msg, l, y + self.s(14), self.col.dim);
            return;
        };
        let (cx, cy) = ((l + self.s(56)) as f32, (y + self.s(56)) as f32);
        let life = f32::from(h.life_left) / 100.0;
        let c = if h.life_left < 10 { self.col.crit } else if h.life_left < 30 { self.col.warn } else { self.col.good };
        self.ring(cv, cx, cy, self.sf(50.0), self.sf(10.0), life, c);
        self.text_c(cv, self.f.head, &format!("{}%", h.life_left), cx as i32, cy as i32 - self.s(6), self.col.fg);
        self.text_c(cv, self.f.tiny, "life left", cx as i32, cy as i32 + self.s(14), self.col.dim);
        let days = (h.power_on_hours as f64 / 24.0).max(1.0);
        let daily = h.written_bytes as f64 / days;
        let mut items: Vec<(&str, String)> = Vec::new();
        if let Some(t) = h.temp_c {
            items.push(("TEMPERATURE", format!("{t} \u{00B0}C")));
        }
        items.push(("WRITTEN", fmt::bytes(h.written_bytes.min(u128::from(u64::MAX)) as u64)));
        items.push(("PER DAY", fmt::bytes(daily as u64)));
        items.push(("POWERED ON", format!("{} h", group(h.power_on_hours))));
        items.push(("SPARE", format!("{}%", h.spare)));
        items.push(("ERRORS", h.media_errors.to_string()));
        if let Some(y2) = crate::sensors::deep::disk::forecast(h, daily) {
            items.push(("YEARS LEFT", format!("about {:.0}", y2.min(99.0))));
        }
        items.push(("UNSAFE OFFS", h.unsafe_shutdowns.to_string()));
        let kx = l + self.s(124);
        let cols = ((r.r - self.s(16) - kx) / self.s(130)).clamp(1, 3);
        let shown = (cols * 2) as usize;
        self.kv_grid(cv, kx, y, r.r - self.s(16) - kx, cols, &items[..items.len().min(shown)]);
    }

    fn signal_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let y = self.card_head(cv, r, "Signal chain", "");
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let sc = m.screen();
        let gpu = sc.and_then(|s| m.st().gpus.iter().find(|g| g.luid == s.adapter_luid)).map_or(m.det.gpu_name.clone(), |g| g.name.clone());
        let conn = sc.map_or(String::new(), |s| s.connector.to_string());
        let panel = sc.map_or(String::new(), |s| s.edid.as_ref().filter(|e| !e.model.is_empty()).map_or(s.name.clone(), |e| e.model.clone()));
        let boxes = [("GPU", gpu), ("CABLE", conn), ("PANEL", panel)];
        let g = self.s(22);
        let bw = (rr - l - g * 2) / 3;
        for (i, (k, v)) in boxes.iter().enumerate() {
            let x = l + i as i32 * (bw + g);
            let b = Rect { l: x, t: y, r: x + bw, b: y + self.s(44) };
            cv.fill_round(b, self.sf(6.0), self.sub);
            self.cap(cv, k, b.l + self.s(10), b.t + self.s(6));
            let t = cv.ellipsize(self.f.cap, v, bw - self.s(20));
            cv.text(self.f.cap, &t, b.l + self.s(10), b.t + self.s(22), self.col.fg);
            if i < 2 {
                let ax = b.r + self.s(4);
                let cy = (b.t + b.b) / 2;
                cv.fill_rect(Rect { l: ax, t: cy, r: ax + g - self.s(8), b: cy + self.s(2) }, self.col.accent);
                chevron(cv, ax + g - self.s(10), cy + 1, self.s(5), false, self.col.accent);
            }
        }
    }

    fn hdr_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let y = self.card_head(cv, r, "Brightness and colour", "");
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let Some(s) = m.screen() else { return };
        let mut items: Vec<(&str, String)> = Vec::new();
        if let Some(n) = s.max_nits.filter(|n| *n > 0.0) {
            items.push(("PEAK", format!("{:.0} nits", n)));
        }
        if let Some(n) = s.max_full_frame_nits.filter(|n| *n > 0.0) {
            items.push(("FULL SCREEN", format!("{:.0} nits", n)));
        }
        if let Some(n) = s.min_nits.filter(|n| *n > 0.0) {
            items.push(("BLACK", format!("{:.3} nits", n)));
        }
        // Gamut triangle against sRGB, if the panel reports its primaries.
        let tri = self.s(110);
        let tw = if s.primaries.is_some() && rr - l > self.s(320) { tri + self.s(16) } else { 0 };
        if let Some(p) = s.primaries {
            let (srgb, p3) = display::gamut_cover(&p);
            items.push(("SRGB", format!("{:.0}%", srgb)));
            items.push(("DCI-P3", format!("{:.0}%", p3)));
            if tw > 0 {
                let b = Rect { l: rr - tri, t: y, r: rr, b: y + tri - self.s(10) };
                let map = |(x, yy): (f32, f32)| (b.l as f32 + x / 0.8 * b.w() as f32, b.b as f32 - yy / 0.9 * b.h() as f32);
                let srgb_p = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06), (0.64, 0.33)];
                let pts: Vec<(f32, f32)> = srgb_p.iter().map(|q| map(*q)).collect();
                let dim = self.col.dim;
                cv.stroke_poly(&pts, self.sf(1.0), |_, _| dim);
                let pts: Vec<(f32, f32)> = [p[0], p[1], p[2], p[0]].iter().map(|q| map(*q)).collect();
                let acc = self.col.accent;
                cv.stroke_poly(&pts, self.sf(2.0), |_, _| acc);
                cv.text(self.f.tiny, "panel vs sRGB", b.l, b.b + self.s(2), self.col.dim);
            }
        }
        if items.is_empty() {
            text_mid(cv, self.f.small, "The panel does not report brightness or colour data", l, y + self.s(14), self.col.dim);
            return;
        }
        let w = rr - l - tw;
        let cols = (w / self.s(120)).clamp(1, 2);
        self.kv_grid(cv, l, y, w, cols, &items);
    }

    fn path_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let y = self.card_head(cv, r, "Path check", "pinged once a second");
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let p = m.live().ping.as_ref();
        let (gl, nl) = p.map_or((None, None), |p| {
            let n = |v: &Vec<Option<f32>>| (!v.is_empty()).then(|| net::loss(&v[v.len().saturating_sub(20)..]));
            (n(&p.gw), n(&p.net))
        });
        let state = |l: Option<f32>| match l {
            None => self.col.off,
            Some(x) if x >= 0.5 => self.col.crit,
            Some(x) if x >= 0.1 => self.col.warn,
            _ => self.col.good,
        };
        let hops = [("This PC", self.col.good), ("Router", state(gl)), ("Internet", state(nl))];
        let g = self.s(22);
        let bw = (rr - l - g * 2) / 3;
        for (i, (k, c)) in hops.iter().enumerate() {
            let x = l + i as i32 * (bw + g);
            let b = Rect { l: x, t: y, r: x + bw, b: y + self.s(40) };
            cv.fill_round(b, self.sf(6.0), self.sub);
            let cy = (b.t + b.b) / 2;
            cv.fill_disc((b.l + self.s(14)) as f32, cy as f32, self.sf(5.0), *c);
            let t = cv.ellipsize(self.f.cap, k, bw - self.s(34));
            text_mid(cv, self.f.cap, &t, b.l + self.s(26), cy, self.col.fg);
            if i < 2 {
                let link = if i == 0 { hops[1].1 } else { hops[2].1 };
                cv.fill_rect(Rect { l: b.r + self.s(3), t: cy - self.s(1), r: b.r + g - self.s(3), b: cy + self.s(1) }, link);
            }
        }
        let mut ty = y + self.s(56);
        let last = |v: &Vec<Option<f32>>| v.iter().rev().flatten().next().copied();
        let rows: Vec<(&str, String)> = match p {
            Some(p) => vec![
                ("Router", format!("{} \u{00B7} {} \u{00B7} loss {:.0}%", p.gateway, last(&p.gw).map_or("\u{2013}".into(), |v| format!("{:.0} ms", v)), gl.unwrap_or(0.0))),
                ("Internet", format!("{} \u{00B7} {} \u{00B7} loss {:.0}%", p.target, last(&p.net).map_or("\u{2013}".into(), |v| format!("{:.0} ms", v)), nl.unwrap_or(0.0))),
                ("Jitter", format!("{:.1} ms", net::jitter(&p.net[p.net.len().saturating_sub(30)..]))),
            ],
            None => vec![("Status", "Starting\u{2026}".into())],
        };
        for (k, v) in rows {
            text_mid(cv, self.f.small, k, l, ty + self.s(10), self.col.dim);
            let t = cv.ellipsize(self.f.small, &v, rr - l - self.s(90));
            text_mid(cv, self.f.small, &t, l + self.s(90), ty + self.s(10), self.col.fg);
            ty += self.s(22);
        }
    }

    fn outage_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let list = m.outages();
        let y = self.card_head(cv, r, "Drops", &format!("{} since the page opened", list.len()));
        if let Some(b) = self.hit_rect(Hit::CopyOutages) {
            let hot = self.is_hot(Hit::CopyOutages);
            cv.fill_round(b, b.h() as f32 / 2.0, if hot { self.col.accent.with_a(70) } else { self.sub });
            self.text_c(cv, self.f.small, "Copy for my ISP", (b.l + b.r) / 2, (b.t + b.b) / 2, if list.is_empty() { self.col.dim } else { self.col.fg });
        }
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        if list.is_empty() {
            text_mid(cv, self.f.small, "No drops. Leave this page open to log them.", l, y + self.s(12), self.col.dim);
            return;
        }
        let mut ty = y;
        for o in list.iter().rev().take(8) {
            let when = clock_time(o.start);
            let cause = match o.cause {
                net::Verdict::LocalLoss => "local link",
                net::Verdict::IspLoss => "provider",
                net::Verdict::Offline => "offline",
                net::Verdict::NoGateway => "no router",
                _ => "drop",
            };
            let c = if matches!(o.cause, net::Verdict::IspLoss) { self.col.warn } else { self.col.crit };
            cv.fill_disc((l + self.s(5)) as f32, (ty + self.s(11)) as f32, self.sf(4.0), c);
            text_mid(cv, self.f.small, &when, l + self.s(16), ty + self.s(11), self.col.fg);
            let t = format!("{} \u{00B7} {}", fmt::duration(u64::from(o.secs.max(1))), cause);
            let t = cv.ellipsize(self.f.small, &t, rr - l - self.s(100));
            text_mid(cv, self.f.small, &t, l + self.s(100), ty + self.s(11), self.col.dim);
            ty += self.s(26);
        }
    }

    fn conns_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let rows = m.conn_rows();
        let total: usize = rows.iter().map(|x| x.2).sum();
        let y = self.card_head(cv, r, "Apps talking to the internet", &format!("{total} open connections"));
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        if rows.is_empty() {
            text_mid(cv, self.f.small, "Collecting\u{2026}", l, y + self.s(14), self.col.dim);
            return;
        }
        for (i, (pid, name, n, hosts)) in rows.iter().enumerate() {
            let t = y + i as i32 * self.s(30);
            let cy = t + self.s(15);
            if self.is_hot(Hit::Proc(*pid)) {
                cv.fill_round(Rect { l: r.l + self.s(8), t, r: r.r - self.s(8), b: t + self.s(30) }, self.sf(5.0), self.sub);
            }
            let nm = cv.ellipsize(self.f.small, name, (rr - l) / 3);
            text_mid(cv, self.f.small, &nm, l, cy, self.col.fg);
            let ct = format!("{n}");
            let cw = cv.measure(self.f.cap, &ct).0;
            text_mid(cv, self.f.cap, &ct, rr - cw, cy, self.col.fg);
            let hx = l + (rr - l) / 3 + self.s(10);
            let h = cv.ellipsize(self.f.small, hosts, rr - cw - self.s(12) - hx);
            text_mid(cv, self.f.small, &h, hx, cy, self.col.dim);
        }
    }

    fn space_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let dp = &m.dp;
        let prog = dp.scan.as_ref().map(space::Scan::progress);
        let sub = match (&dp.scan_tree, &prog) {
            (Some(t), p) => format!("{} \u{00B7} {} files{}", fmt::bytes(t.total()), t.total_files(), if p.as_ref().is_some_and(|p| p.fast) { "" } else { " \u{00B7} folder walk" }),
            (None, Some(p)) if !p.done => format!("scanning\u{2026} {} files, {}", p.files, fmt::bytes(p.bytes)),
            _ => String::new(),
        };
        let y = self.card_head(cv, r, "What fills this drive", &sub);
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let busy = prog.as_ref().is_some_and(|p| !p.done);
        if let Some(b) = self.hit_rect(Hit::Scan).filter(|_| !busy) {
            let hot = self.is_hot(Hit::Scan);
            cv.fill_round(b, self.sf(5.0), if hot { self.col.fg.with_a(40) } else { self.sub });
            self.text_c(cv, self.f.cap, if dp.scan_tree.is_some() { "Scan again" } else { "Scan" }, (b.l + b.r) / 2, (b.t + b.b) / 2, self.col.fg);
        }
        let note = |cv: &mut Canvas, t: &str| {
            let t = cv.ellipsize(self.f.small, t, rr - l);
            cv.text(self.f.small, &t, l, y, self.col.dim);
        };
        let Some(tree) = &dp.scan_tree else {
            match prog {
                Some(p) if p.done => note(cv, p.error.as_deref().unwrap_or("The scan found nothing.")),
                Some(_) => note(cv, "Reading the drive. Other pages stay live meanwhile."),
                None if m.st().admin => note(cv, "Find the biggest folders. Takes a few seconds."),
                None => {
                    note(cv, "Find the biggest folders. Much faster when the app runs as administrator.");
                    if let Some(b) = self.hit_rect(Hit::RunAdmin(1)) {
                        self.admin_button(cv, b, Hit::RunAdmin(1));
                    }
                }
            }
            return;
        };
        // Where we are; click to go up.
        let here = if dp.scan_dir == 0 { dp.scan_root.clone() } else { tree.path(dp.scan_dir) };
        let cy = y + self.s(14);
        let mut x = l;
        if let Some(b) = self.hit_rect(Hit::FolderUp) {
            if self.is_hot(Hit::FolderUp) {
                cv.fill_round(b, self.sf(5.0), self.sub);
            }
            chevron_left(cv, l + self.s(6), cy, self.s(8), self.col.fg);
            x += self.s(18);
        }
        let ht = cv.ellipsize(self.f.small, &here, rr - x);
        text_mid(cv, self.f.small, &ht, x, cy, self.col.fg);
        let top = dp.scan_rows.first().map_or(1, |e| e.bytes).max(1) as f32;
        let c = self.acc(Metric::DiskRead);
        let nw = ((rr - l) as f32 * 0.42) as i32;
        for (i, e) in dp.scan_rows.iter().enumerate() {
            let t = y + self.s(34) + i as i32 * self.s(30);
            let cy = t + self.s(15);
            if !e.is_files_here && self.is_hot(Hit::Folder(e.id)) {
                cv.fill_round(Rect { l: r.l + self.s(8), t, r: r.r - self.s(8), b: t + self.s(30) }, self.sf(5.0), self.sub);
            }
            let name = if e.is_files_here { "Files in this folder" } else { e.name.as_str() };
            let nm = cv.ellipsize(self.f.small, name, nw - self.s(8));
            text_mid(cv, self.f.small, &nm, l, cy, if e.is_files_here { self.col.dim } else { self.col.fg });
            let val = fmt::bytes(e.bytes);
            let vw = self.s(76);
            let br = Rect { l: l + nw, t: cy - self.s(4), r: rr - vw - self.s(8), b: cy + self.s(4) };
            if br.w() > self.s(20) {
                self.hbar(cv, br, e.bytes as f32 / top, c);
            }
            let vwid = cv.measure(self.f.cap, &val).0;
            text_mid(cv, self.f.cap, &val, rr - vwid, cy, self.col.fg);
        }
    }

    /// Accent button that restarts the app elevated, with a small shield.
    fn admin_button(&self, cv: &mut Canvas, b: Rect, h: Hit) {
        let hot = self.is_hot(h);
        cv.fill_round(b, b.h() as f32 / 2.0, if hot { self.col.accent.mix(self.col.fg, 0.15) } else { self.col.accent });
        let white = Rgba::rgb(255, 255, 255);
        // Shield: a rounded block narrowing to a point.
        let (sx, cy) = (b.l + self.s(14), (b.t + b.b) / 2);
        let (hw, top, mid, bot) = (self.s(5), cy - self.s(6), cy + self.s(1), cy + self.s(7));
        cv.fill_round(Rect { l: sx - hw, t: top, r: sx + hw, b: mid }, self.sf(1.5), white);
        for k in 0..(bot - mid) {
            let w = hw - hw * k / (bot - mid).max(1);
            cv.fill_rect(Rect { l: sx - w, t: mid + k, r: sx + w, b: mid + k + 1 }, white);
        }
        text_mid(cv, self.f.cap, "Run as administrator", b.l + self.s(26), cy, white);
    }

    fn traffic_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let st = m.live().app_net_state;
        let y = self.card_head(cv, r, "Traffic by app", if st == AppNet::On { "click for its menu" } else { "" });
        let l = r.l + self.s(16);
        let note = |cv: &mut Canvas, t: &str| {
            let t = cv.ellipsize(self.f.small, t, r.r - l - self.s(16));
            cv.text(self.f.small, &t, l, y, self.col.dim);
        };
        match st {
            AppNet::On => {
                let c = self.acc(Metric::Down);
                let rows = m.traffic_rows();
                if rows.is_empty() {
                    note(cv, "No app is using the network right now.");
                } else {
                    self.bar_rows(cv, r, y, rows.into_iter().map(|x| (x.0, x.1, x.2, x.3, c)).collect());
                }
            }
            AppNet::NeedsAdmin => {
                note(cv, "Windows shares bytes per app only with an administrator.");
                if let Some(b) = self.hit_rect(Hit::Elevate) {
                    let hot = self.is_hot(Hit::Elevate);
                    cv.fill_round(b, self.sf(5.0), if hot { self.col.accent.mix(self.col.fg, 0.15) } else { self.col.accent });
                    self.text_c(cv, self.f.cap, "Restart as administrator", (b.l + b.r) / 2, (b.t + b.b) / 2, Rgba::rgb(255, 255, 255));
                }
            }
            AppNet::Failed => note(cv, "Windows would not start the traffic counter."),
            AppNet::Starting | AppNet::Off => note(cv, "Starting the traffic counter\u{2026}"),
        }
    }

    fn capacity_card(&self, cv: &mut Canvas, r: Rect) {
        let m = self.m;
        let y = self.card_head(cv, r, "Capacity", "");
        let (l, rr) = (r.l + self.s(16), r.r - self.s(16));
        let Some(b) = m.live().batteries.first() else {
            text_mid(cv, self.f.small, "Reading the battery\u{2026}", l, y + self.s(14), self.col.dim);
            return;
        };
        let frac = if b.design_mwh > 0 { b.full_mwh as f32 / b.design_mwh as f32 } else { 0.0 };
        let br = Rect { l, t: y, r: rr, b: y + self.s(30) };
        let c = if frac < 0.6 { self.col.warn } else { self.col.good };
        cv.meter(br, frac, c, self.track);
        let t = format!("{:.0}% of new", frac * 100.0);
        text_mid(cv, self.f.cap, &t, br.l + self.s(10), (br.t + br.b) / 2, self.col.fg);
        self.facts(cv, r, y + self.s(40), &m.bat_facts());
    }

    fn admin_card(&self, cv: &mut Canvas, r: Rect) {
        let y = self.card_head(cv, r, "More with admin rights", "");
        let l = r.l + self.s(16);
        let t = cv.ellipsize(self.f.small, "SATA and USB drives only share health with an administrator.", r.r - l - self.s(16));
        cv.text(self.f.small, &t, l, y, self.col.dim);
        if let Some(b) = self.hit_rect(Hit::Elevate) {
            let hot = self.is_hot(Hit::Elevate);
            cv.fill_round(b, self.sf(5.0), if hot { self.col.accent.mix(self.col.fg, 0.15) } else { self.col.accent });
            self.text_c(cv, self.f.cap, "Restart as administrator", (b.l + b.r) / 2, (b.t + b.b) / 2, Rgba::rgb(255, 255, 255));
        }
    }
}

/// Average `n` newest samples into `cols` points (NaN stays NaN when a bucket has no data).
fn reduce_nan(h: &[f32], n: usize, cols: usize) -> Vec<f32> {
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
            let (sum, cnt) = data[lo..hi].iter().filter(|v| v.is_finite()).fold((0.0f32, 0u32), |a, v| (a.0 + v, a.1 + 1));
            if cnt == 0 {
                f32::NAN
            } else {
                sum / cnt as f32
            }
        })
        .collect()
}

fn chevron_left(cv: &mut Canvas, cx: i32, cy: i32, h: i32, c: Rgba) {
    let half = h / 2;
    for k in 0..=half {
        cv.fill_rect(Rect { l: cx - half + k, t: cy - k, r: cx - half + k + 2, b: cy - k + 1 }, c);
        cv.fill_rect(Rect { l: cx - half + k, t: cy + k, r: cx - half + k + 2, b: cy + k + 1 }, c);
    }
}

/// The app runs elevated (checked once).
fn is_admin() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(crate::sensors::deep::reader::elevated)
}

/// Local time minus UTC, seconds (DST included).
fn local_offset_secs() -> i64 {
    use windows_sys::Win32::System::SystemInformation::GetSystemTime;
    let l = crate::util::local_time();
    // SAFETY: plain out struct.
    let mut u: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetSystemTime(&mut u) };
    let secs = |t: &windows_sys::Win32::Foundation::SYSTEMTIME| i64::from(t.wDay) * 86_400 + i64::from(t.wHour) * 3600 + i64::from(t.wMinute) * 60;
    let mut d = secs(&l) - secs(&u);
    // Across a month boundary the day numbers jump: fold into +-14 h.
    if d > 14 * 3600 {
        d -= ((d + 12 * 3600) / 86_400) * 86_400;
    } else if d < -14 * 3600 {
        d += ((-d + 12 * 3600) / 86_400) * 86_400;
    }
    // Offsets are whole quarter hours.
    (d as f64 / 900.0).round() as i64 * 900
}

/// Local clock time of a unix timestamp, "14:05".
fn clock_time(unix: u64) -> String {
    let local = local_offset_secs();
    let t = (unix as i64 + local).rem_euclid(86_400);
    format!("{:02}:{:02}", t / 3600, t / 60 % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kit_speeds_from_part_numbers() {
        assert_eq!(kit_speed("F5-6000J3038F16G"), Some(6000));
        assert_eq!(kit_speed("CMK32GX5M2B6000C36"), Some(6000));
        assert_eq!(kit_speed("KF560C36-16"), Some(6000));
        assert_eq!(kit_speed("KF432C16BB/8"), Some(3200));
        assert_eq!(kit_speed("CP16G4DFRA32A"), Some(3200));
        assert_eq!(kit_speed("M425R1GB4BB0-CQKOL"), None);
    }

    #[test]
    fn nice_axis_maximum() {
        assert_eq!(nice_max(73.0), 100.0);
        assert_eq!(nice_max(180.0), 200.0);
        assert_eq!(nice_max(2.2), 2.5);
        assert_eq!(nice_max(0.0), 1.0);
    }

    #[test]
    fn stats_skip_gaps() {
        let d = [1.0, f32::NAN, 3.0, 5.0];
        assert_eq!(stats(&d, 4), Some((1.0, 3.0, 5.0)));
        assert_eq!(stats(&[f32::NAN], 4), None);
    }

    #[test]
    fn reduce_keeps_gaps() {
        let d = [1.0, 1.0, f32::NAN, f32::NAN];
        let r = reduce_nan(&d, 4, 2);
        assert_eq!(r[0], 1.0);
        assert!(r[1].is_nan());
    }
}
