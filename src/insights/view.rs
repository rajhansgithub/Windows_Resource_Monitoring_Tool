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
//! (one per feature, with cards and charts) for a laptop, so the layout can be
//! checked before real data exists; `RESMON_INSIGHTS_DEMO=desktop` shows the
//! same for a PC without a battery.

use super::{time, Chart, Panel, Role, Tone, Visual};
use crate::render::{measure_dc, Canvas, Rgba};
use crate::util::Rect;
use std::sync::OnceLock;
use windows_sys::Win32::Graphics::Gdi::{HDC, HFONT};

const CARD_PAD: i32 = 16;
/// Height of a chart's plot area.
const PLOT_H: i32 = 150;
/// Side of the Held Back gauge.
const GAUGE: i32 = 130;
/// Height of the memory split bar.
const SPLIT_H: i32 = 26;
/// Gauge sweep, open at the bottom (radians, clockwise from 3 o'clock; the design's
/// 0.8 pi to 2.2 pi, about 250 degrees).
const GAUGE_A0: f32 = std::f32::consts::PI * 0.8;
const GAUGE_A1: f32 = std::f32::consts::PI * 2.2;

/// Which built-in demo to show (`RESMON_INSIGHTS_DEMO`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Demo {
    Off,
    /// "1" (or "laptop"): a laptop with a battery.
    Laptop,
    /// "desktop": a PC without a battery.
    Desktop,
}

fn demo_mode(v: Option<&str>) -> Demo {
    match v.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "laptop") => Demo::Laptop,
        Some("desktop") => Demo::Desktop,
        _ => Demo::Off,
    }
}

/// `RESMON_INSIGHTS_DEMO=1` / `=desktop`: show the built-in demo panels.
pub fn demo() -> Demo {
    static D: OnceLock<Demo> = OnceLock::new();
    *D.get_or_init(|| demo_mode(std::env::var("RESMON_INSIGHTS_DEMO").ok().as_deref()))
}

/// This PC has a battery (from the latest snapshot; the demo decides in demo mode).
/// Battery-only settings (Bag Guard, the battery alert) are hidden without one.
pub fn has_battery(a: &crate::app::App) -> bool {
    match demo() {
        Demo::Laptop => true,
        Demo::Desktop => false,
        Demo::Off => a.m.snap.battery.is_some(),
    }
}

