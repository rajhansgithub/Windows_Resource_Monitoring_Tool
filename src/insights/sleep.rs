//! Sleep Verdict: what happened while the screen was off or the lid was shut.
//!
//! A "night" starts at the first of DisplayOff / LidClosed / Suspend and ends when the
//! display is really back on (DisplayOn, or LidOpen / Resume while the display is on).
//! Nights shorter than 10 minutes are dropped.
//!
//! Awake share for free: Windows freezes our process during real sleep (S3, and Modern
//! Standby once the Desktop Activity Moderator suspends desktop apps). Ticks keep coming
//! (every ~5 s) only while the PC is awake with the screen off. So normal tick gaps add up
//! to "awake" time and one big gap means "asleep". No timers, no polling of our own.
//!
//! Battery: `CallNtPowerInformation(SystemBatteryState)` (mWh), read only at the start,
//! at the end, and once a minute while the night is awake (for Bag Guard). Falls back to
//! the snapshot's battery percent when mWh is not reported.
//!
//! Culprit: while a night is awake with the display off, `wants_procs()` asks for the top
//! process list (the tick is 5 s then, so it is cheap) and CPU-seconds are summed per name.
//!
//! Wake reason: Event Log records (Power-Troubleshooter 1, Kernel-Power 506/507/42/107)
//! are delivered shortly after resume, sometimes after the night ended. They may attach to
//! the latest night for 2 minutes; the single toast per night goes out once a wake reason
//! arrived or the 2 minutes are over.
//!
//! Bag Guard (opt-in, `bag_guard`): lid closed, on battery, awake, and draining at 8 %/h or
//! more over the last 10 minutes -> hibernate once (on a short-lived thread, so the UI
//! thread still answers the suspend broadcast). Never falls back to plain sleep.

use super::events::SysEvent;
use super::store::{num, obj};
use super::{time, Chart, Ctx, Item, Notice, Panel, Power, Tone};
use crate::achievements::json::Json;
use crate::sensors::Battery;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Nights shorter than this are not recorded.
const MIN_NIGHT_MS: u64 = 10 * 60_000;
/// Nights shorter than this never toast.
const NOTICE_MIN_MS: u64 = 20 * 60_000;
/// Late wake events may attach to the latest night for this long after it ended.
const ATTACH_MS: u64 = 2 * 60_000;
/// Battery read cadence while a night is awake.
const READ_EVERY_MS: u64 = 60_000;
const MAX_NIGHTS: usize = 14;
/// Battery reads kept for the Bag Guard window (one per minute).
const MAX_READS: usize = 12;
/// Distinct process names tracked per night.
const MAX_PROCS: usize = 24;
/// Less CPU than this while awake is not worth naming.
const MIN_CULPRIT_CPU_S: f32 = 10.0;
/// Drain per hour thresholds for the verdict.
const GOOD_RATE: f32 = 1.5;
const WARN_RATE: f32 = 4.0;
/// Bag Guard fires at this drain (%/h) measured over `GUARD_WINDOW_MS`.
const GUARD_RATE: f32 = 8.0;
const GUARD_WINDOW_MS: u64 = 10 * 60_000;
/// The measured window must span at least this much.
const GUARD_MIN_SPAN_MS: u64 = 9 * 60_000;
/// Two wake records closer than this describe the same wake.
const SAME_WAKE_MS: u64 = 60_000;

/// One battery read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BatRead {
    pub present: bool,
    pub on_ac: bool,
    /// Remaining energy, mWh.
    pub mwh: Option<u32>,
    /// Full charge capacity, mWh.
    pub max_mwh: Option<u32>,
    /// 0..100.
    pub pct: Option<f32>,
}

/// Best wake reason seen so far for a night.
#[derive(Clone, Debug, PartialEq)]
pub struct Wake {
    pub t_ms: u64,
    /// 2 = Power-Troubleshooter (names the device), 1 = Kernel-Power 507 (reason code).
    pub rank: u8,
    pub reason: String,
}

/// One recorded night (persisted).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Night {
    pub start_ms: u64,
    pub end_ms: u64,
    pub awake_s: u32,
    /// A battery was present.
    pub battery: bool,
    /// On AC at any point of the night.
    pub on_ac: bool,
    pub mwh0: Option<u32>,
    pub mwh1: Option<u32>,
    pub mwh_max: Option<u32>,
    pub pct0: Option<f32>,
    pub pct1: Option<f32>,
    /// (process name, CPU-seconds while awake).
    pub culprit: Option<(String, f32)>,
    pub wake: Option<Wake>,
    /// Sleep confirmed (a long frozen gap or a sleep / standby event).
    pub slept: bool,
    /// Bag Guard hibernated at this time.
    pub guard_ms: Option<u64>,
}

impl Night {
    pub fn total_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// 0..100.
    pub fn awake_pct(&self) -> f32 {
        let t = self.total_ms();
        if t == 0 {
            return 0.0;
        }
        (self.awake_s as f32 * 1000.0 / t as f32 * 100.0).clamp(0.0, 100.0)
    }

    /// Battery percent lost; None on AC, without a battery or without both reads.
    pub fn lost_pct(&self) -> Option<f32> {
        if !self.battery || self.on_ac {
            return None;
        }
        let lost = match (self.mwh0, self.mwh1, self.mwh_max) {
            (Some(a), Some(b), Some(m)) if m > 0 => (a as f32 - b as f32) / m as f32 * 100.0,
            _ => self.pct0? - self.pct1?,
        };
        Some(lost.max(0.0))
    }

    /// Percent per hour of the night.
    pub fn drain_per_h(&self) -> Option<f32> {
        let h = self.total_ms() as f32 / 3_600_000.0;
        if h <= 0.0 {
            return None;
        }
        Some(self.lost_pct()? / h)
    }
}

/// The night in progress (not persisted).
struct Session {
    start_ms: u64,
    last_tick_ms: u64,
    ticked: bool,
    interval_ms: u64,
    awake_ms: u64,
    slept: bool,
    on_ac: bool,
    bat0: BatRead,
    max_mwh: Option<u32>,
    last_read_ms: u64,
    /// (unix ms, mWh) once a minute while awake on battery, oldest first.
    reads: Vec<(u64, u32)>,
    /// CPU-seconds per process name while awake with the display off.
    cpu: Vec<(String, f32)>,
    wake: Option<Wake>,
    guard_tried: bool,
    guard_ms: Option<u64>,
}

