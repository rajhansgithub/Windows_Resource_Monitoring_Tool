//! Insights window: a navigation rail with the five panels on the left, the
//! selected panel on the right (title, headline, tone pill, facts, chart and
//! the list of cards), in the style of the Settings window.
//!
//! Like Settings there are no child windows: the client area is one
//! `render::Canvas` (reused between paints, resized only when the window is)
//! copied with one `BitBlt`, on the Mica backdrop on Windows 11.
//!
//! Cost rules: `refresh()` runs on every app tick. With the window closed it
//! is one null check; with it open the panels are rebuilt at most every 2 s
//! and the window repaints only when something visible changed.
//!
//! `RESMON_INSIGHTS_DEMO=1` in the environment shows built-in demo panels
//! (one per feature, with cards and charts) so the layout can be checked
//! before real data exists.

use super::{time, unix_ms, Chart, Panel, Tone};
use crate::app::{app, C_CRIT, C_WARN};
use crate::popup::style_window;
use crate::render::{create_font, measure_dc, ui_face, Canvas, Rgba};
use crate::util::*;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "ResourceMonitor.Insights";
const TITLE: &str = "Insights \u{2014} Resource Monitor";

// ------------------------------------------------------------------ layout (DIP, 96 dpi)

const CLIENT_W: i32 = 920;
const CLIENT_H: i32 = 660;
const MIN_W: i32 = 640;
const MIN_H: i32 = 440;
const NAV_W: i32 = 240;
const NAV_TOP: i32 = 64;
const NAV_ITEM_H: i32 = 40;
const NAV_GAP: i32 = 4;
const HEADER_H: i32 = 76;
const PAGE_PAD_L: i32 = 8;
const PAGE_PAD_R: i32 = 24;
const CARD_PAD: i32 = 16;
const CHART_H: i32 = 180;
const WHEEL_STEP: i32 = 48;

/// Rebuild the panels at most this often while the window is open.
const REBUILD_MS: u64 = 2000;
const T_SCROLL: usize = 1;

const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_MOUSELEAVE: u32 = 0x02A3;

// ------------------------------------------------------------------ state

static mut UI: *mut Ui = std::ptr::null_mut();

#[allow(static_mut_refs)]
fn ui() -> Option<&'static mut Ui> {
    // SAFETY: single UI thread; the pointer is either null or a live `Box<Ui>`
    // owned by this module (freed only in `free_ui`).
    unsafe { UI.as_mut() }
}

#[allow(static_mut_refs)]
fn free_ui() {
    // SAFETY: single UI thread; the pointer was made by `Box::into_raw` and is
    // swapped out before being freed, so it is freed once.
    unsafe {
        let p = std::ptr::replace(std::ptr::addr_of_mut!(UI), std::ptr::null_mut());
        if !p.is_null() {
            let u = Box::from_raw(p);
            u.fonts.free();
            if !u.mdc.is_null() {
                DeleteDC(u.mdc);
            }
        }
    }
}

/// `RESMON_INSIGHTS_DEMO=1`: show the built-in demo panels.
fn demo() -> bool {
    static D: OnceLock<bool> = OnceLock::new();
    *D.get_or_init(|| std::env::var("RESMON_INSIGHTS_DEMO").is_ok_and(|v| v.trim() == "1"))
}

// ------------------------------------------------------------------ palette

const fn hx(v: u32) -> Rgba {
    Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const fn wa(a: u8) -> Rgba {
    Rgba(255, 255, 255, a)
}

const fn ka(a: u8) -> Rgba {
    Rgba(0, 0, 0, a)
}

/// Window colours; the same values as the Settings window so both look alike.
#[derive(Clone, Copy)]
struct Pal {
    dark: bool,
    bg: Rgba,
    card: Rgba,
    card_border: Rgba,
    /// Opaque fill for the chart value bubble.
    bubble: Rgba,
    nav_sel: Rgba,
    nav_hover: Rgba,
    nav_press: Rgba,
    fg: Rgba,
    dim: Rgba,
    off: Rgba,
    line: Rgba,
    sb: Rgba,
    sb_track: Rgba,
    good: Rgba,
    learning: Rgba,
    accent: Rgba,
    accent_hover: Rgba,
    accent_press: Rgba,
    on_accent: Rgba,
}

impl Pal {
    fn new(dark: bool, mica: bool, accent: Rgba) -> Pal {
        let lum = (accent.0 as u32 * 299 + accent.1 as u32 * 587 + accent.2 as u32 * 114) / 1000;
        let on_accent = if lum > 150 { hx(0x000000) } else { hx(0xFFFFFF) };
        let shade = if dark { hx(0x000000) } else { hx(0xFFFFFF) };
        let (accent_hover, accent_press) = (accent.mix(shade, 0.1), accent.mix(shade, 0.2));
        if dark {
            Pal {
                dark,
                bg: if mica { Rgba(0, 0, 0, 0) } else { hx(0x202020) },
                card: if mica { wa(0x0D) } else { hx(0x2B2B2B) },
                card_border: if mica { wa(0x0F) } else { ka(0x33) },
                bubble: hx(0x2C2C2C),
                nav_sel: wa(0x0F),
                nav_hover: wa(0x0A),
                nav_press: wa(0x07),
                fg: hx(0xFFFFFF),
                dim: hx(0xC5C5C5),
                off: hx(0x787878),
                line: wa(0x17),
                sb: wa(0x8B),
                sb_track: wa(0x0B),
                good: hx(0x6CCB5F),
                learning: hx(0x9D9D9D),
                accent,
                accent_hover,
                accent_press,
                on_accent,
            }
        } else {
            Pal {
                dark,
                bg: if mica { Rgba(0, 0, 0, 0) } else { hx(0xF3F3F3) },
                card: if mica { wa(0xB3) } else { hx(0xFBFBFB) },
                card_border: ka(0x0F),
                bubble: hx(0xFFFFFF),
                nav_sel: ka(0x0A),
                nav_hover: ka(0x06),
                nav_press: ka(0x04),
                fg: hx(0x1A1A1A),
                dim: hx(0x5D5D5D),
                off: hx(0xA0A0A0),
                line: ka(0x14),
                sb: ka(0x72),
                sb_track: ka(0x06),
                good: hx(0x0F7B0F),
                learning: hx(0x8A8A8A),
                accent,
                accent_hover,
                accent_press,
                on_accent,
            }
        }
    }

    fn tone(&self, t: Tone) -> Rgba {
        match t {
            Tone::Good => self.good,
            Tone::Info => self.accent,
            Tone::Warn => C_WARN,
            Tone::Bad => C_CRIT,
            Tone::Learning => self.learning,
            Tone::Unavailable => self.off,
        }
    }
}

fn tone_label(t: Tone) -> &'static str {
    match t {
        Tone::Good => "All good",
        Tone::Info => "Note",
        Tone::Warn => "Worth a look",
        Tone::Bad => "Problem",
        Tone::Learning => "Learning",
        Tone::Unavailable => "Not available",
    }
}

