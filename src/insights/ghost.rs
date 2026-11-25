//! Ghost Memory: "RAM is full. Nothing is using it."
//!
//! Once a minute (by `ctx.now_ms`, never per tick) we read `GetPerformanceInfo` and the working
//! set of the "Memory Compression" process (PID found once, handle reused). The kernel pool
//! (paged + nonpaged) is the memory no process owns; a driver that leaks grows it day after day.
//! We keep hourly points for the current boot (for the slope) and one row per local day
//! (for the 30-day chart). Pool tags would name the driver but need admin, so we never try.

use super::store::{num, obj};
use super::{time, Chart, Ctx, Panel, Tone};
use crate::achievements::json::Json;
use std::cell::Cell;

const SAMPLE_EVERY_MS: u64 = 60_000;
/// One trend point per hour of the current boot.
const POINT_EVERY_MS: u64 = 3_600_000;
/// Above this many points, every other one is dropped (keeps the whole boot, coarser).
const POINTS_CAP: usize = 240;
const DAYS_KEEP: usize = 30;
/// Daily rows are marked dirty at most this often (plus on a new day / new boot).
const ROW_SAVE_EVERY_MS: u64 = 3_600_000;
/// Retry the Memory Compression lookup at most this often after a failure.
const COMP_RETRY_MS: u64 = 3_600_000;
/// One value per minute for today's median.
const TODAY_CAP: usize = 1440;

/// Leak: pool grows at least this fast...
const LEAK_GB_PER_DAY: f32 = 0.3;
/// ...over at least this many days of uptime.
const LEAK_MIN_DAYS: f32 = 3.0;
/// Pool above this share of RAM is bad on its own.
const LARGE_SHARE: f32 = 0.25;
/// Below this much trend data we only watch.
const LEARN_MIN_DAYS: f32 = 1.0;
/// The slope is fitted over this many most recent days of the boot only, so a leak that
/// starts after a long flat stretch is not averaged away by the old flat points.
const FIT_WINDOW_DAYS: f32 = 7.0;

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

fn gb(bytes: u64) -> f32 {
    (bytes as f64 / GB) as f32
}

/// One `GetPerformanceInfo` reading, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemSample {
    pub total: u64,
    pub avail: u64,
    pub cache: u64,
    pub kpaged: u64,
    pub knonpaged: u64,
    pub commit: u64,
    pub commit_limit: u64,
    /// Working set of the Memory Compression process; `None` when it could not be read.
    pub compressed: Option<u64>,
}

impl MemSample {
    /// Kernel pool (paged + nonpaged): memory no process owns.
    pub fn pool(&self) -> u64 {
        self.kpaged.saturating_add(self.knonpaged)
    }
}

/// Where physical memory goes. The five parts always sum to `total`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Breakdown {
    pub apps: u64,
    pub compressed: u64,
    pub kernel: u64,
    pub cache: u64,
    pub free: u64,
}

/// Split physical memory into five parts that sum to the total.
///
/// * Free = Available (free + zeroed + standby). Standby is cache Windows hands back at once,
///   so it counts as free, like Task Manager's "Available".
/// * Used = Total - Available, carved in this order, each part clamped to what is left:
///   1. Kernel pool = paged + nonpaged pool. Paged pool can be partly on disk, so this is an
///      upper bound of its share of RAM; clamping keeps the sum honest.
///   2. Compressed = Memory Compression working set (resident by definition).
///   3. Cache in use = SystemCache - Available. SystemCache is "standby list + system working
///      set"; standby is already inside Available, so what is left is a lower bound of the
///      system working set (file cache in use).
///   4. Apps = the rest: process memory plus small kernel items we cannot see (driver locked
///      pages, page tables).
pub fn breakdown(m: &MemSample) -> Breakdown {
    let free = m.avail.min(m.total);
    let mut rest = m.total - free;
    let mut take = |want: u64| {
        let v = want.min(rest);
        rest -= v;
        v
    };
    let kernel = take(m.pool());
    let compressed = take(m.compressed.unwrap_or(0));
    let cache = take(m.cache.saturating_sub(m.avail));
    Breakdown { apps: rest, compressed, kernel, cache, free }
}