impl Session {
    fn new(start_ms: u64, bat0: BatRead, on_ac: bool) -> Session {
        let on_ac = on_ac || bat0.on_ac;
        let mut reads = Vec::new();
        if let (Some(m), false) = (bat0.mwh, on_ac) {
            reads.push((start_ms, m));
        }
        Session {
            start_ms,
            last_tick_ms: start_ms,
            ticked: false,
            interval_ms: 5000,
            awake_ms: 0,
            slept: false,
            on_ac,
            bat0,
            max_mwh: bat0.max_mwh,
            last_read_ms: start_ms,
            reads,
            cpu: Vec::new(),
            wake: None,
            guard_tried: false,
            guard_ms: None,
        }
    }

    /// Account one gap between ticks. Returns the awake ms it added.
    fn add_gap(&mut self, gap_ms: u64) -> u64 {
        let (awake, asleep) = split_gap(gap_ms, self.interval_ms);
        self.awake_ms += awake;
        if asleep {
            self.slept = true;
        }
        awake
    }

    fn add_cpu(&mut self, name: &str, cpu_s: f32) {
        if cpu_s <= 0.0 || ignored_proc(name) {
            return;
        }
        if let Some(e) = self.cpu.iter_mut().find(|(n, _)| n == name) {
            e.1 += cpu_s;
        } else if self.cpu.len() < MAX_PROCS {
            self.cpu.push((name.to_string(), cpu_s));
        }
    }

    fn culprit(&self) -> Option<(String, f32)> {
        let top = self.cpu.iter().max_by(|a, b| a.1.total_cmp(&b.1))?;
        (top.1 >= MIN_CULPRIT_CPU_S).then(|| (top.0.clone(), (top.1 * 10.0).round() / 10.0))
    }

    fn push_read(&mut self, t_ms: u64, mwh: u32) {
        if self.reads.len() >= MAX_READS {
            self.reads.remove(0);
        }
        self.reads.push((t_ms, mwh));
    }
}

/// Split a tick gap into awake ms and whether the PC slept in it. A gap up to 3 intervals
/// is awake; a longer one counts one interval awake and the rest asleep.
pub fn split_gap(gap_ms: u64, interval_ms: u64) -> (u64, bool) {
    let iv = interval_ms.max(1000);
    if gap_ms <= 3 * iv {
        (gap_ms, false)
    } else {
        (iv, true)
    }
}

/// Our own process and the idle pseudo-processes never count as culprits.
fn ignored_proc(name: &str) -> bool {
    ["ResourceMonitor", "Idle", "System Idle Process", "_Total"].iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Bag Guard decision. `reads` are (unix ms, mWh), oldest first, taken once a minute while
/// awake on battery. Fires when the drain over the last 10 minutes is at least 8 %/h.
pub fn bag_guard_due(
    enabled: bool,
    lid_closed: bool,
    on_ac: bool,
    tried: bool,
    max_mwh: Option<u32>,
    reads: &[(u64, u32)],
) -> bool {
    if !enabled || !lid_closed || on_ac || tried {
        return false;
    }
    let max = match max_mwh {
        Some(m) if m > 0 => m as f32,
        _ => return false,
    };
    let Some(&(t1, m1)) = reads.last() else { return false };
    let Some(&(t0, m0)) = reads.iter().find(|(t, _)| t1.saturating_sub(*t) <= GUARD_WINDOW_MS + 60_000) else {
        return false;
    };
    let span = t1.saturating_sub(t0);
    if span < GUARD_MIN_SPAN_MS {
        return false;
    }
    let lost_pct = (m0 as f32 - m1 as f32) / max * 100.0;
    lost_pct / (span as f32 / 3_600_000.0) >= GUARD_RATE
}

/// "7h 42m", "45m", "3h".
fn dur(ms: u64) -> String {
    let m = (ms + 30_000) / 60_000;
    match (m / 60, m % 60) {
        (0, m) => format!("{}m", m),
        (h, 0) => format!("{}h", h),
        (h, m) => format!("{}h {}m", h, m),
    }
}

/// Like `dur` but whole hours from 2 h up ("5h").
fn dur_short(ms: u64) -> String {
    if ms >= 2 * 3_600_000 {
        format!("{}h", (ms + 1_800_000) / 3_600_000)
    } else {
        dur(ms)
    }
}

/// Tone and one-line verdict for a night.
pub fn verdict(n: &Night) -> (Tone, String) {
    let total = n.total_ms();
    let awake = n.awake_pct().round() as u32;
    let culprit = n.culprit.as_ref().map(|(c, _)| c.as_str());
    match n.drain_per_h() {
        None => {
            let place = if n.on_ac { " on power" } else { "" };
            if awake > 50 {
                let mut t = format!("Stayed awake {}% of {}{}.", awake, dur(total), place);
                if let Some(c) = culprit {
                    t += &format!(" {} was busy.", c);
                }
                (Tone::Warn, t)
            } else {
                (Tone::Info, format!("Slept {}{}. Awake {}% of the time.", dur(total), place, awake))
            }
        }
        Some(rate) => {
            let lost = n.lost_pct().unwrap_or(0.0).round() as u32;
            if rate <= GOOD_RATE {
                return (Tone::Good, format!("Slept {}. Lost {}%. Good.", dur(total), lost));
            }
            let tone = if rate <= WARN_RATE { Tone::Warn } else { Tone::Bad };
            let mut t = format!("Lost {}% in {}.", lost, dur_short(total));
            if awake > 30 {
                match culprit {
                    Some(c) => t += &format!(" Awake {}% of the time. {} was busy.", awake, c),
                    None => t += &format!(" It stayed awake {}% of the time.", awake),
                }
            } else {
                t += " It was asleep most of the time.";
            }
            (tone, t)
        }
    }
}

/// The tray notice for a night, if it deserves one.
pub fn notice_for(n: &Night) -> Option<Notice> {
    if n.total_ms() < NOTICE_MIN_MS {
        return None;
    }
    if n.drain_per_h().is_none() && n.awake_pct() <= 50.0 {
        return None;
    }
    let (tone, text) = verdict(n);
    let title = if tone == Tone::Bad { "Battery drained while asleep" } else { "Sleep verdict" };
    Some(Notice { title: title.into(), text })
}

/// Field lookup by several candidate names, ignoring case; empty values count as missing.
fn field<'a>(e: &'a SysEvent, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|k| {
        e.data.iter().find(|(n, v)| n.eq_ignore_ascii_case(k) && !v.trim().is_empty()).map(|(_, v)| v.trim())
    })
}