/// Segoe Fluent Icons / MDL2 glyph of a panel.
fn panel_glyph(key: &str) -> char {
    match key {
        "incidents" => '\u{E945}', // lightning bolt
        "sleep" => '\u{E708}',     // moon
        "heldback" => '\u{EC4A}',  // speedometer
        "baseline" => '\u{E81C}',  // history
        "ghost" => '\u{E950}',     // chip
        _ => '\u{E946}',           // info
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
fn wrap(measure: &dyn Fn(&str) -> i32, s: &str, max_w: i32) -> Vec<String> {
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

/// Days of data read better as bars; long series as a line.
fn use_bars(c: &Chart) -> bool {
    c.points.len() <= 40
}

/// Index of the data point under column `dx` (0..w) of a plot `w` wide.
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

/// X offset (0..w) of point `i` (centre of its bar for bar charts).
fn x_of(i: usize, w: i32, n: usize, bars: bool) -> f32 {
    if bars || n <= 1 {
        (i as f32 + 0.5) * w as f32 / n.max(1) as f32
    } else {
        i as f32 * (w - 1) as f32 / (n - 1) as f32
    }
}

/// Fingerprint of everything visible, to repaint only when it changed.
fn signature(enabled: bool, panels: &[Panel]) -> u64 {
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
            c.max.to_bits().hash(&mut h);
            for v in &c.points {
                // Sub-pixel changes are invisible: round before hashing.
                ((v * 10.0).round() as i64).hash(&mut h);
            }
            c.markers.hash(&mut h);
        }
    }
    h.finish()
}

/// Built-in realistic panels for `RESMON_INSIGHTS_DEMO=1`.
fn demo_panels(now: u64) -> Vec<Panel> {
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

    let cpu: Vec<f32> = (0..120)
        .map(|i| {
            let base = 12.0 + 6.0 * noise(i);
            if (36..42).contains(&i) {
                95.0 + 2.0 * noise(i)
            } else if (88..92).contains(&i) {
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
    let limit: Vec<f32> = (0..60).map(|i| if i < 34 { 100.0 } else { 62.0 + 3.0 * noise(i) }).collect();
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
                label: "CPU, last 2 hours".into(),
                points: cpu,
                max: 100.0,
                unit: "%",
                markers: vec![(36, "Scan".into()), (88, "Index".into())],
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
                label: "CPU speed limit, last hour".into(),
                points: limit,
                max: 100.0,
                unit: "%",
                markers: vec![(34, "Hot".into())],
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
            }),
            items: vec![
                it(3 * HOUR, Tone::Warn, "4.8 GB today.", "Highest in 14 days."),
                it(DAY, Tone::Info, "4.6 GB.", "Up 0.4 GB after the network driver update."),
                it(6 * DAY, Tone::Good, "3.6 GB.", "Normal for this PC."),
            ],
        },
    ]
}

/// Panels to show now and whether Insights is on.
fn fetch(a: &crate::app::App, now: u64) -> (bool, Vec<Panel>) {
    if demo() {
        (true, demo_panels(now))
    } else if a.ins.enabled() {
        (true, a.ins.panels(now))
    } else {
        (false, Vec::new())
    }
}

// ------------------------------------------------------------------ fonts

struct Fonts {
    body: HFONT,
    small: HFONT,
    tiny: HFONT,
    semi: HFONT,
    head: HFONT,
    title: HFONT,
    app: HFONT,
    icon: HFONT,
}

impl Fonts {
    fn new(dpi: u32) -> Fonts {
        let sc = |v: i32| ((v * dpi as i32 + 48) / 96).max(1);
        let text = ui_face();
        let display = crate::settings_ui::pick_face("Segoe UI Variable Display", text);
        let icons = crate::settings_ui::pick_face("Segoe Fluent Icons", "Segoe MDL2 Assets");
        Fonts {
            body: create_font(sc(14), 400, text, true),
            small: create_font(sc(12), 400, text, true),
            tiny: create_font(sc(11), 400, text, true),
            semi: create_font(sc(14), 600, text, true),
            head: create_font(sc(20), 600, display, true),
            title: create_font(sc(28), 600, display, true),
            app: create_font(sc(18), 600, display, true),
            icon: create_font(sc(16), 400, icons, true),
        }
    }

    fn free(&self) {
        for f in [self.body, self.small, self.tiny, self.semi, self.head, self.title, self.app, self.icon] {
            // SAFETY: fonts created in `new`, freed once (the owner is dropped or replaced).
            unsafe { DeleteObject(f) };
        }
    }
}

// ------------------------------------------------------------------ model

#[derive(Clone, Copy, PartialEq)]
enum F {
    Head,
    Body,
    Semi,
}

