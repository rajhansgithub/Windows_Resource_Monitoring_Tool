//! Incident Cards. OWNER: Incident Cards worker.
//!
//! Spikes the user may have missed, written down with their cause:
//! "14:02. CPU held at 97% for 3 min. MsMpEng 38%, disk 100%."
//!
//! Per tick (`on_sample`) we only run a few threshold checks on the tick just pushed to the
//! ring (`Hold::step` per condition, O(1)). While any condition is pending or active,
//! `wants_procs()` asks the app for process lists so the ring holds a culprit. When every
//! condition has ended (plus a short linger), the overlapping conditions become one card;
//! only then do we aggregate peaks and culprits over the ring. Event Log records (driver
//! resets, hardware errors, crashes) become cards at once, fused with what the ring saw.

use super::events::SysEvent;
use super::ring::{flag, Field, Ring, Tick, NONE16, NONE8};
use super::store;
use super::{time, Chart, Ctx, Item, Notice, Panel, Tone};
use crate::achievements::json::Json;
use std::cell::Cell;
use std::collections::VecDeque;

// Thresholds (seconds / percent).
const CPU_HIGH: u8 = 85;
const CPU_NEED_S: u32 = 30;
const DISK_HIGH: u8 = 95;
const DISK_NEED_S: u32 = 20;
const RAM_HIGH: u8 = 95;
const RAM_NEED_S: u32 = 10;
const RAM_RISE: u8 = 20;
const RAM_RISE_WINDOW_S: u32 = 60;
const FAN_RISE: f32 = 1.4;
/// Below this the fan is treated as "barely spinning" (zero-RPM fans idle at 0).
const FAN_FLOOR_RPM: f32 = 600.0;
const FAN_NEED_S: u32 = 60;
const FAN_WINDOW: usize = 300;
const FAN_MIN_SAMPLES: usize = 60;
const FAN_BASE_EVERY_S: u32 = 30;
const GPU_HOT_C: u8 = 87;
const GPU_HOT_NEED_S: u32 = 30;
const LATE_MS: u32 = 2000;
/// A condition may drop out this long without ending (one noisy sample).
const GRACE_S: u32 = 3;
/// After the last condition ends, wait this long (and keep sampling processes) before
/// writing the card, so a culprit is caught right after a freeze.
const LINGER_S: u32 = 5;
const COOLDOWN_S: u32 = 10 * 60;
const AWAY_TOAST_MIN_S: u32 = 120;
const TOASTS_PER_DAY: u32 = 3;
const KEEP_MS: u64 = 7 * 86_400_000;
const MAX_CARDS: usize = 60;
const DAY_MS: u64 = 86_400_000;

/// What a card is about. The first six are load conditions from the ring (a bit mask in
/// `Card::mask`); the rest come from the Event Log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Cpu = 0,
    Disk = 1,
    Ram = 2,
    Fan = 3,
    GpuHot = 4,
    Stutter = 5,
    DriverReset = 6,
    HwError = 7,
    HwFatal = 8,
    Crash = 9,
    Hang = 10,
    Unclean = 11,
    DiskError = 12,
}

const LOAD: [Kind; 6] = [Kind::Cpu, Kind::Disk, Kind::Ram, Kind::Fan, Kind::GpuHot, Kind::Stutter];
/// Which load kind leads a merged card.
const PRIORITY: [Kind; 6] = [Kind::Stutter, Kind::Cpu, Kind::Disk, Kind::Ram, Kind::GpuHot, Kind::Fan];
const ALL: [Kind; 13] = [
    Kind::Cpu,
    Kind::Disk,
    Kind::Ram,
    Kind::Fan,
    Kind::GpuHot,
    Kind::Stutter,
    Kind::DriverReset,
    Kind::HwError,
    Kind::HwFatal,
    Kind::Crash,
    Kind::Hang,
    Kind::Unclean,
    Kind::DiskError,
];

impl Kind {
    fn bit(self) -> u8 {
        1 << (self as u8)
    }
    fn from_u8(v: u8) -> Option<Kind> {
        ALL.get(v as usize).copied()
    }
    /// Short chart-marker label.
    fn short(self) -> &'static str {
        match self {
            Kind::Cpu => "CPU",
            Kind::Disk => "Disk",
            Kind::Ram => "RAM",
            Kind::Fan => "Fan",
            Kind::GpuHot => "Hot",
            Kind::Stutter => "Froze",
            Kind::DriverReset => "GPU",
            Kind::HwError | Kind::HwFatal => "HW",
            Kind::Crash => "Crash",
            Kind::Hang => "Hang",
            Kind::Unclean => "Restart",
            Kind::DiskError => "Disk",
        }
    }
}

/// One finished incident.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// Start, unix ms.
    pub t_ms: u64,
    pub dur_s: u32,
    /// Leading kind.
    pub kind: Kind,
    /// All load kinds that were part of it.
    pub mask: u8,
    /// Hardware error / crash (vs. plain load).
    pub bad: bool,
    pub away: bool,
    /// Main sentence without time and final period: "CPU held at 97% for 3 min".
    pub what: String,
    /// Following sentence(s): "MsMpEng 38%, disk 100%."
    pub more: String,
    pub detail: String,
}

impl Card {
    /// "14:02. CPU held at 97% for 3 min. MsMpEng 38%, disk 100%."
    pub fn title(&self) -> String {
        join_sentence(&time::hhmm(self.t_ms), &self.what, &self.more)
    }
}

fn join_sentence(hh: &str, what: &str, more: &str) -> String {
    if more.is_empty() {
        format!("{}. {}.", hh, what)
    } else {
        format!("{}. {}. {}", hh, what, more)
    }
}

/// A threshold that must hold for a while. Times are unix seconds.
#[derive(Clone, Copy, Default, Debug)]
struct Hold {
    since: Option<u32>,
    last_true: u32,
    active: bool,
}

#[derive(Debug, PartialEq)]
enum Step {
    None,
    Began(u32),
    Ended(u32, u32),
}

impl Hold {
    /// Feed one sample. `instant` makes the condition active at once. Each true sample
    /// counts as one second, so `need` = 30 means 30 samples in a row at the 1 s interval.
    fn step(&mut self, t: u32, cond: bool, need: u32, instant: bool) -> Step {
        if cond {
            let since = *self.since.get_or_insert(t);
            self.last_true = t;
            if !self.active && (instant || t.saturating_sub(since) + 1 >= need) {
                self.active = true;
                return Step::Began(since);
            }
            Step::None
        } else if self.since.is_some() && t.saturating_sub(self.last_true) > GRACE_S {
            self.end()
        } else {
            Step::None
        }
    }