fn parse_code(v: &str) -> Option<u32> {
    let v = v.trim();
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
}

/// Modern Standby exit reason codes (POWER_MONITOR_REQUEST_REASON) we can name plainly.
fn reason_from_code(c: u32) -> Option<&'static str> {
    Some(match c {
        1 => "Power button",
        2 => "remote connection",
        4 => "user input",
        5 => "charger",
        10 => "unlock",
        14 => "Sleep button",
        15 => "lid",
        31 => "keyboard",
        32 => "mouse",
        33 => "touchpad",
        34 => "pen",
        36 => "input device",
        _ => return None,
    })
}

/// Turn a wake source text ("Device -Intel(R) Ethernet Connection") into plain words.
pub fn clean_reason(text: &str) -> Option<String> {
    let t = text.trim();
    let t = t.strip_prefix("Device -").or_else(|| t.strip_prefix("Device-")).unwrap_or(t).trim();
    let l = t.to_ascii_lowercase();
    if l.is_empty() || l.contains("unknown") {
        return None;
    }
    let has = |ks: &[&str]| ks.iter().any(|k| l.contains(k));
    let s = if has(&["power button"]) {
        "Power button"
    } else if has(&["sleep button"]) {
        "Sleep button"
    } else if has(&["ethernet", "wi-fi", "wifi", "wireless", "wlan", "network", "gbe family"]) {
        "network adapter"
    } else if has(&["keyboard"]) {
        "keyboard"
    } else if has(&["mouse"]) {
        "mouse"
    } else if has(&["touchpad", "touch pad"]) {
        "touchpad"
    } else if has(&["timer"]) {
        "a wake timer"
    } else if has(&["lid"]) {
        "lid"
    } else if has(&["usb"]) {
        "USB device"
    } else {
        let end = t.char_indices().nth(40).map_or(t.len(), |(i, _)| i);
        return Some(t[..end].trim_end().to_string());
    };
    Some(s.to_string())
}

/// What a power-related Event Log record tells us: (sleep confirmed, wake reason).
/// None = not a record we care about.
pub fn wake_info(e: &SysEvent) -> Option<(bool, Option<Wake>)> {
    let p = e.provider.to_ascii_lowercase();
    if p.ends_with("power-troubleshooter") && e.id == 1 {
        let reason = field(e, &["WakeSourceText", "WakeSource", "WakeSourceName", "Source"]).and_then(clean_reason);
        return Some((true, reason.map(|reason| Wake { t_ms: e.t_ms, rank: 2, reason })));
    }
    if !p.ends_with("kernel-power") {
        return None;
    }
    match e.id {
        507 => {
            let by_code = field(e, &["Reason", "ExitReason", "ReasonCode"])
                .and_then(parse_code)
                .and_then(reason_from_code)
                .map(str::to_string);
            let reason = by_code.or_else(|| {
                field(e, &["ReasonText", "ExitReasonText", "ReasonString", "Description"]).and_then(clean_reason)
            });
            Some((true, reason.map(|reason| Wake { t_ms: e.t_ms, rank: 1, reason })))
        }
        42 | 107 | 506 => Some((true, None)),
        _ => None,
    }
}

/// Keep the best wake: a later wake wins; for the same wake the richer source wins.
fn merge_wake(cur: &mut Option<Wake>, new: Wake) {
    let replace = match cur {
        None => true,
        Some(c) if new.t_ms > c.t_ms + SAME_WAKE_MS => true,
        Some(c) => new.t_ms + SAME_WAKE_MS >= c.t_ms && new.rank >= c.rank,
    };
    if replace {
        *cur = Some(new);
    }
}

/// Read the battery (mWh when the firmware reports it), else the snapshot's percent.
fn read_battery(fallback: Option<Battery>) -> BatRead {
    use windows_sys::Win32::System::Power::{CallNtPowerInformation, SystemBatteryState, SYSTEM_BATTERY_STATE};
    let fb = BatRead {
        present: fallback.is_some(),
        on_ac: fallback.is_some_and(|b| b.on_ac),
        mwh: None,
        max_mwh: None,
        pct: fallback.map(|b| b.percent as f32),
    };
    // SAFETY: plain out struct of the documented size; no input buffer.
    let st = unsafe {
        let mut st: SYSTEM_BATTERY_STATE = std::mem::zeroed();
        let r = CallNtPowerInformation(
            SystemBatteryState,
            std::ptr::null(),
            0,
            &mut st as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<SYSTEM_BATTERY_STATE>() as u32,
        );
        if r != 0 {
            return fb;
        }
        st
    };
    if st.BatteryPresent == 0 {
        return BatRead { present: false, on_ac: st.AcOnLine != 0, ..Default::default() };
    }
    let known = |v: u32| v != 0 && v != u32::MAX;
    let (mwh, max_mwh) = if known(st.MaxCapacity) && st.RemainingCapacity != u32::MAX
        && st.RemainingCapacity <= st.MaxCapacity.saturating_add(st.MaxCapacity / 10)
    {
        (Some(st.RemainingCapacity), Some(st.MaxCapacity))
    } else {
        (None, None)
    };
    let pct = match (mwh, max_mwh) {
        (Some(r), Some(m)) => Some((r as f32 / m as f32 * 100.0).min(100.0)),
        _ => fb.pct,
    };
    BatRead { present: true, on_ac: st.AcOnLine != 0, mwh, max_mwh, pct }
}

/// Start hibernation on a short-lived thread. The receiver gets true when it worked.
fn hibernate_async() -> Option<Receiver<bool>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("bag-guard".into())
        .spawn(move || {
            let _ = tx.send(hibernate_blocking());
        })
        .ok()?;
    Some(rx)
}

/// Enable SeShutdownPrivilege (standard users hold it on workstations) and hibernate.
/// Blocks until resume on success. Never falls back to plain sleep.
fn hibernate_blocking() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Power::{IsPwrHibernateAllowed, SetSuspendState};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: no arguments.
    if unsafe { IsPwrHibernateAllowed() } == 0 {
        crate::util::diag("Bag Guard: hibernate is not available on this PC.");
        return false;
    }
    // SAFETY: token handle is ours and closed below; the privilege struct is fully initialised.
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut tok) != 0 {
            let mut luid = LUID { LowPart: 0, HighPart: 0 };
            if LookupPrivilegeValueW(std::ptr::null(), SE_SHUTDOWN_NAME, &mut luid) != 0 {
                let tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
                };
                AdjustTokenPrivileges(tok, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
            }
            CloseHandle(tok);
        }
    }
    // SAFETY: plain flags: hibernate, no force, wake events allowed.
    if unsafe { SetSuspendState(1, 0, 0) } == 0 {
        // SAFETY: no arguments.
        let err = unsafe { GetLastError() };
        crate::util::diag(&format!("Bag Guard: hibernate failed (error {}).", err));
        return false;
    }
    true
}

