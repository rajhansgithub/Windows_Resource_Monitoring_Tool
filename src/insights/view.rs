//! Insights panel view: lays out and paints one `Panel` (headline, tone pill,
//! facts, chart, item cards) into a column of the main window's content area,
//! plus the pure helpers shared with the Overview page (tone labels, glyphs,
//! fingerprint, demo data).
//!
//! The view owns no window: the host passes fonts, colours, the column's
//! left / right edges and the scroll offset, and asks it for hit-tests.
//! Layout runs only when the data or the window size changes; painting reads
//! the laid-out blocks.
//!
//! `RESMON_INSIGHTS_DEMO=1` in the environment shows built-in demo panels
//! (one per feature, with cards and charts) so the layout can be checked
//! before real data exists.

use super::{time, Chart, Panel, Tone};
use crate::render::{measure_dc, Canvas, Rgba};
use crate::util::Rect;
use std::sync::OnceLock;
use windows_sys::Win32::Graphics::Gdi::{HDC, HFONT};

const CARD_PAD: i32 = 16;
const CHART_H: i32 = 180;

/// `RESMON_INSIGHTS_DEMO=1`: show the built-in demo panels.
pub fn demo() -> bool {
    static D: OnceLock<bool> = OnceLock::new();
    *D.get_or_init(|| std::env::var("RESMON_INSIGHTS_DEMO").is_ok_and(|v| v.trim() == "1"))
}

/// Panels to show now and whether Insights is on.
pub fn fetch(a: &crate::app::App, now: u64) -> (bool, Vec<Panel>) {
    if demo() {
        (true, demo_panels(now))
    } else if a.ins.enabled() {
        (true, a.ins.panels(now))
    } else {
        (false, Vec::new())
    }
}

// ------------------------------------------------------------------ style

/// Font handles owned by the host window.
#[derive(Clone, Copy)]
pub struct Fonts {
    pub body: HFONT,
    pub small: HFONT,
    pub tiny: HFONT,
    pub semi: HFONT,
    pub head: HFONT,
}

/// Colours the view paints with (the host's palette).
#[derive(Clone, Copy)]
pub struct Colors {
    pub dark: bool,
    pub card: Rgba,
    pub card_border: Rgba,
    /// Opaque fill for the chart value bubble.
    pub bubble: Rgba,
    pub fg: Rgba,
    pub dim: Rgba,
    pub off: Rgba,
    pub line: Rgba,
    pub good: Rgba,
    pub learning: Rgba,
    pub accent: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
}

impl Colors {
    pub fn tone(&self, t: Tone) -> Rgba {
        match t {
            Tone::Good => self.good,
            Tone::Info => self.accent,
            Tone::Warn => self.warn,
            Tone::Bad => self.crit,
            Tone::Learning => self.learning,
            Tone::Unavailable => self.off,
        }
    }
}

pub fn tone_label(t: Tone) -> &'static str {
    match t {
        Tone::Good => "All good",
        Tone::Info => "Note",
        Tone::Warn => "Worth a look",
        Tone::Bad => "Problem",
        Tone::Learning => "Learning",
        Tone::Unavailable => "Not available",
    }
}

fn items_heading(key: &str) -> &'static str {
    match key {
        "incidents" => "Recent incidents",
        "sleep" => "Recent nights",
        "heldback" => "Recent limits",
        "baseline" => "Changes",
        "ghost" => "By day",
        _ => "History",
    }
}

fn empty_text(key: &str) -> &'static str {
    match key {
        "incidents" => "No incidents yet. Spikes you miss will show up here.",
        "sleep" => "No sleep recorded yet. Check back after the PC sleeps.",
        "heldback" => "No limits seen yet.",
        "baseline" => "Nothing yet. This needs a few days of idle time.",
        "ghost" => "Nothing yet. Check back tomorrow.",
        _ => "Nothing here yet.",
    }
}

// ------------------------------------------------------------------ pure helpers