    /// Stop now (condition gone, or the PC slept).
    fn end(&mut self) -> Step {
        let h = std::mem::take(self);
        match h.since {
            Some(s) if h.active => Step::Ended(s, h.last_true),
            _ => Step::None,
        }
    }

    fn busy(&self) -> bool {
        self.since.is_some()
    }
}

/// An incident being collected (one or more overlapping conditions).
#[derive(Clone, Debug, Default)]
struct Draft {
    start_s: u32,
    end_s: u32,
    mask: u8,
    /// Per load kind: (start, end) seconds; (0, 0) = not part of it.
    spans: [(u32, u32); 6],
    /// Longest freeze (late tick) in this incident, ms, and that tick.
    freeze_ms: u32,
    freeze_tick: Option<Tick>,
}

impl Draft {
    fn add(&mut self, k: Kind, start: u32, now: u32) {
        if self.mask == 0 {
            self.start_s = start;
        }
        self.mask |= k.bit();
        self.start_s = self.start_s.min(start);
        self.end_s = self.end_s.max(now);
        let sp = &mut self.spans[k as usize];
        if *sp == (0, 0) {
            *sp = (start, now);
        } else {
            sp.0 = sp.0.min(start);
            sp.1 = sp.1.max(now);
        }
    }
    fn touch(&mut self, k: Kind, now: u32) {
        if self.mask & k.bit() != 0 {
            self.spans[k as usize].1 = self.spans[k as usize].1.max(now);
            self.end_s = self.end_s.max(now);
        }
    }
    fn has(&self, k: Kind) -> bool {
        self.mask & k.bit() != 0
    }
}

/// What the ring saw over a draft's span.
#[derive(Clone, Debug, Default)]
struct Facts {
    cpu: u8,
    ram: u8,
    disk: Option<u8>,
    gpu: Option<u8>,
    temp: Option<u8>,
    fan: Option<u16>,
    away: bool,
    top: Vec<(String, f32)>,
    io: Option<String>,
}

fn facts(ring: &Ring, from_s: u32, to_s: u32) -> Facts {
    let (from_ms, to_ms) = (from_s as u64 * 1000, to_s as u64 * 1000);
    let mut f = Facts::default();
    let (mut n, mut idle) = (0u32, 0u32);
    for t in ring.span(from_ms, to_ms) {
        n += 1;
        if t.has(flag::IDLE) {
            idle += 1;
        }
        f.cpu = f.cpu.max(t.cpu);
        f.ram = f.ram.max(t.ram);
        let mx = |a: Option<u8>, v: u8| if v == NONE8 { a } else { Some(a.map_or(v, |a| a.max(v))) };
        f.disk = mx(f.disk, t.disk);
        f.gpu = mx(f.gpu, t.gpu);
        f.temp = mx(f.temp, t.gpu_temp);
        if t.fan_rpm != NONE16 {
            f.fan = Some(f.fan.map_or(t.fan_rpm, |a| a.max(t.fan_rpm)));
        }
    }
    f.away = n > 0 && idle * 2 > n;
    f.top = ring.top_cpu(from_ms, to_ms, 3).into_iter().filter(|(_, v)| *v >= 1.0).collect();
    f.io = ring.top_io(from_ms, to_ms).map(|(n, _)| n);
    f
}

/// Length of a condition span: its samples are one second each, so first..last inclusive.
fn span_s(sp: (u32, u32)) -> u32 {
    sp.1.saturating_sub(sp.0) + 1
}