pub struct Sleep {
    /// Bag Guard (Settings): hibernate when the bag gets hot. Off by default.
    pub bag_guard: bool,
    /// Oldest first, at most `MAX_NIGHTS`.
    nights: Vec<Night>,
    cur: Option<Session>,
    disp_on: bool,
    lid_closed: bool,
    ac: Option<bool>,
    last_bat: Option<Battery>,
    last_now: u64,
    /// The latest night still owes its single toast.
    pending: bool,
    /// A wake reason attached to the latest night after it ended.
    late_wake: bool,
    attach_until: u64,
    guard_rx: Option<Receiver<bool>>,
    cores: f32,
    dirty: bool,
    reader: fn(Option<Battery>) -> BatRead,
    hibernate: fn() -> Option<Receiver<bool>>,
}

impl Sleep {
    pub fn new() -> Sleep {
        Sleep {
            bag_guard: false,
            nights: Vec::new(),
            cur: None,
            disp_on: true,
            lid_closed: false,
            ac: None,
            last_bat: None,
            last_now: 0,
            pending: false,
            late_wake: false,
            attach_until: 0,
            guard_rx: None,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()) as f32,
            dirty: false,
            reader: read_battery,
            hibernate: hibernate_async,
        }
    }

    pub fn on_sample(&mut self, c: &Ctx) -> Vec<Notice> {
        let now = c.now_ms;
        self.last_now = now;
        self.last_bat = c.snap.battery;
        if !c.display_on {
            self.disp_on = false;
        }
        self.poll_guard();
        let mut out = Vec::new();
        if self.cur.is_some() {
            self.session_tick(c);
        }
        if self.pending && (self.late_wake || now >= self.attach_until) {
            self.pending = false;
            if let Some(n) = self.nights.last().and_then(notice_for) {
                out.push(n);
            }
        }
        out
    }

    /// Per-tick work for the night in progress: awake time, culprit, battery, Bag Guard.
    fn session_tick(&mut self, c: &Ctx) {
        let now = c.now_ms;
        let reader = self.reader;
        let disp_off = !self.disp_on && !c.display_on;
        let (lid_closed, bag_guard, cores) = (self.lid_closed, self.bag_guard, self.cores);
        let Some(start) = self.cur.as_ref().map(|s| s.start_ms) else { return };
        // Real return without a DisplayOn message (e.g. after S3 on some PCs): screen on
        // and keyboard / mouse input after the night began.
        let input_at = now.saturating_sub(c.input_idle_ms);
        if c.display_on && c.input_idle_ms > 0 && input_at > start + 2000 {
            self.end_session(input_at);
            return;
        }
        let Some(s) = self.cur.as_mut() else { return };
        let wall = now.saturating_sub(s.last_tick_ms);
        let gap = if s.ticked { wall.max((c.snap.elapsed * 1000.0) as u64) } else { wall };
        s.interval_ms = (c.interval_ms as u64).max(1000);
        s.ticked = true;
        s.last_tick_ms = now;
        let awake = s.add_gap(gap);
        if let Some(b) = c.snap.battery {
            s.on_ac |= b.on_ac;
        }
        if awake == gap && disp_off && c.procs_sampled {
            let secs = awake as f32 / 1000.0;
            for p in &c.snap.top_cpu {
                s.add_cpu(&p.name, p.cpu / 100.0 * cores * secs);
            }
        }
        if s.bat0.present && now.saturating_sub(s.last_read_ms) >= READ_EVERY_MS {
            let r = reader(c.snap.battery);
            s.last_read_ms = now;
            s.on_ac |= r.on_ac;
            if s.max_mwh.is_none() {
                s.max_mwh = r.max_mwh;
            }
            match (s.on_ac, r.mwh) {
                (true, _) => s.reads.clear(),
                (false, Some(m)) => s.push_read(now, m),
                _ => {}
            }
        }
        if bag_guard_due(bag_guard, lid_closed, s.on_ac, s.guard_tried, s.max_mwh, &s.reads) {
            s.guard_tried = true;
            crate::util::diag("Bag Guard: fast drain with the lid closed. Hibernating.");
            match (self.hibernate)() {
                Some(rx) => {
                    s.guard_ms = Some(now);
                    self.guard_rx = Some(rx);
                }
                None => crate::util::diag("Bag Guard: could not start the hibernate thread."),
            }
        }
    }

    /// A hibernate attempt reported back: drop the record if it failed.
    fn poll_guard(&mut self) {
        let Some(rx) = &self.guard_rx else { return };
        match rx.try_recv() {
            Ok(true) | Err(TryRecvError::Disconnected) => self.guard_rx = None,
            Ok(false) => {
                self.guard_rx = None;
                if let Some(s) = self.cur.as_mut() {
                    s.guard_ms = None;
                } else if let Some(n) = self.nights.last_mut() {
                    if n.guard_ms.take().is_some() {
                        self.dirty = true;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    pub fn on_event(&mut self, e: &SysEvent) {
        let Some((slept, wake)) = wake_info(e) else { return };
        if let Some(s) = self.cur.as_mut() {
            if e.t_ms + SAME_WAKE_MS >= s.start_ms {
                s.slept |= slept;
                if let Some(w) = wake {
                    merge_wake(&mut s.wake, w);
                }
                return;
            }
        }
        if self.last_now > self.attach_until {
            return;
        }
        let Some(n) = self.nights.last_mut() else { return };
        if e.t_ms + SAME_WAKE_MS < n.start_ms || e.t_ms > n.end_ms + ATTACH_MS {
            return;
        }
        let before = (n.slept, n.wake.clone());
        n.slept |= slept;
        if let Some(w) = wake {
            merge_wake(&mut n.wake, w);
            self.late_wake = true;
        }
        if (n.slept, n.wake.clone()) != before {
            self.dirty = true;
        }
    }

    pub fn on_power(&mut self, p: Power, now_ms: u64) -> Vec<Notice> {
        self.last_now = now_ms;
        match p {
            Power::DisplayOff => {
                self.disp_on = false;
                self.begin(now_ms);
            }
            Power::LidClosed => {
                self.lid_closed = true;
                self.begin(now_ms);
            }
            Power::Suspend => {
                // The screen goes dark with the system; wait for a real DisplayOn.
                self.disp_on = false;
                self.begin(now_ms);
            }
            Power::DisplayOn => {
                self.disp_on = true;
                self.end_session(now_ms);
            }
            Power::LidOpen | Power::Resume => {
                if p == Power::LidOpen {
                    self.lid_closed = false;
                }
                if self.disp_on {
                    self.end_session(now_ms);
                }
            }
            Power::Ac(on) => {
                self.ac = Some(on);
                if let (true, Some(s)) = (on, self.cur.as_mut()) {
                    s.on_ac = true;
                    s.reads.clear();
                }
            }
            Power::DisplayDim | Power::Mode(_) => {}
        }
        Vec::new()
    }

    fn begin(&mut self, now: u64) {
        if self.cur.is_some() {
            return;
        }
        let b = (self.reader)(self.last_bat);
        self.cur = Some(Session::new(now, b, self.ac == Some(true)));
    }

    /// Close the night in progress, store it if long enough, and queue its toast.
    fn end_session(&mut self, now: u64) {
        let Some(mut s) = self.cur.take() else { return };
        let tail = now.saturating_sub(s.last_tick_ms);
        s.add_gap(tail);
        let total = now.saturating_sub(s.start_ms);
        if total < MIN_NIGHT_MS {
            return;
        }
        let b1 = if s.bat0.present { (self.reader)(self.last_bat) } else { BatRead::default() };
        let both = |a: Option<u32>, b: Option<u32>| a.is_some() && b.is_some();
        let mwh_ok = both(s.bat0.mwh, b1.mwh);
        let pct_ok = s.bat0.pct.is_some() && b1.pct.is_some();
        let n = Night {
            start_ms: s.start_ms,
            end_ms: now,
            awake_s: (s.awake_ms.min(total) / 1000) as u32,
            battery: s.bat0.present || b1.present,
            on_ac: s.on_ac || b1.on_ac,
            mwh0: if mwh_ok { s.bat0.mwh } else { None },
            mwh1: if mwh_ok { b1.mwh } else { None },
            mwh_max: if mwh_ok { b1.max_mwh.or(s.max_mwh) } else { None },
            pct0: if pct_ok { s.bat0.pct } else { None },
            pct1: if pct_ok { b1.pct } else { None },
            culprit: s.culprit(),
            wake: s.wake.take(),
            slept: s.slept,
            guard_ms: s.guard_ms,
        };
        self.pending = notice_for(&n).is_some();
        self.late_wake = false;
        self.attach_until = now + ATTACH_MS;
        self.nights.push(n);
        if self.nights.len() > MAX_NIGHTS {
            self.nights.remove(0);
        }
        self.dirty = true;
    }

    pub fn wants_procs(&self) -> bool {
        self.cur.is_some() && !self.disp_on
    }

    /// Persistent state changed since the last save.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn save(&self) -> Json {
        let nights = self.nights.iter().map(night_json).collect();
        obj(vec![("nights", Json::Arr(nights))])
    }

    pub fn load(&mut self, j: &Json) {
        let Some(arr) = j.get("nights").and_then(Json::as_arr) else { return };
        let mut v: Vec<Night> = arr.iter().filter_map(night_from).collect();
        v.sort_by_key(|n| n.start_ms);
        let skip = v.len().saturating_sub(MAX_NIGHTS);
        self.nights = v.split_off(skip);
    }

    pub fn panel(&self, _now_ms: u64) -> Panel {
        let guard = if self.bag_guard { "Bag Guard is on." } else { "Bag Guard is off." };
        let Some(n) = self.nights.last() else {
            return Panel {
                key: "sleep",
                title: "Sleep Verdict",
                tone: Tone::Learning,
                headline: "No sleep recorded yet. Close the lid or let the screen turn off.".into(),
                lines: vec![guard.into()],
                chart: None,
                items: vec![],
            };
        };
        let (tone, headline) = verdict(n);
        let mut lines = Vec::new();
        match n.drain_per_h() {
            Some(r) => lines.push(format!("Drain {:.1}%/h.", r)),
            None if n.on_ac => lines.push("On power. No drain to show.".into()),
            None if !n.battery => lines.push("No battery. Awake time only.".into()),
            None => lines.push("Battery level not reported.".into()),
        }
        lines.push(format!("Awake {}% of the time.", n.awake_pct().round() as u32));
        if let Some(w) = &n.wake {
            lines.push(format!("Woke by: {}.", w.reason));
        }
        if let Some((name, s)) = &n.culprit {
            // "40 s" / "4 min".
            let cpu = if *s < 90.0 {
                format!("{} s", s.round() as u32)
            } else {
                format!("{} min", (s / 60.0).round() as u32)
            };
            lines.push(format!("Busiest while awake: {}, {} of CPU.", name, cpu));
        }
        if let Some(t) = n.guard_ms {
            lines.push(format!("Bag Guard hibernated at {}.", time::hhmm(t)));
        }
        lines.push(guard.into());
        let points: Vec<f32> = self.nights.iter().filter_map(Night::drain_per_h).collect();
        let chart = (!points.is_empty()).then(|| Chart {
            label: "Drain per hour, last 14 nights".into(),
            points,
            max: 0.0,
            unit: "%/h",
            markers: vec![],
        });
        let item = |n: &Night| {
            let (tone, title) = verdict(n);
            let mut detail = format!("{} to {}", time::hhmm(n.start_ms), time::hhmm(n.end_ms));
            if let Some(w) = &n.wake {
                detail += &format!(" \u{b7} woke by {}", w.reason);
            }
            if let Some(t) = n.guard_ms {
                detail += &format!(" \u{b7} Bag Guard hibernated at {}", time::hhmm(t));
            }
            Item { t_ms: n.start_ms, tone, title, detail }
        };
        let items = self.nights.iter().rev().map(item).collect();
        Panel { key: "sleep", title: "Sleep Verdict", tone, headline, lines, chart, items }
    }
}

fn night_json(n: &Night) -> Json {
    let mut v = vec![
        ("s", Json::Num(n.start_ms as f64)),
        ("e", Json::Num(n.end_ms as f64)),
        ("aw", Json::Num(n.awake_s as f64)),
        ("bat", Json::Bool(n.battery)),
        ("ac", Json::Bool(n.on_ac)),
        ("sl", Json::Bool(n.slept)),
    ];
    let opt = |v: &mut Vec<(&str, Json)>, k, x: Option<f64>| {
        if let Some(x) = x {
            v.push((k, Json::Num(x)));
        }
    };
    opt(&mut v, "m0", n.mwh0.map(f64::from));
    opt(&mut v, "m1", n.mwh1.map(f64::from));
    opt(&mut v, "mx", n.mwh_max.map(f64::from));
    opt(&mut v, "p0", n.pct0.map(|p| (p as f64 * 10.0).round() / 10.0));
    opt(&mut v, "p1", n.pct1.map(|p| (p as f64 * 10.0).round() / 10.0));
    opt(&mut v, "bg", n.guard_ms.map(|t| t as f64));
    if let Some((name, s)) = &n.culprit {
        v.push(("cn", Json::Str(name.clone())));
        v.push(("cs", Json::Num(*s as f64)));
    }
    if let Some(w) = &n.wake {
        v.push(("wk", Json::Str(w.reason.clone())));
        v.push(("wt", Json::Num(w.t_ms as f64)));
        v.push(("wr", Json::Num(w.rank as f64)));
    }
    obj(v)
}

fn night_from(j: &Json) -> Option<Night> {
    let u32f = |k| num(j, k).filter(|x| *x >= 0.0 && *x <= u32::MAX as f64).map(|x| x as u32);
    let pct = |k| num(j, k).filter(|x| (0.0..=100.0).contains(x)).map(|x| x as f32);
    let flag = |k| matches!(j.get(k), Some(Json::Bool(true)));
    let start_ms = num(j, "s").filter(|x| *x > 0.0)? as u64;
    let end_ms = num(j, "e").filter(|x| *x > 0.0)? as u64;
    if end_ms <= start_ms {
        return None;
    }
    let culprit = match (j.get("cn").and_then(Json::as_str), num(j, "cs")) {
        (Some(n), Some(s)) if !n.is_empty() && s >= 0.0 => Some((n.chars().take(64).collect(), s as f32)),
        _ => None,
    };
    let wake = j.get("wk").and_then(Json::as_str).filter(|s| !s.is_empty()).map(|r| Wake {
        t_ms: num(j, "wt").unwrap_or(0.0).max(0.0) as u64,
        rank: num(j, "wr").unwrap_or(0.0).clamp(0.0, 2.0) as u8,
        reason: r.chars().take(64).collect(),
    });
    let total_s = ((end_ms - start_ms) / 1000) as u32;
    Some(Night {
        start_ms,
        end_ms,
        awake_s: u32f("aw").unwrap_or(0).min(total_s),
        battery: flag("bat"),
        on_ac: flag("ac"),
        mwh0: u32f("m0"),
        mwh1: u32f("m1"),
        mwh_max: u32f("mx"),
        pct0: pct("p0"),
        pct1: pct("p1"),
        culprit,
        wake,
        slept: flag("sl"),
        guard_ms: num(j, "bg").filter(|x| *x > 0.0).map(|x| x as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::{ProcStat, Snapshot};
    use std::cell::Cell;

    const T0: u64 = 1_800_000_000_000;
    const MIN: u64 = 60_000;
    const H: u64 = 3_600_000;

    thread_local! {
        /// (present, on_ac, mWh, max mWh) the fake battery reports.
        static BAT: Cell<(bool, bool, u32, u32)> = const { Cell::new((true, false, 40_000, 50_000)) };
        static HIB: Cell<u32> = const { Cell::new(0) };
    }

    fn fake_bat(_: Option<Battery>) -> BatRead {
        let (present, on_ac, m, mx) = BAT.with(|b| b.get());
        if !present {
            return BatRead::default();
        }
        BatRead { present, on_ac, mwh: Some(m), max_mwh: Some(mx), pct: Some(m as f32 / mx as f32 * 100.0) }
    }

    fn fake_hib() -> Option<Receiver<bool>> {
        HIB.with(|h| h.set(h.get() + 1));
        let (tx, rx) = mpsc::channel();
        tx.send(true).ok()?;
        Some(rx)
    }

    fn set_bat(present: bool, on_ac: bool, mwh: u32) {
        BAT.with(|b| b.set((present, on_ac, mwh, 50_000)));
    }

    fn sleep() -> Sleep {
        let mut s = Sleep::new();
        s.reader = fake_bat;
        s.hibernate = fake_hib;
        s.cores = 1.0;
        set_bat(true, false, 40_000);
        HIB.with(|h| h.set(0));
        s
    }

    fn tick_with(s: &mut Sleep, now: u64, elapsed_s: f64, procs: &[(&str, f32)]) -> Vec<Notice> {
        let snap = Snapshot {
            elapsed: elapsed_s,
            top_cpu: procs
                .iter()
                .map(|(n, c)| ProcStat { name: n.to_string(), cpu: *c, ..Default::default() })
                .collect(),
            ..Default::default()
        };
        let c = Ctx {
            now_ms: now,
            snap: &snap,
            display_on: s.disp_on,
            input_idle_ms: 10 * H,
            interval_ms: 5000,
            procs_sampled: !procs.is_empty(),
        };
        s.on_sample(&c)
    }

    fn tick(s: &mut Sleep, now: u64) -> Vec<Notice> {
        tick_with(s, now, 5.0, &[])
    }

    /// 5 s ticks from `from` (exclusive) to `to` (inclusive).
    fn ticks(s: &mut Sleep, from: u64, to: u64) -> Vec<Notice> {
        let mut out = Vec::new();
        let mut t = from + 5000;
        while t <= to {
            out.extend(tick(s, t));
            t += 5000;
        }
        out
    }

    fn night(total_ms: u64, pct0: f32, pct1: f32, awake_pct: f32) -> Night {
        Night {
            start_ms: T0,
            end_ms: T0 + total_ms,
            awake_s: (total_ms as f32 / 1000.0 * awake_pct / 100.0) as u32,
            battery: true,
            pct0: Some(pct0),
            pct1: Some(pct1),
            ..Default::default()
        }
    }

    #[test]
    fn gap_split() {
        assert_eq!(split_gap(5000, 5000), (5000, false));
        assert_eq!(split_gap(15_000, 5000), (15_000, false));
        assert_eq!(split_gap(3 * H, 5000), (5000, true));
    }

    #[test]
    fn awake_and_asleep_from_ticks() {
        let mut s = sleep();
        s.on_power(Power::DisplayOff, T0);
        ticks(&mut s, T0, T0 + 30 * MIN);
        // Frozen for 3 h: one late tick.
        let t1 = T0 + 30 * MIN + 3 * H;
        tick_with(&mut s, t1, 3.0 * 3600.0, &[]);
        ticks(&mut s, t1, t1 + 10 * MIN);
        s.on_power(Power::DisplayOn, t1 + 10 * MIN);
        let n = s.nights.last().expect("night stored");
        assert_eq!(n.total_ms(), 3 * H + 40 * MIN);
        // 30 min + 10 min awake, plus one interval for the frozen gap.
        assert_eq!(n.awake_s, 40 * 60 + 5);
        assert!(n.slept);
        let pct = n.awake_pct();
        assert!(pct > 17.0 && pct < 19.0, "{pct}");
    }

    #[test]
    fn s3_without_ticks_is_asleep() {
        let mut s = sleep();
        s.on_power(Power::Suspend, T0);
        s.on_power(Power::Resume, T0 + 3 * H);
        assert!(s.cur.is_some(), "resume alone with the display off does not end the night");
        s.on_power(Power::DisplayOn, T0 + 3 * H + 2000);
        let n = s.nights.last().unwrap();
        assert!(n.slept);
        assert!(n.awake_s <= 5);
    }

    #[test]
    fn debounce_short_sessions() {
        let mut s = sleep();
        s.on_power(Power::DisplayOff, T0);
        ticks(&mut s, T0, T0 + 9 * MIN);
        s.on_power(Power::DisplayOn, T0 + 9 * MIN);
        assert!(s.nights.is_empty());
        assert!(!s.dirty());
    }

    #[test]
    fn verdict_texts() {
        let good = night(7 * H + 42 * MIN, 80.0, 77.0, 4.0);
        assert_eq!(verdict(&good), (Tone::Good, "Slept 7h 42m. Lost 3%. Good.".into()));

        let mut bad = night(2 * H, 90.0, 49.0, 71.0);
        bad.culprit = Some(("SearchIndexer".into(), 900.0));
        assert_eq!(
            verdict(&bad),
            (Tone::Bad, "Lost 41% in 2h. Awake 71% of the time. SearchIndexer was busy.".into())
        );

        let insomnia = night(5 * H, 90.0, 72.0, 60.0);
        assert_eq!(verdict(&insomnia), (Tone::Warn, "Lost 18% in 5h. It stayed awake 60% of the time.".into()));

        let mut ac = night(7 * H + 42 * MIN, 80.0, 100.0, 4.0);
        ac.on_ac = true;
        assert_eq!(verdict(&ac), (Tone::Info, "Slept 7h 42m on power. Awake 4% of the time.".into()));

        let mut desk = night(7 * H + 42 * MIN, 0.0, 0.0, 4.0);
        desk.battery = false;
        desk.pct0 = None;
        desk.pct1 = None;
        assert_eq!(verdict(&desk), (Tone::Info, "Slept 7h 42m. Awake 4% of the time.".into()));
        assert!(notice_for(&desk).is_none(), "no toast without drain unless mostly awake");

        let n = notice_for(&bad).unwrap();
        assert_eq!(n.title, "Battery drained while asleep");
        assert_eq!(notice_for(&good).unwrap().title, "Sleep verdict");
        assert!(notice_for(&night(15 * MIN, 90.0, 50.0, 90.0)).is_none(), "too short to toast");
    }

    #[test]
    fn mwh_beats_percent() {
        let mut n = night(4 * H, 80.0, 80.0, 10.0);
        n.mwh0 = Some(40_000);
        n.mwh1 = Some(38_000);
        n.mwh_max = Some(50_000);
        assert!((n.lost_pct().unwrap() - 4.0).abs() < 0.01);
        assert!((n.drain_per_h().unwrap() - 1.0).abs() < 0.01);
    }

    #[test]
    fn one_toast_per_night() {
        let mut s = sleep();
        s.on_power(Power::DisplayOff, T0);
        ticks(&mut s, T0, T0 + MIN);
        set_bat(true, false, 20_000); // lost 40% in 1 h
        let end = T0 + H;
        s.on_power(Power::DisplayOn, end);
        assert!(s.dirty());
        assert!(ticks(&mut s, end, end + 110_000).is_empty(), "waits for the wake event");
        let out = ticks(&mut s, end + 110_000, end + 4 * MIN);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Battery drained while asleep");
        assert!(ticks(&mut s, end + 4 * MIN, end + 10 * MIN).is_empty());
    }

    #[test]
    fn late_wake_event_attaches_and_toasts() {
        let mut s = sleep();
        s.on_power(Power::DisplayOff, T0);
        set_bat(true, false, 30_000);
        let end = T0 + 2 * H;
        s.on_power(Power::DisplayOn, end);
        let e = SysEvent {
            channel: "System".into(),
            provider: "Microsoft-Windows-Power-Troubleshooter".into(),
            id: 1,
            t_ms: end - 1000,
            data: vec![("WakeSourceText".into(), "Power Button".into())],
        };
        s.on_event(&e);
        assert_eq!(s.nights.last().unwrap().wake.as_ref().unwrap().reason, "Power button");
        let out = tick(&mut s, end + 5000);
        assert_eq!(out.len(), 1, "wake reason arrived, no need to wait");
        assert!(tick(&mut s, end + 10_000).is_empty());
        // Too late: outside the attach window.
        tick(&mut s, end + 3 * MIN);
        let late = SysEvent {
            provider: "Microsoft-Windows-Kernel-Power".into(),
            id: 507,
            t_ms: end + 3 * MIN,
            data: vec![("Reason".into(), "31".into())],
            ..Default::default()
        };
        s.on_event(&late);
        assert_eq!(s.nights.last().unwrap().wake.as_ref().unwrap().reason, "Power button");
        assert!(s.panel(end).lines.iter().any(|l| l == "Woke by: Power button."));
    }

    #[test]
    fn wake_reason_parsing() {
        let ev = |p: &str, id, k: &str, v: &str| SysEvent {
            provider: p.into(),
            id,
            data: vec![(k.into(), v.into())],
            ..Default::default()
        };
        let kp = "Microsoft-Windows-Kernel-Power";
        let r = |e: SysEvent| wake_info(&e).and_then(|(_, w)| w).map(|w| w.reason);
        assert_eq!(r(ev(kp, 507, "Reason", "31")).as_deref(), Some("keyboard"));
        assert_eq!(r(ev(kp, 507, "reason", "0x1")).as_deref(), Some("Power button"));
        assert_eq!(r(ev(kp, 507, "Reason", "999")), None);
        let pt = "Microsoft-Windows-Power-Troubleshooter";
        let eth = ev(pt, 1, "WakeSourceText", "Device -Intel(R) Ethernet Connection");
        assert_eq!(r(eth).as_deref(), Some("network adapter"));
        assert_eq!(r(ev(pt, 1, "WakeSourceText", "Unknown")), None);
        assert!(wake_info(&ev(kp, 42, "x", "y")).unwrap().0);
        assert!(wake_info(&ev(kp, 41, "x", "y")).is_none());
        assert!(wake_info(&ev("Display", 4101, "x", "y")).is_none());
    }

    #[test]
    fn culprit_and_wants_procs() {
        let mut s = sleep();
        assert!(!s.wants_procs());
        s.on_power(Power::DisplayOff, T0);
        assert!(s.wants_procs());
        let mut t = T0;
        for _ in 0..240 {
            t += 5000;
            tick_with(&mut s, t, 5.0, &[("ResourceMonitor", 50.0), ("SearchIndexer", 20.0), ("Idle", 90.0)]);
        }
        s.on_power(Power::DisplayOn, T0 + H);
        assert!(!s.wants_procs());
        let (name, cpu) = s.nights.last().unwrap().culprit.clone().unwrap();
        assert_eq!(name, "SearchIndexer");
        assert!((cpu - 240.0).abs() < 1.0, "{cpu}"); // 20% of 1 core for 20 min
    }

    #[test]
    fn bag_guard_decision() {
        // 50 Wh battery, 1% per minute = 60 %/h.
        let fast: Vec<(u64, u32)> = (0..11).map(|i| (T0 + i * MIN, 40_000 - i as u32 * 500)).collect();
        assert!(bag_guard_due(true, true, false, false, Some(50_000), &fast));
        assert!(!bag_guard_due(false, true, false, false, Some(50_000), &fast), "opt-in");
        assert!(!bag_guard_due(true, false, false, false, Some(50_000), &fast), "lid open");
        assert!(!bag_guard_due(true, true, true, false, Some(50_000), &fast), "on AC");
        assert!(!bag_guard_due(true, true, false, true, Some(50_000), &fast), "once per night");
        assert!(!bag_guard_due(true, true, false, false, None, &fast), "no mWh");
        assert!(!bag_guard_due(true, true, false, false, Some(50_000), &fast[..5]), "window too short");
        // 0.1% per minute = 6 %/h: below the line.
        let slow: Vec<(u64, u32)> = (0..11).map(|i| (T0 + i * MIN, 40_000 - i as u32 * 50)).collect();
        assert!(!bag_guard_due(true, true, false, false, Some(50_000), &slow));
    }

    #[test]
    fn bag_guard_hibernates_once() {
        let mut s = sleep();
        s.bag_guard = true;
        s.on_power(Power::LidClosed, T0);
        s.on_power(Power::DisplayOff, T0 + 1000);
        let mut t = T0;
        let mut m = 40_000;
        while t < T0 + 20 * MIN {
            t += 5000;
            if (t - T0) % MIN == 0 {
                m -= 500;
                set_bat(true, false, m);
            }
            tick(&mut s, t);
        }
        assert_eq!(HIB.with(|h| h.get()), 1);
        s.on_power(Power::LidOpen, t + 1000);
        s.on_power(Power::DisplayOn, t + 2000);
        let n = s.nights.last().unwrap();
        assert_eq!(n.guard_ms, Some(T0 + 9 * MIN));
        assert!(s.panel(t).lines.iter().any(|l| l.starts_with("Bag Guard hibernated at")));
    }

    #[test]
    fn save_load_roundtrip_and_garbage() {
        let mut s = sleep();
        let mut n = night(2 * H, 90.0, 49.0, 71.0);
        n.culprit = Some(("SearchIndexer".into(), 900.0));
        n.wake = Some(Wake { t_ms: T0 + 2 * H, rank: 2, reason: "Power button".into() });
        n.mwh0 = Some(45_000);
        n.mwh1 = Some(24_500);
        n.mwh_max = Some(50_000);
        n.guard_ms = Some(T0 + H);
        s.nights.push(n.clone());
        let j = crate::achievements::json::parse(&s.save().to_string()).unwrap();
        let mut t = sleep();
        t.load(&j);
        assert_eq!(t.nights, vec![n]);

        let text = r#"{"nights":[{"s":"x"},{"s":5,"e":2},7,{"s":10,"e":20,"aw":99,"p0":500}]}"#;
        let junk = crate::achievements::json::parse(text).unwrap();
        let mut u = sleep();
        u.load(&junk);
        assert_eq!(u.nights.len(), 1);
        assert_eq!(u.nights[0].awake_s, 0);
        assert_eq!(u.nights[0].pct0, None);
        u.load(&Json::Null);
        assert_eq!(u.nights.len(), 1);
    }

    #[test]
    fn panel_empty_and_full() {
        let s = sleep();
        let p = s.panel(T0);
        assert_eq!(p.tone, Tone::Learning);
        assert_eq!(p.headline, "No sleep recorded yet. Close the lid or let the screen turn off.");
        let mut s = sleep();
        for i in 0..16 {
            let mut n = night(8 * H, 90.0, 90.0 - i as f32, 5.0);
            n.start_ms += i * 24 * H;
            n.end_ms += i * 24 * H;
            s.nights.push(n);
            if s.nights.len() > MAX_NIGHTS {
                s.nights.remove(0);
            }
        }
        let p = s.panel(T0);
        assert_eq!(p.items.len(), 14);
        assert!(p.items[0].t_ms > p.items[13].t_ms, "newest first");
        let c = p.chart.unwrap();
        assert_eq!(c.points.len(), 14);
        assert!(c.points[0] < c.points[13], "oldest first");
        assert_eq!(c.unit, "%/h");
        assert!(p.lines.iter().any(|l| l == "Bag Guard is off."));
    }

    #[test]
    fn desktop_without_battery() {
        let mut s = sleep();
        set_bat(false, false, 0);
        s.on_power(Power::DisplayOff, T0);
        ticks(&mut s, T0, T0 + 30 * MIN);
        s.on_power(Power::DisplayOn, T0 + 30 * MIN);
        let n = s.nights.last().unwrap();
        assert!(!n.battery);
        assert!(n.drain_per_h().is_none());
        let (_, text) = verdict(n);
        assert_eq!(text, "Stayed awake 100% of 30m.");
        assert!(notice_for(n).is_some(), "mostly awake: worth a toast");
    }
}