/// Boot id: boot time in unix minutes (now - uptime, rounded). `GetTickCount64` counts sleep,
/// so it stays put across sleep; clock nudges move it by a minute at most.
pub fn boot_id(now_ms: u64, uptime_secs: u64) -> u64 {
    ((now_ms / 1000).saturating_sub(uptime_secs) + 30) / 60
}

fn same_boot(a: u64, b: u64) -> bool {
    a.abs_diff(b) <= 2
}

/// Hourly pool points of the current boot: (uptime hours, pool GB).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trend {
    pub boot: u64,
    pub last_uptime: u64,
    pub pts: Vec<(f32, f32)>,
    last_pt_ms: u64,
}

impl Trend {
    /// Feed one reading. Returns true when a new boot was detected (points reset).
    pub fn observe(&mut self, now_ms: u64, uptime_secs: u64, pool_gb: f32) -> bool {
        let id = boot_id(now_ms, uptime_secs);
        let new_boot = uptime_secs < self.last_uptime || !same_boot(id, self.boot);
        if new_boot {
            self.boot = id;
            self.pts.clear();
            self.last_pt_ms = 0;
        }
        self.last_uptime = uptime_secs;
        let due = now_ms.saturating_sub(self.last_pt_ms) >= POINT_EVERY_MS || now_ms < self.last_pt_ms;
        if self.pts.is_empty() || due {
            self.last_pt_ms = now_ms;
            self.pts.push((uptime_secs as f32 / 3600.0, pool_gb));
            if self.pts.len() > POINTS_CAP {
                let mut i = 0;
                self.pts.retain(|_| {
                    i += 1;
                    i % 2 == 1
                });
            }
        }
        new_boot
    }

    /// Days covered by the points.
    #[cfg(test)]
    pub fn span_days(&self) -> f32 {
        span_days(&self.pts)
    }

    /// The points of the last `FIT_WINDOW_DAYS` of uptime (what the slope is fitted on).
    pub fn recent(&self) -> &[(f32, f32)] {
        let Some(last) = self.pts.last() else { return &self.pts };
        let from = last.0 - FIT_WINDOW_DAYS * 24.0;
        let i = self.pts.iter().position(|p| p.0 >= from).unwrap_or(0);
        &self.pts[i..]
    }
}

/// Days covered by a run of (uptime hours, GB) points.
fn span_days(pts: &[(f32, f32)]) -> f32 {
    match (pts.first(), pts.last()) {
        (Some(a), Some(b)) => (b.0 - a.0).max(0.0) / 24.0,
        _ => 0.0,
    }
}

/// Least-squares slope of GB over uptime hours, as GB per day. `None` with fewer than three
/// points or under an hour of spread.
pub fn slope_per_day(pts: &[(f32, f32)]) -> Option<f32> {
    if pts.len() < 3 {
        return None;
    }
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p.0 as f64).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1 as f64).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &(x, y) in pts {
        let dx = x as f64 - mx;
        sxy += dx * (y as f64 - my);
        sxx += dx * dx;
    }
    let spread = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) - pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    if sxx <= 0.0 || spread < 1.0 {
        return None;
    }
    Some((sxy / sxx * 24.0) as f32)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    Learning,
    Steady { pool_gb: f32 },
    /// Growing, but not for long enough to call it a leak.
    Growing { per_day: f32 },
    Leak { per_day: f32, days: f32 },
    Large { pool_gb: f32, share: f32 },
}