/// "45 s", "3 min", "2 h", "1.5 h".
fn dur(s: u32) -> String {
    if s < 60 {
        format!("{} s", s.max(1))
    } else if s < 90 * 60 {
        format!("{} min", ((s + 30) / 60).max(1))
    } else {
        let half_h = (s + 900) / 1800;
        if half_h % 2 == 0 {
            format!("{} h", half_h / 2)
        } else {
            format!("{}.5 h", half_h / 2)
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn cut(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut n = max;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
    s
}

/// Turn a finished draft into card text. Pure: used by tests directly.
fn card_text(primary: Kind, d: &Draft, f: &Facts) -> (String, String, String) {
    let span = d.spans[primary as usize];
    let pdur = span_s(span);
    let disk = f.disk.unwrap_or(0);
    let what = match primary {
        Kind::Cpu => format!("CPU held at {}% for {}", f.cpu, dur(pdur)),
        Kind::Disk => format!("Disk at {}% for {}", disk, dur(pdur)),
        Kind::Ram if f.ram >= RAM_HIGH => format!("RAM at {}% for {}", f.ram, dur(pdur)),
        Kind::Ram => format!("RAM jumped to {}%", f.ram),
        Kind::Fan => match f.fan {
            Some(r) => format!("GPU fan up to {} rpm for {}", r, dur(pdur)),
            None => format!("GPU fan up for {}", dur(pdur)),
        },
        Kind::GpuHot => format!("GPU hot at {} °C for {}", f.temp.unwrap_or(GPU_HOT_C), dur(pdur)),
        _ => format!("Froze for {} s", ((d.freeze_ms + 500) / 1000).max(2)),
    };

    // Other conditions in the same card, as short facts.
    let with_dur = primary == Kind::Stutter;
    let kdur = |k: Kind| {
        let sp = d.spans[k as usize];
        if with_dur {
            format!(" for {}", dur(span_s(sp)))
        } else {
            String::new()
        }
    };
    let mut fx: Vec<String> = Vec::new();
    for k in PRIORITY.iter().copied().filter(|&k| k != primary && d.has(k)) {
        fx.push(match k {
            Kind::Cpu => format!("CPU {}%{}", f.cpu, kdur(k)),
            Kind::Disk => format!("disk {}%{}", disk, kdur(k)),
            Kind::Ram => format!("RAM {}%", f.ram),
            Kind::GpuHot => format!("GPU {} °C", f.temp.unwrap_or(GPU_HOT_C)),
            Kind::Fan => "GPU fan up".to_string(),
            _ => format!("froze for {} s", ((d.freeze_ms + 500) / 1000).max(2)),
        });
    }
    // A freeze alone: say what was busy in that moment.
    if let (Kind::Stutter, Some(t)) = (primary, d.freeze_tick) {
        if !d.has(Kind::Disk) && t.disk != NONE8 && t.disk >= 90 {
            fx.push(format!("disk {}%", t.disk));
        }
        if !d.has(Kind::Cpu) && t.cpu >= CPU_HIGH {
            fx.push(format!("CPU {}%", t.cpu));
        }
        if !d.has(Kind::Ram) && t.ram >= 90 {
            fx.push(format!("RAM {}%", t.ram));
        }
    }
    let disk_busy = d.has(Kind::Disk) || fx.iter().any(|s| s.starts_with("disk"));
    let cpu_name = f.top.first().filter(|(_, v)| *v >= 5.0);
    let io_name = f.io.as_ref().filter(|_| disk_busy);

    let mut parts: Vec<String> = Vec::new();
    match primary {
        Kind::Cpu => {
            if let Some((n, v)) = cpu_name {
                parts.push(format!("{} {}%", n, v.round()));
            }
            parts.extend(fx);
            if let Some(io) = io_name.filter(|io| cpu_name.map_or(true, |(n, _)| n != *io)) {
                parts.push(io.clone());
            }
        }
        _ => {
            parts.extend(fx);
            match (io_name, cpu_name) {
                (Some(io), _) => parts.push(io.clone()),
                (None, Some((n, v))) => parts.push(format!("{} {}% CPU", n, v.round())),
                _ => {}
            }
        }
    }
    let mut more = if parts.is_empty() { String::new() } else { capitalize(&parts.join(", ")) + "." };
    if f.away && primary != Kind::Stutter {
        if !more.is_empty() {
            more.push(' ');
        }
        more.push_str("While you were away.");
    }

    // Detail: peaks and the full culprit list.
    let mut peaks = vec![format!("CPU {}%", f.cpu), format!("RAM {}%", f.ram)];
    if let Some(v) = f.disk {
        peaks.push(format!("disk {}%", v));
    }
    if let Some(v) = f.gpu {
        peaks.push(format!("GPU {}%", v));
    }
    if let Some(v) = f.temp {
        peaks.push(format!("GPU {} °C", v));
    }
    if let Some(v) = f.fan {
        peaks.push(format!("fan {} rpm", v));
    }
    let mut detail = format!("Peaks: {}.", peaks.join(", "));
    if !f.top.is_empty() {
        let t: Vec<String> = f.top.iter().map(|(n, v)| format!("{} {}%", n, v.round())).collect();
        detail.push_str(&format!(" Top CPU: {}.", t.join(", ")));
    }
    if let Some(io) = &f.io {
        detail.push_str(&format!(" Top disk: {}.", io));
    }
    (what, more, cut(detail, 200))
}

/// Map an Event Log record to (kind, sentence, cooldown key). None = not a card.
fn classify(e: &SysEvent) -> Option<(Kind, String, String)> {
    let app = || {
        let raw = e.get("AppName").or_else(|| e.get("0")).unwrap_or("").trim();
        let n = raw.strip_suffix(".exe").or_else(|| raw.strip_suffix(".EXE")).unwrap_or(raw);
        cut(if n.is_empty() { "An app".to_string() } else { n.to_string() }, 40)
    };
    let (k, what) = match (e.provider.as_str(), e.id) {
        ("Display", 4101) => (Kind::DriverReset, "GPU driver reset".to_string()),
        ("Microsoft-Windows-WHEA-Logger", 18) => (Kind::HwFatal, "Fatal hardware error".to_string()),
        ("Microsoft-Windows-WHEA-Logger", 17 | 19 | 47) => (Kind::HwError, "Hardware error corrected".to_string()),
        ("Application Error", 1000) => (Kind::Crash, format!("{} crashed", app())),
        ("Application Hang", 1002) => (Kind::Hang, format!("{} stopped responding", app())),
        ("Microsoft-Windows-Kernel-Power", 41) => (Kind::Unclean, "Restarted without a clean shutdown".to_string()),
        ("disk", 7) => (Kind::DiskError, "Disk error, bad block".to_string()),
        ("disk", 51) => (Kind::DiskError, "Disk error while paging".to_string()),
        ("disk", 153) => (Kind::DiskError, "Disk had to retry".to_string()),
        _ => return None,
    };
    let key = if matches!(k, Kind::Crash | Kind::Hang) { format!("{}:{}", k as u8, what) } else { (k as u8).to_string() };
    Some((k, what, key))
}

/// Event kinds that may raise a tray notice.
fn event_toasts(k: Kind) -> bool {
    matches!(k, Kind::DriverReset | Kind::HwFatal | Kind::Crash | Kind::Unclean)
}

pub struct Incidents {
    cards: Vec<Card>,
    holds: [Hold; 6],
    draft: Option<Draft>,
    /// End (unix s) of the last card per kind, for the 10 min cooldown.
    last_end: [Option<u32>; 13],
    /// Event cooldown keys (crash per app) -> event time, unix s.
    ev_cool: Vec<(String, u32)>,
    /// (t, ram %) increasing minima over the last minute, for the RAM-rise check.
    ram_min: VecDeque<(u32, u8)>,
    fan_base: Option<f32>,
    fan_base_at: u32,
    /// Ignore the next tick for freeze detection (right after a resume / display on).
    skip_next: bool,
    /// Notices wait for a calm tick (never shown in the middle of a freeze).
    pending: Vec<Notice>,
    toast_day: u32,
    toast_n: u32,
    /// Per-minute CPU max for the last hour (chart): (minute number, value).
    minutes: [(u32, u8); 60],
    dirty: Cell<bool>,
}

impl Incidents {
    pub fn new() -> Incidents {
        Incidents {
            cards: Vec::new(),
            holds: [Hold::default(); 6],
            draft: None,
            last_end: [None; 13],
            ev_cool: Vec::new(),
            ram_min: VecDeque::new(),
            fan_base: None,
            fan_base_at: 0,
            skip_next: true,
            pending: Vec::new(),
            toast_day: 0,
            toast_n: 0,
            minutes: [(u32::MAX, 0); 60],
            dirty: Cell::new(false),
        }
    }

    /// Per tick, right after `ring.push(c)`. A handful of comparisons; ring aggregation
    /// only when a card is written.
    pub fn on_sample(&mut self, c: &Ctx, ring: &Ring) -> Vec<Notice> {
        let Some(&t) = ring.last() else { return Vec::new() };
        let prev = ring.prev().copied();
        let now = t.t_s;
        self.chart_point(&t);

        if t.has(flag::GAP) {
            // The PC slept: conditions end at their last true sample; no freeze.
            for k in LOAD {
                let st = self.holds[k as usize].end();
                self.apply(k, st, now);
            }
            self.ram_min.clear();
            self.skip_next = true;
        } else {
            self.check_load(&t, now, ring);
            self.check_freeze(&t, prev);
        }
        self.maybe_finalize(now, ring);
        self.flush_notices(c.now_ms, &t)
    }

    fn check_load(&mut self, t: &Tick, now: u32, ring: &Ring) {
        let disk = Tick::opt8(t.disk);
        let temp = Tick::opt8(t.gpu_temp);
        let rise = self.ram_rise(now, t.ram);
        let fan_up = self.fan_up(t, now, ring);
        let steps = [
            (Kind::Cpu, t.cpu >= CPU_HIGH, CPU_NEED_S, false),
            (Kind::Disk, disk.is_some_and(|d| d >= DISK_HIGH), DISK_NEED_S, false),
            (Kind::Ram, t.ram >= RAM_HIGH || rise, RAM_NEED_S, rise),
            (Kind::Fan, fan_up, FAN_NEED_S, false),
            (Kind::GpuHot, temp.is_some_and(|v| v >= GPU_HOT_C), GPU_HOT_NEED_S, false),
        ];
        for (k, cond, need, instant) in steps {
            let st = self.holds[k as usize].step(now, cond, need, instant);
            self.apply(k, st, now);
        }
    }

    fn apply(&mut self, k: Kind, st: Step, now: u32) {
        match st {
            Step::Began(since) => self.draft.get_or_insert_with(Draft::default).add(k, since, now),
            Step::Ended(_, last) => {
                if let Some(d) = &mut self.draft {
                    d.touch(k, last);
                }
            }
            Step::None => {
                let h = self.holds[k as usize];
                if h.active {
                    if let Some(d) = &mut self.draft {
                        d.touch(k, h.last_true);
                    }
                }
            }
        }
    }

    /// RAM rose by `RAM_RISE` points within the last minute (monotonic min queue, O(1) amortised).
    fn ram_rise(&mut self, now: u32, ram: u8) -> bool {
        while self.ram_min.back().is_some_and(|&(_, v)| v >= ram) {
            self.ram_min.pop_back();
        }
        if self.ram_min.len() >= 128 {
            self.ram_min.pop_front();
        }
        self.ram_min.push_back((now, ram));
        while self.ram_min.front().is_some_and(|&(t, _)| now.saturating_sub(t) > RAM_RISE_WINDOW_S) {
            self.ram_min.pop_front();
        }
        self.ram_min.front().is_some_and(|&(_, m)| ram.saturating_sub(m) >= RAM_RISE)
    }

    /// Fan at least 40% over its 5 min median. The median is refreshed every 30 s, and not
    /// while the fan condition is running (so a long spin-up does not become the new normal).
    fn fan_up(&mut self, t: &Tick, now: u32, ring: &Ring) -> bool {
        if t.fan_rpm == NONE16 {
            return false;
        }
        let busy = self.holds[Kind::Fan as usize].busy();
        if !busy && (self.fan_base.is_none() || now.saturating_sub(self.fan_base_at) >= FAN_BASE_EVERY_S) {
            self.fan_base_at = now;
            self.fan_base = fan_median(ring);
        }
        match self.fan_base {
            Some(b) => t.fan_rpm as f32 >= b.max(FAN_FLOOR_RPM) * FAN_RISE,
            None => false,
        }
    }

    fn check_freeze(&mut self, t: &Tick, prev: Option<Tick>) {
        let on = t.has(flag::DISPLAY);
        let just_on = on && !prev.is_some_and(|p| p.has(flag::DISPLAY));
        if just_on || prev.is_none() {
            self.skip_next = true;
            return;
        }
        if std::mem::take(&mut self.skip_next) || !on || t.late_ms < LATE_MS {
            return;
        }
        let now = t.t_s;
        let start = now.saturating_sub((t.late_ms + 999) / 1000);
        let d = self.draft.get_or_insert_with(Draft::default);
        d.add(Kind::Stutter, start, now);
        if t.late_ms >= d.freeze_ms {
            d.freeze_ms = t.late_ms;
            d.freeze_tick = Some(*t);
        }
    }

    fn maybe_finalize(&mut self, now: u32, ring: &Ring) {
        let Some(d) = &self.draft else { return };
        if self.holds.iter().any(|h| h.busy()) || now.saturating_sub(d.end_s) < LINGER_S {
            return;
        }
        let d = self.draft.take().unwrap_or_default();
        let fresh: Vec<Kind> = LOAD
            .iter()
            .copied()
            .filter(|&k| d.has(k) && self.last_end[k as usize].map_or(true, |e| d.start_s.saturating_sub(e) >= COOLDOWN_S))
            .collect();
        for k in LOAD.iter().copied().filter(|&k| d.has(k)) {
            self.last_end[k as usize] = Some(d.end_s);
        }
        let Some(primary) = PRIORITY.iter().copied().find(|k| fresh.contains(k)) else { return };
        // Culprits over the incident itself; for a freeze also the linger after it (the
        // process lists only start right after the late tick).
        let to = if d.has(Kind::Stutter) { now } else { d.end_s };
        let f = facts(ring, d.start_s, to);
        let (what, more, detail) = card_text(primary, &d, &f);
        let span = d.spans[primary as usize];
        let dur_s = if primary == Kind::Stutter { (d.freeze_ms + 500) / 1000 } else { span_s(span) };
        let card = Card {
            t_ms: d.start_s as u64 * 1000,
            dur_s,
            kind: primary,
            mask: d.mask,
            bad: false,
            away: f.away,
            what,
            more,
            detail,
        };
        if card.away && primary != Kind::Stutter && dur_s >= AWAY_TOAST_MIN_S {
            self.queue(Notice { key: "incidents", title: "While you were away".into(), text: card.title() });
        }
        self.add_card(card);
    }

    fn add_card(&mut self, card: Card) {
        let cutoff = card.t_ms.saturating_sub(KEEP_MS);
        self.cards.retain(|c| c.t_ms >= cutoff);
        let at = self.cards.iter().position(|c| c.t_ms > card.t_ms).unwrap_or(self.cards.len());
        self.cards.insert(at, card);
        if self.cards.len() > MAX_CARDS {
            let extra = self.cards.len() - MAX_CARDS;
            self.cards.drain(..extra);
        }
        self.dirty.set(true);
    }

    fn queue(&mut self, n: Notice) {
        if self.pending.len() < 5 {
            self.pending.push(n);
        }
    }

    /// Hand out queued notices on a calm tick, at most `TOASTS_PER_DAY` per local day.
    fn flush_notices(&mut self, now_ms: u64, t: &Tick) -> Vec<Notice> {
        if self.pending.is_empty() || t.late_ms >= LATE_MS {
            return Vec::new();
        }
        let day = time::day(now_ms);
        if day != self.toast_day {
            self.toast_day = day;
            self.toast_n = 0;
        }
        let room = TOASTS_PER_DAY.saturating_sub(self.toast_n) as usize;
        let mut out = std::mem::take(&mut self.pending);
        out.truncate(room);
        if !out.is_empty() {
            self.toast_n += out.len() as u32;
            self.dirty.set(true);
        }
        out
    }

    fn chart_point(&mut self, t: &Tick) {
        let m = t.t_s / 60;
        let slot = &mut self.minutes[(m % 60) as usize];
        if slot.0 != m {
            *slot = (m, 0);
        }
        slot.1 = slot.1.max(t.cpu);
    }

    /// An Event Log record: a card at once, fused with what the ring saw at that moment.
    pub fn on_event(&mut self, e: &SysEvent, ring: &Ring, now_ms: u64) -> Vec<Notice> {
        let Some((k, what, key)) = classify(e) else { return Vec::new() };
        let t_ms = if e.t_ms == 0 { now_ms } else { e.t_ms };
        if now_ms.saturating_sub(t_ms) > KEEP_MS {
            return Vec::new();
        }
        if self.cards.iter().any(|c| c.kind == k && c.t_ms == t_ms) {
            return Vec::new(); // replayed (boot events are re-read on start)
        }
        let t_s = (t_ms / 1000) as u32;
        if let Some(i) = self.ev_cool.iter().position(|(n, _)| *n == key) {
            if t_s.saturating_sub(self.ev_cool[i].1) < COOLDOWN_S && t_s >= self.ev_cool[i].1 {
                return Vec::new();
            }
            self.ev_cool[i].1 = t_s;
        } else {
            if self.ev_cool.len() >= 16 {
                self.ev_cool.remove(0);
            }
            self.ev_cool.push((key, t_s));
        }
        let (more, detail) = event_context(k, ring, t_ms);
        let card = Card {
            t_ms,
            dur_s: 0,
            kind: k,
            mask: 0,
            bad: k != Kind::Hang,
            away: false,
            what,
            more,
            detail: cut(detail, 200),
        };
        let notice = (event_toasts(k) && now_ms.saturating_sub(t_ms) <= DAY_MS)
            .then(|| Notice { key: "incidents", title: card.what.clone(), text: card.title() });
        self.last_end[k as usize] = Some(t_s);
        self.add_card(card);
        // Only toast a card that was kept: an old one dropped at once by the 60-card cap is
        // not in the list, so a boot-event replay would otherwise toast it on every start.
        if let Some(n) = notice.filter(|_| self.cards.iter().any(|c| c.kind == k && c.t_ms == t_ms)) {
            self.queue(n);
        }
        Vec::new()
    }

    /// True while any condition is pending or active (or a card is about to be written),
    /// so the app samples the processes that will be named as the culprit.
    pub fn wants_procs(&self) -> bool {
        self.draft.is_some() || self.holds.iter().any(|h| h.busy())
    }

    /// Persistent state changed since the last save.
    pub fn dirty(&self) -> bool {
        self.dirty.get()
    }

    pub fn save(&self) -> Json {
        self.dirty.set(false);
        let cards = self
            .cards
            .iter()
            .map(|c| {
                store::obj(vec![
                    ("t", Json::Num(c.t_ms as f64)),
                    ("d", Json::Num(c.dur_s as f64)),
                    ("k", Json::Num(c.kind as u8 as f64)),
                    ("m", Json::Num(c.mask as f64)),
                    ("b", Json::Bool(c.bad)),
                    ("a", Json::Bool(c.away)),
                    ("w", Json::Str(c.what.clone())),
                    ("s", Json::Str(c.more.clone())),
                    ("x", Json::Str(c.detail.clone())),
                ])
            })
            .collect();
        store::obj(vec![
            ("cards", Json::Arr(cards)),
            ("td", Json::Num(self.toast_day as f64)),
            ("tn", Json::Num(self.toast_n as f64)),
        ])
    }

    pub fn load(&mut self, j: &Json) {
        let now = super::unix_ms();
        let mut cards: Vec<Card> = j
            .get("cards")
            .and_then(|a| a.as_arr())
            .unwrap_or(&[])
            .iter()
            .filter_map(card_from_json)
            .filter(|c| c.t_ms.saturating_add(KEEP_MS) >= now)
            .collect();
        cards.sort_by_key(|c| c.t_ms);
        let extra = cards.len().saturating_sub(MAX_CARDS);
        cards.drain(..extra);
        for c in &cards {
            let end = ((c.t_ms / 1000) as u32).saturating_add(c.dur_s);
            let e = &mut self.last_end[c.kind as usize];
            *e = Some(e.map_or(end, |x| x.max(end)));
        }
        self.cards = cards;
        self.toast_day = store::num(j, "td").map_or(0, |v| v.clamp(0.0, u32::MAX as f64) as u32);
        self.toast_n = store::num(j, "tn").map_or(0, |v| v.clamp(0.0, 100.0) as u32);
    }

    pub fn panel(&self, now_ms: u64) -> Panel {
        let recent: Vec<&Card> = self.cards.iter().filter(|c| c.t_ms.saturating_add(DAY_MS) >= now_ms).collect();
        let today = time::day(now_ms);
        let n_today = recent.iter().filter(|c| time::day(c.t_ms) == today).count();
        let (tone, headline) = match recent.last() {
            None => (Tone::Good, "Quiet. Nothing odd in the last 24 hours.".to_string()),
            Some(last) => {
                let lead = if n_today > 0 {
                    format!("{} today", plural(n_today, "incident"))
                } else {
                    format!("{} in the last 24 hours", plural(recent.len(), "incident"))
                };
                let tone = if recent.iter().any(|c| c.bad) { Tone::Bad } else { Tone::Warn };
                (tone, format!("{}. Last at {}: {}.", lead, time::hhmm(last.t_ms), last.what))
            }
        };
        let mut lines = Vec::new();
        if self.cards.len() > recent.len() {
            lines.push(format!("{} in the last 7 days.", plural(self.cards.len(), "incident")));
        }
        let items = self
            .cards
            .iter()
            .rev()
            .map(|c| Item {
                t_ms: c.t_ms,
                tone: if c.bad { Tone::Bad } else { Tone::Warn },
                title: c.title(),
                detail: c.detail.clone(),
            })
            .collect();
        Panel { key: "incidents", title: "Incident Cards", tone, headline, lines, visual: None, chart: self.chart(now_ms), items }
    }

    /// CPU % over the last hour, one slot per minute (peak), with incident markers.
    /// Minutes without a sample are gaps (NaN), so the hour keeps its full width.
    fn chart(&self, now_ms: u64) -> Option<Chart> {
        let now_m = (now_ms / 60_000) as u32;
        let first = now_m.saturating_sub(59);
        let mut points = Vec::with_capacity(60);
        let mut filled = 0;
        for m in first..first + 60 {
            let (id, v) = self.minutes[(m % 60) as usize];
            if id == m && m <= now_m {
                points.push(v as f32);
                filled += 1;
            } else {
                points.push(f32::NAN);
            }
        }
        if filled < 2 {
            return None;
        }
        let markers = self
            .cards
            .iter()
            .filter_map(|c| {
                let m = (c.t_ms / 60_000) as u32;
                (first..=now_m).contains(&m).then(|| ((m - first) as usize, c.kind.short().to_string()))
            })
            .collect();
        Some(Chart {
            label: "CPU, last hour".into(),
            points,
            max: 100.0,
            unit: "%",
            markers,
            bars: false,
            slots: 60,
            x_labels: super::last_hour(),
            tones: Vec::new(),
        })
    }
}

fn plural(n: usize, w: &str) -> String {
    if n == 1 {
        format!("1 {}", w)
    } else {
        format!("{} {}s", n, w)
    }
}

/// 5 min median fan speed from the ring (None until there is enough data).
fn fan_median(ring: &Ring) -> Option<f32> {
    let mut v: Vec<f32> = ring.series(Field::Fan, FAN_WINDOW).into_iter().filter(|x| !x.is_nan()).collect();
    if v.len() < FAN_MIN_SAMPLES {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// What the PC was doing when an event fired: (follow-up sentence, detail).
fn event_context(k: Kind, ring: &Ring, t_ms: u64) -> (String, String) {
    let Some(t) = ring.at(t_ms, 5_000) else { return (String::new(), String::new()) };
    let mut fx = Vec::new();
    let gpu_kind = matches!(k, Kind::DriverReset | Kind::HwError | Kind::HwFatal | Kind::Unclean);
    if gpu_kind {
        if let Some(g) = Tick::opt8(t.gpu) {
            fx.push(format!("GPU {}%", g));
        }
        if let Some(c) = Tick::opt8(t.gpu_temp) {
            fx.push(format!("{} °C", c));
        }
    }
    fx.push(format!("CPU {}%", t.cpu));
    fx.push(format!("RAM {}%", t.ram));
    if let Some(d) = Tick::opt8(t.disk).filter(|&d| d >= 50 || k == Kind::DiskError) {
        fx.push(format!("disk {}%", d));
    }
    let top = ring.top_cpu(t_ms.saturating_sub(30_000), t_ms.saturating_add(5_000), 1);
    if let Some((n, v)) = top.first().filter(|(_, v)| *v >= 5.0) {
        fx.push(format!("{} {}% CPU", n, v.round()));
    }
    let more = capitalize(&fx.join(", ")) + ".";
    let detail = format!("At that moment: {}", more);
    (more, detail)
}

fn card_from_json(j: &Json) -> Option<Card> {
    let t_ms = store::num(j, "t")? as u64;
    let kind = Kind::from_u8(store::num(j, "k")? as u8)?;
    let s = |k: &str, max: usize| cut(j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string(), max);
    let b = |k: &str| matches!(j.get(k), Some(Json::Bool(true)));
    let what = s("w", 120);
    if what.is_empty() {
        return None;
    }
    Some(Card {
        t_ms,
        dur_s: store::num(j, "d").map_or(0, |v| v.clamp(0.0, 1e7) as u32),
        kind,
        mask: store::num(j, "m").map_or(0, |v| v.clamp(0.0, 63.0) as u8),
        bad: b("b"),
        away: b("a"),
        what,
        more: s("s", 200),
        detail: s("x", 200),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::{ProcStat, Snapshot};

    /// Drives a Ring + Incidents with synthetic samples, one per second.
    struct Sim {
        ring: Ring,
        inc: Incidents,
        t_s: u64,
        notices: Vec<Notice>,
    }

    #[derive(Clone)]
    struct S {
        cpu: f32,
        ram: f32,
        disk: Option<f32>,
        temp: Option<f32>,
        fan: Option<u32>,
        elapsed: f64,
        display: bool,
        idle_ms: u64,
        procs: Vec<(&'static str, f32)>,
        io: Option<&'static str>,
    }

    impl Default for S {
        fn default() -> S {
            S {
                cpu: 10.0,
                ram: 40.0,
                disk: Some(2.0),
                temp: None,
                fan: None,
                elapsed: 1.0,
                display: true,
                idle_ms: 0,
                procs: vec![],
                io: None,
            }
        }
    }

    impl Sim {
        fn new() -> Sim {
            Sim { ring: Ring::new(), inc: Incidents::new(), t_s: 1_764_000_000, notices: vec![] }
        }

        fn tick(&mut self, s: &S) {
            self.t_s += s.elapsed.round().max(1.0) as u64;
            let procs = self.inc.wants_procs();
            let snap = Snapshot {
                cpu: s.cpu,
                ram_pct: s.ram,
                disk_active: s.disk,
                gpu_temp: s.temp,
                gpu_fan_rpm: s.fan,
                elapsed: s.elapsed,
                top_cpu: if procs {
                    s.procs.iter().map(|(n, c)| ProcStat { name: n.to_string(), cpu: *c, ..Default::default() }).collect()
                } else {
                    vec![]
                },
                top_io: match (procs, s.io) {
                    (true, Some(n)) => vec![ProcStat { name: n.to_string(), io_bps: 5e7, ..Default::default() }],
                    _ => vec![],
                },
                ..Default::default()
            };
            let c = Ctx {
                now_ms: self.t_s * 1000,
                snap: &snap,
                display_on: s.display,
                input_idle_ms: s.idle_ms,
                interval_ms: 1000,
                procs_sampled: procs,
            };
            self.ring.push(&c);
            let n = self.inc.on_sample(&c, &self.ring);
            self.notices.extend(n);
        }

        fn run(&mut self, n: usize, s: &S) {
            for _ in 0..n {
                self.tick(s);
            }
        }

        fn calm(&mut self, n: usize) {
            self.run(n, &S::default());
        }
    }

    fn busy_cpu() -> S {
        S { cpu: 97.0, procs: vec![("MsMpEng", 38.0), ("svchost", 9.0)], ..S::default() }
    }

    #[test]
    fn hold_needs_duration_and_tolerates_one_dip() {
        let mut h = Hold::default();
        assert_eq!(h.step(0, true, 30, false), Step::None);
        // 10 s without the condition ends a pending hold silently.
        assert_eq!(h.step(10, false, 30, false), Step::None);
        assert!(!h.busy());
        h.step(100, true, 30, false);
        assert_eq!(h.step(101, false, 30, false), Step::None); // one noisy sample
        assert!(h.busy());
        assert_eq!(h.step(130, true, 30, false), Step::Began(100));
        assert_eq!(h.step(140, false, 30, false), Step::Ended(100, 130));
    }

    #[test]
    fn cpu_held_makes_one_card_with_culprit() {
        let mut s = Sim::new();
        s.calm(5);
        s.run(20, &busy_cpu());
        assert!(s.inc.wants_procs(), "pending condition samples the culprit");
        s.run(100, &busy_cpu());
        assert!(s.inc.cards.is_empty(), "no card while it still holds");
        s.calm(15);
        assert!(!s.inc.wants_procs());
        assert_eq!(s.inc.cards.len(), 1);
        let c = &s.inc.cards[0];
        assert_eq!(c.kind, Kind::Cpu);
        assert_eq!(c.what, "CPU held at 97% for 2 min");
        assert_eq!(c.more, "MsMpEng 38%.");
        assert!(c.detail.contains("Top CPU: MsMpEng 38%, svchost 9%."), "{}", c.detail);
        assert!(!c.bad && !c.away);
        assert!(s.inc.dirty());
        assert!(s.notices.is_empty(), "load at the desk is a silent card");
    }

    #[test]
    fn short_spike_is_ignored() {
        let mut s = Sim::new();
        s.calm(5);
        s.run(25, &busy_cpu());
        s.calm(20);
        assert!(s.inc.cards.is_empty());
        assert!(!s.inc.wants_procs());
    }

    #[test]
    fn disk_pegged_names_io_process() {
        let mut s = Sim::new();
        s.calm(5);
        let d = S { disk: Some(100.0), io: Some("SearchIndexer"), procs: vec![("SearchIndexer", 3.0)], ..S::default() };
        s.run(40, &d);
        s.calm(15);
        assert_eq!(s.inc.cards.len(), 1);
        assert_eq!(s.inc.cards[0].what, "Disk at 100% for 40 s");
        assert_eq!(s.inc.cards[0].more, "SearchIndexer.");
    }

    #[test]
    fn overlapping_kinds_merge_into_one_card() {
        let mut s = Sim::new();
        s.calm(5);
        s.run(30, &busy_cpu());
        let both = S { disk: Some(100.0), io: Some("MsMpEng"), ..busy_cpu() };
        s.run(40, &both);
        s.run(30, &busy_cpu());
        s.calm(15);
        assert_eq!(s.inc.cards.len(), 1);
        let c = &s.inc.cards[0];
        assert_eq!(c.kind, Kind::Cpu);
        assert_eq!(c.mask, Kind::Cpu.bit() | Kind::Disk.bit());
        assert_eq!(c.title()[5..], *". CPU held at 97% for 2 min. MsMpEng 38%, disk 100%.");
    }

    #[test]
    fn cooldown_per_kind() {
        let mut s = Sim::new();
        s.calm(5);
        s.run(40, &busy_cpu());
        s.calm(120);
        s.run(40, &busy_cpu());
        s.calm(20);
        assert_eq!(s.inc.cards.len(), 1, "second CPU spike within 10 min is not a new card");
        s.calm(600);
        s.run(40, &busy_cpu());
        s.calm(20);
        assert_eq!(s.inc.cards.len(), 2);
    }

    #[test]
    fn late_tick_is_a_freeze_but_sleep_is_not() {
        let mut s = Sim::new();
        s.calm(5);
        // Sleep gap: 2 min between samples.
        s.tick(&S { elapsed: 120.0, ..S::default() });
        s.tick(&S { elapsed: 3.0, ..S::default() }); // first tick after resume: ignored
        s.calm(20);
        assert!(s.inc.cards.is_empty());
        // Display just came on (5 s interval before): ignored.
        s.run(3, &S { display: false, elapsed: 5.0, ..S::default() });
        s.tick(&S { elapsed: 5.0, ..S::default() });
        s.calm(20);
        assert!(s.inc.cards.is_empty());
        // Display off: a late tick is not a visible freeze.
        s.run(3, &S { display: false, elapsed: 5.0, ..S::default() });
        s.calm(20);
        assert!(s.inc.cards.is_empty());
        // A real freeze: 4 s late while in use, disk busy.
        s.tick(&S { elapsed: 5.0, disk: Some(100.0), ..S::default() });
        assert!(s.inc.wants_procs(), "keeps sampling to catch the culprit");
        let io = S { io: Some("SearchIndexer"), ..S::default() };
        s.run(10, &io);
        assert_eq!(s.inc.cards.len(), 1);
        let c = &s.inc.cards[0];
        assert_eq!(c.kind, Kind::Stutter);
        assert_eq!(c.what, "Froze for 4 s");
        assert_eq!(c.more, "Disk 100%, SearchIndexer.");
        assert!(s.notices.is_empty(), "never a toast for a freeze");
    }

    #[test]
    fn away_incident_toasts() {
        let mut s = Sim::new();
        s.calm(5);
        s.run(200, &S { idle_ms: 600_000, display: false, elapsed: 1.0, ..busy_cpu() });
        s.calm(15);
        assert_eq!(s.inc.cards.len(), 1);
        assert!(s.inc.cards[0].away);
        assert!(s.inc.cards[0].more.ends_with("While you were away."));
        assert_eq!(s.notices.len(), 1);
        assert_eq!(s.notices[0].title, "While you were away");
    }

    fn crash(app: &str, t_ms: u64) -> SysEvent {
        SysEvent {
            channel: "Application".into(),
            provider: "Application Error".into(),
            id: 1000,
            t_ms,
            data: vec![("AppName".into(), app.into())],
        }
    }

    #[test]
    fn toast_cap_three_per_day_and_crash_text() {
        let mut s = Sim::new();
        s.calm(5);
        let now = s.t_s * 1000;
        for (i, app) in ["Game.exe", "a.exe", "b.exe", "c.exe", "d.exe"].iter().enumerate() {
            let r = s.inc.on_event(&crash(app, now - 1000 * i as u64), &s.ring, now);
            assert!(r.is_empty(), "notices wait for the next tick");
        }
        s.calm(3);
        assert_eq!(s.notices.len(), 3);
        assert_eq!(s.notices[0].title, "Game crashed");
        assert_eq!(s.inc.cards.len(), 5);
        let c = s.inc.cards.iter().find(|c| c.what == "Game crashed").expect("card");
        assert!(c.bad);
        assert_eq!(c.more, "CPU 10%, RAM 40%.");
        // Same app again within 10 min: no new card; replayed same event: no new card.
        s.inc.on_event(&crash("Game.exe", now + 60_000), &s.ring, now + 60_000);
        s.inc.on_event(&crash("Game.exe", now), &s.ring, now);
        assert_eq!(s.inc.cards.len(), 5);
        s.calm(3);
        assert_eq!(s.notices.len(), 3);
    }

    #[test]
    fn driver_reset_fused_with_ring() {
        let mut s = Sim::new();
        s.run(5, &S { temp: Some(84.0), ..S::default() });
        let now = s.t_s * 1000;
        let e = SysEvent { provider: "Display".into(), id: 4101, t_ms: now, ..Default::default() };
        s.inc.on_event(&e, &s.ring, now);
        let c = &s.inc.cards[0];
        assert_eq!(c.kind, Kind::DriverReset);
        assert_eq!(c.what, "GPU driver reset");
        assert_eq!(c.more, "84 °C, CPU 10%, RAM 40%.");
        // Unrelated events are not cards.
        let e = SysEvent { provider: "Microsoft-Windows-Kernel-Power".into(), id: 107, ..Default::default() };
        assert!(classify(&e).is_none());
        let e = SysEvent { provider: "Microsoft-Windows-WHEA-Logger".into(), id: 18, ..Default::default() };
        assert_eq!(classify(&e).map(|x| x.0), Some(Kind::HwFatal));
    }

    #[test]
    fn ram_rise_is_instant() {
        let mut s = Sim::new();
        s.run(10, &S { ram: 50.0, ..S::default() });
        s.run(5, &S { ram: 75.0, ..S::default() });
        s.run(80, &S::default());
        assert_eq!(s.inc.cards.len(), 1);
        assert_eq!(s.inc.cards[0].what, "RAM jumped to 75%");
    }

    #[test]
    fn fan_up_over_median() {
        let mut s = Sim::new();
        s.run(120, &S { fan: Some(1200), ..S::default() });
        s.run(70, &S { fan: Some(2400), procs: vec![("MsMpEng", 38.0)], ..S::default() });
        s.run(20, &S { fan: Some(1200), ..S::default() });
        assert_eq!(s.inc.cards.len(), 1);
        assert_eq!(s.inc.cards[0].kind, Kind::Fan);
        assert_eq!(s.inc.cards[0].what, "GPU fan up to 2400 rpm for 1 min");
        assert_eq!(s.inc.cards[0].more, "MsMpEng 38% CPU.");
    }

    #[test]
    fn gpu_hot() {
        let mut s = Sim::new();
        s.run(40, &S { temp: Some(90.0), ..S::default() });
        s.calm(15);
        assert_eq!(s.inc.cards.len(), 1);
        assert_eq!(s.inc.cards[0].what, "GPU hot at 90 °C for 40 s");
    }

    #[test]
    fn duration_words() {
        assert_eq!(dur(4), "4 s");
        assert_eq!(dur(170), "3 min");
        assert_eq!(dur(2 * 3600), "2 h");
        assert_eq!(dur(5400), "1.5 h");
    }

    #[test]
    fn save_load_roundtrip_and_panel() {
        let mut s = Sim::new();
        s.calm(5);
        s.run(40, &busy_cpu());
        s.calm(15);
        let j = s.inc.save();
        assert!(!s.inc.dirty());
        let mut b = Incidents::new();
        // Card time is in the past relative to the real clock only if the sim time is; use the
        // sim's card directly to check the parse.
        let parsed: Vec<Card> = j.get("cards").and_then(|a| a.as_arr()).unwrap().iter().filter_map(card_from_json).collect();
        assert_eq!(parsed, s.inc.cards);
        b.load(&Json::Str("garbage".into()));
        assert!(b.cards.is_empty());
        let p = s.inc.panel(s.t_s * 1000);
        assert_eq!(p.key, "incidents");
        assert_eq!(p.tone, Tone::Warn);
        assert!(p.headline.starts_with("1 incident today. Last at ") || p.headline.starts_with("1 incident in the last"));
        assert!(p.headline.ends_with(": CPU held at 97% for 40 s."), "{}", p.headline);
        assert_eq!(p.items.len(), 1);
        let ch = p.chart.expect("chart");
        assert_eq!(ch.markers.len(), 1);
        let quiet = Incidents::new().panel(s.t_s * 1000);
        assert_eq!(quiet.tone, Tone::Good);
        assert_eq!(quiet.headline, "Quiet. Nothing odd in the last 24 hours.");
    }
}
