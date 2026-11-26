//! Before / After: what idle looks like on this PC, per day, and what changed it.
//!
//! * Idle samples: no input for 5 min, display on, booted for 10 min, tick on time.
//! * Per local day a fingerprint is built from fixed histograms (no per-tick allocation):
//!   median idle CPU, RAM, process count, background disk and network, idle minutes and
//!   the top idle processes (only from short process-list windows we ask for while idle).
//! * Change markers: Windows Update installs (Event Log, live + 30-day back-fill), Windows
//!   build, GPU driver version and new startup entries. The slow reads run once a day on a
//!   short-lived thread; the result comes back through a channel polled in `on_sample`.
//! * `diff_at` compares the median of daily medians before and after a marker and reports
//!   only changes big enough to matter.

use super::events::SysEvent;
use super::store::obj;
use super::time;
use super::{Chart, Ctx, Item, Panel, Tone};
use crate::achievements::json::Json;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// No input for this long = idle.
const IDLE_INPUT_MS: u64 = 5 * 60 * 1000;
/// Skip the first minutes after boot (startup work is not idle).
const MIN_UPTIME_SECS: u64 = 10 * 60;
/// A tick later than this past its interval means the PC froze or slept.
const LATE_SLACK_S: f64 = 2.0;
/// A day needs this much idle time to count as a baseline day.
const USABLE_MINS: f32 = 20.0;
/// Days needed on each side of a marker before we compare.
const NEED_DAYS: usize = 3;
/// How far before / after a marker we look.
const WINDOW_DAYS: u32 = 14;
const MAX_ROWS: usize = 60;
const MAX_MARKERS: usize = 60;
const TOP_KEEP: usize = 5;
/// Process-list window while idle: ticks per window (first has no CPU %) and spacing.
const PROC_WINDOW_TICKS: u8 = 11;
const PROC_EVERY_MS: u64 = 10 * 60 * 1000;
/// Registry / Event Log check: first one shortly after start, then once a day.
const PROBE_FIRST_DELAY_MS: u64 = 2 * 60 * 1000;
const PROBE_EVERY_MS: u64 = 24 * 3600 * 1000;
/// Mark the partial day dirty this often (idle seconds) so a restart keeps most of it.
const PARTIAL_SAVE_SECS: f64 = 30.0 * 60.0;

const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * MB;

// ---------------------------------------------------------------------------------------
// Idle gate

/// True when this tick is an idle sample.
pub fn is_idle(input_idle_ms: u64, display_on: bool, uptime_secs: u64, elapsed_s: f64, interval_ms: u32) -> bool {
    input_idle_ms >= IDLE_INPUT_MS
        && display_on
        && uptime_secs >= MIN_UPTIME_SECS
        && elapsed_s > 0.0
        && elapsed_s <= interval_ms as f64 / 1000.0 + LATE_SLACK_S
}

// ---------------------------------------------------------------------------------------
// Streaming medians: fixed histograms

const CPU_BUCKETS: usize = 200; // 0.5 % each
const RAM_BUCKETS: usize = 1024; // 64 MB each, wider on PCs with more than 64 GB
const RAM_STEP_MIN: u64 = 64 << 20;
const PROC_BUCKETS: usize = 1024; // one per process
const LOG_BUCKETS: usize = 160; // quarter octaves of bytes/s, bucket 0 = zero

/// Counting histogram with a fixed number of buckets; the median is a bucket index.
struct Hist<const N: usize> {
    c: [u32; N],
    n: u32,
}

impl<const N: usize> Hist<N> {
    fn new() -> Self {
        Hist { c: [0; N], n: 0 }
    }
    fn add(&mut self, i: usize) {
        let i = i.min(N - 1);
        self.c[i] = self.c[i].saturating_add(1);
        self.n = self.n.saturating_add(1);
    }
    fn clear(&mut self) {
        self.c = [0; N];
        self.n = 0;
    }
    /// Bucket holding the median sample.
    fn median(&self) -> Option<usize> {
        if self.n == 0 {
            return None;
        }
        let half = (self.n as u64 + 1) / 2;
        let mut acc = 0u64;
        for (i, &c) in self.c.iter().enumerate() {
            acc += c as u64;
            if acc >= half {
                return Some(i);
            }
        }
        Some(N - 1)
    }
}

fn cpu_bucket(pct: f32) -> usize {
    (pct.clamp(0.0, 100.0) * 2.0) as usize
}
fn cpu_value(i: usize) -> f32 {
    (i as f32 + 0.5) * 0.5
}
fn log_bucket(x: f64) -> usize {
    if !(x >= 1.0) {
        0
    } else {
        (x.log2() * 4.0) as usize + 1
    }
}
fn log_value(i: usize) -> f64 {
    if i == 0 {
        0.0
    } else {
        2f64.powf((i as f64 - 0.5) / 4.0)
    }
}
/// RAM bucket width for a PC with `total` bytes: 64 MB, or wider so 1024 buckets cover it.
fn ram_step(total: u64) -> u64 {
    let need = total.div_ceil(RAM_BUCKETS as u64);
    need.div_ceil(RAM_STEP_MIN).max(1) * RAM_STEP_MIN
}

/// One slot of the idle top-process table.
#[derive(Default)]
struct Slot {
    name: String,
    sum: f32,
}

/// Today's idle fingerprint being built. Constant size; reset (not reallocated) each day.
struct Accum {
    day: u32,
    secs: f64,
    ram_step: u64,
    cpu: Hist<CPU_BUCKETS>,
    ram: Hist<RAM_BUCKETS>,
    procs: Hist<PROC_BUCKETS>,
    disk: Hist<LOG_BUCKETS>,
    net: Hist<LOG_BUCKETS>,
    /// Idle ticks with valid per-process CPU %.
    proc_ticks: u32,
    top: [Slot; 16],
}

impl Accum {
    fn new() -> Accum {
        Accum {
            day: 0,
            secs: 0.0,
            ram_step: RAM_STEP_MIN,
            cpu: Hist::new(),
            ram: Hist::new(),
            procs: Hist::new(),
            disk: Hist::new(),
            net: Hist::new(),
            proc_ticks: 0,
            top: std::array::from_fn(|_| Slot::default()),
        }
    }

    fn reset(&mut self, day: u32) {
        self.day = day;
        self.secs = 0.0;
        self.ram_step = RAM_STEP_MIN;
        self.cpu.clear();
        self.ram.clear();
        self.procs.clear();
        self.disk.clear();
        self.net.clear();
        self.proc_ticks = 0;
        for s in &mut self.top {
            s.name.clear();
            s.sum = 0.0;
        }
    }

    /// One idle sample. `count_procs`: `proc_count` is valid; `proc_cpu`: per-process CPU % is.
    fn add(&mut self, c: &Ctx, count_procs: bool, proc_cpu: bool) {
        let s = c.snap;
        if self.cpu.n == 0 && s.ram_total > 0 {
            self.ram_step = ram_step(s.ram_total);
        }
        self.secs += s.elapsed;
        self.cpu.add(cpu_bucket(s.cpu));
        self.ram.add((s.ram_used / self.ram_step) as usize);
        self.disk.add(log_bucket(s.disk_read + s.disk_write));
        self.net.add(log_bucket(s.net_up + s.net_down));
        if count_procs && s.proc_count > 0 {
            self.procs.add(s.proc_count as usize);
        }
        if proc_cpu {
            self.proc_ticks += 1;
            for p in &s.top_cpu {
                self.add_proc(&p.name, p.cpu);
            }
        }
    }