/// Judge the kernel pool. `slope` in GB/day over `span_days` of the current boot.
pub fn verdict(pool_gb: f32, total_gb: f32, slope: Option<f32>, span_days: f32) -> Verdict {
    if total_gb > 0.0 && pool_gb / total_gb > LARGE_SHARE {
        return Verdict::Large { pool_gb, share: pool_gb / total_gb };
    }
    let Some(per_day) = slope.filter(|_| span_days >= LEARN_MIN_DAYS) else {
        return Verdict::Learning;
    };
    if per_day >= LEAK_GB_PER_DAY {
        if span_days >= LEAK_MIN_DAYS {
            Verdict::Leak { per_day, days: span_days }
        } else {
            Verdict::Growing { per_day }
        }
    } else {
        Verdict::Steady { pool_gb }
    }
}

/// (tone, headline) for a verdict.
pub fn headline(v: Verdict) -> (Tone, String) {
    match v {
        Verdict::Learning => (Tone::Learning, "Watching. Needs a day of uptime to judge the trend.".into()),
        Verdict::Steady { pool_gb } => {
            (Tone::Good, format!("No ghosts. Kernel memory is steady at {:.1} GB.", pool_gb))
        }
        Verdict::Growing { per_day } => {
            (Tone::Info, format!("Kernel pool is growing, +{:.1} GB a day. Watching.", per_day))
        }
        Verdict::Leak { per_day, days } => (
            Tone::Warn,
            format!(
                "Kernel pool +{:.1} GB a day for {} days. A driver is leaking. A restart buys time.",
                per_day,
                days.round().max(1.0) as u32
            ),
        ),
        Verdict::Large { pool_gb, share } => (
            Tone::Bad,
            format!(
                "Kernel pool is {:.1} GB, {}% of RAM. A driver may be leaking. A restart buys time.",
                pool_gb,
                (share * 100.0).round() as u32
            ),
        ),
    }
}

/// One local day of kernel pool readings.
#[derive(Clone, Debug, PartialEq)]
pub struct DayRow {
    pub day: u32,
    pub max_gb: f32,
    pub med_gb: f32,
    /// Uptime at the latest sample of the day.
    pub uptime_h: f32,
    /// Boot id of the latest sample of the day.
    pub boot: u64,
}

fn median(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 }
}

/// Open handle to the Memory Compression process.
struct Comp {
    handle: isize,
}

impl Drop for Comp {
    fn drop(&mut self) {
        // SAFETY: handle owned by us, closed exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle as _) };
    }
}

pub struct Ghost {
    sampled: bool,
    last_ms: u64,
    /// GetPerformanceInfo failed on the last try.
    failed: bool,
    latest: Option<MemSample>,
    comp: Option<Comp>,
    comp_next_try_ms: u64,
    trend: Trend,
    /// Oldest first, at most `DAYS_KEEP`.
    rows: Vec<DayRow>,
    today: Vec<f32>,
    today_day: u32,
    last_row_save_ms: u64,
    /// Cleared by `save()`.
    dirty: Cell<bool>,
}

impl Ghost {
    pub fn new() -> Ghost {
        Ghost {
            sampled: false,
            last_ms: 0,
            failed: false,
            latest: None,
            comp: None,
            comp_next_try_ms: 0,
            trend: Trend::default(),
            rows: Vec::new(),
            today: Vec::new(),
            today_day: 0,
            last_row_save_ms: 0,
            dirty: Cell::new(false),
        }
    }

    pub fn on_sample(&mut self, c: &Ctx) {
        let now = c.now_ms;
        if self.sampled && now >= self.last_ms && now - self.last_ms < SAMPLE_EVERY_MS {
            return;
        }
        self.sampled = true;
        self.last_ms = now;
        let Some(mut m) = perf_info() else {
            self.failed = true;
            return;
        };
        self.failed = false;
        m.compressed = self.compressed(now);
        self.latest = Some(m);
        self.record(now, c.snap.uptime_secs, gb(m.pool()));
    }

