//! Achievements observer: feeds the app's own events into the pure core.
//!
//! No timers, threads or polling of its own. It piggybacks on the sample tick
//! (`on_sample`) and on events the app already has (alerts, End task, update
//! restart, billing cycle). Pure logic (minute bucketing, the active decision,
//! cycle rollover, availability) lives in plain functions with unit tests.
//!
//! Active minute: wall-clock minute in which a widget was on a taskbar
//! (`shown || fullscreen`) AND the user gave input within the last 5 minutes.
//! The input check runs on each sample tick; a minute counts when at least one
//! of its samples saw recent input.
//!
//! `Ctx.unavailable` (ids this machine cannot earn right now):
//! * `fumes`: battery data never seen this run (desktop).
//! * `render`, `running_hot`: GPU load / GPU temperature never seen this run,
//!   but only once the sensors were actually queried for 3+ samples (GPU sensors
//!   are only read while a GPU metric is shown or the panel is warm; if they
//!   were never asked, the badge stays available).
//! * `under_cap`: no data cap set.

use std::time::{Duration, Instant};

use super::core::{apply, Ctx, Event, LocalTime, Minute, State};
use super::store::{self, Moment};
use crate::usage::{prev_day, UsageStore};

/// Input younger than this makes the user "there".
pub const INPUT_WINDOW_MS: u64 = 5 * 60_000;
const SAVE_EVERY: Duration = Duration::from_secs(60);
/// Samples with the GPU queried before "no GPU data" counts as unavailable.
const GPU_TRIES: u32 = 3;

/// Alert kinds for `Event::AlertFired` / `AlertCleared`.
pub mod kind {
    pub const CPU: u8 = 0;
    pub const RAM: u8 = 1;
    pub const DISK: u8 = 2;
    pub const BATTERY: u8 = 3;
    pub const CAP: u8 = 4;
}

/// Everything one sample tick tells the observer.
#[derive(Clone, Debug, Default)]
pub struct SampleInput {
    pub now_ms: u64,
    pub cpu: f32,
    pub gpu: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub net_down: u64,
    pub net_up: u64,
    /// (read + write) bytes/s times the elapsed seconds.
    pub disk_bytes: u64,
    pub ram_pct: f32,
    pub disk_used_pct: Option<f32>,
    pub battery: Option<(u8, bool)>,
    pub uptime_s: u64,
    /// Overlays with `shown || fullscreen` (0 when the display is off).
    pub taskbars: u8,
    pub fullscreen: bool,
    /// User input within `INPUT_WINDOW_MS`.
    pub input_recent: bool,
    /// The GPU sensors were queried on this sample.
    pub gpu_queried: bool,
    pub cap_set: bool,
}

/// One wall-clock minute being accumulated.
#[derive(Clone, Debug, PartialEq)]
pub struct Bucket {
    pub minute: u64,
    last_ms: u64,
    samples: u32,
    cpu_sum: f64,
    gpu_sum: f64,
    gpu_n: u32,
    down: u64,
    up: u64,
    disk: u64,
    gpu_temp: Option<f32>,
    ram_max: f32,
    disk_used_pct: Option<f32>,
    battery: Option<(u8, bool)>,
    uptime_s: u64,
    taskbars: u8,
    fullscreen: bool,
    input_ok: bool,
}

impl Bucket {
    fn start(s: &SampleInput) -> Bucket {
        let mut b = Bucket {
            minute: s.now_ms / 60_000,
            last_ms: s.now_ms,
            samples: 0,
            cpu_sum: 0.0,
            gpu_sum: 0.0,
            gpu_n: 0,
            down: 0,
            up: 0,
            disk: 0,
            gpu_temp: None,
            ram_max: 0.0,
            disk_used_pct: None,
            battery: None,
            uptime_s: 0,
            taskbars: 0,
            fullscreen: false,
            input_ok: false,
        };
        b.add(s);
        b
    }