    fn add_proc(&mut self, name: &str, cpu: f32) {
        if name.is_empty() || !(cpu > 0.0) {
            return;
        }
        if let Some(s) = self.top.iter_mut().find(|s| s.name == name) {
            s.sum += cpu;
            return;
        }
        // Empty slot first, else replace the smallest one (reuses its String buffer).
        let i = (0..self.top.len())
            .min_by(|&a, &b| {
                let ka = if self.top[a].name.is_empty() { -1.0 } else { self.top[a].sum };
                let kb = if self.top[b].name.is_empty() { -1.0 } else { self.top[b].sum };
                ka.total_cmp(&kb)
            })
            .unwrap_or(0);
        let s = &mut self.top[i];
        s.name.clear();
        s.name.push_str(name);
        s.sum = cpu;
    }

    /// The day's fingerprint so far (None before the first idle sample).
    fn row(&self) -> Option<DayRow> {
        let cpu = self.cpu.median()?;
        let mut top: Vec<(String, f32)> = Vec::new();
        if self.proc_ticks > 0 {
            for s in self.top.iter().filter(|s| !s.name.is_empty()) {
                top.push((s.name.clone(), s.sum / self.proc_ticks as f32));
            }
            top.sort_by(|a, b| b.1.total_cmp(&a.1));
            top.truncate(TOP_KEEP);
        }
        Some(DayRow {
            day: self.day,
            mins: (self.secs / 60.0) as f32,
            cpu: cpu_value(cpu),
            ram: self.ram.median().map_or(0, |i| i as u64 * self.ram_step + self.ram_step / 2),
            procs: self.procs.median().map_or(0, |i| i as u32),
            disk: self.disk.median().map_or(0.0, log_value),
            net: self.net.median().map_or(0.0, log_value),
            top,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Plain data: day rows, markers, diff

/// One day's idle fingerprint.
#[derive(Clone, Debug, PartialEq)]
pub struct DayRow {
    pub day: u32,
    pub mins: f32,
    /// Median idle CPU %.
    pub cpu: f32,
    /// Median idle RAM used, bytes.
    pub ram: u64,
    /// Median process count (0 = never sampled).
    pub procs: u32,
    /// Median background disk read+write, bytes/s.
    pub disk: f64,
    /// Median network up+down, bytes/s.
    pub net: f64,
    /// Top idle processes: name, average CPU %. Highest first.
    pub top: Vec<(String, f32)>,
}

impl DayRow {
    pub fn usable(&self) -> bool {
        self.mins >= USABLE_MINS
    }

    /// Combine two partial rows of the same day (before and after a restart), weighted by minutes.
    fn merge(&self, o: &DayRow) -> DayRow {
        let (a, b) = (self.mins.max(0.0) as f64, o.mins.max(0.0) as f64);
        let t = (a + b).max(1e-9);
        let w = |x: f64, y: f64| (x * a + y * b) / t;
        let procs = match (self.procs, o.procs) {
            (0, p) | (p, 0) => p,
            (p, q) => w(p as f64, q as f64).round() as u32,
        };
        let mut top: Vec<(String, f32)> = Vec::new();
        for (n, v) in &self.top {
            top.push((n.clone(), (*v as f64 * a / t) as f32));
        }
        for (n, v) in &o.top {
            let add = (*v as f64 * b / t) as f32;
            match top.iter_mut().find(|(m, _)| m == n) {
                Some(e) => e.1 += add,
                None => top.push((n.clone(), add)),
            }
        }
        top.sort_by(|x, y| y.1.total_cmp(&x.1));
        top.truncate(TOP_KEEP);
        DayRow {
            day: self.day,
            mins: self.mins + o.mins,
            cpu: w(self.cpu as f64, o.cpu as f64) as f32,
            ram: w(self.ram as f64, o.ram as f64) as u64,
            procs,
            disk: w(self.disk, o.disk),
            net: w(self.net, o.net),
            top,
        }
    }
}

/// What kind of change a marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Update,
    Build,
    Driver,
    Startup,
}

impl Kind {
    fn code(self) -> u64 {
        match self {
            Kind::Update => 0,
            Kind::Build => 1,
            Kind::Driver => 2,
            Kind::Startup => 3,
        }
    }
    fn from_code(c: u64) -> Option<Kind> {
        Some(match c {
            0 => Kind::Update,
            1 => Kind::Build,
            2 => Kind::Driver,
            3 => Kind::Startup,
            _ => return None,
        })
    }
    /// "Since the 12 Nov <noun>".
    fn noun(self) -> &'static str {
        match self {
            Kind::Update => "update",
            Kind::Build => "Windows build change",
            Kind::Driver => "driver update",
            Kind::Startup => "startup change",
        }
    }
    /// Short chart label.
    fn short(self) -> &'static str {
        match self {
            Kind::Update => "Update",
            Kind::Build => "Build",
            Kind::Driver => "Driver",
            Kind::Startup => "Startup",
        }
    }
}

/// Something changed on this PC on `day`.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub day: u32,
    pub kind: Kind,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Metric {
    Ram,
    Cpu,
    Disk,
    Procs,
}

/// One meaningful before/after change (medians of daily medians).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub metric: Metric,
    pub before: f64,
    pub after: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffResult {
    pub changes: Vec<Change>,
    /// Idle processes after the marker that never showed up before it.
    pub new_procs: Vec<String>,
}

impl DiffResult {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.new_procs.is_empty()
    }
    /// Anything got worse (more CPU / RAM / disk / processes, or new idle processes).
    pub fn worse(&self) -> bool {
        !self.new_procs.is_empty() || self.changes.iter().any(|c| c.after > c.before)
    }
}

/// Median of a list (mean of the middle two for an even count).
pub fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

fn med_of<'a>(rows: &[&'a DayRow], f: impl Fn(&'a DayRow) -> Option<f64>) -> (Option<f64>, usize) {
    let mut v: Vec<f64> = rows.iter().filter_map(|r| f(r)).collect();
    let n = v.len();
    (median(&mut v), n)
}

/// Usable days before (within 14 days) and after (within 14 days) a marker day.
/// The marker day itself is mixed and left out.
pub fn window(rows: &[DayRow], day: u32) -> (Vec<&DayRow>, Vec<&DayRow>) {
    let before = rows.iter().filter(|r| r.usable() && r.day < day && r.day + WINDOW_DAYS >= day).collect();
    let after = rows.iter().filter(|r| r.usable() && r.day > day && r.day <= day + WINDOW_DAYS).collect();
    (before, after)
}