/// Panels to show now and whether Insights is on.
pub fn fetch(a: &crate::app::App, now: u64) -> (bool, Vec<Panel>) {
    if demo() != Demo::Off {
        (true, demo_panels(now, demo() == Demo::Laptop))
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
    /// Small semibold (legend values).
    pub cap: HFONT,
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

/// Share of the whole for each part of a split bar (negative and non-finite parts
/// count as 0). Sums to 1, or all zeros when there is nothing to split.
pub fn split_fracs(vals: &[f32]) -> Vec<f32> {
    let v: Vec<f32> = vals.iter().map(|x| if x.is_finite() { x.max(0.0) } else { 0.0 }).collect();
    let total: f32 = v.iter().sum();
    if total <= 0.0 {
        return vec![0.0; v.len()];
    }
    v.iter().map(|x| x / total).collect()
}

/// Pixel spans `[x0, x1)` of the parts across a bar `w` wide: adjacent, no gaps,
/// the last one ends at `w` (rounded on the running sum, so errors never add up).
pub fn split_spans(vals: &[f32], w: i32) -> Vec<(i32, i32)> {
    let mut acc = 0.0f32;
    let mut x0 = 0;
    split_fracs(vals)
        .into_iter()
        .map(|f| {
            acc += f;
            let x1 = if acc > 0.0 { (acc * w as f32).round().min(w as f32) as i32 } else { 0 };
            let span = (x0, x1.max(x0));
            x0 = span.1;
            span
        })
        .collect()
}

/// Angle of `pct` (0..100) on the gauge, radians clockwise from 3 o'clock.
pub fn gauge_angle(pct: f32) -> f32 {
    let t = if pct.is_finite() { (pct / 100.0).clamp(0.0, 1.0) } else { 0.0 };
    GAUGE_A0 + (GAUGE_A1 - GAUGE_A0) * t
}

/// "7.2 GB": a split part's value for the legend.
fn split_val(v: f32, unit: &str) -> String {
    if unit.is_empty() {
        format!("{:.1}", v)
    } else {
        format!("{:.1} {}", v, unit)
    }
}

/// Y-axis labels: top (with unit), middle, bottom.
fn y_labels(c: &Chart) -> [String; 3] {
    let max = chart_max(c);
    [fmt_val(max, c.unit), fmt_val(max / 2.0, ""), "0".into()]
}

/// Fingerprint of everything visible, to repaint only when it changed.
/// `visual_of`: index of the panel whose visual (gauge, split bar) is on screen;
/// visuals of other panels are left out, so a live CPU figure on the gauge does not
/// repaint Overview.
pub fn signature(enabled: bool, panels: &[Panel], visual_of: Option<usize>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    enabled.hash(&mut h);
    for (i, p) in panels.iter().enumerate() {
        if visual_of == Some(i) {
            match &p.visual {
                None => 0u8.hash(&mut h),
                Some(Visual::Split { parts, unit }) => {
                    unit.hash(&mut h);
                    for (l, v, r) in parts {
                        l.hash(&mut h);
                        ((v * 10.0).round() as i64).hash(&mut h);
                        r.hash(&mut h);
                    }
                }
                Some(Visual::Gauge { ceiling, used, reason }) => {
                    ceiling.map(|c| c.round() as i32).hash(&mut h);
                    (used.round() as i32).hash(&mut h);
                    reason.hash(&mut h);
                }
            }
        }
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
            c.x_labels.hash(&mut h);
            for t in &c.tones {
                (*t as u8).hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Built-in realistic panels for `RESMON_INSIGHTS_DEMO`: a laptop (`battery`) or a
/// desktop without a battery. Only what the real modules can show.
pub fn demo_panels(now: u64, battery: bool) -> Vec<Panel> {
    use super::{last_hour, x_labels, Item};
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
    let today = time::day(now);

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
    let idle: Vec<f32> = (0..21).map(|i| if i < 12 { 3.1 + 0.1 * noise(i) } else { 3.4 + 0.1 * noise(i) }).collect();
    let ghost: Vec<f32> = (0..14).map(|i| 0.9 + i as f32 * 0.02 + if i >= 9 { 0.5 * (i - 8) as f32 } else { 0.0 }).collect();
    let tone_of = |v: f32, good: f32, warn: f32| if v <= good { Tone::Good } else if v <= warn { Tone::Warn } else { Tone::Bad };

    let (sleep, held) = if battery {
        let drain = [0.4f32, 0.3, 1.2, 0.5, 0.6, 2.4, 0.4, 0.5, 0.3, 0.4, 1.1, 0.5, 0.4, 1.7];
        let sleep = Panel {
            key: "sleep",
            title: "Sleep Verdict",
            tone: Tone::Bad,
            headline: "Lost 14% in 8h. Awake 22% of the time. Microsoft Teams was busy.".into(),
            lines: vec![
                "Drain 1.7%/h.".into(),
                "Awake 22% of the time.".into(),
                "Woke by: Network adapter.".into(),
                "Busiest while awake: Teams, 12 min of CPU.".into(),
                "Bag Guard is off.".into(),
            ],
            visual: None,
            chart: Some(Chart {
                label: "Drain per hour, last 14 nights".into(),
                points: drain.to_vec(),
                max: 0.0,
                unit: "%/h",
                markers: vec![],
                bars: true,
                slots: 0,
                x_labels: x_labels("Older", "", "Last night"),
                tones: drain.iter().map(|&d| tone_of(d, 1.0, 2.0)).collect(),
            }),
            items: vec![
                it(2 * HOUR, Tone::Bad, "Lost 14% in 8h. Awake 22% of the time. Teams was busy.", "23:10 to 07:12 \u{b7} woke by Network adapter"),
                it(DAY + 2 * HOUR, Tone::Good, "Slept 7h 50m. Lost 3%. Good.", "23:30 to 07:20 \u{b7} woke by Power button"),
                it(2 * DAY + 2 * HOUR, Tone::Good, "Slept 8h 5m. Lost 2%. Good.", "23:05 to 07:10 \u{b7} woke by Lid"),
            ],
        };
        let limit: Vec<f32> = (0..240).map(|i| if i < 140 { 100.0 } else { 62.0 + 3.0 * noise(i) }).collect();
        let held = Panel {
            key: "heldback",
            title: "Held Back",
            tone: Tone::Bad,
            headline: "Capped at 62%. Firmware thermal limit.".into(),
            lines: vec![
                "Power mode: Balanced. On battery. In front: Code.".into(),
                "Power plan: Balanced.".into(),
                "Current speed 71% of normal. Thermal limit 62%.".into(),
            ],
            visual: Some(Visual::Gauge { ceiling: Some(62.0), used: 48.0, reason: "Firmware thermal limit".into() }),
            chart: Some(Chart {
                label: "CPU ceiling, last hour".into(),
                points: limit,
                max: 100.0,
                unit: "%",
                markers: vec![],
                bars: false,
                slots: 360,
                x_labels: last_hour(),
                tones: vec![],
            }),
            items: vec![
                it(23 * MIN, Tone::Bad, "Capped at 59% for 23 min.", "Firmware thermal limit."),
                it(5 * HOUR, Tone::Warn, "Capped at 80% for 40 min.", "Battery saver."),
            ],
        };
        (sleep, held)
    } else {
        let awake = [3.0f32, 2.0, 4.0, 2.0, 61.0, 3.0, 2.0];
        let sleep = Panel {
            key: "sleep",
            title: "Sleep Verdict",
            tone: Tone::Info,
            headline: "Slept 7h 42m. Awake 4% of the time.".into(),
            lines: vec![
                "Asleep 7h 24m of 7h 42m.".into(),
                "Awake 4% of the time.".into(),
                "Woke by: Power button.".into(),
                "Busiest while awake: SearchIndexer, 40 s of CPU.".into(),
            ],
            visual: None,
            chart: Some(Chart {
                label: "Time awake per night".into(),
                points: awake.to_vec(),
                max: 100.0,
                unit: "%",
                markers: vec![],
                bars: true,
                slots: 0,
                x_labels: x_labels("Older", "", "Last night"),
                tones: awake.iter().map(|&a| if a > 50.0 { Tone::Warn } else { Tone::Info }).collect(),
            }),
            items: vec![
                it(2 * HOUR, Tone::Info, "Slept 7h 42m. Awake 4% of the time.", "23:30 to 07:12 \u{b7} woke by Power button"),
                it(2 * DAY + 2 * HOUR, Tone::Warn, "Stayed awake 61% of 6h. MsMpEng was busy.", "00:10 to 06:10 \u{b7} woke by Keyboard"),
            ],
        };
        let held = Panel {
            key: "heldback",
            title: "Held Back",
            tone: Tone::Good,
            headline: "Full power. Nothing is holding this PC back.".into(),
            lines: vec![
                "Power mode: Balanced. In front: Code.".into(),
                "Power plan: Balanced.".into(),
                "Current speed 64% of normal.".into(),
            ],
            visual: Some(Visual::Gauge { ceiling: Some(100.0), used: 6.0, reason: "none".into() }),
            chart: Some(Chart {
                label: "CPU ceiling, last hour".into(),
                points: vec![100.0; 240],
                max: 100.0,
                unit: "%",
                markers: vec![],
                bars: false,
                slots: 360,
                x_labels: last_hour(),
                tones: vec![],
            }),
            items: vec![],
        };
        (sleep, held)
    };

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
            visual: None,
            chart: Some(Chart {
                label: "CPU, last hour".into(),
                points: cpu,
                max: 100.0,
                unit: "%",
                markers: vec![(14, "Scan".into()), (30, "Index".into())],
                bars: false,
                slots: 60,
                x_labels: last_hour(),
                tones: vec![],
            }),
            items: vec![
                it(25 * MIN, Tone::Bad, "CPU held at 97% for 3 min.", "MsMpEng 38%, disk 100%. A Defender scan."),
                it(78 * MIN, Tone::Warn, "Disk busy at 100% for 6 min.", "SearchIndexer wrote 1.2 GB."),
                it(3 * HOUR, Tone::Warn, "Display driver reset.", "nvlddmkm stopped and recovered. Apps may have flickered."),
                it(DAY + 2 * HOUR, Tone::Info, "Memory at 92% for 4 min.", "msedge 3.1 GB, Teams 1.4 GB."),
            ],
        },
        sleep,
        held,
        Panel {
            key: "baseline",
            title: "Before / After",
            tone: Tone::Info,
            headline: "Idle RAM up 0.3 GB since the update on 12 Nov.".into(),
            lines: vec!["Today: idle RAM 3.4 GB, CPU 2%, 214 processes.".into(), "Baseline: 21 days collected.".into()],
            visual: None,
            chart: Some(Chart {
                label: "Idle RAM per day".into(),
                points: idle,
                max: 0.0,
                unit: "GB",
                markers: vec![(12, "Update".into())],
                bars: true,
                slots: 0,
                x_labels: super::day_span(today - 20, today, now),
                tones: vec![],
            }),
            items: vec![
                it(9 * DAY, Tone::Warn, "Windows update KB5046633.", "Idle RAM rose from 3.1 GB to 3.4 GB after it."),
                it(15 * DAY, Tone::Good, "Graphics driver 31.0.101.5590.", "No change in idle use."),
            ],
        },
        Panel {
            key: "ghost",
            title: "Ghost Memory",
            tone: Tone::Warn,
            headline: "Kernel pool +0.5 GB a day for 5 days. A driver is leaking. A restart buys time.".into(),
            lines: vec![
                "Committed: 11.9 of 25.6 GB.".into(),
                "Kernel pool is memory no app owns. Windows can't say which driver holds it without admin.".into(),
            ],
            visual: Some(Visual::Split {
                parts: vec![
                    ("Apps", 7.2, Role::Accent),
                    ("Compressed", 0.7, Role::AccentAlt),
                    ("Kernel pool", 3.4, Role::Warn),
                    ("Cache", 1.6, Role::Neutral),
                    ("Free", 3.1, Role::Track),
                ],
                unit: "GB",
            }),
            chart: Some(Chart {
                label: "Kernel pool, peak per day".into(),
                points: ghost,
                max: 0.0,
                unit: "GB",
                markers: vec![(4, "Restart".into())],
                bars: true,
                slots: 0,
                x_labels: super::day_span(today - 13, today, now),
                tones: vec![],
            }),
            items: vec![],
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
    /// The split bar and the top-left of each legend entry.
    Split { bar: Rect, legend: Vec<(i32, i32)> },
    /// Gauge card, the square the ring sits in and the top-left of the first list row.
    Gauge { card: Rect, ring: Rect, list: (i32, i32) },
    Chart { rect: Rect },
    Item { rect: Rect, tone: Tone, time: String, title: String, detail: Vec<String>, tx: i32 },
}

/// Colour of a split-bar part.
fn role(c: &Colors, r: Role) -> Rgba {
    match r {
        Role::Accent => c.accent,
        Role::AccentAlt => c.accent.mix(c.good, 0.45),
        Role::Warn => c.warn,
        Role::Neutral => c.fg.with_a(0x4D),
        Role::Track => c.fg.with_a(0x1C),
    }
}

/// Gauge list rows: (label, value).
fn gauge_rows(ceiling: Option<f32>, used: f32, reason: &str) -> Vec<(&'static str, String)> {
    let pct = |v: f32| format!("{}%", v.round() as i32);
    match ceiling {
        Some(c) => vec![("Ceiling", pct(c)), ("CPU use", pct(used)), ("Reason", reason.to_string())],
        None => vec![("Ceiling", "not reported".into()), ("CPU use", pct(used))],
    }
}

/// Run `f` with drawing limited to `r` (inside the current clip), then restore it.
fn clipped(cv: &mut Canvas, r: Rect, f: impl FnOnce(&mut Canvas)) {
    let old = cv.clip();
    let n = match old {
        Some(o) => Rect { l: r.l.max(o.l), t: r.t.max(o.t), r: r.r.min(o.r), b: r.b.min(o.b) },
        None => r,
    };
    if n.l < n.r && n.t < n.b {
        cv.set_clip(Some(n));
        f(cv);
    }
    cv.set_clip(old);
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
        let tw = |font: HFONT, s: &str| measure_dc(mdc, font, s).0;
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
        let pad = self.sc(CARD_PAD);
        let smh = lh(f.small);
        match &p.visual {
            Some(Visual::Split { parts, unit }) if !parts.is_empty() => {
                y += self.sc(12);
                let bar = Rect { l, t: y, r, b: y + self.sc(SPLIT_H) };
                y = bar.b + self.sc(8);
                // Legend: "■ Apps 7.2 GB" entries, wrapped to the column.
                let mut legend = Vec::with_capacity(parts.len());
                let (gap_x, row_h) = (self.sc(16), smh + self.sc(6));
                let mut x = l;
                for (label, v, _) in parts {
                    let ew = self.sc(15) + tw(f.small, label) + self.sc(4) + tw(f.cap, &split_val(*v, unit));
                    if x > l && x + ew > r {
                        x = l;
                        y += row_h;
                    }
                    legend.push((x, y));
                    x += ew + gap_x;
                }
                y += smh;
                blocks.push(Block::Split { bar, legend });
            }
            Some(Visual::Gauge { ceiling, used, reason }) => {
                y += self.sc(12);
                let rows = gauge_rows(*ceiling, *used, reason);
                let row_h = smh + self.sc(4);
                let list_h = rows.len() as i32 * row_h - self.sc(4);
                let g = self.sc(GAUGE);
                let vpad = self.sc(8);
                let label_w = rows.iter().map(|(k, v)| tw(f.small, k) + self.sc(6) + tw(f.cap, v)).max().unwrap_or(0);
                // Ring and list side by side; the list goes under the ring when narrow.
                let side = w >= pad * 2 + g + self.sc(16) + label_w.min(self.sc(200));
                let ring = Rect { l: l + pad, t: y + vpad, r: l + pad + g, b: y + vpad + g };
                let (list, bottom) = if side {
                    ((ring.r + self.sc(16), ring.t + (g - list_h) / 2), ring.b + vpad)
                } else {
                    ((l + pad, ring.b + self.sc(4)), ring.b + self.sc(4) + list_h + pad)
                };
                let card = Rect { l, t: y, r, b: bottom };
                y = card.b;
                blocks.push(Block::Gauge { card, ring, list });
            }
            _ => {}
        }
        if let Some(c) = p.chart.as_ref().filter(|c| c.points.iter().any(|v| v.is_finite())) {
            y += self.sc(16);
            let tiny_h = lh(f.tiny);
            let gutter = y_labels(c).iter().map(|s| tw(f.tiny, s)).max().unwrap_or(0) + self.sc(8);
            let xl = c.x_labels.iter().any(|s| !s.is_empty());
            let top = y + self.sc(12) + lh(f.semi) + self.sc(12);
            let plot = Rect { l: l + pad + gutter, t: top, r: r - pad, b: top + self.sc(PLOT_H) };
            let bottom = plot.b + if xl { self.sc(6) + tiny_h } else { 0 } + self.sc(12);
            self.plot = Some(plot);
            blocks.push(Block::Chart { rect: Rect { l, t: y, r, b: bottom } });
            y = bottom;
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
            let tx = l + self.sc(24) + if time_w > 0 { time_w + self.sc(16) } else { 0 };
            let text_w = (r - pad - tx).max(self.sc(80));
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
                Block::Split { bar, legend } => {
                    let Some(Visual::Split { parts, unit }) = &p.visual else { continue };
                    let bar = shift(*bar, dy);
                    if bar.t > vb || legend.last().is_some_and(|l| l.1 + dy + self.sc(30) < vt) {
                        continue;
                    }
                    self.draw_split(cv, f, c, bar, legend, parts, unit, dy);
                }
                Block::Gauge { card, ring, list } => {
                    let Some(Visual::Gauge { ceiling, used, reason }) = &p.visual else { continue };
                    let card = shift(*card, dy);
                    if card.b < vt || card.t > vb {
                        continue;
                    }
                    cv.fill_round(card, rad, c.card);
                    cv.stroke_round(card, rad, c.card_border);
                    self.draw_gauge(cv, f, c, shift(*ring, dy), *ceiling, *used);
                    let (lx, ly) = (list.0, list.1 + dy);
                    let row_h = lh(cv, f.small) + self.sc(4);
                    for (i, (k, v)) in gauge_rows(*ceiling, *used, reason).iter().enumerate() {
                        let yy = ly + i as i32 * row_h;
                        cv.text(f.small, k, lx, yy, c.dim);
                        let kw = cv.measure(f.small, k).0;
                        let vx = lx + kw + self.sc(6);
                        let v = cv.ellipsize(f.cap, v, card.r - self.sc(CARD_PAD) - vx);
                        cv.text(f.cap, &v, vx, yy, c.fg);
                    }
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

    /// Stacked bar (rounded ends, parts clipped out of one rounded shape) and legend.
    #[allow(clippy::too_many_arguments)]
    fn draw_split(&self, cv: &mut Canvas, f: &Fonts, c: &Colors, bar: Rect, legend: &[(i32, i32)], parts: &[(&str, f32, Role)], unit: &str, dy: i32) {
        let rad = self.sc(6) as f32;
        let vals: Vec<f32> = parts.iter().map(|p| p.1).collect();
        // The track first, so a zero total still shows an empty bar.
        cv.fill_round(bar, rad, role(c, Role::Track));
        for ((x0, x1), (_, _, r)) in split_spans(&vals, bar.w()).into_iter().zip(parts) {
            if x1 > x0 && *r != Role::Track {
                clipped(cv, Rect { l: bar.l + x0, r: bar.l + x1, ..bar }, |cv| cv.fill_round(bar, rad, role(c, *r)));
            }
        }
        cv.stroke_round(bar, rad, c.card_border);
        let sq = self.sc(9);
        let smh = cv.measure(f.small, "Ag").1;
        for ((x, y), (label, v, r)) in legend.iter().zip(parts) {
            let y = y + dy;
            let sy = y + (smh - sq) / 2;
            let sw = Rect { l: *x, t: sy, r: x + sq, b: sy + sq };
            cv.fill_round(sw, self.sc(2) as f32, role(c, *r));
            if *r == Role::Track {
                cv.stroke_round(sw, self.sc(2) as f32, c.card_border);
            }
            let lx = x + sq + self.sc(6);
            cv.text(f.small, label, lx, y, c.dim);
            let lw = cv.measure(f.small, label).0;
            cv.text(f.cap, &split_val(*v, unit), lx + lw + self.sc(4), y, c.fg);
        }
    }

    /// Gauge ring: track, the part above the ceiling tinted, CPU use in accent,
    /// a dot on the ceiling, the value in the middle.
    fn draw_gauge(&self, cv: &mut Canvas, f: &Fonts, c: &Colors, ring: Rect, ceiling: Option<f32>, used: f32) {
        let s = ring.w() as f32;
        let (cx, cy) = (ring.l as f32 + s / 2.0, ring.t as f32 + s * 0.54);
        let rr = s * 0.385;
        let th = s * 0.062;
        cv.stroke_arc(cx, cy, rr, th, GAUGE_A0, GAUGE_A1, c.fg.with_a(0x1C));
        let top = ceiling.unwrap_or(100.0);
        if ceiling.is_some_and(|x| x < 100.0) {
            cv.stroke_arc(cx, cy, rr, th, gauge_angle(top), GAUGE_A1, c.warn.with_a(0x59));
        }
        let fill = used.min(top);
        if fill >= 0.5 {
            cv.stroke_arc(cx, cy, rr, th, GAUGE_A0, gauge_angle(fill), c.accent);
        }
        if let Some(x) = ceiling {
            let a = gauge_angle(x);
            let d = (s * 0.085).round() as i32;
            let (dx, dy) = ((cx + rr * a.cos()).round() as i32, (cy + rr * a.sin()).round() as i32);
            let col = if x < 95.0 { c.warn } else { c.good };
            cv.fill_round(Rect { l: dx - d / 2, t: dy - d / 2, r: dx - d / 2 + d, b: dy - d / 2 + d }, d as f32 / 2.0, col);
        }
        let (big, cap) = match ceiling {
            Some(x) => (format!("{}%", x.round() as i32), "ceiling"),
            None => (format!("{}%", used.round() as i32), "CPU use"),
        };
        let hh = cv.measure(f.head, "Ag").1;
        let ty = cy.round() as i32 - hh / 2 - self.sc(6);
        cv.text_center(f.head, &big, cx.round() as i32, ty, c.fg);
        cv.text_center(f.tiny, cap, cx.round() as i32, ty + hh, c.dim);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chart(&self, cv: &mut Canvas, f: &Fonts, p: &Colors, card: Rect, pr: Rect, c: &Chart, hover_x: Option<i32>) {
        let rad = self.sc(6) as f32;
        cv.fill_round(card, rad, p.card);
        cv.stroke_round(card, rad, p.card_border);
        let pad = self.sc(CARD_PAD);
        let max = chart_max(c);
        let max_s = format!("max {}", fmt_val(max, c.unit));
        let mw = cv.measure(f.small, &max_s).0;
        let label = cv.ellipsize(f.semi, &c.label, card.w() - pad * 3 - mw);
        let head_t = card.t + self.sc(12);
        cv.text(f.semi, &label, card.l + pad, head_t, p.fg);
        let (sh, smh) = (cv.measure(f.semi, "Ag").1, cv.measure(f.small, "Ag").1);
        cv.text_right(f.small, &max_s, card.r - pad, head_t + (sh - smh) / 2, p.dim);
        let (w, h) = (pr.w(), pr.h());
        if w <= 2 || h <= 2 {
            return;
        }
        // Y axis: dashed lines at max and half, solid baseline, labels in the gutter.
        let th_ = cv.measure(f.tiny, "Ag").1;
        let ys = [pr.t, pr.t + (h - 1) / 2, pr.b - 1];
        for (k, (yy, s)) in ys.iter().zip(y_labels(c)).enumerate() {
            if k == 2 {
                cv.fill_rect(Rect { l: pr.l, t: *yy, r: pr.r, b: yy + 1 }, p.line.mix(p.dim, 0.3));
            } else {
                let mut x = pr.l;
                while x < pr.r {
                    cv.fill_rect(Rect { l: x, t: *yy, r: (x + self.sc(3)).min(pr.r), b: yy + 1 }, p.line);
                    x += self.sc(7);
                }
            }
            cv.text_right(f.tiny, &s, pr.l - self.sc(8), yy - th_ / 2, p.dim);
        }
        // X axis labels: left, middle, right.
        let xy = pr.b + self.sc(6);
        let [xl, xm, xr] = &c.x_labels;
        if !xl.is_empty() {
            cv.text(f.tiny, xl, pr.l, xy, p.dim);
        }
        if !xm.is_empty() {
            cv.text_center(f.tiny, xm, (pr.l + pr.r) / 2, xy, p.dim);
        }
        if !xr.is_empty() {
            cv.text_right(f.tiny, xr, pr.r, xy, p.dim);
        }

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
            let br_rad = self.sc(3);
            for i in 0..n {
                let Some(v) = val(i) else { continue };
                let cx = pr.l as f32 + x_of(i, w, n, true);
                let top = y_of(v).min(pr.b as f32 - 2.0);
                let (bl, brr) = ((cx - bw / 2.0).round() as i32, (cx + bw / 2.0).round() as i32);
                let br = Rect { l: bl, t: top.round() as i32, r: brr, b: pr.b - 1 };
                let col = c.tones.get(i - off).map_or(acc, |t| p.tone(*t));
                // The newest bar (and the one under the mouse) at full strength.
                let full = i + 1 == n || hover_i == Some(i);
                let col = if full { col } else { col.with_a((col.3 as f32 * 0.75) as u8) };
                // Rounded top, square bottom: a taller rounded bar cut at the baseline.
                clipped(cv, br, |cv| cv.fill_round(Rect { b: br.b + br_rad, ..br }, br_rad as f32, col));
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
            // The newest slot, when it has data, gets a dot too ("now").
            for i in 0..n {
                let Some(v) = val(i) else { continue };
                let lone = (i == 0 || val(i - 1).is_none()) && (i + 1 >= n || val(i + 1).is_none());
                let d = if i + 1 == n { self.sc(7) } else if lone { self.sc(6) } else { continue };
                let cx = pr.l + x_of(i, w, n, false).round() as i32;
                let cy = y_of(v).round() as i32;
                cv.fill_round(Rect { l: cx - d / 2, t: cy - d / 2, r: cx - d / 2 + d, b: cy - d / 2 + d }, d as f32 / 2.0, acc);
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
                cv.fill_rect(Rect { l: mx, t: yy, r: mx + 1, b: (yy + self.sc(3)).min(pr.b) }, p.warn.with_a(0xC0));
                yy += self.sc(6);
            }
            let (lw, _) = cv.measure(f.tiny, lab);
            let lx = if mx + self.sc(4) + lw > pr.r { mx - self.sc(4) - lw } else { mx + self.sc(4) };
            if lx > last_r + self.sc(4) {
                cv.text(f.tiny, lab, lx, pr.t + self.sc(3), p.warn);
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
        let a = demo_panels(1_764_000_000_000, true);
        let s = signature(true, &a, None);
        assert_eq!(s, signature(true, &a.clone(), None));
        assert_ne!(s, signature(false, &a, None));
        let mut b = a.clone();
        b[2].headline.push('!');
        assert_ne!(s, signature(true, &b, None));
        let mut b = a.clone();
        b[0].items.pop();
        assert_ne!(s, signature(true, &b, None));
        let mut b = a.clone();
        b[1].chart.as_mut().unwrap().points.push(50.0);
        assert_ne!(s, signature(true, &b, None));
        let mut b = a.clone();
        b[0].chart.as_mut().unwrap().points[0] = f32::NAN;
        assert_ne!(s, signature(true, &b, None));
        let mut b = a.clone();
        b[1].chart.as_mut().unwrap().tones[0] = Tone::Bad;
        assert_ne!(s, signature(true, &b, None));
        // A live gauge value counts only while its page is on screen.
        let mut b = a.clone();
        b[2].visual = Some(Visual::Gauge { ceiling: Some(62.0), used: 90.0, reason: "Firmware thermal limit".into() });
        assert_eq!(s, signature(true, &b, None));
        assert_ne!(signature(true, &a, Some(2)), signature(true, &b, Some(2)));
    }

    #[test]
    fn demo_has_every_feature() {
        for battery in [true, false] {
            let p = demo_panels(1_764_000_000_000, battery);
            let keys: Vec<_> = p.iter().map(|x| x.key).collect();
            assert_eq!(keys, ["incidents", "sleep", "heldback", "baseline", "ghost"]);
            for x in &p {
                let c = x.chart.as_ref().unwrap();
                assert!(!c.points.is_empty());
                assert!(c.markers.iter().all(|(i, _)| *i < c.points.len()));
                assert!(c.tones.is_empty() || c.tones.len() == c.points.len());
                assert!(!x.headline.contains('\u{2014}'));
            }
            // Time series are lines; per-day values are bars.
            assert!(!p[0].chart.as_ref().unwrap().bars);
            assert!(p[3].chart.as_ref().unwrap().bars);
            assert!(p[4].chart.as_ref().unwrap().bars);
            assert!(matches!(p[2].visual, Some(Visual::Gauge { .. })));
            assert!(matches!(p[4].visual, Some(Visual::Split { .. })));
        }
        // The desktop demo never mentions a battery or a lid.
        for x in demo_panels(1_764_000_000_000, false) {
            let mut text = vec![x.headline.clone(), x.chart.unwrap().label];
            text.extend(x.lines);
            text.extend(x.items.into_iter().flat_map(|i| [i.title, i.detail]));
            for t in text {
                let t = t.to_ascii_lowercase();
                assert!(!t.contains("battery") && !t.contains("lid") && !t.contains("drain"), "{}", t);
            }
        }
    }

    #[test]
    fn demo_modes() {
        assert_eq!(demo_mode(None), Demo::Off);
        assert_eq!(demo_mode(Some("1")), Demo::Laptop);
        assert_eq!(demo_mode(Some(" laptop ")), Demo::Laptop);
        assert_eq!(demo_mode(Some("Desktop")), Demo::Desktop);
        assert_eq!(demo_mode(Some("0")), Demo::Off);
    }

    #[test]
    fn split_fractions_sum_to_one() {
        let f = split_fracs(&[7.2, 0.7, 3.4, 1.6, 3.1]);
        assert!((f.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!((f[0] - 0.45).abs() < 1e-5);
        // Negative and NaN parts count as nothing; an empty whole splits into nothing.
        let f = split_fracs(&[1.0, -2.0, f32::NAN, 3.0]);
        assert_eq!(f, vec![0.25, 0.0, 0.0, 0.75]);
        assert_eq!(split_fracs(&[0.0, 0.0]), vec![0.0, 0.0]);
        assert!(split_fracs(&[]).is_empty());
    }

    #[test]
    fn split_spans_tile_the_bar() {
        for w in [1, 97, 600, 1001] {
            let s = split_spans(&[7.2, 0.7, 3.4, 1.6, 3.1], w);
            assert_eq!(s[0].0, 0);
            assert_eq!(s.last().unwrap().1, w);
            assert!(s.windows(2).all(|p| p[0].1 == p[1].0), "{:?}", s);
        }
        // Thirds of 100 px: rounding on the running sum, never off by more than 1.
        assert_eq!(split_spans(&[1.0, 1.0, 1.0], 100), vec![(0, 33), (33, 67), (67, 100)]);
        assert_eq!(split_spans(&[0.0, 0.0], 50), vec![(0, 0), (0, 0)]);
    }

    #[test]
    fn gauge_angles() {
        use std::f32::consts::PI;
        let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(near(gauge_angle(0.0), 0.8 * PI));
        assert!(near(gauge_angle(100.0), 2.2 * PI));
        // Half way is straight up (12 o'clock is 1.5 pi with y pointing down).
        assert!(near(gauge_angle(50.0), 1.5 * PI));
        // The sweep is 1.4 pi and symmetric about 12 o'clock; out-of-range values stay on the ring.
        assert!(near(gauge_angle(100.0) - gauge_angle(0.0), 1.4 * PI));
        assert!(near(gauge_angle(25.0) + gauge_angle(75.0), 3.0 * PI));
        assert!(near(gauge_angle(-5.0), gauge_angle(0.0)));
        assert!(near(gauge_angle(140.0), gauge_angle(100.0)));
        assert!(near(gauge_angle(f32::NAN), gauge_angle(0.0)));
    }

    #[test]
    fn gauge_list() {
        let r = gauge_rows(Some(100.0), 6.2, "none");
        assert_eq!(r, vec![("Ceiling", "100%".to_string()), ("CPU use", "6%".into()), ("Reason", "none".into())]);
        // Not reported: no reason row, CPU use still shown.
        let r = gauge_rows(None, 6.0, "none");
        assert_eq!(r, vec![("Ceiling", "not reported".to_string()), ("CPU use", "6%".into())]);
    }

    #[test]
    fn y_axis_labels() {
        let c = Chart { points: vec![1.0, 4.4], unit: "GB", ..Default::default() };
        assert_eq!(y_labels(&c), ["5 GB".to_string(), "2.5".into(), "0".into()]);
        let c = Chart { points: vec![10.0], max: 100.0, unit: "%", ..Default::default() };
        assert_eq!(y_labels(&c), ["100%".to_string(), "50".into(), "0".into()]);
    }
}