    fn add(&mut self, s: &SampleInput) {
        self.last_ms = s.now_ms;
        self.samples += 1;
        self.cpu_sum += s.cpu as f64;
        if let Some(g) = s.gpu {
            self.gpu_sum += g as f64;
            self.gpu_n += 1;
        }
        self.down = self.down.saturating_add(s.net_down);
        self.up = self.up.saturating_add(s.net_up);
        self.disk = self.disk.saturating_add(s.disk_bytes);
        if let Some(t) = s.gpu_temp {
            self.gpu_temp = Some(self.gpu_temp.map_or(t, |o| o.max(t)));
        }
        self.ram_max = self.ram_max.max(s.ram_pct);
        if s.disk_used_pct.is_some() {
            self.disk_used_pct = s.disk_used_pct;
        }
        if s.battery.is_some() {
            self.battery = s.battery;
        }
        self.uptime_s = s.uptime_s;
        self.taskbars = self.taskbars.max(s.taskbars);
        self.fullscreen |= s.fullscreen;
        self.input_ok |= s.input_recent;
    }

    /// A real minute: a widget was on a taskbar and the user was there.
    pub fn active(&self) -> bool {
        self.taskbars > 0 && self.input_ok
    }

    pub fn to_minute(&self, tz_offset_min: i32) -> Minute {
        let n = self.samples.max(1) as f64;
        Minute {
            widget_taskbars: self.taskbars,
            cpu: (self.cpu_sum / n) as f32,
            gpu: (self.gpu_n > 0).then(|| (self.gpu_sum / self.gpu_n as f64) as f32),
            fullscreen: self.fullscreen,
            net_bytes: self.down.saturating_add(self.up),
            down_bytes: self.down,
            up_bytes: self.up,
            disk_bytes: self.disk,
            gpu_temp: self.gpu_temp,
            ram_pct: self.ram_max,
            disk_used_pct: self.disk_used_pct,
            battery: self.battery,
            uptime_s: self.uptime_s,
            local: LocalTime::from_unix_ms(self.last_ms, tz_offset_min),
            now_ms: self.last_ms,
        }
    }
}

/// Add a sample to the current bucket. When it belongs to another minute
/// (including a clock that went back) the old bucket is returned, finished.
pub fn roll(cur: &mut Option<Bucket>, s: &SampleInput) -> Option<Bucket> {
    match cur {
        Some(b) if b.minute == s.now_ms / 60_000 => {
            b.add(s);
            None
        }
        _ => cur.replace(Bucket::start(s)),
    }
}

/// Local UTC offset in minutes from `TIME_ZONE_INFORMATION` values.
/// `id`: 1 = standard time, 2 = daylight time, anything else = no extra bias.
pub fn tz_offset_from(bias: i32, standard_bias: i32, daylight_bias: i32, id: u32) -> i32 {
    let extra = match id {
        1 => standard_bias,
        2 => daylight_bias,
        _ => 0,
    };
    -(bias + extra)
}

/// Billing cycle bookkeeping. `stored` = (cycle start, reset day) last seen.
/// Returns the new stored value and the start of a cycle that just closed.
pub fn cycle_step(stored: Option<(u32, u32)>, start: u32, reset: u32) -> (Option<(u32, u32)>, Option<u32>) {
    let now = Some((start, reset));
    match stored {
        None => (now, None),
        Some(s) if s == (start, reset) => (stored, None),
        Some((s0, r0)) => {
            // Only the immediate predecessor counts (not a changed reset day,
            // nor a cycle long gone while no cap or no app was around).
            let closed = r0 == reset && UsageStore::cycle_start(reset, prev_day(start)) == s0;
            (now, closed.then_some(s0))
        }
    }
}