/// Laid-out content of the selected panel (content coordinates: y = 0 at the
/// top of the scrolling view).
enum Block {
    Text { x: i32, y: i32, font: F, dim: bool, s: String },
    Pill { x: i32, y: i32, tone: Tone },
    Chart { rect: Rect },
    Item { rect: Rect, tone: Tone, time: String, title: String, detail: Vec<String>, tx: i32 },
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Nav(usize),
    /// "Open Settings" on the "Insights is off" screen.
    Button,
    Scrollbar,
    /// Over the chart plot at client x.
    Chart(i32),
}

struct Ui {
    hwnd: HWND,
    dpi: u32,
    dark: bool,
    mica: bool,
    pal: Pal,
    fonts: Fonts,
    /// Memory DC for measuring text during layout.
    mdc: HDC,
    cw: i32,
    ch: i32,
    enabled: bool,
    panels: Vec<Panel>,
    sig: u64,
    /// GetTickCount64 of the last rebuild.
    built: u64,
    sel: usize,
    blocks: Vec<Block>,
    content_h: i32,
    scroll: f32,
    target: i32,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    /// Keyboard focus is on the "Open Settings" button (off screen only).
    focus_btn: bool,
    focus_visible: bool,
    thumb_grab: Option<i32>,
    tracking: bool,
    cv: Canvas,
}

impl Ui {
    fn new(dpi: u32, dark: bool, enabled: bool, panels: Vec<Panel>) -> Ui {
        let sig = signature(enabled, &panels);
        Ui {
            hwnd: std::ptr::null_mut(),
            dpi,
            dark,
            mica: false,
            pal: Pal::new(dark, false, crate::settings_ui::accent_color(dark)),
            fonts: Fonts::new(dpi),
            // SAFETY: a memory DC, deleted in `free_ui`.
            mdc: unsafe { CreateCompatibleDC(std::ptr::null_mut()) },
            cw: 0,
            ch: 0,
            enabled,
            panels,
            sig,
            // SAFETY: no preconditions.
            built: unsafe { GetTickCount64() },
            sel: 0,
            blocks: Vec::new(),
            content_h: 0,
            scroll: 0.0,
            target: 0,
            hover: None,
            pressed: None,
            focus_btn: false,
            focus_visible: false,
            thumb_grab: None,
            tracking: false,
            cv: Canvas::new(),
        }
    }

    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    fn font(&self, f: F) -> HFONT {
        match f {
            F::Head => self.fonts.head,
            F::Body => self.fonts.body,
            F::Semi => self.fonts.semi,
        }
    }

    fn measure(&self, font: HFONT, s: &str) -> (i32, i32) {
        measure_dc(self.mdc, font, s)
    }

    fn line_h(&self, font: HFONT) -> i32 {
        self.measure(font, "Ag").1
    }

    fn set_dpi(&mut self, dpi: u32) {
        let old = self.dpi.max(1);
        self.dpi = dpi;
        let f = std::mem::replace(&mut self.fonts, Fonts::new(dpi));
        f.free();
        self.target = self.target * dpi as i32 / old as i32;
        self.scroll = self.target as f32;
    }

    fn set_palette(&mut self) {
        self.pal = Pal::new(self.dark, self.mica, crate::settings_ui::accent_color(self.dark));
    }

    /// Take new data. Returns true when something visible changed.
    fn take(&mut self, enabled: bool, panels: Vec<Panel>) -> bool {
        // SAFETY: no preconditions.
        self.built = unsafe { GetTickCount64() };
        let sig = signature(enabled, &panels);
        if sig == self.sig {
            return false;
        }
        self.sig = sig;
        self.enabled = enabled;
        self.panels = panels;
        self.sel = self.sel.min(self.panels.len().saturating_sub(1));
        self.layout();
        true
    }

    // -------------------------------------------------------------- geometry

    fn view(&self) -> (i32, i32) {
        let t = self.sc(HEADER_H);
        (t, self.ch.max(t + 1))
    }

    fn max_scroll(&self) -> i32 {
        let (vt, vb) = self.view();
        (self.content_h - (vb - vt)).max(0)
    }

    fn scroll_px(&self) -> i32 {
        self.scroll.round() as i32
    }

    fn page_lr(&self) -> (i32, i32) {
        let l = self.sc(NAV_W + PAGE_PAD_L);
        (l, (self.cw - self.sc(PAGE_PAD_R)).max(l + self.sc(200)))
    }

    fn nav_rect(&self, i: usize) -> Rect {
        let t = self.sc(NAV_TOP) + i as i32 * (self.sc(NAV_ITEM_H) + self.sc(NAV_GAP));
        Rect { l: self.sc(8), t, r: self.sc(NAV_W - 8), b: t + self.sc(NAV_ITEM_H) }
    }

    /// "Insights is off" message top and button rectangle.
    fn off_layout(&self) -> (i32, Rect) {
        let mh = self.line_h(self.fonts.head);
        let bh = self.sc(32);
        let top = (self.ch - mh - self.sc(20) - bh) / 2;
        let bw = self.measure(self.fonts.body, "Open Settings").0 + self.sc(48);
        let bt = top + mh + self.sc(20);
        (top, Rect { l: (self.cw - bw) / 2, t: bt, r: (self.cw - bw) / 2 + bw, b: bt + bh })
    }

    /// Plot area inside a chart card (client or content coordinates alike).
    fn plot_rect(&self, card: Rect) -> Rect {
        let pad = self.sc(CARD_PAD);
        let top = card.t + pad + self.line_h(self.fonts.semi) + self.sc(10);
        Rect { l: card.l + pad, t: top, r: card.r - pad, b: card.b - pad }
    }