/// Compare idle before vs after `day`. None = not enough usable days on one side.
pub fn diff_at(rows: &[DayRow], day: u32) -> Option<DiffResult> {
    let (b, a) = window(rows, day);
    if b.len() < NEED_DAYS || a.len() < NEED_DAYS {
        return None;
    }
    let mut changes = Vec::new();
    let both = |f: &dyn Fn(&DayRow) -> Option<f64>| {
        let (x, nx) = med_of(&b, f);
        let (y, ny) = med_of(&a, f);
        match (x, y) {
            (Some(x), Some(y)) if nx >= NEED_DAYS && ny >= NEED_DAYS => Some((x, y)),
            _ => None,
        }
    };
    if let Some((x, y)) = both(&|r| (r.ram > 0).then_some(r.ram as f64)) {
        if (y - x).abs() >= 300.0 * MB {
            changes.push(Change { metric: Metric::Ram, before: x, after: y });
        }
    }
    if let Some((x, y)) = both(&|r| Some(r.cpu as f64)) {
        let d = (y - x).abs();
        if d >= 1.5 && d >= 0.5 * x {
            changes.push(Change { metric: Metric::Cpu, before: x, after: y });
        }
    }
    if let Some((x, y)) = both(&|r| Some(r.disk)) {
        let (lo, hi) = (x.min(y), x.max(y));
        if hi - lo >= 200.0 * 1024.0 && hi >= 2.0 * lo {
            changes.push(Change { metric: Metric::Disk, before: x, after: y });
        }
    }
    if let Some((x, y)) = both(&|r| (r.procs > 0).then_some(r.procs as f64)) {
        if (y - x).abs() >= 15.0 {
            changes.push(Change { metric: Metric::Procs, before: x, after: y });
        }
    }
    let mut new_procs: Vec<String> = Vec::new();
    for r in &a {
        for (n, v) in &r.top {
            let seen = b.iter().any(|br| br.top.iter().any(|(m, _)| m.eq_ignore_ascii_case(n)));
            if *v >= 0.5 && !seen && !new_procs.iter().any(|m| m.eq_ignore_ascii_case(n)) {
                new_procs.push(n.clone());
            }
        }
    }
    new_procs.truncate(3);
    Some(DiffResult { changes, new_procs })
}

// ---------------------------------------------------------------------------------------
// Text

/// "2%", "0.8%", "12%".
fn fmt_cpu(v: f64) -> String {
    if v < 9.95 && (v - v.round()).abs() >= 0.05 {
        format!("{:.1}%", v)
    } else {
        format!("{:.0}%", v)
    }
}
fn fmt_gb(bytes: f64) -> String {
    format!("{:.1} GB", bytes / GB)
}
/// "+1.1 GB", "-450 MB".
fn fmt_ram_delta(d: f64) -> String {
    let sign = if d < 0.0 { "-" } else { "+" };
    let a = d.abs();
    if a >= GB {
        format!("{}{:.1} GB", sign, a / GB)
    } else {
        format!("{}{} MB", sign, ((a / MB / 10.0).round() * 10.0) as u64)
    }
}
/// "40 KB/s", "1.2 MB/s".
fn fmt_rate(bps: f64) -> String {
    if bps >= MB {
        format!("{:.1} MB/s", bps / MB)
    } else {
        format!("{} KB/s", (bps / 1024.0).round() as u64)
    }
}

fn change_text(c: &Change) -> String {
    match c.metric {
        Metric::Ram => format!("idle RAM {}", fmt_ram_delta(c.after - c.before)),
        Metric::Cpu => format!("idle CPU {} to {}", fmt_cpu(c.before), fmt_cpu(c.after)),
        Metric::Disk => format!("background disk {} to {}", fmt_rate(c.before), fmt_rate(c.after)),
        Metric::Procs => {
            let d = (c.after - c.before).round() as i64;
            if d > 0 {
                format!("{} more processes", d)
            } else {
                format!("{} fewer processes", -d)
            }
        }
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Headline for a diff, e.g. "Since the 12 Nov update: idle RAM +1.1 GB, idle CPU 2% to 6%.
/// New at idle: WidgetService."
pub fn diff_headline(m: &Marker, d: &DiffResult) -> String {
    let when = format!("the {} {}", time::day_label(m.day), m.kind.noun());
    if d.is_empty() {
        return upper_first(&format!("{} changed nothing at idle.", when));
    }
    let parts: Vec<String> = d.changes.iter().map(change_text).collect();
    let new = d.new_procs.join(", ");
    if parts.is_empty() {
        return format!("New at idle since {}: {}.", when, new);
    }
    let mut s = format!("Since {}: {}.", when, parts.join(", "));
    if !new.is_empty() {
        s.push_str(&format!(" New at idle: {}.", new));
    }
    s
}

/// Short item detail for a marker: date plus what the diff found, or why there is none yet.
fn marker_detail(rows: &[DayRow], m: &Marker) -> (Tone, String) {
    let date = time::day_label(m.day);
    if let Some(d) = diff_at(rows, m.day) {
        if d.is_empty() {
            return (Tone::Good, format!("{}. Nothing changed at idle.", date));
        }
        let mut parts: Vec<String> = d.changes.iter().map(change_text).collect();
        if !d.new_procs.is_empty() {
            parts.push(format!("new at idle: {}", d.new_procs.join(", ")));
        }
        let tone = if d.worse() { Tone::Warn } else { Tone::Good };
        return (tone, format!("{}. {}.", date, upper_first(&parts.join(", "))));
    }
    let (b, a) = window(rows, m.day);
    if b.len() < NEED_DAYS {
        (Tone::Info, format!("{}. Not enough idle days before it.", date))
    } else {
        (Tone::Learning, format!("{}. Watching: {} of {} days after.", date, a.len(), NEED_DAYS))
    }
}

/// "CPU 2%, RAM 5.1 GB, 210 processes" from a set of rows (medians).
fn steady_facts(rows: &[&DayRow]) -> String {
    let (cpu, _) = med_of(rows, |r| Some(r.cpu as f64));
    let (ram, _) = med_of(rows, |r| (r.ram > 0).then_some(r.ram as f64));
    let (procs, _) = med_of(rows, |r| (r.procs > 0).then_some(r.procs as f64));
    let mut s = format!("CPU {}", fmt_cpu(cpu.unwrap_or(0.0)));
    if let Some(r) = ram {
        s.push_str(&format!(", RAM {}", fmt_gb(r)));
    }
    if let Some(p) = procs {
        s.push_str(&format!(", {} processes", p.round() as u64));
    }
    s
}

/// The panel verdict from plain data. `rows` oldest first, markers oldest first.
pub fn verdict(rows: &[DayRow], markers: &[Marker]) -> (Tone, String) {
    let usable: Vec<&DayRow> = rows.iter().filter(|r| r.usable()).collect();
    if usable.len() < NEED_DAYS {
        return (Tone::Learning, format!("Learning what idle looks like. {} of {} days.", usable.len(), NEED_DAYS));
    }
    for m in markers.iter().rev() {
        if let Some(d) = diff_at(rows, m.day) {
            let tone = if d.is_empty() || !d.worse() { Tone::Good } else { Tone::Warn };
            return (tone, diff_headline(m, &d));
        }
    }
    // Newest marker with enough days before it but not yet after: say we are watching it.
    if let Some(m) = markers.iter().rev().find(|m| window(rows, m.day).0.len() >= NEED_DAYS) {
        let after = window(rows, m.day).1.len();
        if after < NEED_DAYS && rows.last().map_or(true, |r| r.day <= m.day + WINDOW_DAYS) {
            return (
                Tone::Learning,
                format!("Watching the {} {}. {} of {} days after.", time::day_label(m.day), m.kind.noun(), after, NEED_DAYS),
            );
        }
    }
    let recent = &usable[usable.len().saturating_sub(WINDOW_DAYS as usize)..];
    (Tone::Info, format!("Idle looks steady. {}.", steady_facts(recent)))
}

// ---------------------------------------------------------------------------------------
// Marker labels

/// Short label for a Windows Update title, or None for noise (Defender definitions,
/// Store app updates, the monthly malware tool).
pub fn update_label(title: &str) -> Option<String> {
    let t = title.trim();
    if t.is_empty() {
        return None;
    }
    let low = t.to_ascii_lowercase();
    const NOISE: [&str; 6] = [
        "security intelligence",
        "definition update",
        "malicious software removal",
        "kb2267602",
        "kb890830",
        "kb4052623",
    ];
    if NOISE.iter().any(|n| low.contains(n)) || is_store_app(t) {
        return None;
    }
    match find_kb(t) {
        Some(kb) => Some(format!("Windows update {}", kb)),
        None => Some(format!("Update: {}", clip(t, 48))),
    }
}

/// "KB5044284" inside a title.
fn find_kb(t: &str) -> Option<String> {
    let b = t.as_bytes();
    let mut i = 0;
    while i + 2 < b.len() {
        if b[i].eq_ignore_ascii_case(&b'K') && b[i + 1].eq_ignore_ascii_case(&b'B') {
            let n = b[i + 2..].iter().take_while(|c| c.is_ascii_digit()).count();
            if n >= 6 {
                return Some(format!("KB{}", &t[i + 2..i + 2 + n]));
            }
        }
        i += 1;
    }
    None
}

/// Store app updates look like "9NBLGGH4NNS1-Microsoft.DesktopAppInstaller".
fn is_store_app(t: &str) -> bool {
    let id = t.split('-').next().unwrap_or("");
    id.len() == 12 && id.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) && !t.contains(' ')
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut o: String = s.chars().take(n - 1).collect();
        o.push('…');
        o
    }
}