/// Badge ids this machine cannot earn right now (see the module docs).
pub fn unavailable_ids(battery_seen: bool, samples: u32, gpu_tries: u32, gpu_seen: bool, temp_seen: bool, cap_set: bool) -> Vec<&'static str> {
    let mut v = Vec::new();
    if samples > 0 && !battery_seen {
        v.push("fumes");
    }
    if gpu_tries >= GPU_TRIES {
        if !gpu_seen {
            v.push("render");
        }
        if !temp_seen {
            v.push("running_hot");
        }
    }
    if !cap_set {
        v.push("under_cap");
    }
    v
}

pub struct Observer {
    state: State,
    bucket: Option<Bucket>,
    dirty: bool,
    last_save: Instant,
    persist: bool,
    tz_override: Option<i32>,
    // What the machine has shown us this run.
    samples: u32,
    battery_seen: bool,
    gpu_tries: u32,
    gpu_seen: bool,
    temp_seen: bool,
    cap_set: bool,
    cycle: Option<(u32, u32)>,
    last_cycle_minute: u64,
}

impl Observer {
    pub fn new() -> Observer {
        let mut o = Observer::with_state(store::load(), true);
        o.cycle = read_cycle();
        o
    }

    fn with_state(state: State, persist: bool) -> Observer {
        Observer {
            state,
            bucket: None,
            dirty: false,
            last_save: Instant::now(),
            persist,
            tz_override: None,
            samples: 0,
            battery_seen: false,
            gpu_tries: 0,
            gpu_seen: false,
            temp_seen: false,
            cap_set: false,
            cycle: None,
            last_cycle_minute: 0,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// One rub of the polishing cloth on medal `id` (the Achievements page). Saved at once. Returns the shine earned.
    pub fn polish(&mut self, id: &str) -> u64 {
        let (_, gain) = self.state.polish(id, unix_ms());
        if gain > 0 {
            self.dirty = true;
            self.save_if_due(true);
        }
        gain
    }

    /// Achievements switched off: forget the half-built minute.
    pub fn pause(&mut self) {
        self.bucket = None;
    }

    /// One sample tick. Returns the moments unlocked by a minute that just ended.
    pub fn on_sample(&mut self, s: &SampleInput) -> Vec<Moment> {
        self.samples += 1;
        self.battery_seen |= s.battery.is_some();
        self.gpu_seen |= s.gpu.is_some();
        self.temp_seen |= s.gpu_temp.is_some();
        if s.gpu_queried {
            self.gpu_tries += 1;
        }
        self.cap_set = s.cap_set;
        let mut out = Vec::new();
        if let Some(done) = roll(&mut self.bucket, s) {
            if done.active() {
                let tz = self.tz_override.unwrap_or_else(tz_offset_min);
                out = self.run(&Event::ActiveMinute(done.to_minute(tz)));
            }
        }
        self.save_if_due(false);
        out
    }

    /// Alert, End task, update and cycle events.
    pub fn event(&mut self, ev: Event) -> Vec<Moment> {
        let out = self.run(&ev);
        self.save_if_due(false);
        out
    }

    /// True once per wall-clock minute: time to look at the billing cycle.
    pub fn cycle_due(&mut self, now_ms: u64) -> bool {
        let m = now_ms / 60_000;
        let due = m != self.last_cycle_minute;
        self.last_cycle_minute = m;
        due
    }

    /// Feed the current cycle start; returns the start of a cycle that just closed.
    pub fn cycle_seen(&mut self, start: u32, reset: u32) -> Option<u32> {
        let (stored, closed) = cycle_step(self.cycle, start, reset);
        if stored != self.cycle {
            self.cycle = stored;
            if self.persist {
                write_cycle(stored);
            }
        }
        closed
    }

    /// Save now (exit, WM_ENDSESSION, suspend), if anything changed.
    pub fn flush(&mut self) {
        self.save_if_due(true);
    }

    fn unavailable(&self) -> Vec<&'static str> {
        unavailable_ids(self.battery_seen, self.samples, self.gpu_tries, self.gpu_seen, self.temp_seen, self.cap_set)
    }

    fn run(&mut self, ev: &Event) -> Vec<Moment> {
        let un = self.unavailable();
        let (next, unlocks) = apply(&self.state, ev, &Ctx { unavailable: &un });
        if next != self.state {
            self.state = next;
            self.dirty = true;
        }
        if unlocks.is_empty() {
            return Vec::new();
        }
        let at = ev.now_ms();
        let moments: Vec<Moment> = unlocks.iter().map(|u| Moment::from_unlock(u, at)).collect();
        if self.persist {
            let _ = store::push_pending(&moments);
        }
        // Rare: write unlocks right away so a crash cannot repeat the moment.
        self.save_if_due(true);
        moments
    }

    fn save_if_due(&mut self, force: bool) {
        if !self.dirty || !self.persist || (!force && self.last_save.elapsed() < SAVE_EVERY) {
            return;
        }
        self.last_save = Instant::now();
        if let Ok(merged) = store::save(&self.state) {
            self.state = merged;
            self.dirty = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Windows glue
// ---------------------------------------------------------------------------

/// Unix milliseconds now.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Local UTC offset in minutes, DST included.
pub fn tz_offset_min() -> i32 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    // SAFETY: valid out pointer.
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut tz);
        if id == u32::MAX {
            return 0;
        }
        tz_offset_from(tz.Bias, tz.StandardBias, tz.DaylightBias, id)
    }
}

/// True when the user gave input within `INPUT_WINDOW_MS`.
pub fn input_recent() -> bool {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    // SAFETY: valid, sized out pointer.
    unsafe {
        let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        GetLastInputInfo(&mut li) != 0 && (GetTickCount().wrapping_sub(li.dwTime) as u64) < INPUT_WINDOW_MS
    }
}

fn cycle_path() -> std::path::PathBuf {
    crate::util::data_dir().join("achievements-cycle.txt")
}

fn read_cycle() -> Option<(u32, u32)> {
    let t = std::fs::read_to_string(cycle_path()).ok()?;
    let mut it = t.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

fn write_cycle(c: Option<(u32, u32)>) {
    if let Some((s, r)) = c {
        let _ = crate::util::write_atomic(&cycle_path(), format!("{} {}\n", s, r).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::day;

    const T0: u64 = 1_800_000_000_000 / 60_000 * 60_000; // a minute boundary

    fn sample(t: u64) -> SampleInput {
        SampleInput {
            now_ms: t,
            cpu: 10.0,
            net_down: 1000,
            net_up: 10,
            taskbars: 1,
            input_recent: true,
            battery: Some((80, true)),
            ..Default::default()
        }
    }

    #[test]
    fn bucket_accumulates_and_rolls() {
        let mut cur = None;
        assert!(roll(&mut cur, &sample(T0 + 1_000)).is_none());
        let mut s = sample(T0 + 31_000);
        s.cpu = 30.0;
        s.gpu = Some(50.0);
        s.gpu_temp = Some(70.0);
        s.fullscreen = true;
        s.taskbars = 2;
        s.ram_pct = 60.0;
        assert!(roll(&mut cur, &s).is_none());
        let done = roll(&mut cur, &sample(T0 + 61_000)).expect("rolled");
        assert!(done.active());
        let m = done.to_minute(0);
        assert_eq!(m.cpu, 20.0);
        assert_eq!(m.gpu, Some(50.0));
        assert_eq!(m.gpu_temp, Some(70.0));
        assert_eq!((m.down_bytes, m.up_bytes, m.net_bytes), (2000, 20, 2020));
        assert_eq!(m.widget_taskbars, 2);
        assert!(m.fullscreen);
        assert_eq!(m.ram_pct, 60.0);
        assert_eq!(m.now_ms, T0 + 31_000);
        assert_eq!(m.battery, Some((80, true)));
        assert_eq!(cur.as_ref().unwrap().minute, T0 / 60_000 + 1);
    }

    #[test]
    fn active_needs_widget_and_input() {
        let mut s = sample(T0);
        s.taskbars = 0;
        assert!(!Bucket::start(&s).active());
        let mut s = sample(T0);
        s.input_recent = false;
        assert!(!Bucket::start(&s).active());
        // Input seen in any sample of the minute is enough.
        let mut b = Bucket::start(&s);
        b.add(&sample(T0 + 5_000));
        assert!(b.active());
        // Full-screen counts as present (the app passes shown || fullscreen).
        assert!(Bucket::start(&sample(T0)).active());
    }

    #[test]
    fn clock_going_back_closes_the_bucket() {
        let mut cur = None;
        roll(&mut cur, &sample(T0 + 120_000));
        let old = roll(&mut cur, &sample(T0 + 1_000)).unwrap();
        assert_eq!(old.minute, T0 / 60_000 + 2);
        assert_eq!(cur.unwrap().minute, T0 / 60_000);
    }

    #[test]
    fn timezone_offsets() {
        // US Pacific: Bias 480, standard 0, daylight -60.
        assert_eq!(tz_offset_from(480, 0, -60, 1), -480);
        assert_eq!(tz_offset_from(480, 0, -60, 2), -420);
        // Central Europe: Bias -60.
        assert_eq!(tz_offset_from(-60, 0, -60, 2), 120);
        assert_eq!(tz_offset_from(-60, 0, -60, 0), 60);
    }

    #[test]
    fn cycles_close_only_in_order() {
        let (a, b) = (day(2026, 1, 15), day(2026, 2, 15));
        let (st, closed) = cycle_step(None, a, 15);
        assert_eq!((st, closed), (Some((a, 15)), None));
        assert_eq!(cycle_step(st, a, 15), (st, None));
        assert_eq!(cycle_step(st, b, 15), (Some((b, 15)), Some(a)));
        // Gap of a whole cycle: not the predecessor.
        assert_eq!(cycle_step(st, day(2026, 3, 15), 15), (Some((day(2026, 3, 15), 15)), None));
        // Reset day changed: no closure.
        assert_eq!(cycle_step(st, day(2026, 1, 20), 20), (Some((day(2026, 1, 20), 20)), None));
    }

    #[test]
    fn availability() {
        assert_eq!(unavailable_ids(false, 0, 0, false, false, true), Vec::<&str>::new());
        assert_eq!(unavailable_ids(false, 5, 0, false, false, true), vec!["fumes"]);
        assert_eq!(unavailable_ids(true, 5, 2, false, false, true), Vec::<&str>::new());
        assert_eq!(unavailable_ids(true, 5, 3, false, true, false), vec!["render", "under_cap"]);
        assert_eq!(unavailable_ids(true, 5, 9, true, true, true), Vec::<&str>::new());
    }

    #[test]
    fn minutes_reach_the_core() {
        let mut o = Observer::with_state(State::default(), false);
        o.tz_override = Some(0);
        for i in 0..4u64 {
            for k in 0..3u64 {
                o.on_sample(&sample(T0 + i * 60_000 + k * 20_000));
            }
        }
        // Minutes 0..2 were closed by the next one; the 4th is still open.
        assert_eq!(o.state().counter("min"), 3);
        assert!(o.dirty);
        o.pause();
        assert!(o.bucket.is_none());
        // No input: the minute does not count.
        let mut idle = sample(T0 + 10 * 60_000);
        idle.input_recent = false;
        o.on_sample(&idle);
        o.on_sample(&sample(T0 + 11 * 60_000));
        assert_eq!(o.state().counter("min"), 3);
    }

    #[test]
    fn cycle_due_once_per_minute() {
        let mut o = Observer::with_state(State::default(), false);
        assert!(o.cycle_due(T0));
        assert!(!o.cycle_due(T0 + 30_000));
        assert!(o.cycle_due(T0 + 60_000));
        assert_eq!(o.cycle_seen(20260115, 15), None);
        assert_eq!(o.cycle_seen(20260215, 15), Some(20260115));
    }
}