    fn chart_card_client(&self) -> Option<Rect> {
        let dy = self.view().0 - self.scroll_px();
        self.blocks.iter().find_map(|b| match b {
            Block::Chart { rect } => Some(shift(*rect, dy)),
            _ => None,
        })
    }

    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        let max = self.max_scroll();
        if max <= 0 || !self.enabled {
            return None;
        }
        let (vt, vb) = self.view();
        let track = Rect { l: self.cw - self.sc(16), t: vt + self.sc(4), r: self.cw - self.sc(2), b: vb - self.sc(4) };
        let th = (track.h() * (vb - vt) / self.content_h.max(1)).max(self.sc(32)).min(track.h());
        let ty = track.t + ((track.h() - th) as f32 * (self.scroll / max as f32)).round() as i32;
        let wide = self.hover == Some(Hit::Scrollbar) || self.thumb_grab.is_some();
        let w = if wide { self.sc(6) } else { self.sc(4) };
        let r = track.r - self.sc(2);
        Some((track, Rect { l: r - w, t: ty, r, b: ty + th }))
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.enabled {
            return self.off_layout().1.contains(x, y).then_some(Hit::Button);
        }
        if let Some((track, _)) = self.scrollbar() {
            if track.contains(x, y) {
                return Some(Hit::Scrollbar);
            }
        }
        for i in 0..self.panels.len() {
            if self.nav_rect(i).contains(x, y) {
                return Some(Hit::Nav(i));
            }
        }
        let (vt, _) = self.view();
        if y >= vt {
            if let Some(card) = self.chart_card_client() {
                if self.plot_rect(card).contains(x, y) {
                    return Some(Hit::Chart(x));
                }
            }
        }
        None
    }

    // -------------------------------------------------------------- layout

    /// Lay out the selected panel: headline, pill, lines, chart, cards.
    fn layout(&mut self) {
        let mut blocks = Vec::new();
        let (l, r) = self.page_lr();
        let w = r - l;
        let mut y = 0;
        if let Some(p) = self.panels.get(self.sel) {
            let mdc = self.mdc;
            let m = |f: HFONT| move |s: &str| measure_dc(mdc, f, s).0;
            // Headline, wrapped.
            let hh = self.line_h(self.fonts.head);
            for s in wrap(&m(self.fonts.head), &p.headline, w - self.sc(4)) {
                blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Head, dim: false, s });
                y += hh + self.sc(2);
            }
            y += self.sc(8);
            blocks.push(Block::Pill { x: l + self.sc(2), y, tone: p.tone });
            y += self.sc(24) + self.sc(12);
            let bh = self.line_h(self.fonts.body);
            for line in &p.lines {
                for s in wrap(&m(self.fonts.body), line, w - self.sc(4)) {
                    blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Body, dim: true, s });
                    y += bh + self.sc(2);
                }
            }
            if p.chart.as_ref().is_some_and(|c| !c.points.is_empty()) {
                y += self.sc(16);
                blocks.push(Block::Chart { rect: Rect { l, t: y, r, b: y + self.sc(CHART_H) } });
                y += self.sc(CHART_H);
            }
            // Cards.
            y += self.sc(24);
            let sh = self.line_h(self.fonts.semi);
            blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Semi, dim: false, s: items_heading(p.key).into() });
            y += sh + self.sc(10);
            if p.items.is_empty() {
                blocks.push(Block::Text { x: l + self.sc(2), y, font: F::Body, dim: true, s: empty_text(p.key).into() });
                y += bh + self.sc(8);
            } else {
                let now = unix_ms();
                let times: Vec<String> = p.items.iter().map(|it| when(it.t_ms, now)).collect();
                let time_w = times.iter().map(|t| self.measure(self.fonts.small, t).0).max().unwrap_or(0);
                let pad = self.sc(CARD_PAD);
                let tx = l + self.sc(24) + if time_w > 0 { time_w + self.sc(16) } else { 0 };
                let text_w = (r - pad - tx).max(self.sc(80));
                let smh = self.line_h(self.fonts.small);
                for (it, time) in p.items.iter().zip(times) {
                    let detail = wrap(&m(self.fonts.small), &it.detail, text_w);
                    let h = pad * 2 - self.sc(4) + sh + detail.len() as i32 * smh + if detail.is_empty() { 0 } else { self.sc(2) };
                    let rect = Rect { l, t: y, r, b: y + h.max(self.sc(48)) };
                    y = rect.b + self.sc(4);
                    blocks.push(Block::Item { rect, tone: it.tone, time, title: it.title.clone(), detail, tx });
                }
            }
        }
        self.blocks = blocks;
        self.content_h = y + self.sc(24);
        let max = self.max_scroll();
        self.target = self.target.clamp(0, max);
        self.scroll = self.scroll.clamp(0.0, max as f32);
    }

    fn select(&mut self, i: usize) {
        if i >= self.panels.len() {
            return;
        }
        if i != self.sel {
            self.sel = i;
            self.scroll = 0.0;
            self.target = 0;
            // SAFETY: plain timer call on our window.
            unsafe { KillTimer(self.hwnd, T_SCROLL) };
            self.layout();
            self.refresh_hover();
        }
        invalidate(self.hwnd);
    }

    // -------------------------------------------------------------- scrolling

    fn scroll_to(&mut self, target: i32, animate: bool) {
        self.target = target.clamp(0, self.max_scroll());
        // SAFETY: plain timer calls on our window.
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
            // SAFETY: plain timer call on our window.
            unsafe { KillTimer(self.hwnd, T_SCROLL) };
        } else {
            self.scroll += d * 0.3;
        }
        self.refresh_hover();
        invalidate(self.hwnd);
    }

    fn refresh_hover(&mut self) {
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: valid out pointer; our window handle.
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(self.hwnd, &mut pt);
        }
        let inside = pt.x >= 0 && pt.y >= 0 && pt.x < self.cw && pt.y < self.ch;
        self.hover = if inside && self.thumb_grab.is_none() { self.hit(pt.x, pt.y) } else { None };
    }

    // -------------------------------------------------------------- painting

    fn paint(&mut self, dc: HDC) {
        // The canvas is reused; `resize` is a no-op unless the size changed.
        let mut cv = std::mem::replace(&mut self.cv, Canvas::new());
        cv.resize(self.cw, self.ch);
        self.draw(&mut cv);
        cv.blit(dc, 0, 0);
        self.cv = cv;
    }

    fn draw(&self, cv: &mut Canvas) {
        let p = self.pal;
        let f = &self.fonts;
        cv.clear(p.bg);
        if !self.enabled {
            self.draw_off(cv);
            return;
        }
        let (vt, vb) = self.view();
        let dy = vt - self.scroll_px();
        let rad = self.sc(6) as f32;

        for b in &self.blocks {
            match b {
                Block::Text { x, y, font, dim, s } => {
                    let y = y + dy;
                    let fh = self.font(*font);
                    if y > vb || y + self.sc(40) < vt {
                        continue;
                    }
                    let (_, r) = self.page_lr();
                    let s = cv.ellipsize(fh, s, r - x);
                    cv.text(fh, &s, *x, y, if *dim { p.dim } else { p.fg });
                }
                Block::Pill { x, y, tone } => self.draw_pill(cv, *x, y + dy, *tone),
                Block::Chart { rect } => {
                    let r = shift(*rect, dy);
                    if r.b < vt || r.t > vb {
                        continue;
                    }
                    if let Some(c) = self.panels.get(self.sel).and_then(|p| p.chart.as_ref()) {
                        self.draw_chart(cv, r, c);
                    }
                }
                Block::Item { rect, tone, time, title, detail, tx } => {
                    let r = shift(*rect, dy);
                    if r.b < vt || r.t > vb {
                        continue;
                    }
                    cv.fill_round(r, rad, p.card);
                    cv.stroke_round(r, rad, p.card_border);
                    let pad = self.sc(CARD_PAD);
                    let stripe = Rect { l: r.l + self.sc(8), t: r.t + self.sc(10), r: r.l + self.sc(11), b: r.b - self.sc(10) };
                    cv.fill_round(stripe, stripe.w() as f32 / 2.0, p.tone(*tone));
                    let ty = r.t + pad - self.sc(2);
                    let sh = self.line_h(f.semi);
                    if !time.is_empty() {
                        let th = self.line_h(f.small);
                        cv.text(f.small, time, r.l + self.sc(24), ty + (sh - th) / 2 + self.sc(1), p.dim);
                    }
                    let maxw = r.r - pad - tx;
                    let t = cv.ellipsize(f.semi, title, maxw);
                    cv.text(f.semi, &t, *tx, ty, p.fg);
                    let mut yy = ty + sh + self.sc(2);
                    let smh = self.line_h(f.small);
                    for d in detail {
                        let d = cv.ellipsize(f.small, d, maxw);
                        cv.text(f.small, &d, *tx, yy, p.dim);
                        yy += smh;
                    }
                }
            }
        }

        // Header strip (clips the scrolled content), then the page title.
        let px = self.sc(NAV_W);
        cv.clear_rect(Rect { l: px, t: 0, r: cv.w, b: vt }, p.bg);
        if let Some(pn) = self.panels.get(self.sel) {
            let th = cv.measure(f.title, pn.title).1;
            let t = cv.ellipsize(f.title, pn.title, self.page_lr().1 - self.page_lr().0);
            cv.text(f.title, &t, self.sc(NAV_W + PAGE_PAD_L + 2), (vt - th) / 2 + self.sc(2), p.fg);
        }

        // Rail: app title, items, footer note.
        cv.clear_rect(Rect { l: 0, t: 0, r: px, b: cv.h }, p.bg);
        cv.text(f.app, "Insights", self.sc(20), self.sc(18), p.fg);
        for (i, pn) in self.panels.iter().enumerate() {
            self.draw_nav(cv, i, pn);
        }
        let note_y = self.ch - self.sc(28);
        cv.text(f.small, "Nothing leaves this PC.", self.sc(20), note_y, p.dim);
        if demo() {
            cv.text(f.small, "Demo data", self.sc(20), note_y - self.line_h(f.small) - self.sc(2), p.accent);
        }
        if self.panels.is_empty() {
            let s = "Nothing to show yet.";
            cv.text(f.body, s, self.sc(NAV_W + PAGE_PAD_L + 2), vt, p.dim);
        }

        if let Some((track, thumb)) = self.scrollbar() {
            if self.hover == Some(Hit::Scrollbar) || self.thumb_grab.is_some() {
                let tr = Rect { l: thumb.l - self.sc(1), r: thumb.r + self.sc(1), t: track.t, b: track.b };
                cv.fill_round(tr, tr.w() as f32 / 2.0, p.sb_track);
            }
            cv.fill_round(thumb, thumb.w() as f32 / 2.0, p.sb);
        }
    }

    fn draw_nav(&self, cv: &mut Canvas, i: usize, pn: &Panel) {
        let p = self.pal;
        let f = &self.fonts;
        let r = self.nav_rect(i);
        let cy = (r.t + r.b) / 2;
        let rad4 = self.sc(4) as f32;
        let sel = i == self.sel;
        let hover = self.hover == Some(Hit::Nav(i));
        let fill = if hover && self.pressed == Some(Hit::Nav(i)) {
            Some(p.nav_press)
        } else if sel {
            Some(p.nav_sel)
        } else if hover {
            Some(p.nav_hover)
        } else {
            None
        };
        if let Some(c) = fill {
            cv.fill_round(r, rad4, c);
        }
        if sel {
            let (pw, ph) = (self.sc(3), self.sc(16));
            cv.fill_round(Rect { l: r.l, t: cy - ph / 2, r: r.l + pw, b: cy - ph / 2 + ph }, pw as f32 / 2.0, p.accent);
        }
        glyph(cv, f.icon, panel_glyph(pn.key), r.l + self.sc(24), cy, p.fg);
        let dot = self.sc(8);
        let dx = r.r - self.sc(14);
        let label = cv.ellipsize(f.body, pn.title, dx - dot - self.sc(8) - (r.l + self.sc(48)));
        text_mid(cv, f.body, &label, r.l + self.sc(48), cy, p.fg);
        let dr = Rect { l: dx - dot / 2, t: cy - dot / 2, r: dx - dot / 2 + dot, b: cy - dot / 2 + dot };
        cv.fill_round(dr, dot as f32 / 2.0, p.tone(pn.tone));
        if sel && self.focus_visible {
            ring(cv, r, rad4, p.fg);
        }
    }

    fn draw_pill(&self, cv: &mut Canvas, x: i32, y: i32, tone: Tone) {
        let p = self.pal;
        let f = &self.fonts;
        let c = p.tone(tone);
        let label = tone_label(tone);
        let (tw, th) = cv.measure(f.small, label);
        let h = self.sc(24);
        let dot = self.sc(8);
        let w = self.sc(10) + dot + self.sc(6) + tw + self.sc(12);
        let r = Rect { l: x, t: y, r: x + w, b: y + h };
        cv.fill_round(r, h as f32 / 2.0, c.with_a(if p.dark { 0x2E } else { 0x26 }));
        cv.stroke_round(r, h as f32 / 2.0, c.with_a(0x55));
        let cy = y + h / 2;
        let dl = x + self.sc(10);
        cv.fill_round(Rect { l: dl, t: cy - dot / 2, r: dl + dot, b: cy - dot / 2 + dot }, dot as f32 / 2.0, c);
        cv.text(f.small, label, dl + dot + self.sc(6), cy - th / 2, p.fg);
    }

    fn draw_chart(&self, cv: &mut Canvas, card: Rect, c: &Chart) {
        let p = self.pal;
        let f = &self.fonts;
        let rad = self.sc(6) as f32;
        cv.fill_round(card, rad, p.card);
        cv.stroke_round(card, rad, p.card_border);
        let pad = self.sc(CARD_PAD);
        let max = chart_max(c);
        let max_s = fmt_val(max, c.unit);
        let mw = cv.measure(f.small, &max_s).0;
        let label = cv.ellipsize(f.semi, &c.label, card.w() - pad * 3 - mw);
        cv.text(f.semi, &label, card.l + pad, card.t + pad - self.sc(2), p.fg);
        let pr = self.plot_rect(card);
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

        let n = c.points.len();
        let bars = use_bars(c);
        let val = |i: usize| c.points.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0).max(0.0);
        let y_of = |v: f32| pr.b as f32 - 1.0 - (v / max).clamp(0.0, 1.0) * (h - 2) as f32;
        let hover_i = match self.hover {
            Some(Hit::Chart(hx)) => index_at(hx - pr.l, w, n, bars),
            _ => None,
        };
        let acc = p.accent;
        let fill_a = if p.dark { 0x50 } else { 0x40 } as f32;

        if bars {
            let slot = w as f32 / n as f32;
            let bw = (slot * 0.62).max(self.sc(2) as f32).min(slot);
            for i in 0..n {
                let cx = pr.l as f32 + x_of(i, w, n, true);
                let top = y_of(val(i)).min(pr.b as f32 - 2.0);
                let (bl, brr) = ((cx - bw / 2.0).round() as i32, (cx + bw / 2.0).round() as i32);
                let br = Rect { l: bl, t: top.round() as i32, r: brr, b: pr.b - 1 };
                let hot = hover_i == Some(i);
                cv.fill_round(br, self.sc(2) as f32, if hot { acc } else { acc.with_a(fill_a as u8 + 0x30) });
                cv.fill_rect(Rect { b: br.t + self.sc(2), ..br }, acc);
            }
        } else {
            let th = (self.sc(2) as f32).max(1.5);
            let mut prev: Option<f32> = None;
            for col in 0..w {
                let pos = col as f32 * (n - 1) as f32 / (w - 1) as f32;
                let (a, b) = (val(pos.floor() as usize), val((pos.ceil() as usize).min(n - 1)));
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

        // Markers: dashed vertical line and a tiny label, skipped if it would overlap.
        let mut last_r = i32::MIN;
        for (idx, lab) in &c.markers {
            if *idx >= n {
                continue;
            }
            let mx = pr.l + x_of(*idx, w, n, bars).round() as i32;
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
            let v = val(i);
            let hy = y_of(v).round() as i32;
            if !bars {
                cv.fill_rect(Rect { l: hx, t: pr.t, r: hx + 1, b: pr.b }, p.dim.with_a(0xA0));
                let d = self.sc(8);
                cv.fill_round(Rect { l: hx - d / 2, t: hy - d / 2, r: hx - d / 2 + d, b: hy - d / 2 + d }, d as f32 / 2.0, acc);
            }
            let mut s = fmt_val(v, c.unit);
            if let Some((_, lab)) = c.markers.iter().find(|(m, _)| *m == i) {
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

    fn draw_off(&self, cv: &mut Canvas) {
        let p = self.pal;
        let f = &self.fonts;
        let (top, br) = self.off_layout();
        let msg = "Insights is off. Turn it on in Settings.";
        let msg = cv.ellipsize(f.head, msg, self.cw - self.sc(32));
        cv.text_center(f.head, &msg, self.cw / 2, top, p.fg);
        let hover = self.hover == Some(Hit::Button);
        let fill = if hover && self.pressed == Some(Hit::Button) {
            p.accent_press
        } else if hover {
            p.accent_hover
        } else {
            p.accent
        };
        let rad4 = self.sc(4) as f32;
        cv.fill_round(br, rad4, fill);
        let (tw, th) = cv.measure(f.body, "Open Settings");
        cv.text(f.body, "Open Settings", (br.l + br.r - tw) / 2, (br.t + br.b - th) / 2, p.on_accent);
        if self.focus_btn && self.focus_visible {
            ring(cv, inflate(br, self.sc(3)), rad4 + self.sc(3) as f32, p.fg);
        }
    }
}

// ------------------------------------------------------------------ drawing helpers

fn inflate(r: Rect, d: i32) -> Rect {
    Rect { l: r.l - d, t: r.t - d, r: r.r + d, b: r.b + d }
}

fn shift(r: Rect, dy: i32) -> Rect {
    Rect { t: r.t + dy, b: r.b + dy, ..r }
}

fn ring(cv: &mut Canvas, r: Rect, rad: f32, c: Rgba) {
    cv.stroke_round(r, rad, c);
    cv.stroke_round(inflate(r, -1), (rad - 1.0).max(0.0), c);
}

fn text_mid(cv: &mut Canvas, font: HFONT, s: &str, x: i32, cy: i32, c: Rgba) {
    let (_, th) = cv.measure(font, s);
    cv.text(font, s, x, cy - th / 2, c);
}

fn glyph(cv: &mut Canvas, font: HFONT, g: char, cx: i32, cy: i32, c: Rgba) {
    let mut buf = [0u8; 4];
    let s = g.encode_utf8(&mut buf);
    let (w, h) = cv.measure(font, s);
    cv.text(font, s, cx - w / 2, cy - h / 2, c);
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

fn invalidate(hwnd: HWND) {
    if !hwnd.is_null() {
        // SAFETY: our window handle; null rect = whole client.
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
    }
}

// ------------------------------------------------------------------ input

fn open_settings() {
    if let Some(a) = app() {
        crate::settings_ui::open_page(a, crate::settings_ui::PAGE_INSIGHTS);
    }
}

fn on_key(hwnd: HWND, key: u16) {
    // SAFETY: no preconditions.
    let shift_down = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
    let Some(u) = ui() else { return };
    if key == VK_ESCAPE {
        // SAFETY: our window.
        unsafe { DestroyWindow(hwnd) };
        return;
    }
    if !u.enabled {
        match key {
            VK_TAB => {
                u.focus_btn = !u.focus_btn;
                u.focus_visible = true;
            }
            VK_RETURN | VK_SPACE if u.focus_btn => {
                open_settings();
                return;
            }
            _ => {}
        }
        invalidate(hwnd);
        return;
    }
    let n = u.panels.len();
    let (vt, vb) = u.view();
    let page = (vb - vt - u.sc(WHEEL_STEP)).max(u.sc(WHEEL_STEP));
    match key {
        VK_UP | VK_DOWN | VK_TAB if n > 0 => {
            let back = key == VK_UP || (key == VK_TAB && shift_down);
            let next = if key == VK_TAB {
                if back {
                    (u.sel + n - 1) % n
                } else {
                    (u.sel + 1) % n
                }
            } else if back {
                u.sel.saturating_sub(1)
            } else {
                (u.sel + 1).min(n - 1)
            };
            u.focus_visible = true;
            u.select(next);
        }
        VK_RETURN | VK_SPACE => u.focus_visible = true,
        VK_PRIOR => u.scroll_to(u.target - page, true),
        VK_NEXT => u.scroll_to(u.target + page, true),
        VK_HOME => u.scroll_to(0, true),
        VK_END => u.scroll_to(u.max_scroll(), true),
        _ => {}
    }
    invalidate(hwnd);
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    if !u.tracking {
        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        // SAFETY: valid, sized struct for our window.
        u.tracking = unsafe { TrackMouseEvent(&mut t) } != 0;
    }
    if let Some(grab) = u.thumb_grab {
        if let Some((track, thumb)) = u.scrollbar() {
            let span = (track.h() - thumb.h()).max(1);
            let frac = ((y - grab - track.t) as f32 / span as f32).clamp(0.0, 1.0);
            let t = (frac * u.max_scroll() as f32).round() as i32;
            u.scroll_to(t, false);
        }
        return;
    }
    let h = u.hit(x, y);
    if h != u.hover {
        u.hover = h;
        invalidate(hwnd);
    }
}

fn on_button_down(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    u.focus_visible = false;
    let h = u.hit(x, y);
    u.hover = h;
    u.pressed = h;
    if h == Some(Hit::Scrollbar) {
        if let Some((_, thumb)) = u.scrollbar() {
            if y >= thumb.t && y < thumb.b {
                u.thumb_grab = Some(y - thumb.t);
            } else {
                let (vt, vb) = u.view();
                let page = (vb - vt - u.sc(WHEEL_STEP)).max(u.sc(WHEEL_STEP));
                let t = if y < thumb.t { u.target - page } else { u.target + page };
                u.scroll_to(t, true);
            }
        }
    }
    if h.is_some() {
        // SAFETY: our window.
        unsafe { SetCapture(hwnd) };
    }
    invalidate(hwnd);
}

fn on_button_up(hwnd: HWND, x: i32, y: i32) {
    let Some(u) = ui() else { return };
    let was = u.pressed.take();
    u.thumb_grab = None;
    // SAFETY: releases our own capture (WM_CAPTURECHANGED only clears state cleared above).
    unsafe { ReleaseCapture() };
    let Some(u) = ui() else { return };
    let h = u.hit(x, y);
    u.hover = h;
    invalidate(hwnd);
    match (was, h) {
        (Some(Hit::Nav(i)), Some(Hit::Nav(j))) if i == j => u.select(i),
        (Some(Hit::Button), Some(Hit::Button)) => open_settings(),
        _ => {}
    }
}

fn on_wheel(delta: i32) {
    let Some(u) = ui() else { return };
    if u.enabled {
        let t = u.target - delta * u.sc(WHEEL_STEP) / 120;
        u.scroll_to(t, true);
    }
}

// ------------------------------------------------------------------ window

pub fn register(hinst: HINSTANCE) {
    // SAFETY: valid class description; strings outlive the call.
    unsafe {
        let cls = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: LoadIconW(hinst, 1 as _),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
    }
}

const STYLE: u32 = WS_OVERLAPPEDWINDOW;

/// Open the Insights window, or bring the existing one to the front.
pub fn open(a: &mut crate::app::App) {
    if let Some(u) = ui() {
        let (on, panels) = fetch(a, unix_ms());
        u.take(on, panels);
        invalidate(u.hwnd);
        // SAFETY: our live window.
        unsafe {
            ShowWindow(u.hwnd, SW_RESTORE);
            SetForegroundWindow(u.hwnd);
        }
        return;
    }
    let tb = a.primary_taskbar();
    let dpi = tb.map_or(96, |t| t.dpi).max(96);
    let work = tb.map(|t| t.work).unwrap_or(Rect { l: 0, t: 0, r: 1280, b: 720 });
    let (on, panels) = fetch(a, unix_ms());
    let state = Box::new(Ui::new(dpi, a.m.dark, on, panels));
    // SAFETY: single UI thread; replaces a null pointer (checked above) with our box.
    unsafe { UI = Box::into_raw(state) };
    let sc = |v: i32| (v * dpi as i32 + 48) / 96;
    let mut rc = RECT { left: 0, top: 0, right: sc(CLIENT_W), bottom: sc(CLIENT_H) };
    // SAFETY: plain Win32 calls with valid arguments; strings outlive the calls.
    unsafe {
        AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi);
        let (w, h) = ((rc.right - rc.left).min(work.w()), (rc.bottom - rc.top).min(work.h()));
        let x = work.l + (work.w() - w) / 2;
        let y = work.t + ((work.h() - h) / 2).max(0);
        let cls = wide(CLASS);
        let title = wide(TITLE);
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            title.as_ptr(),
            STYLE,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            free_ui();
            return;
        }
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
    }
}

/// New data: repaint if the window is open (cheap no-op when closed).
/// Called on every app tick; rebuilds the panels at most every 2 s.
pub fn refresh() {
    let Some(u) = ui() else { return };
    // SAFETY: no preconditions.
    let now = unsafe { GetTickCount64() };
    if now.saturating_sub(u.built) < REBUILD_MS {
        return;
    }
    reload();
}

/// Rebuild the panels now (after Clear history, or the on/off setting changed).
pub fn reload() {
    let Some(u) = ui() else { return };
    let Some(a) = app() else { return };
    let (on, panels) = fetch(a, unix_ms());
    if u.take(on, panels) {
        u.refresh_hover();
        invalidate(u.hwnd);
    }
}

pub fn set_dark(dark: bool) {
    let Some(u) = ui() else { return };
    if u.dark != dark {
        u.dark = dark;
        u.set_palette();
        style_window(u.hwnd, dark);
        invalidate(u.hwnd);
    }
    // Also called after every settings change: Insights may have been turned on or off.
    reload();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let Some(u) = ui() else { return -1 };
            u.hwnd = hwnd;
            let dpi = GetDpiForWindow(hwnd);
            if dpi != 0 && dpi != u.dpi {
                u.set_dpi(dpi);
            }
            u.mica = crate::settings_ui::enable_mica(hwnd);
            u.set_palette();
            style_window(hwnd, u.dark);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            u.cw = rc.right;
            u.ch = rc.bottom;
            u.layout();
            0
        }
        WM_SIZE => {
            if let Some(u) = ui() {
                u.cw = (l & 0xFFFF) as i32;
                u.ch = ((l >> 16) & 0xFFFF) as i32;
                u.layout();
            }
            invalidate(hwnd);
            0
        }
        WM_GETMINMAXINFO => {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let sc = |v: i32| (v * dpi as i32 + 48) / 96;
            let mut rc = RECT { left: 0, top: 0, right: sc(MIN_W), bottom: sc(MIN_H) };
            AdjustWindowRectExForDpi(&mut rc, STYLE, 0, 0, dpi);
            let mm = &mut *(l as *mut MINMAXINFO);
            mm.ptMinTrackSize = POINT { x: rc.right - rc.left, y: rc.bottom - rc.top };
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
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, get_x(l), get_y(l));
            0
        }
        WM_MOUSELEAVE => {
            if let Some(u) = ui() {
                u.tracking = false;
                if u.thumb_grab.is_none() && u.hover.is_some() {
                    u.hover = None;
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
                if u.pressed.is_some() || u.thumb_grab.is_some() {
                    u.pressed = None;
                    u.thumb_grab = None;
                    invalidate(hwnd);
                }
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
            if w == T_SCROLL {
                if let Some(u) = ui() {
                    u.scroll_tick();
                }
            }
            0
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            if let Some(u) = ui() {
                u.set_palette();
                invalidate(hwnd);
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_DPICHANGED => {
            let dpi = ((w >> 16) & 0xFFFF) as u32;
            let r = &*(l as *const RECT);
            if let Some(u) = ui() {
                u.set_dpi(dpi);
            }
            // WM_SIZE lays out again with the new fonts.
            SetWindowPos(hwnd, std::ptr::null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(u) = ui() {
                u.layout();
            }
            invalidate(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, T_SCROLL);
            0
        }
        WM_NCDESTROY => {
            free_ui();
            DefWindowProcW(hwnd, msg, w, l)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
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
        for &(n, bars) in &[(120usize, false), (21, true), (1, true), (2, false)] {
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
        assert!(use_bars(p[3].chart.as_ref().unwrap()));
        assert!(!use_bars(p[0].chart.as_ref().unwrap()));
    }
}