    /// Trend point + daily row for one reading.
    fn record(&mut self, now_ms: u64, uptime_secs: u64, pool_gb: f32) {
        let new_boot = self.trend.observe(now_ms, uptime_secs, pool_gb);
        let day = time::day(now_ms);
        if day != self.today_day {
            self.today_day = day;
            self.today.clear();
            // App restarted during the day: carry the stored median as one sample.
            if let Some(r) = self.rows.iter().find(|r| r.day == day) {
                self.today.push(r.med_gb);
            }
        }
        if self.today.len() < TODAY_CAP {
            self.today.push(pool_gb);
        }
        let boot = self.trend.boot;
        let uptime_h = uptime_secs as f32 / 3600.0;
        let mut save = new_boot || now_ms.saturating_sub(self.last_row_save_ms) >= ROW_SAVE_EVERY_MS;
        let i = match self.rows.iter().position(|r| r.day == day) {
            Some(i) => i,
            None => {
                save = true;
                let at = self.rows.iter().position(|r| r.day > day).unwrap_or(self.rows.len());
                self.rows.insert(at, DayRow { day, max_gb: pool_gb, med_gb: pool_gb, uptime_h, boot });
                at
            }
        };
        let r = &mut self.rows[i];
        r.max_gb = r.max_gb.max(pool_gb);
        r.uptime_h = uptime_h;
        r.boot = boot;
        if save {
            r.med_gb = median(&self.today);
            self.last_row_save_ms = now_ms;
            self.dirty.set(true);
        }
        if self.rows.len() > DAYS_KEEP {
            self.rows.drain(..self.rows.len() - DAYS_KEEP);
        }
    }

    /// Memory Compression working set. PID lookup once (retried hourly on failure), then the
    /// same handle every minute. `None` if the process is missing or access is denied.
    fn compressed(&mut self, now_ms: u64) -> Option<u64> {
        if self.comp.is_none() && now_ms >= self.comp_next_try_ms {
            self.comp_next_try_ms = now_ms + COMP_RETRY_MS;
            self.comp = crate::sensors::find_process("Memory Compression").and_then(open_process);
        }
        let h = self.comp.as_ref()?.handle;
        let ws = working_set(h);
        if ws.is_none() {
            // Dead handle or access lost: drop it and look again later.
            self.comp = None;
        }
        ws
    }

    /// Persistent state changed since the last save.
    pub fn dirty(&self) -> bool {
        self.dirty.get()
    }

    pub fn save(&self) -> Json {
        self.dirty.set(false);
        let r2 = |v: f32| Json::Num((v as f64 * 100.0).round() / 100.0);
        let rows = self
            .rows
            .iter()
            .map(|r| {
                obj(vec![
                    ("d", Json::Num(r.day as f64)),
                    ("x", r2(r.max_gb)),
                    ("m", r2(r.med_gb)),
                    ("u", r2(r.uptime_h)),
                    ("b", Json::Num(r.boot as f64)),
                ])
            })
            .collect();
        let pts = self.trend.pts.iter().map(|p| Json::Arr(vec![r2(p.0), r2(p.1)])).collect();
        obj(vec![
            ("rows", Json::Arr(rows)),
            ("boot", Json::Num(self.trend.boot as f64)),
            ("up", Json::Num(self.trend.last_uptime as f64)),
            ("pts", Json::Arr(pts)),
        ])
    }