// ---------------------------------------------------------------------------------------
// Daily probe (background thread): updates, build, GPU driver, startup entries

/// What the daily check found. `None` = could not read.
#[derive(Default)]
struct Probe {
    /// (event time unix ms, update title), newest first.
    updates: Vec<(u64, String)>,
    build: Option<String>,
    gpus: Option<Vec<(String, String)>>,
    run: Option<Vec<String>>,
}

/// Last seen system state, to spot changes between daily checks. None = never read.
#[derive(Default, Clone, Debug, PartialEq)]
struct Seen {
    build: Option<String>,
    gpus: Option<Vec<(String, String)>>,
    run: Option<Vec<String>>,
}

const UPDATE_XPATH: &str = "*[System[Provider[@Name='Microsoft-Windows-WindowsUpdateClient'] and EventID=19 \
     and TimeCreated[timediff(@SystemTime) <= 2592000000]]]";
const CV_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const DISPLAY_CLASS: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_KEY_WOW: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const MAX_RUN: usize = 64;

/// Slow reads; runs on a short-lived thread once a day.
fn run_probe() -> Probe {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let updates = super::events::query("System", UPDATE_XPATH, 50)
        .into_iter()
        .filter_map(|e| e.get("updateTitle").map(|t| (e.t_ms, t.to_string())))
        .collect();
    let build = reg::read_sz(HKEY_LOCAL_MACHINE, CV_KEY, "CurrentBuild").map(|b| {
        match crate::util::read_dword(HKEY_LOCAL_MACHINE, CV_KEY, "UBR") {
            Some(u) => format!("{}.{}", b, u),
            None => b,
        }
    });
    let mut gpus = Vec::new();
    for i in 0..10 {
        let sub = format!(r"{}\{:04}", DISPLAY_CLASS, i);
        if let (Some(d), Some(v)) = (
            reg::read_sz(HKEY_LOCAL_MACHINE, &sub, "DriverDesc"),
            reg::read_sz(HKEY_LOCAL_MACHINE, &sub, "DriverVersion"),
        ) {
            gpus.push((d, v));
        }
    }
    let mut run: Option<Vec<String>> = None;
    for (root, sub) in [(HKEY_CURRENT_USER, RUN_KEY), (HKEY_LOCAL_MACHINE, RUN_KEY), (HKEY_LOCAL_MACHINE, RUN_KEY_WOW)] {
        if let Some(names) = reg::value_names(root, sub, MAX_RUN) {
            run.get_or_insert_with(Vec::new).extend(names);
        }
    }
    if let Some(r) = &mut run {
        r.sort();
        r.dedup();
        r.truncate(MAX_RUN);
    }
    Probe { updates, build, gpus: (!gpus.is_empty()).then_some(gpus), run }
}

/// Small registry readers (standard-user read access).
mod reg {
    use crate::util::{from_wide, wide};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::*;

    /// A REG_SZ / REG_EXPAND_SZ value.
    pub fn read_sz(root: HKEY, sub: &str, name: &str) -> Option<String> {
        let sub = wide(sub);
        let name = wide(name);
        let mut buf = [0u16; 512];
        let mut sz = (buf.len() * 2) as u32;
        // SAFETY: NUL-terminated strings; `buf` is writable for `sz` bytes.
        let r = unsafe {
            RegGetValueW(
                root,
                sub.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut _,
                &mut sz,
            )
        };
        let s = from_wide(&buf);
        (r == ERROR_SUCCESS && !s.trim().is_empty()).then(|| s.trim().to_string())
    }