/// Greedy word wrap to `max_w` using `measure` (width of a string). A word
/// wider than the line is put on its own line (it is ellipsized when drawn).
pub fn wrap(measure: &dyn Fn(&str) -> i32, s: &str, max_w: i32) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
                continue;
            }
            let cand = format!("{} {}", line, word);
            if measure(&cand) <= max_w {
                line = cand;
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

/// Time shown on a card: "14:02" today, "12 Nov" on other days, nothing for 0.
fn when(t_ms: u64, now_ms: u64) -> String {
    if t_ms == 0 {
        String::new()
    } else if time::day(t_ms) == time::day(now_ms) {
        time::hhmm(t_ms)
    } else {
        time::day_label(time::day(t_ms))
    }
}

/// Top of the Y axis: the chart's own max, else a round number above the data.
fn chart_max(c: &Chart) -> f32 {
    if c.max > 0.0 {
        return c.max;
    }
    let hi = c.points.iter().copied().filter(|v| v.is_finite()).fold(0.0f32, f32::max);
    if hi <= 0.0 {
        return 1.0;
    }
    let mag = 10f32.powf(hi.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if hi <= m * mag + 1e-6 {
            return m * mag;
        }
    }
    10.0 * mag
}

/// "97%", "2.5 GB", "12 W": a value with its unit, rounded for reading.
fn fmt_val(v: f32, unit: &str) -> String {
    let n = if v.abs() >= 10.0 || (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{:.1}", v)
    };
    match unit {
        "" => n,
        "%" => format!("{}%", n),
        u => format!("{} {}", n, u),
    }
}

/// Slots on the x axis and the slot of `points[0]` (points fill the newest slots).
fn axis(c: &Chart) -> (usize, usize) {
    let n = if c.bars { c.points.len() } else { c.slots.max(c.points.len()) };
    (n, n - c.points.len())
}

/// Index of the slot under column `dx` (0..w) of a plot `w` wide.
fn index_at(dx: i32, w: i32, n: usize, bars: bool) -> Option<usize> {
    if n == 0 || w <= 0 || dx < 0 || dx >= w {
        return None;
    }
    let i = if bars || n == 1 {
        (dx as i64 * n as i64 / w as i64) as usize
    } else {
        ((dx as f32 * (n - 1) as f32 / (w - 1).max(1) as f32).round()) as usize
    };
    Some(i.min(n - 1))
}

/// X offset (0..w) of slot `i` (centre of its bar for bar charts; a single
/// point sits in the middle).
fn x_of(i: usize, w: i32, n: usize, bars: bool) -> f32 {
    if bars || n <= 1 {
        (i as f32 + 0.5) * w as f32 / n.max(1) as f32
    } else {
        i as f32 * (w - 1) as f32 / (n - 1) as f32
    }
}

/// Fingerprint of everything visible, to repaint only when it changed.
pub fn signature(enabled: bool, panels: &[Panel]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    enabled.hash(&mut h);
    for p in panels {
        p.key.hash(&mut h);
        (p.tone as u8).hash(&mut h);
        p.headline.hash(&mut h);
        p.lines.hash(&mut h);
        p.items.len().hash(&mut h);
        if let Some(it) = p.items.first() {
            it.t_ms.hash(&mut h);
            it.title.hash(&mut h);
            it.detail.hash(&mut h);
        }
        if let Some(c) = &p.chart {
            c.label.hash(&mut h);
            c.points.len().hash(&mut h);
            c.slots.hash(&mut h);
            c.bars.hash(&mut h);
            c.max.to_bits().hash(&mut h);
            for v in &c.points {
                // Sub-pixel changes are invisible: round before hashing.
                if v.is_finite() {
                    ((v * 10.0).round() as i64).hash(&mut h);
                } else {
                    i64::MIN.hash(&mut h);
                }
            }
            c.markers.hash(&mut h);
        }
    }
    h.finish()
}

/// Built-in realistic panels for `RESMON_INSIGHTS_DEMO=1`.
pub fn demo_panels(now: u64) -> Vec<Panel> {
    use super::Item;
    const MIN: u64 = 60_000;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    let it = |ago: u64, tone, title: &str, detail: &str| Item {
        t_ms: now.saturating_sub(ago),
        tone,
        title: title.into(),
        detail: detail.into(),
    };
    // Deterministic wiggle so the charts look like real samples.
    let noise = |i: usize| (((i * 7919) % 97) as f32 / 97.0 - 0.5) * 2.0;

    // The app was started 40 minutes ago: the hour is only partly filled.
    let cpu: Vec<f32> = (0..40)
        .map(|i| {
            let base = 12.0 + 6.0 * noise(i);
            if (14..18).contains(&i) {
                95.0 + 2.0 * noise(i)
            } else if (30..32).contains(&i) {
                70.0 + 8.0 * noise(i)
            } else {
                base
            }
        })
        .collect();
    let battery: Vec<f32> = (0..49)
        .map(|i| {
            let awake = [(12usize, 16usize), (30, 35)];
            let extra: f32 = awake.iter().map(|(a, b)| (i.clamp(*a, *b) - a) as f32 * 1.1).sum();
            92.0 - i as f32 * 0.12 - extra
        })
        .collect();
    let limit: Vec<f32> = (0..300).map(|i| if i < 170 { 100.0 } else { 62.0 + 3.0 * noise(i) }).collect();
    let idle: Vec<f32> = (0..21).map(|i| if i < 12 { 2.0 + 0.4 * noise(i) } else { 6.0 + 0.6 * noise(i) }).collect();
    let ghost: Vec<f32> = (0..14).map(|i| 3.6 + i as f32 * 0.09 + if i >= 9 { 0.4 } else { 0.0 }).collect();

    vec![
        Panel {
            key: "incidents",
            title: "Incident Cards",
            tone: Tone::Warn,
            headline: "3 spikes today. Windows Defender caused 2 of them.".into(),
            lines: vec![
                "Longest: CPU held at 97% for 3 min at 14:02.".into(),
                "Watching CPU, disk and memory. Nothing leaves this PC.".into(),
            ],
            chart: Some(Chart {
                label: "CPU, last hour".into(),
                points: cpu,
                max: 100.0,
                unit: "%",
                markers: vec![(14, "Scan".into()), (30, "Index".into())],
                bars: false,
                slots: 60,
            }),
            items: vec![
                it(25 * MIN, Tone::Bad, "CPU held at 97% for 3 min.", "MsMpEng 38%, disk 100%. A Defender scan."),
                it(78 * MIN, Tone::Warn, "Disk busy at 100% for 6 min.", "SearchIndexer wrote 1.2 GB."),
                it(3 * HOUR, Tone::Warn, "Display driver reset.", "nvlddmkm stopped and recovered. Apps may have flickered."),
                it(DAY + 2 * HOUR, Tone::Info, "Memory at 92% for 4 min.", "msedge 3.1 GB, Teams 1.4 GB."),
            ],
        },
        Panel {
            key: "sleep",
            title: "Sleep Verdict",
            tone: Tone::Bad,
            headline: "Lost 14% overnight. Awake 22% of the time.".into(),
            lines: vec!["Asleep 8 h 10 min, on battery.".into(), "Top culprit: Microsoft Teams woke it 31 times.".into()],
            chart: Some(Chart {
                label: "Battery during sleep".into(),
                points: battery,
                max: 100.0,
                unit: "%",
                markers: vec![(12, "Wake".into()), (30, "Wake".into())],
                bars: false,
                slots: 0,
            }),
            items: vec![
                it(2 * HOUR, Tone::Bad, "Last night: lost 14%.", "Awake 22%. Teams, then Windows Update."),
                it(DAY + 2 * HOUR, Tone::Good, "Lost 3%.", "Awake 2%. Normal."),
                it(2 * DAY + 2 * HOUR, Tone::Good, "Lost 2%.", "Awake 1%. Normal."),
                it(3 * DAY + 2 * HOUR, Tone::Warn, "Lost 9%.", "Awake 12%. The network card kept waking it."),
            ],
        },
        Panel {
            key: "heldback",
            title: "Held Back",
            tone: Tone::Warn,
            headline: "Capped at 62%. Firmware thermal limit.".into(),
            lines: vec!["Max speed now 2.1 GHz of 3.4 GHz.".into(), "On battery. Power mode: Balanced.".into()],
            chart: Some(Chart {
                label: "CPU ceiling, last hour".into(),
                points: limit,
                max: 100.0,
                unit: "%",
                markers: vec![(170, "Hot".into())],
                bars: false,
                slots: 360,
            }),
            items: vec![
                it(26 * MIN, Tone::Warn, "Thermal limit started.", "Speed fell to 62% after 4 min of heavy load."),
                it(5 * HOUR, Tone::Info, "Battery saver on.", "Speed limited to 80% until you plug in."),
            ],
        },
        Panel {
            key: "baseline",
            title: "Before / After",
            tone: Tone::Info,
            headline: "Idle CPU up 4 points since the update on 12 Nov.".into(),
            lines: vec!["Idle CPU 2% before, 6% after.".into(), "Idle memory 3.1 GB before, 3.4 GB after.".into()],
            chart: Some(Chart {
                label: "Idle CPU per day".into(),
                points: idle,
                max: 0.0,
                unit: "%",
                markers: vec![(12, "Update".into())],
                bars: true,
                slots: 0,
            }),
            items: vec![
                it(9 * DAY, Tone::Warn, "Windows update KB5046633.", "Idle CPU rose from 2% to 6% after it."),
                it(15 * DAY, Tone::Good, "Graphics driver 31.0.101.5590.", "No change in idle use."),
            ],
        },
        Panel {
            key: "ghost",
            title: "Ghost Memory",
            tone: Tone::Warn,
            headline: "4.8 GB of memory has no owner. Up 1.2 GB in a week.".into(),
            lines: vec![
                "Kernel pool 2.9 GB. Driver locked 1.1 GB.".into(),
                "A restart frees it. If it keeps growing, update your drivers.".into(),
            ],
            chart: Some(Chart {
                label: "Memory no app owns, per day".into(),
                points: ghost,
                max: 0.0,
                unit: "GB",
                markers: vec![(9, "Driver".into())],
                bars: true,
                slots: 0,
            }),
            items: vec![
                it(3 * HOUR, Tone::Warn, "4.8 GB today.", "Highest in 14 days."),
                it(DAY, Tone::Info, "4.6 GB.", "Up 0.4 GB after the network driver update."),
                it(6 * DAY, Tone::Good, "3.6 GB.", "Normal for this PC."),
            ],
        },
    ]
}

// ------------------------------------------------------------------ drawing helpers

fn shift(r: Rect, dy: i32) -> Rect {
    Rect { t: r.t + dy, b: r.b + dy, ..r }
}

/// One pixel column from `y0` to `y1` with anti-aliased ends.
fn vspan(cv: &mut Canvas, x: i32, y0: f32, y1: f32, c: Rgba) {
    let mut y = y0.floor() as i32;
    while (y as f32) < y1 {
        let cov = ((y + 1) as f32).min(y1) - (y as f32).max(y0);
        if cov > 0.0 {
            cv.fill_rect(Rect { l: x, t: y, r: x + 1, b: y + 1 }, c.with_a((c.3 as f32 * cov.min(1.0)).round() as u8));
        }
        y += 1;
    }
}

/// Tone pill ("● Worth a look") with its top-left at (x, y). Returns its width.
pub fn draw_pill(cv: &mut Canvas, f: &Fonts, c: &Colors, dpi: u32, x: i32, y: i32, tone: Tone) -> i32 {
    let sc = |v: i32| (v * dpi as i32 + 48) / 96;
    let col = c.tone(tone);
    let label = tone_label(tone);
    let (tw, th) = cv.measure(f.small, label);
    let h = sc(24);
    let dot = sc(8);
    let w = sc(10) + dot + sc(6) + tw + sc(12);
    let r = Rect { l: x, t: y, r: x + w, b: y + h };
    cv.fill_round(r, h as f32 / 2.0, col.with_a(if c.dark { 0x2E } else { 0x26 }));
    cv.stroke_round(r, h as f32 / 2.0, col.with_a(0x55));
    let cy = y + h / 2;
    let dl = x + sc(10);
    cv.fill_round(Rect { l: dl, t: cy - dot / 2, r: dl + dot, b: cy - dot / 2 + dot }, dot as f32 / 2.0, col);
    cv.text(f.small, label, dl + dot + sc(6), cy - th / 2, c.fg);
    w
}

// ------------------------------------------------------------------ view

#[derive(Clone, Copy, PartialEq)]
enum F {
    Head,
    Body,
    Semi,
}

/// Laid-out content of a panel (content coordinates).
enum Block {
    Text { x: i32, y: i32, font: F, dim: bool, s: String },
    Pill { x: i32, y: i32, tone: Tone },
    Chart { rect: Rect },
    Item { rect: Rect, tone: Tone, time: String, title: String, detail: Vec<String>, tx: i32 },
}

/// One laid-out panel.
pub struct View {
    blocks: Vec<Block>,
    dpi: u32,
    /// Right edge of the column.
    r: i32,
    /// Plot area of the chart (content coordinates).
    plot: Option<Rect>,
}

impl View {
    pub fn new() -> View {
        View { blocks: Vec::new(), dpi: 96, r: 0, plot: None }
    }

    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.plot = None;
    }

    /// Lay out `p` in the column `l..r` from content y `y0`. Returns the bottom.
    #[allow(clippy::too_many_arguments)]
    pub fn layout(&mut self, p: &Panel, mdc: HDC, f: &Fonts, dpi: u32, l: i32, r: i32, y0: i32, now_ms: u64) -> i32 {
        self.dpi = dpi;
        self.r = r;
        self.plot = None;
        let mut blocks = Vec::new();
        let w = r - l;
        let lh = |font: HFONT| measure_dc(mdc, font, "Ag").1;
        let m = |font: HFONT| move |s: &str| measure_dc(mdc, font, s).0;
        let mut y = y0;
        // Headline, wrapped.
        let hh = lh(f.head);
        for s in wrap(&m(f.head), &p.headline, w - self.sc(4)) {
            blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Head, dim: false, s });
            y += hh + self.sc(2);
        }
        y += self.sc(8);
        blocks.push(Block::Pill { x: l + self.sc(2), y, tone: p.tone });
        y += self.sc(24) + self.sc(12);
        let bh = lh(f.body);
        for line in &p.lines {
            for s in wrap(&m(f.body), line, w - self.sc(4)) {
                blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Body, dim: true, s });
                y += bh + self.sc(2);
            }
        }
        if p.chart.as_ref().is_some_and(|c| c.points.iter().any(|v| v.is_finite())) {
            y += self.sc(16);
            let rect = Rect { l, t: y, r, b: y + self.sc(CHART_H) };
            let pad = self.sc(CARD_PAD);
            let top = rect.t + pad + lh(f.semi) + self.sc(10);
            self.plot = Some(Rect { l: rect.l + pad, t: top, r: rect.r - pad, b: rect.b - pad });
            blocks.push(Block::Chart { rect });
            y += self.sc(CHART_H);
        }
        // Cards.
        y += self.sc(24);
        let sh = lh(f.semi);
        blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Semi, dim: false, s: items_heading(p.key).into() });
        y += sh + self.sc(10);
        if p.items.is_empty() {
            blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Body, dim: true, s: empty_text(p.key).into() });
            y += bh + self.sc(8);
        } else {
            let times: Vec<String> = p.items.iter().map(|it| when(it.t_ms, now_ms)).collect();
            let time_w = times.iter().map(|t| measure_dc(mdc, f.small, t).0).max().unwrap_or(0);
            let pad = self.sc(CARD_PAD);
            let tx = l + self.sc(24) + if time_w > 0 { time_w + self.sc(16) } else { 0 };
            let text_w = (r - pad - tx).max(self.sc(80));
            let smh = lh(f.small);
            for (it, time) in p.items.iter().zip(times) {
                let detail = wrap(&m(f.small), &it.detail, text_w);
                let h = pad * 2 - self.sc(4) + sh + detail.len() as i32 * smh + if detail.is_empty() { 0 } else { self.sc(2) };
                let rect = Rect { l, t: y, r, b: y + h.max(self.sc(48)) };
                y = rect.b + self.sc(4);
                blocks.push(Block::Item { rect, tone: it.tone, time, title: it.title.clone(), detail, tx });
            }
        }
        self.blocks = blocks;
        y
    }

    /// The chart plot contains content point (x, y).
    pub fn over_chart(&self, x: i32, y: i32) -> bool {
        self.plot.is_some_and(|r| r.contains(x, y))
    }

    /// Paint with content y shifted by `dy` (client = content + dy); `vt..vb`
    /// is the visible band. `hover_x` = client x over the chart plot.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, cv: &mut Canvas, p: &Panel, f: &Fonts, c: &Colors, dy: i32, vt: i32, vb: i32, hover_x: Option<i32>) {
        let rad = self.sc(6) as f32;
        let lh = |cv: &Canvas, font: HFONT| cv.measure(font, "Ag").1;
        for b in &self.blocks {
            match b {
                Block::Text { x, y, font, dim, s } => {
                    let y = y + dy;
                    if y > vb || y + self.sc(40) < vt {
                        continue;
                    }
                    let fh = match font {
                        F::Head => f.head,
                        F::Body => f.body,
                        F::Semi => f.semi,
                    };
                    let s = cv.ellipsize(fh, s, self.r - x);
                    cv.text(fh, &s, *x, y, if *dim { c.dim } else { c.fg });
                }
                Block::Pill { x, y, tone } => {
                    draw_pill(cv, f, c, self.dpi, *x, y + dy, *tone);
                }
                Block::Chart { rect } => {
                    let r = shift(*rect, dy);
                    if r.b < vt || r.t > vb {
                        continue;
                    }
                    if let (Some(ch), Some(plot)) = (p.chart.as_ref(), self.plot) {
                        self.draw_chart(cv, f, c, r, shift(plot, dy), ch, hover_x);
                    }
                }
                Block::Item { rect, tone, time, title, detail, tx } => {
                    let r = shift(*rect, dy);
                    if r.b < vt || r.t > vb {
                        continue;
                    }
                    cv.fill_round(r, rad, c.card);
                    cv.stroke_round(r, rad, c.card_border);
                    let pad = self.sc(CARD_PAD);
                    let stripe = Rect { l: r.l + self.sc(8), t: r.t + self.sc(10), r: r.l + self.sc(11), b: r.b - self.sc(10) };
                    cv.fill_round(stripe, stripe.w() as f32 / 2.0, c.tone(*tone));
                    let ty = r.t + pad - self.sc(2);
                    let sh = lh(cv, f.semi);
                    if !time.is_empty() {
                        let th = lh(cv, f.small);
                        cv.text(f.small, time, r.l + self.sc(24), ty + (sh - th) / 2 + self.sc(1), c.dim);
                    }
                    let maxw = r.r - pad - tx;
                    let t = cv.ellipsize(f.semi, title, maxw);
                    cv.text(f.semi, &t, *tx, ty, c.fg);
                    let mut yy = ty + sh + self.sc(2);
                    let smh = lh(cv, f.small);
                    for d in detail {
                        let d = cv.ellipsize(f.small, d, maxw);
                        cv.text(f.small, &d, *tx, yy, c.dim);
                        yy += smh;
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chart(&self, cv: &mut Canvas, f: &Fonts, p: &Colors, card: Rect, pr: Rect, c: &Chart, hover_x: Option<i32>) {
        let rad = self.sc(6) as f32;
        cv.fill_round(card, rad, p.card);
        cv.stroke_round(card, rad, p.card_border);
        let pad = self.sc(CARD_PAD);
        let max = chart_max(c);
        let max_s = fmt_val(max, c.unit);
        let mw = cv.measure(f.small, &max_s).0;
        let label = cv.ellipsize(f.semi, &c.label, card.w() - pad * 3 - mw);
        cv.text(f.semi, &label, card.l + pad, card.t + pad - self.sc(2), p.fg);
        let (w, h) = (pr.w(), pr.h());
        if w <= 2 || h <= 2 {
            return;
        }
        // Axis: dashed top line labelled with the max, solid baseline.
        cv.text_right(f.small, &max_s, card.r - pad, card.t + pad, p.dim);
        let mut x = pr.l;
        while x < pr.r {
            cv.fill_rect(Rect { l: x, t: pr.t, r: (x + self.sc(3)).min(pr.r), b: pr.t + 1 }, p.line);
            x += self.sc(6);
        }
        cv.fill_rect(Rect { l: pr.l, t: pr.b - 1, r: pr.r, b: pr.b }, p.line.mix(p.dim, 0.3));

        let bars = c.bars;
        let (n, off) = axis(c);
        if n == 0 {
            return;
        }
        // Value of slot `i`; None = no data (before the series starts, or a gap).
        let val = |i: usize| -> Option<f32> {
            i.checked_sub(off).and_then(|j| c.points.get(j)).copied().filter(|v| v.is_finite()).map(|v| v.max(0.0))
        };
        let y_of = |v: f32| pr.b as f32 - 1.0 - (v / max).clamp(0.0, 1.0) * (h - 2) as f32;
        let hover_i = hover_x.and_then(|hx| index_at(hx - pr.l, w, n, bars)).filter(|i| val(*i).is_some());
        let acc = p.accent;
        let fill_a = if p.dark { 0x50 } else { 0x40 } as f32;

        if bars {
            let slot = w as f32 / n as f32;
            // Few points: keep bars slim instead of filling the whole card.
            let bw = (slot * 0.62).max(self.sc(2) as f32).min(slot).min(self.sc(28) as f32);
            for i in 0..n {
                let Some(v) = val(i) else { continue };
                let cx = pr.l as f32 + x_of(i, w, n, true);
                let top = y_of(v).min(pr.b as f32 - 2.0);
                let (bl, brr) = ((cx - bw / 2.0).round() as i32, (cx + bw / 2.0).round() as i32);
                let br = Rect { l: bl, t: top.round() as i32, r: brr, b: pr.b - 1 };
                let hot = hover_i == Some(i);
                cv.fill_round(br, self.sc(2) as f32, if hot { acc } else { acc.with_a(fill_a as u8 + 0x30) });
                cv.fill_rect(Rect { b: br.t + self.sc(2), ..br }, acc);
            }
        } else {
            let th = (self.sc(2) as f32).max(1.5);
            if n > 1 {
                let mut prev: Option<f32> = None;
                for col in 0..w {
                    let pos = col as f32 * (n - 1) as f32 / (w - 1) as f32;
                    let (ia, ib) = (pos.floor() as usize, (pos.ceil() as usize).min(n - 1));
                    let (Some(a), Some(b)) = (val(ia), val(ib)) else {
                        prev = None;
                        continue;
                    };
                    let yv = y_of(a + (b - a) * pos.fract());
                    let x = pr.l + col;
                    // Soft fill fading toward the baseline.
                    let span = (pr.b - 1) as f32 - yv;
                    let mut yy = yv.ceil() as i32;
                    while yy < pr.b - 1 {
                        let t = if span > 0.0 { (yy as f32 - yv) / span } else { 1.0 };
                        let al = fill_a * (1.0 - 0.8 * t);
                        cv.fill_rect(Rect { l: x, t: yy, r: x + 1, b: yy + 1 }, acc.with_a(al as u8));
                        yy += 1;
                    }
                    let py = prev.unwrap_or(yv);
                    vspan(cv, x, py.min(yv) - th / 2.0, py.max(yv) + th / 2.0, acc);
                    prev = Some(yv);
                }
            }
            // A point with no neighbour on either side has no line: draw it as a dot.
            let d = self.sc(6);
            for i in 0..n {
                let Some(v) = val(i) else { continue };
                let lone = (i == 0 || val(i - 1).is_none()) && (i + 1 >= n || val(i + 1).is_none());
                if lone {
                    let cx = pr.l + x_of(i, w, n, false).round() as i32;
                    let cy = y_of(v).round() as i32;
                    cv.fill_round(Rect { l: cx - d / 2, t: cy - d / 2, r: cx - d / 2 + d, b: cy - d / 2 + d }, d as f32 / 2.0, acc);
                }
            }
        }

        // Markers: dashed vertical line and a tiny label, skipped if it would overlap.
        let mut last_r = i32::MIN;
        for (idx, lab) in &c.markers {
            let slot = idx + off;
            if slot >= n {
                continue;
            }
            let mx = pr.l + x_of(slot, w, n, bars).round() as i32;
            let mut yy = pr.t;
            while yy < pr.b {
                cv.fill_rect(Rect { l: mx, t: yy, r: mx + 1, b: (yy + self.sc(3)).min(pr.b) }, p.dim.with_a(0x90));
                yy += self.sc(6);
            }
            let (lw, _) = cv.measure(f.tiny, lab);
            let lx = if mx + self.sc(4) + lw > pr.r { mx - self.sc(4) - lw } else { mx + self.sc(4) };
            if lx > last_r + self.sc(4) {
                cv.text(f.tiny, lab, lx, pr.t + self.sc(3), p.dim);
                last_r = lx + lw;
            }
        }

        // Hover: guide line, dot and value bubble.
        if let Some(i) = hover_i {
            let hx = pr.l + x_of(i, w, n, bars).round() as i32;
            let v = val(i).unwrap_or(0.0);
            let hy = y_of(v).round() as i32;
            if !bars {
                cv.fill_rect(Rect { l: hx, t: pr.t, r: hx + 1, b: pr.b }, p.dim.with_a(0xA0));
                let d = self.sc(8);
                cv.fill_round(Rect { l: hx - d / 2, t: hy - d / 2, r: hx - d / 2 + d, b: hy - d / 2 + d }, d as f32 / 2.0, acc);
            }
            let mut s = fmt_val(v, c.unit);
            if let Some((_, lab)) = c.markers.iter().find(|(m, _)| m + off == i) {
                s = format!("{}  \u{b7}  {}", s, lab);
            }
            let (tw, tht) = cv.measure(f.small, &s);
            let (bw, bh) = (tw + self.sc(16), tht + self.sc(8));
            let mut bl = hx + self.sc(10);
            if bl + bw > pr.r {
                bl = hx - self.sc(10) - bw;
            }
            let bt = (hy - bh - self.sc(8)).max(pr.t + self.sc(2));
            let br = Rect { l: bl, t: bt, r: bl + bw, b: bt + bh };
            cv.fill_round(br, self.sc(4) as f32, p.bubble);
            cv.stroke_round(br, self.sc(4) as f32, p.card_border.with_a(0x40));
            cv.text(f.small, &s, bl + self.sc(8), bt + self.sc(4), p.fg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> i32 {
        s.chars().count() as i32
    }

    #[test]
    fn wrap_breaks_on_words() {
        let v = wrap(&chars, "Capped at 62%. Firmware thermal limit.", 16);
        assert_eq!(v, vec!["Capped at 62%.", "Firmware thermal", "limit."]);
        assert!(v.iter().all(|l| chars(l) <= 16));
        assert_eq!(wrap(&chars, "short", 40), vec!["short"]);
        assert!(wrap(&chars, "   ", 10).is_empty());
        // A word longer than the line stays whole on its own line.
        assert_eq!(wrap(&chars, "a verylongwordhere b", 6), vec!["a", "verylongwordhere", "b"]);
    }

    #[test]
    fn card_time_label() {
        let now = 1_764_000_000_000;
        assert_eq!(when(0, now), "");
        assert_eq!(when(now, now), time::hhmm(now));
        let old = now - 3 * 86_400_000;
        assert_eq!(when(old, now), time::day_label(time::day(old)));
    }

    #[test]
    fn axis_max() {
        let c = |pts: Vec<f32>, max| Chart { points: pts, max, ..Default::default() };
        assert_eq!(chart_max(&c(vec![1.0, 2.0], 100.0)), 100.0);
        assert_eq!(chart_max(&c(vec![3.2, 4.8], 0.0)), 5.0);
        assert_eq!(chart_max(&c(vec![6.4], 0.0)), 10.0);
        assert_eq!(chart_max(&c(vec![0.0, 0.0], 0.0)), 1.0);
        assert_eq!(chart_max(&c(vec![], 0.0)), 1.0);
        assert_eq!(chart_max(&c(vec![180.0], 0.0)), 200.0);
        assert_eq!(chart_max(&c(vec![f32::NAN, 2.2], 0.0)), 2.5);
    }

    #[test]
    fn value_format() {
        assert_eq!(fmt_val(97.4, "%"), "97%");
        assert_eq!(fmt_val(2.46, "GB"), "2.5 GB");
        assert_eq!(fmt_val(3.0, "GB"), "3 GB");
        assert_eq!(fmt_val(12.6, ""), "13");
    }

    #[test]
    fn chart_x_mapping_round_trips() {
        for &(n, bars) in &[(120usize, false), (21, true), (1, true), (1, false), (2, false), (60, false)] {
            let w = 600;
            for i in 0..n {
                let x = x_of(i, w, n, bars).round() as i32;
                assert_eq!(index_at(x.min(w - 1), w, n, bars), Some(i), "n={} i={}", n, i);
            }
        }
        assert_eq!(index_at(-1, 600, 10, true), None);
        assert_eq!(index_at(600, 600, 10, true), None);
        assert_eq!(index_at(5, 600, 0, true), None);
    }

    #[test]
    fn fixed_window_keeps_full_axis() {
        // 3 of 60 minutes filled: still 60 slots, data at the right end.
        let c = Chart { points: vec![1.0, 2.0, 3.0], slots: 60, ..Default::default() };
        assert_eq!(axis(&c), (60, 57));
        // Bars ignore `slots`; a plain series is as long as its points.
        let b = Chart { points: vec![1.0, 2.0], slots: 60, bars: true, ..Default::default() };
        assert_eq!(axis(&b), (2, 0));
        let s = Chart { points: vec![1.0; 5], ..Default::default() };
        assert_eq!(axis(&s), (5, 0));
        // More points than slots never underflows.
        let o = Chart { points: vec![1.0; 70], slots: 60, ..Default::default() };
        assert_eq!(axis(&o), (70, 0));
    }

    #[test]
    fn single_point_sits_in_the_middle() {
        assert_eq!(x_of(0, 600, 1, false), 300.0);
        assert_eq!(x_of(0, 600, 1, true), 300.0);
    }

    #[test]
    fn signature_tracks_visible_changes() {
        let a = demo_panels(1_764_000_000_000);
        let s = signature(true, &a);
        assert_eq!(s, signature(true, &a.clone()));
        assert_ne!(s, signature(false, &a));
        let mut b = a.clone();
        b[2].headline.push('!');
        assert_ne!(s, signature(true, &b));
        let mut b = a.clone();
        b[0].items.pop();
        assert_ne!(s, signature(true, &b));
        let mut b = a.clone();
        b[1].chart.as_mut().unwrap().points.push(50.0);
        assert_ne!(s, signature(true, &b));
        let mut b = a.clone();
        b[0].chart.as_mut().unwrap().points[0] = f32::NAN;
        assert_ne!(s, signature(true, &b));
    }

    #[test]
    fn demo_has_every_feature() {
        let p = demo_panels(1_764_000_000_000);
        let keys: Vec<_> = p.iter().map(|x| x.key).collect();
        assert_eq!(keys, ["incidents", "sleep", "heldback", "baseline", "ghost"]);
        for x in &p {
            assert!(!x.items.is_empty());
            let c = x.chart.as_ref().unwrap();
            assert!(!c.points.is_empty());
            assert!(c.markers.iter().all(|(i, _)| *i < c.points.len()));
            assert!(!x.headline.contains('\u{2014}'));
        }
        // Time series are lines; per-day values are bars.
        assert!(!p[0].chart.as_ref().unwrap().bars);
        assert!(p[3].chart.as_ref().unwrap().bars);
        assert!(p[4].chart.as_ref().unwrap().bars);
    }
}