    pub fn load(&mut self, j: &Json) {
        let ok = |v: f64| v.is_finite() && (0.0..1.0e6).contains(&v);
        if let Some(list) = j.get("rows").and_then(Json::as_arr) {
            let mut rows: Vec<DayRow> = list
                .iter()
                .filter_map(|x| {
                    let (d, mx, md, u) = (num(x, "d")?, num(x, "x")?, num(x, "m")?, num(x, "u")?);
                    let b = num(x, "b").unwrap_or(0.0);
                    if !(ok(d) && ok(mx) && ok(md) && ok(u) && b >= 0.0) {
                        return None;
                    }
                    Some(DayRow { day: d as u32, max_gb: mx as f32, med_gb: md as f32, uptime_h: u as f32, boot: b as u64 })
                })
                .collect();
            rows.sort_by_key(|r| r.day);
            rows.dedup_by_key(|r| r.day);
            if rows.len() > DAYS_KEEP {
                rows.drain(..rows.len() - DAYS_KEEP);
            }
            self.rows = rows;
        }
        self.trend.boot = j.get("boot").and_then(Json::as_u64).unwrap_or(0);
        self.trend.last_uptime = j.get("up").and_then(Json::as_u64).unwrap_or(0);
        self.trend.pts = j
            .get("pts")
            .and_then(Json::as_arr)
            .map(|a| {
                a.iter()
                    .filter_map(|p| match p.as_arr()? {
                        [Json::Num(x), Json::Num(y)] if ok(*x) && ok(*y) => Some((*x as f32, *y as f32)),
                        _ => None,
                    })
                    .take(POINTS_CAP)
                    .collect()
            })
            .unwrap_or_default();
    }

    pub fn panel(&self, _now_ms: u64) -> Panel {
        let mut p = Panel {
            key: "ghost",
            title: "Ghost Memory",
            tone: Tone::Learning,
            headline: "Watching. Needs a day of uptime to judge the trend.".into(),
            lines: Vec::new(),
            chart: None,
            items: Vec::new(),
        };
        let Some(m) = self.latest else {
            if self.failed {
                p.tone = Tone::Unavailable;
                p.headline = "This PC does not report memory details.".into();
            }
            return p;
        };
        let recent = self.trend.recent();
        let slope = slope_per_day(recent);
        (p.tone, p.headline) = headline(verdict(gb(m.pool()), gb(m.total), slope, span_days(recent)));
        let b = breakdown(&m);
        p.lines.push(format!("Apps: {:.1} GB", gb(b.apps)));
        p.lines.push(match m.compressed {
            Some(_) => format!("Compressed: {:.1} GB", gb(b.compressed)),
            None => "Compressed: not available".into(),
        });
        p.lines.push(format!("Kernel pool: {:.1} GB", gb(b.kernel)));
        p.lines.push(format!("Cache in use: {:.1} GB", gb(b.cache)));
        p.lines.push(format!("Free: {:.1} GB", gb(b.free)));
        if m.commit_limit > 0 {
            p.lines.push(format!("Committed: {:.1} of {:.1} GB", gb(m.commit), gb(m.commit_limit)));
        }
        if !self.rows.is_empty() {
            let markers = (1..self.rows.len())
                .filter(|&i| !same_boot(self.rows[i].boot, self.rows[i - 1].boot))
                .map(|i| (i, "Restart".to_string()))
                .collect();
            p.chart = Some(Chart {
                label: "Kernel pool, peak per day".into(),
                points: self.rows.iter().map(|r| r.max_gb).collect(),
                max: 0.0,
                unit: "GB",
                markers,
            });
        }
        p
    }
}

// ---- Win32 -------------------------------------------------------------------------------

/// `GetPerformanceInfo`, converted from pages to bytes.
fn perf_info() -> Option<MemSample> {
    use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    // SAFETY: plain old data; cb set before the call.
    let mut pi: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
    pi.cb = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
    // SAFETY: valid, sized out pointer.
    if unsafe { GetPerformanceInfo(&mut pi, pi.cb) } == 0 || pi.PageSize == 0 || pi.PhysicalTotal == 0 {
        return None;
    }
    let pg = |n: usize| (n as u64).saturating_mul(pi.PageSize as u64);
    Some(MemSample {
        total: pg(pi.PhysicalTotal),
        avail: pg(pi.PhysicalAvailable),
        cache: pg(pi.SystemCache),
        kpaged: pg(pi.KernelPaged),
        knonpaged: pg(pi.KernelNonpaged),
        commit: pg(pi.CommitTotal),
        commit_limit: pg(pi.CommitLimit),
        compressed: None,
    })
}

fn open_process(pid: u32) -> Option<Comp> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain call; a null result means access denied or gone.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    (!h.is_null()).then_some(Comp { handle: h as isize })
}