    /// Value names under a key (None = key missing or unreadable).
    pub fn value_names(root: HKEY, sub: &str, max: usize) -> Option<Vec<String>> {
        let sub = wide(sub);
        let mut hk: HKEY = std::ptr::null_mut();
        // SAFETY: NUL-terminated subkey, valid out pointer.
        if unsafe { RegOpenKeyExW(root, sub.as_ptr(), 0, KEY_QUERY_VALUE, &mut hk) } != ERROR_SUCCESS {
            return None;
        }
        let mut out = Vec::new();
        let mut name = [0u16; 256];
        for i in 0..max as u32 {
            let mut len = name.len() as u32;
            // SAFETY: `hk` is open; `name` holds `len` chars; other out pointers may be null.
            let r = unsafe {
                RegEnumValueW(
                    hk,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            let n = from_wide(&name[..(len as usize).min(name.len())]);
            if !n.is_empty() {
                out.push(n);
            }
        }
        // SAFETY: `hk` was opened above and is closed once.
        unsafe { RegCloseKey(hk) };
        Some(out)
    }
}

// ---------------------------------------------------------------------------------------
// The feature

pub struct Baseline {
    /// Finished days, oldest first.
    rows: Vec<DayRow>,
    /// Oldest first.
    markers: Vec<Marker>,
    today: Box<Accum>,
    /// Part of today saved before a restart; merged with `today` when the day ends.
    carry: Option<DayRow>,
    seen: Seen,
    /// Process-list window: ticks still wanted, and when the next window opens.
    proc_left: u8,
    next_proc_ms: u64,
    prev_sampled: bool,
    next_partial_secs: f64,
    last_probe_ms: u64,
    next_probe_ms: u64,
    rx: Option<Receiver<Probe>>,
    /// Off in unit tests so no thread reads the real registry.
    probe_enabled: bool,
    dirty: bool,
}

impl Baseline {
    pub fn new() -> Baseline {
        Baseline {
            rows: Vec::new(),
            markers: Vec::new(),
            today: Box::new(Accum::new()),
            carry: None,
            seen: Seen::default(),
            proc_left: 0,
            next_proc_ms: 0,
            prev_sampled: false,
            next_partial_secs: PARTIAL_SAVE_SECS,
            last_probe_ms: 0,
            next_probe_ms: 0,
            rx: None,
            probe_enabled: true,
            dirty: false,
        }
    }

    pub fn on_sample(&mut self, c: &Ctx) {
        self.poll_probe(c.now_ms);
        let sampled_before = self.prev_sampled;
        self.prev_sampled = c.procs_sampled;
        let s = c.snap;
        if !is_idle(c.input_idle_ms, c.display_on, s.uptime_secs, s.elapsed, c.interval_ms) {
            self.proc_left = 0;
            return;
        }
        let day = time::day(c.now_ms);
        if day != self.today.day {
            self.roll(day);
        }
        // CPU % per process needs two sampled ticks in a row.
        self.today.add(c, c.procs_sampled, c.procs_sampled && sampled_before);
        if self.today.secs >= self.next_partial_secs {
            self.next_partial_secs += PARTIAL_SAVE_SECS;
            self.dirty = true;
        }
        // Ask for process lists in short windows while idle.
        if self.proc_left > 0 {
            self.proc_left -= 1;
        } else if c.now_ms >= self.next_proc_ms {
            self.proc_left = PROC_WINDOW_TICKS;
            self.next_proc_ms = c.now_ms + PROC_EVERY_MS;
        }
    }

    pub fn on_event(&mut self, e: &SysEvent) {
        if e.provider == "Microsoft-Windows-WindowsUpdateClient" && e.id == 19 {
            if let Some(t) = e.get("updateTitle") {
                let ms = if e.t_ms > 0 { e.t_ms } else { super::unix_ms() };
                if let Some(l) = update_label(t) {
                    self.add_marker(time::day(ms), Kind::Update, l);
                }
            }
        }
    }

    pub fn wants_procs(&self) -> bool {
        self.proc_left > 0
    }

    /// Persistent state changed since the last save.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The state was written (`save` takes `&self`, so the orchestrator clears the flag).
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Start a new day: finish the old one (merged with any carried part) and keep 60.
    fn roll(&mut self, day: u32) {
        if let Some(r) = self.finished_today() {
            self.rows.retain(|x| x.day != r.day);
            self.rows.push(r);
            self.rows.sort_by_key(|r| r.day);
            if self.rows.len() > MAX_ROWS {
                self.rows.drain(..self.rows.len() - MAX_ROWS);
            }
            self.dirty = true;
        }
        self.today.reset(day);
        self.next_partial_secs = PARTIAL_SAVE_SECS;
        // A row for this very day came from before a restart: keep adding to it.
        self.carry = match self.rows.last() {
            Some(r) if r.day == day => self.rows.pop(),
            _ => None,
        };
    }

    /// Today's row so far, merged with the part saved before a restart.
    fn finished_today(&self) -> Option<DayRow> {
        match (self.carry.as_ref(), self.today.row()) {
            (Some(c), Some(t)) => Some(c.merge(&t)),
            (Some(c), None) => Some(c.clone()),
            (None, t) => t,
        }
    }

    /// All rows including today's partial one, oldest first.
    fn all_rows(&self) -> Vec<DayRow> {
        let mut v = self.rows.clone();
        if let Some(t) = self.finished_today() {
            v.retain(|r| r.day != t.day);
            v.push(t);
        }
        v
    }

    /// Add a marker unless the same one is already there. Returns true when added.
    fn add_marker(&mut self, day: u32, kind: Kind, label: String) -> bool {
        if self.markers.iter().any(|m| m.kind == kind && m.label == label) {
            return false;
        }
        self.markers.push(Marker { day, kind, label });
        self.markers.sort_by_key(|m| m.day);
        if self.markers.len() > MAX_MARKERS {
            self.markers.drain(..self.markers.len() - MAX_MARKERS);
        }
        self.dirty = true;
        true
    }

    fn poll_probe(&mut self, now_ms: u64) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(p) => {
                    self.rx = None;
                    self.apply_probe(p, time::day(now_ms));
                }
                Err(TryRecvError::Disconnected) => self.rx = None,
                Err(TryRecvError::Empty) => {}
            }
            return;
        }
        if !self.probe_enabled {
            return;
        }
        if self.next_probe_ms == 0 {
            // At most a day out: a saved time from a clock that was ahead must not stop the
            // check for months (and must not overflow).
            self.next_probe_ms = (now_ms + PROBE_FIRST_DELAY_MS)
                .max(self.last_probe_ms.saturating_add(PROBE_EVERY_MS))
                .min(now_ms + PROBE_EVERY_MS);
        }
        if now_ms < self.next_probe_ms {
            return;
        }
        self.last_probe_ms = now_ms;
        self.next_probe_ms = now_ms + PROBE_EVERY_MS;
        self.dirty = true;
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("baseline-probe".into()).spawn(move || {
            let _ = tx.send(run_probe());
        });
        if spawned.is_ok() {
            self.rx = Some(rx);
        }
    }

    /// Turn a daily check into markers. The first ever check only records the state.
    fn apply_probe(&mut self, p: Probe, today: u32) {
        for (t, title) in p.updates.iter().rev() {
            if let Some(l) = update_label(title) {
                self.add_marker(time::day(*t), Kind::Update, l);
            }
        }
        if let Some(b) = p.build {
            if let Some(old) = self.seen.build.as_deref() {
                if old != b {
                    let major = |s: &str| s.split('.').next().unwrap_or("").to_string();
                    // A new UBR is the monthly update, already marked by its update event.
                    let recent_update =
                        self.markers.iter().any(|m| m.kind == Kind::Update && m.day + 3 >= today);
                    if major(old) != major(&b) || !recent_update {
                        self.add_marker(today, Kind::Build, format!("Windows build {}", b));
                    }
                }
            }
            if self.seen.build.as_ref() != Some(&b) {
                self.seen.build = Some(b);
                self.dirty = true;
            }
        }
        if let Some(g) = p.gpus {
            if let Some(old) = &self.seen.gpus {
                let mut add = Vec::new();
                for (desc, ver) in &g {
                    match old.iter().find(|(d, _)| d == desc) {
                        Some((_, v)) if v != ver => add.push(ver.clone()),
                        // A new adapter; Microsoft's remote / basic adapters come and go.
                        None if !desc.starts_with("Microsoft") => add.push(ver.clone()),
                        _ => {}
                    }
                }
                for v in add {
                    self.add_marker(today, Kind::Driver, format!("GPU driver {}", v));
                }
            }
            if self.seen.gpus.as_ref() != Some(&g) {
                self.seen.gpus = Some(g);
                self.dirty = true;
            }
        }
        if let Some(r) = p.run {
            if let Some(old) = &self.seen.run {
                let new: Vec<String> = r.iter().filter(|n| !old.contains(n)).cloned().collect();
                for n in new {
                    self.add_marker(today, Kind::Startup, format!("New at startup: {}", clip(&n, 40)));
                }
            }
            if self.seen.run.as_ref() != Some(&r) {
                self.seen.run = Some(r);
                self.dirty = true;
            }
        }
    }

    pub fn save(&self) -> Json {
        let num = |x: f64| Json::Num(x);
        let rows: Vec<Json> = self
            .all_rows()
            .iter()
            .map(|r| {
                let top = r
                    .top
                    .iter()
                    .map(|(n, v)| Json::Arr(vec![Json::Str(clip(n, 24)), num((*v as f64 * 10.0).round() / 10.0)]))
                    .collect();
                Json::Arr(vec![
                    num(r.day as f64),
                    num(r.mins.round() as f64),
                    num((r.cpu as f64 * 10.0).round() / 10.0),
                    num((r.ram as f64 / MB).round()),
                    num(r.procs as f64),
                    num(r.disk.round()),
                    num(r.net.round()),
                    Json::Arr(top),
                ])
            })
            .collect();
        let marks = self
            .markers
            .iter()
            .map(|m| Json::Arr(vec![num(m.day as f64), num(m.kind.code() as f64), Json::Str(m.label.clone())]))
            .collect();
        let strs = |v: &[String]| Json::Arr(v.iter().map(|s| Json::Str(s.clone())).collect());
        let mut seen = Vec::new();
        if let Some(b) = &self.seen.build {
            seen.push(("build", Json::Str(b.clone())));
        }
        if let Some(g) = &self.seen.gpus {
            let a = g.iter().map(|(d, v)| Json::Arr(vec![Json::Str(d.clone()), Json::Str(v.clone())])).collect();
            seen.push(("gpu", Json::Arr(a)));
        }
        if let Some(r) = &self.seen.run {
            seen.push(("run", strs(r)));
        }
        obj(vec![
            ("rows", Json::Arr(rows)),
            ("marks", Json::Arr(marks)),
            ("seen", obj(seen)),
            ("lp", num(self.last_probe_ms as f64)),
        ])
    }

    pub fn load(&mut self, j: &Json) {
        let n = |a: &[Json], i: usize| a.get(i).and_then(|x| match x {
            Json::Num(v) if v.is_finite() && *v >= 0.0 => Some(*v),
            _ => None,
        });
        let mut rows = Vec::new();
        for r in j.get("rows").and_then(Json::as_arr).unwrap_or(&[]) {
            let Some(a) = r.as_arr() else { continue };
            let (Some(day), Some(mins), Some(cpu), Some(ram)) = (n(a, 0), n(a, 1), n(a, 2), n(a, 3)) else { continue };
            let mut top = Vec::new();
            for t in a.get(7).and_then(Json::as_arr).unwrap_or(&[]).iter().take(TOP_KEEP) {
                if let Some(p) = t.as_arr() {
                    if let (Some(name), Some(v)) = (p.first().and_then(Json::as_str), n(p, 1)) {
                        top.push((name.to_string(), v as f32));
                    }
                }
            }
            rows.push(DayRow {
                day: day as u32,
                mins: mins as f32,
                cpu: cpu.min(100.0) as f32,
                ram: (ram * MB) as u64,
                procs: n(a, 4).unwrap_or(0.0) as u32,
                disk: n(a, 5).unwrap_or(0.0),
                net: n(a, 6).unwrap_or(0.0),
                top,
            });
        }
        rows.sort_by_key(|r| r.day);
        rows.dedup_by_key(|r| r.day);
        if rows.len() > MAX_ROWS {
            rows.drain(..rows.len() - MAX_ROWS);
        }
        self.rows = rows;
        self.markers.clear();
        for m in j.get("marks").and_then(Json::as_arr).unwrap_or(&[]) {
            let Some(a) = m.as_arr() else { continue };
            let (Some(day), Some(k), Some(label)) =
                (n(a, 0), n(a, 1).and_then(|k| Kind::from_code(k as u64)), a.get(2).and_then(Json::as_str))
            else {
                continue;
            };
            self.add_marker(day as u32, k, label.to_string());
        }
        if let Some(s) = j.get("seen") {
            self.seen.build = s.get("build").and_then(Json::as_str).map(str::to_string);
            self.seen.gpus = s.get("gpu").and_then(Json::as_arr).map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let p = p.as_arr()?;
                        Some((p.first()?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string()))
                    })
                    .collect()
            });
            self.seen.run = s
                .get("run")
                .and_then(Json::as_arr)
                .map(|a| a.iter().filter_map(Json::as_str).map(str::to_string).take(MAX_RUN).collect());
        }
        self.last_probe_ms = super::store::num(j, "lp").unwrap_or(0.0) as u64;
        self.next_probe_ms = 0;
        self.today.reset(0);
        self.carry = None;
        self.dirty = false;
    }

    pub fn panel(&self, now_ms: u64) -> Panel {
        let rows = self.all_rows();
        let (tone, headline) = verdict(&rows, &self.markers);
        let today = time::day(now_ms);
        let mut lines = Vec::new();
        match rows.iter().find(|r| r.day == today) {
            Some(r) => {
                let mut s = format!("Today at idle: CPU {}, RAM {}", fmt_cpu(r.cpu as f64), fmt_gb(r.ram as f64));
                if r.procs > 0 {
                    s.push_str(&format!(", {} processes", r.procs));
                }
                s.push_str(&format!(". {} min idle.", r.mins.round() as u32));
                lines.push(s);
            }
            None => lines.push("Today: no idle time yet.".into()),
        }
        let usable = rows.iter().filter(|r| r.usable()).count();
        lines.push(match usable {
            1 => "Baseline: 1 day collected.".into(),
            n => format!("Baseline: {} days collected.", n),
        });
        if let Some(m) = self.markers.last() {
            lines.push(format!("Last change: {}, {}.", m.label, time::day_label(m.day)));
        }
        Panel {
            key: "baseline",
            title: "Before / After",
            tone,
            headline,
            lines,
            chart: self.chart(&rows),
            items: self
                .markers
                .iter()
                .rev()
                .map(|m| {
                    let (tone, detail) = marker_detail(&rows, m);
                    Item { t_ms: 0, tone, title: m.label.clone(), detail }
                })
                .collect(),
        }
    }

    /// Idle RAM per usable day (up to 30), with markers on the first day on or after each change.
    fn chart(&self, rows: &[DayRow]) -> Option<Chart> {
        let usable: Vec<&DayRow> = rows.iter().filter(|r| r.usable() && r.ram > 0).collect();
        let pts = &usable[usable.len().saturating_sub(30)..];
        if pts.len() < 2 {
            return None;
        }
        let first = pts[0].day;
        let mut markers: Vec<(usize, String)> = Vec::new();
        for m in self.markers.iter().filter(|m| m.day >= first) {
            if let Some(i) = pts.iter().position(|r| r.day >= m.day) {
                if !markers.iter().any(|(j, _)| *j == i) {
                    markers.push((i, m.kind.short().to_string()));
                }
            }
        }
        Some(Chart {
            label: "Idle RAM per day".into(),
            points: pts.iter().map(|r| (r.ram as f64 / GB) as f32).collect(),
            max: 0.0,
            unit: "GB",
            markers,
            bars: true,
            slots: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::{ProcStat, Snapshot};

    fn snap(cpu: f32, ram_gb: f64) -> Snapshot {
        Snapshot {
            cpu,
            ram_used: (ram_gb * GB) as u64,
            ram_total: 16 << 30,
            uptime_secs: 3600,
            elapsed: 1.0,
            ..Default::default()
        }
    }

    fn ctx(now_ms: u64, s: &Snapshot) -> Ctx<'_> {
        Ctx { now_ms, snap: s, display_on: true, input_idle_ms: 600_000, interval_ms: 1000, procs_sampled: false }
    }

    fn quiet() -> Baseline {
        let mut b = Baseline::new();
        b.probe_enabled = false;
        b
    }

    fn row(day: u32, cpu: f32, ram_gb: f64, procs: u32, disk: f64, top: &[(&str, f32)]) -> DayRow {
        DayRow {
            day,
            mins: 60.0,
            cpu,
            ram: (ram_gb * GB) as u64,
            procs,
            disk,
            net: 0.0,
            top: top.iter().map(|(n, v)| (n.to_string(), *v)).collect(),
        }
    }

    const D: u32 = 20_400;
    const T0: u64 = D as u64 * 86_400_000 + 12 * 3_600_000;

    #[test]
    fn idle_gate() {
        assert!(is_idle(300_000, true, 600, 1.0, 1000));
        assert!(!is_idle(299_999, true, 600, 1.0, 1000), "recent input");
        assert!(!is_idle(300_000, false, 600, 1.0, 1000), "display off");
        assert!(!is_idle(300_000, true, 599, 1.0, 1000), "just booted");
        assert!(!is_idle(300_000, true, 600, 3.5, 1000), "late tick");
        assert!(is_idle(300_000, true, 600, 3.0, 1000), "within slack");
        assert!(!is_idle(300_000, true, 600, 0.0, 1000), "first sample");
    }

    #[test]
    fn streaming_median_accuracy() {
        let mut b = quiet();
        // CPU 0.0 .. 9.9 uniform, RAM 4.0 .. 6.0 GB, disk 1 KB/s .. 1 MB/s geometric.
        for i in 0..1000u64 {
            let mut s = snap((i % 100) as f32 / 10.0, 4.0 + (i % 200) as f64 / 100.0);
            s.disk_read = 1024.0 * 2f64.powf((i % 100) as f64 / 10.0);
            b.on_sample(&ctx(T0 + i * 1000, &s));
        }
        let r = b.today.row().unwrap();
        assert!((r.cpu - 4.95).abs() <= 0.5, "cpu {}", r.cpu);
        assert!((r.ram as f64 / GB - 5.0).abs() <= 0.07, "ram {}", r.ram as f64 / GB);
        let want = 1024.0 * 2f64.powf(4.95);
        assert!((r.disk / want - 1.0).abs() < 0.2, "disk {} vs {}", r.disk, want);
        assert!((r.mins - 1000.0 / 60.0).abs() < 0.01);
        assert_eq!(r.procs, 0, "procs never sampled");
    }

    #[test]
    fn median_helper() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&mut []), None);
    }

    #[test]
    fn day_rollover() {
        let mut b = quiet();
        for i in 0..29 * 60u64 {
            b.on_sample(&ctx(T0 + i * 1000, &snap(2.0, 5.0)));
        }
        assert!(b.rows.is_empty());
        assert!(!b.dirty(), "no dirty before the partial-save point");
        let t1 = T0 + 86_400_000;
        b.on_sample(&ctx(t1, &snap(8.0, 7.0)));
        assert!(b.dirty());
        assert_eq!(b.rows.len(), 1);
        let r = &b.rows[0];
        assert_eq!(r.day, time::day(T0));
        assert!((r.cpu - 2.25).abs() < 0.3);
        assert!(r.usable());
        assert_eq!(b.today.day, time::day(t1));
        assert_eq!(b.today.cpu.n, 1);
    }

    #[test]
    fn process_windows_only_while_idle() {
        let mut b = quiet();
        let mut s = snap(1.0, 4.0);
        assert!(!b.wants_procs());
        b.on_sample(&ctx(T0, &s));
        assert!(b.wants_procs(), "window opens when idle");
        s.proc_count = 200;
        s.top_cpu = vec![ProcStat { name: "SearchHost".into(), cpu: 1.0, ..Default::default() }];
        let mut asked = 0;
        for i in 1..30u64 {
            let mut c = ctx(T0 + i * 1000, &s);
            c.procs_sampled = b.wants_procs();
            asked += c.procs_sampled as u32;
            b.on_sample(&c);
        }
        assert_eq!(asked, PROC_WINDOW_TICKS as u32);
        assert!(!b.wants_procs(), "window closed");
        let r = b.today.row().unwrap();
        assert_eq!(r.procs, 200);
        assert_eq!(r.top[0].0, "SearchHost");
        assert!(r.top[0].1 > 0.9 && r.top[0].1 <= 1.0);
        // Input stops the window.
        b.proc_left = 3;
        let mut c = ctx(T0 + 40_000, &s);
        c.input_idle_ms = 0;
        b.on_sample(&c);
        assert!(!b.wants_procs());
    }

    #[test]
    fn diff_thresholds_and_text() {
        let mut rows = Vec::new();
        for d in 0..5 {
            rows.push(row(D + d, 2.0, 5.0, 200, 50_000.0, &[("MsMpEng", 0.6)]));
        }
        let m = D + 5;
        for d in 6..10 {
            rows.push(row(D + d, 6.0, 6.1, 210, 60_000.0, &[("MsMpEng", 0.6), ("WidgetService", 1.2)]));
        }
        let d = diff_at(&rows, m).unwrap();
        assert_eq!(d.changes.len(), 2, "{:?}", d.changes);
        assert_eq!(d.changes[0].metric, Metric::Ram);
        assert_eq!(d.changes[1].metric, Metric::Cpu);
        assert_eq!(d.new_procs, vec!["WidgetService".to_string()]);
        let mk = Marker { day: m, kind: Kind::Update, label: "Windows update KB5044284".into() };
        let h = diff_headline(&mk, &d);
        assert_eq!(
            h,
            format!(
                "Since the {} update: idle RAM +1.1 GB, idle CPU 2% to 6%. New at idle: WidgetService.",
                time::day_label(m)
            )
        );
        let (tone, v) = verdict(&rows, &[mk.clone()]);
        assert_eq!(tone, Tone::Warn);
        assert_eq!(v, h);

        // Below every threshold: CPU +1.4, RAM +290 MB, procs +14, disk 1.9x.
        let mut quiet_rows = Vec::new();
        for d in 0..5 {
            quiet_rows.push(row(D + d, 2.0, 5.0, 200, 300_000.0, &[]));
        }
        for d in 6..10 {
            quiet_rows.push(row(D + d, 3.4, 5.0 + 290.0 / 1024.0, 214, 570_000.0, &[("Tiny", 0.4)]));
        }
        let q = diff_at(&quiet_rows, m).unwrap();
        assert!(q.is_empty(), "{:?}", q);
        assert_eq!(diff_headline(&mk, &q), format!("The {} update changed nothing at idle.", time::day_label(m)));
        assert_eq!(verdict(&quiet_rows, &[mk.clone()]).0, Tone::Good);

        // CPU +2 points but less than 50 % relative: not meaningful.
        let mut rel = Vec::new();
        for d in 0..5 {
            rel.push(row(D + d, 10.0, 5.0, 0, 0.0, &[]));
        }
        for d in 6..10 {
            rel.push(row(D + d, 12.0, 5.0, 0, 0.0, &[]));
        }
        assert!(diff_at(&rel, m).unwrap().is_empty());

        // Disk 3x and +400 KB/s, processes +20.
        let mut io = Vec::new();
        for d in 0..5 {
            io.push(row(D + d, 2.0, 5.0, 200, 200_000.0, &[]));
        }
        for d in 6..10 {
            io.push(row(D + d, 2.0, 5.0, 220, 600_000.0, &[]));
        }
        let x = diff_at(&io, m).unwrap();
        let metrics: Vec<Metric> = x.changes.iter().map(|c| c.metric).collect();
        assert_eq!(metrics, vec![Metric::Disk, Metric::Procs]);
        assert!(diff_headline(&mk, &x).contains("20 more processes"));

        // Only 2 days after: no diff yet.
        assert!(diff_at(&rows[..7], m).is_none());
        // Days more than 14 before the marker do not count.
        assert!(diff_at(&rows, D + 30).is_none());
    }

    #[test]
    fn marker_dedupe_and_labels() {
        let mut b = quiet();
        let e = SysEvent {
            channel: "System".into(),
            provider: "Microsoft-Windows-WindowsUpdateClient".into(),
            id: 19,
            t_ms: T0,
            data: vec![(
                "updateTitle".into(),
                "2024-10 Cumulative Update for Windows 11 Version 23H2 for x64-based Systems (KB5044284)".into(),
            )],
        };
        b.on_event(&e);
        b.on_event(&e);
        assert_eq!(b.markers.len(), 1);
        assert_eq!(b.markers[0].label, "Windows update KB5044284");
        assert_eq!(b.markers[0].day, time::day(T0));
        // Back-fill of the same update does not add it again.
        let p = Probe { updates: vec![(T0, e.data[0].1.clone())], ..Default::default() };
        b.apply_probe(p, D);
        assert_eq!(b.markers.len(), 1);
        assert!(update_label("Security Intelligence Update for Microsoft Defender Antivirus - KB2267602").is_none());
        assert!(update_label("9NBLGGH4NNS1-Microsoft.DesktopAppInstaller").is_none());
        assert_eq!(
            update_label("Intel Corporation - Display - 31.0.101.4502").as_deref(),
            Some("Update: Intel Corporation - Display - 31.0.101.4502")
        );
        // First registry read only records; later changes become markers once.
        let snapshot = |build: &str, ver: &str, run: &[&str]| Probe {
            updates: vec![],
            build: Some(build.into()),
            gpus: Some(vec![("NVIDIA GeForce RTX 4060".into(), ver.into())]),
            run: Some(run.iter().map(|s| s.to_string()).collect()),
        };
        let mut c = quiet();
        c.apply_probe(snapshot("22631.4317", "32.0.15.6094", &["SecurityHealth"]), D);
        assert!(c.markers.is_empty());
        c.apply_probe(snapshot("26100.2314", "32.0.15.6614", &["OneDrive", "SecurityHealth"]), D + 1);
        let labels: Vec<&str> = c.markers.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["Windows build 26100.2314", "GPU driver 32.0.15.6614", "New at startup: OneDrive"]);
        c.apply_probe(snapshot("26100.2314", "32.0.15.6614", &["OneDrive", "SecurityHealth"]), D + 2);
        assert_eq!(c.markers.len(), 3);
        // Cap at 60.
        for i in 0..70 {
            c.add_marker(D + i, Kind::Startup, format!("New at startup: App{}", i));
        }
        assert_eq!(c.markers.len(), MAX_MARKERS);
    }

    #[test]
    fn learning_and_steady_states() {
        let rows: Vec<DayRow> = (0..2).map(|d| row(D + d, 2.0, 5.1, 210, 0.0, &[])).collect();
        assert_eq!(verdict(&rows, &[]), (Tone::Learning, "Learning what idle looks like. 2 of 3 days.".into()));
        let mut short = rows.clone();
        short.push(DayRow { mins: 19.0, ..row(D + 2, 2.0, 5.1, 210, 0.0, &[]) });
        assert_eq!(verdict(&short, &[]).1, "Learning what idle looks like. 2 of 3 days.", "19 min is not a day");
        let rows: Vec<DayRow> = (0..4).map(|d| row(D + d, 2.0, 5.1, 210, 0.0, &[])).collect();
        assert_eq!(verdict(&rows, &[]), (Tone::Info, "Idle looks steady. CPU 2%, RAM 5.1 GB, 210 processes.".into()));
        let mk = Marker { day: D + 3, kind: Kind::Update, label: "Windows update KB1234567".into() };
        let (tone, text) = verdict(&rows[..3], &[mk]);
        assert_eq!(tone, Tone::Learning);
        assert_eq!(text, format!("Watching the {} update. 0 of 3 days after.", time::day_label(D + 3)));
    }

    #[test]
    fn save_load_roundtrip_and_restart_merge() {
        let mut b = quiet();
        b.rows = (0..3).map(|d| row(D - 5 + d, 2.0, 5.0, 200, 1000.0, &[("MsMpEng", 0.6)])).collect();
        b.add_marker(D + 1, Kind::Driver, "GPU driver 1.2.3".into());
        b.seen.build = Some("22631.1".into());
        for i in 0..600u64 {
            b.on_sample(&ctx(T0 + i * 1000, &snap(4.0, 6.0)));
        }
        let j = b.save();
        let mut c = quiet();
        c.load(&crate::achievements::json::parse(&j.to_string()).unwrap());
        assert_eq!(c.rows.len(), 4, "today's partial row saved");
        assert_eq!(c.markers, b.markers);
        assert_eq!(c.seen.build.as_deref(), Some("22631.1"));
        assert!(!c.dirty());
        // After the restart, more idle time on the same day merges with the saved part.
        for i in 0..600u64 {
            c.on_sample(&ctx(T0 + 700_000 + i * 1000, &snap(4.0, 6.0)));
        }
        let today = c.all_rows().pop().unwrap();
        assert_eq!(today.day, time::day(T0));
        assert!((today.mins - 20.0).abs() < 0.5, "mins {}", today.mins);
        assert!(today.usable());
        // Garbage tolerated.
        let mut g = quiet();
        g.load(&crate::achievements::json::parse(r#"{"rows":[1,[2],["x"]],"marks":"no","seen":5}"#).unwrap());
        assert!(g.rows.is_empty() && g.markers.is_empty());
        let p = g.panel(T0);
        assert_eq!(p.tone, Tone::Learning);
    }

    #[test]
    fn saved_state_stays_small() {
        let mut b = quiet();
        let top: Vec<(&str, f32)> =
            vec![("MsMpEng", 1.2), ("SearchIndexer", 0.9), ("WidgetService", 0.7), ("svchost", 0.6), ("dwm", 0.5)];
        b.rows = (0..80).map(|d| row(D + d, 12.3, 7.5, 312, 123_456.0, &top)).collect();
        b.rows.truncate(MAX_ROWS);
        for i in 0..80 {
            b.add_marker(D + i, Kind::Update, format!("Windows update KB50{:05}", i));
        }
        b.seen.run = Some((0..MAX_RUN).map(|i| format!("StartupEntry{}", i)).collect());
        let n = b.save().to_string().len();
        assert!(n < 16 * 1024, "{} bytes", n);
    }

    #[test]
    fn panel_chart_and_items() {
        let mut b = quiet();
        b.rows = (0..8).map(|d| row(D + d, 2.0, 5.0 + d as f64 * 0.1, 200, 0.0, &[])).collect();
        b.add_marker(D + 4, Kind::Update, "Windows update KB5044284".into());
        let p = b.panel(T0 + 30 * 86_400_000);
        let ch = p.chart.unwrap();
        assert_eq!(ch.points.len(), 8);
        assert_eq!(ch.markers, vec![(4, "Update".to_string())]);
        assert_eq!(ch.unit, "GB");
        assert_eq!(p.items.len(), 1);
        assert!(p.items[0].detail.starts_with(&time::day_label(D + 4)));
        assert_eq!(p.lines[0], "Today: no idle time yet.");
        assert_eq!(p.lines[1], "Baseline: 8 days collected.");
    }
}