/// Working set of a live process; `None` if it exited or the call fails.
fn working_set(handle: isize) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0u32;
    // SAFETY: valid handle owned by `Comp`; valid out pointer.
    if unsafe { GetExitCodeProcess(handle as _, &mut code) } == 0 || code != STILL_ACTIVE {
        return None;
    }
    // SAFETY: plain old data; cb set before the call.
    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: valid handle and sized out pointer.
    if unsafe { GetProcessMemoryInfo(handle as _, &mut pmc, pmc.cb) } == 0 {
        return None;
    }
    Some(pmc.WorkingSetSize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn sample() -> MemSample {
        MemSample {
            total: 16 * G,
            avail: 6 * G,
            cache: 7 * G,
            kpaged: G / 2,
            knonpaged: G / 2,
            commit: 12 * G,
            commit_limit: 24 * G,
            compressed: Some(G),
        }
    }

    fn sum(b: &Breakdown) -> u64 {
        b.apps + b.compressed + b.kernel + b.cache + b.free
    }

    #[test]
    fn breakdown_sums_to_total() {
        let m = sample();
        let b = breakdown(&m);
        assert_eq!(sum(&b), m.total);
        assert_eq!(b.free, 6 * G);
        assert_eq!(b.kernel, G);
        assert_eq!(b.compressed, G);
        assert_eq!(b.cache, G);
        assert_eq!(b.apps, 7 * G);
        // Odd inputs: parts larger than used memory, missing compression, avail > total.
        for m in [
            MemSample { kpaged: 40 * G, ..sample() },
            MemSample { compressed: None, cache: 0, ..sample() },
            MemSample { avail: 20 * G, ..sample() },
            MemSample::default(),
        ] {
            assert_eq!(sum(&breakdown(&m)), m.total);
        }
    }

    #[test]
    fn slope_of_a_line() {
        // +0.5 GB a day, sampled every 6 hours for 4 days.
        let pts: Vec<(f32, f32)> = (0..17).map(|i| (i as f32 * 6.0, 1.0 + i as f32 * 0.125)).collect();
        let s = slope_per_day(&pts).unwrap();
        assert!((s - 0.5).abs() < 1e-3, "{}", s);
        let flat: Vec<(f32, f32)> = (0..10).map(|i| (i as f32, 0.9)).collect();
        assert!(slope_per_day(&flat).unwrap().abs() < 1e-6);
        assert_eq!(slope_per_day(&pts[..2]), None);
        assert_eq!(slope_per_day(&[(1.0, 1.0), (1.2, 2.0), (1.5, 3.0)]), None);
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict(0.9, 16.0, None, 0.0), Verdict::Learning);
        assert_eq!(verdict(0.9, 16.0, Some(0.05), 0.5), Verdict::Learning);
        assert_eq!(verdict(0.9, 16.0, Some(0.05), 2.0), Verdict::Steady { pool_gb: 0.9 });
        assert_eq!(verdict(2.0, 16.0, Some(0.6), 2.0), Verdict::Growing { per_day: 0.6 });
        assert_eq!(verdict(5.0, 32.0, Some(0.6), 9.0), Verdict::Leak { per_day: 0.6, days: 9.0 });
        // 5 GB of pool on a 16 GB PC is bad on its own, whatever the trend.
        assert!(matches!(verdict(5.0, 16.0, Some(0.6), 9.0), Verdict::Large { .. }));
        assert!(matches!(verdict(4.2, 16.0, None, 0.0), Verdict::Large { .. }));
        let (tone, h) = headline(Verdict::Leak { per_day: 0.6, days: 9.0 });
        assert_eq!(tone, Tone::Warn);
        assert_eq!(h, "Kernel pool +0.6 GB a day for 9 days. A driver is leaking. A restart buys time.");
        assert_eq!(headline(Verdict::Steady { pool_gb: 0.94 }).1, "No ghosts. Kernel memory is steady at 0.9 GB.");
    }

    #[test]
    fn trend_resets_on_reboot() {
        let mut t = Trend::default();
        let boot_ms = 1_700_000_000_000u64;
        let up = |h: u64| h * 3600;
        assert!(t.observe(boot_ms + up(1) * 1000, up(1), 1.0));
        for h in 2..30 {
            assert!(!t.observe(boot_ms + up(h) * 1000, up(h), 1.0 + h as f32 * 0.01));
        }
        assert_eq!(t.pts.len(), 29);
        // A minute later: no new hourly point.
        assert!(!t.observe(boot_ms + (up(29) + 60) * 1000, up(29) + 60, 1.3));
        assert_eq!(t.pts.len(), 29);
        assert!(t.span_days() > 1.1);
        // Reboot: uptime drops.
        let now = boot_ms + up(40) * 1000;
        assert!(t.observe(now, 120, 0.8));
        assert_eq!(t.pts, vec![(120.0 / 3600.0, 0.8)]);
        assert_eq!(t.boot, boot_id(now, 120));
    }

    #[test]
    fn late_leak_is_not_averaged_away() {
        // 20 flat days, then +0.6 GB a day for 4 days, one point per hour.
        let mut t = Trend::default();
        let boot_ms = 1_700_000_000_000u64;
        for h in 1..=(24 * 24u64) {
            let d = h as f32 / 24.0;
            let gb = if d <= 20.0 { 1.0 } else { 1.0 + (d - 20.0) * 0.6 };
            t.observe(boot_ms + h * 3_600_000, h * 3600, gb);
        }
        let whole = slope_per_day(&t.pts).unwrap();
        assert!(whole < 0.3, "whole-boot fit hides it: {}", whole);
        let r = t.recent();
        let s = slope_per_day(r).unwrap();
        assert!(s >= 0.3, "recent fit sees it: {}", s);
        let v = verdict(3.4, 32.0, Some(s), span_days(r));
        assert!(matches!(v, Verdict::Leak { .. }), "{:?}", v);
    }

    #[test]
    fn trend_points_are_capped() {
        let mut t = Trend::default();
        for h in 0..1000u64 {
            t.observe(1_700_000_000_000 + h * 3_600_000, 3600 + h * 3600, 1.0);
        }
        assert!(t.pts.len() <= POINTS_CAP);
        assert!(t.span_days() > 40.0);
    }

    #[test]
    fn daily_rows_and_markers() {
        let mut g = Ghost::new();
        let day_ms = 86_400_000u64;
        let start = 1_700_000_000_000u64 / day_ms * day_ms;
        // Two days of one boot, then a reboot on day three.
        for m in 0..(2 * 24 * 60) {
            let now = start + m * 60_000;
            g.record(now, 3600 + m * 60, 1.0 + m as f32 / 10_000.0);
        }
        let now = start + 3 * day_ms + 12 * 3_600_000;
        g.record(now, 300, 0.7);
        assert!(g.rows.len() >= 3);
        assert!(g.dirty());
        let _ = g.save();
        assert!(!g.dirty());
        let last = g.rows.last().unwrap();
        assert_eq!(last.max_gb, 0.7);
        assert_ne!(last.boot, g.rows[0].boot);
        // Round trip.
        let mut h = Ghost::new();
        h.load(&g.save());
        assert_eq!(h.rows.len(), g.rows.len());
        assert_eq!(h.trend.boot, g.trend.boot);
        assert_eq!(h.trend.pts.len(), g.trend.pts.len());
        // Garbage is tolerated.
        h.load(&Json::Str("x".into()));
        h.load(&crate::achievements::json::parse(r#"{"rows":[{"d":-1},{"d":"a"}],"pts":[[1],[2,"x"]]}"#).unwrap());
        assert!(h.rows.is_empty());
        assert!(h.trend.pts.is_empty());
    }

    #[test]
    fn median_works() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }
}
