//! Last hour of samples, compact. Fed every tick by `Insights::on_sample`.
//! OWNER: Incident Cards worker.
//!
//! One `Tick` is a small plain struct (no heap). Process names are interned in a table of at
//! most 255 names, so a tick stores one byte per name and a push allocates nothing (except
//! the first time a new process name shows up). Memory is reserved on the first push only,
//! so a disabled Insights costs nothing.

use super::Ctx;
use std::collections::HashMap;

/// Ticks kept: about one hour at the 1 s interval.
pub const CAP: usize = 3600;
/// Marks "no value" in the u8 / u16 fields and "no name" in name slots.
pub const NONE8: u8 = u8::MAX;
pub const NONE16: u16 = u16::MAX;
/// Names the interning table holds at most (index 255 is `NONE8`).
const MAX_NAMES: usize = 255;
/// Input idle for longer than this marks a tick as "user away".
pub const IDLE_MS: u64 = 60_000;
/// A sample gap longer than this is sleep / hibernate, not a freeze.
pub const GAP_MS: f64 = 30_000.0;

/// `Tick::flags` bits.
pub mod flag {
    /// No keyboard / mouse input for over a minute.
    pub const IDLE: u8 = 1;
    pub const DISPLAY: u8 = 2;
    /// Process lists with valid rates (sampled this tick and the one before).
    pub const PROCS: u8 = 4;
    /// More than 30 s since the previous sample: the PC slept.
    pub const GAP: u8 = 8;
}

/// One compact sample. Percent fields are rounded 0..100; `NONE8` / `NONE16` = no data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    /// Wall clock, unix seconds.
    pub t_s: u32,
    /// How late the tick was: elapsed - interval, ms (saturating at 0).
    pub late_ms: u32,
    pub ram_mb: u32,
    /// Disk read + write, KB/s.
    pub disk_kbps: u32,
    /// Network up + down, KB/s.
    pub net_kbps: u32,
    /// I/O rate of the top I/O process, KB/s.
    pub io_kbps: u32,
    pub fan_rpm: u16,
    pub cpu: u8,
    pub ram: u8,
    pub gpu: u8,
    /// GPU temperature, whole deg C.
    pub gpu_temp: u8,
    /// Disk active time %.
    pub disk: u8,
    pub flags: u8,
    /// Top 3 CPU processes: (name index, CPU % of the machine).
    pub top: [(u8, u8); 3],
    /// Top I/O process name index.
    pub io: u8,
}

impl Default for Tick {
    fn default() -> Tick {
        Tick {
            t_s: 0,
            late_ms: 0,
            ram_mb: 0,
            disk_kbps: 0,
            net_kbps: 0,
            io_kbps: 0,
            fan_rpm: NONE16,
            cpu: 0,
            ram: 0,
            gpu: NONE8,
            gpu_temp: NONE8,
            disk: NONE8,
            flags: 0,
            top: [(NONE8, 0); 3],
            io: NONE8,
        }
    }
}

impl Tick {
    pub fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }
    /// Unix ms of this tick.
    pub fn t_ms(&self) -> u64 {
        self.t_s as u64 * 1000
    }
    pub fn opt8(v: u8) -> Option<u8> {
        (v != NONE8).then_some(v)
    }
}

/// A numeric field for `Ring::series`.
#[allow(dead_code)] // API for the other Insights modules and the charts
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Cpu,
    Ram,
    Gpu,
    GpuTemp,
    Fan,
    Disk,
    DiskKbps,
    NetKbps,
    RamMb,
    LateMs,
}

impl Field {
    /// Value of this field, `None` when the tick has no data for it.
    pub fn of(self, t: &Tick) -> Option<f32> {
        let p = |v: u8| Tick::opt8(v).map(|v| v as f32);
        match self {
            Field::Cpu => Some(t.cpu as f32),
            Field::Ram => Some(t.ram as f32),
            Field::Gpu => p(t.gpu),
            Field::GpuTemp => p(t.gpu_temp),
            Field::Fan => (t.fan_rpm != NONE16).then_some(t.fan_rpm as f32),
            Field::Disk => p(t.disk),
            Field::DiskKbps => Some(t.disk_kbps as f32),
            Field::NetKbps => Some(t.net_kbps as f32),
            Field::RamMb => Some(t.ram_mb as f32),
            Field::LateMs => Some(t.late_ms as f32),
        }
    }
}

/// Small name table with least-recently-used reuse.
#[derive(Default)]
struct Names {
    names: Vec<String>,
    last_used: Vec<u32>,
    index: HashMap<String, u8>,
}

impl Names {
    fn intern(&mut self, name: &str, now_s: u32) -> u8 {
        if name.is_empty() {
            return NONE8;
        }
        if let Some(&i) = self.index.get(name) {
            self.last_used[i as usize] = now_s;
            return i;
        }
        let i = if self.names.len() < MAX_NAMES {
            self.names.push(name.to_string());
            self.last_used.push(now_s);
            self.names.len() - 1
        } else {
            // Reuse the slot seen longest ago. Usually older than the whole ring; if not,
            // a few old ticks may show the new name (accepted: 255 busy names in an hour).
            let (i, _) = self.last_used.iter().enumerate().min_by_key(|(_, &t)| t).unwrap_or((0, &0));
            let old = std::mem::replace(&mut self.names[i], name.to_string());
            self.index.remove(&old);
            self.last_used[i] = now_s;
            i
        };
        self.index.insert(name.to_string(), i as u8);
        i as u8
    }

    fn get(&self, i: u8) -> Option<&str> {
        self.names.get(i as usize).map(|s| s.as_str())
    }
}

pub struct Ring {
    buf: Vec<Tick>,
    /// Next write position once the buffer is full.
    head: usize,
    names: Names,
    /// Process lists were sampled on the previous push (rates are valid from the 2nd).
    prev_procs: bool,
    /// Sample interval of the previous push (0 = none yet).
    prev_interval_ms: u32,
}

fn pct(v: f32) -> u8 {
    if v.is_finite() {
        v.round().clamp(0.0, 100.0) as u8
    } else {
        0
    }
}

fn kb(bps: f64) -> u32 {
    if bps.is_finite() && bps > 0.0 {
        (bps / 1024.0).min(u32::MAX as f64) as u32
    } else {
        0
    }
}

impl Ring {
    pub fn new() -> Ring {
        Ring { buf: Vec::new(), head: 0, names: Names::default(), prev_procs: false, prev_interval_ms: 0 }
    }

    /// Record one sample. O(1); allocates only on the first push and for unseen names.
    pub fn push(&mut self, c: &Ctx) {
        let s = c.snap;
        let t_s = (c.now_ms / 1000).min(u32::MAX as u64) as u32;
        let elapsed_ms = if s.elapsed.is_finite() { (s.elapsed * 1000.0).max(0.0) } else { 0.0 };
        // Right after an interval change (Settings, display on / off) this gap was timed by the
        // old interval: measure lateness against the longer one so it is not a false freeze.
        let interval_ms = c.interval_ms.max(self.prev_interval_ms);
        self.prev_interval_ms = c.interval_ms;
        // Wall-clock gap since the last tick: also catches pauses the sample clock did not see
        // (Insights switched off for a while, a clock that stops in sleep).
        let wall_gap_s = self.last().map_or(0, |l| t_s.saturating_sub(l.t_s));
        let mut t = Tick {
            t_s,
            late_ms: (elapsed_ms - interval_ms as f64).clamp(0.0, u32::MAX as f64) as u32,
            ram_mb: (s.ram_used / (1024 * 1024)).min(u32::MAX as u64) as u32,
            disk_kbps: kb(s.disk_read + s.disk_write),
            net_kbps: kb(s.net_up + s.net_down),
            fan_rpm: s.gpu_fan_rpm.map_or(NONE16, |r| r.min(NONE16 as u32 - 1) as u16),
            cpu: pct(s.cpu),
            ram: pct(s.ram_pct),
            gpu: s.gpu.map_or(NONE8, pct),
            gpu_temp: s.gpu_temp.filter(|v| v.is_finite()).map_or(NONE8, |v| v.round().clamp(0.0, 254.0) as u8),
            disk: s.disk_active.map_or(NONE8, pct),
            ..Tick::default()
        };
        if c.input_idle_ms > IDLE_MS {
            t.flags |= flag::IDLE;
        }
        if c.display_on {
            t.flags |= flag::DISPLAY;
        }
        if elapsed_ms > GAP_MS || wall_gap_s as f64 * 1000.0 > GAP_MS {
            t.flags |= flag::GAP;
        }
        if c.procs_sampled && self.prev_procs {
            t.flags |= flag::PROCS;
            for (slot, p) in t.top.iter_mut().zip(&s.top_cpu) {
                let v = pct(p.cpu);
                *slot = (self.names.intern(&p.name, t_s), if v == 0 && p.cpu > 0.0 { 1 } else { v });
            }
            if let Some(p) = s.top_io.first() {
                t.io = self.names.intern(&p.name, t_s);
                t.io_kbps = kb(p.io_bps);
            }
        }
        self.prev_procs = c.procs_sampled;
        self.push_tick(t);
    }

    /// Append a ready-made tick (also used by tests).
    pub fn push_tick(&mut self, t: Tick) {
        if self.buf.capacity() == 0 {
            self.buf.reserve_exact(CAP);
        }
        if self.buf.len() < CAP {
            self.buf.push(t);
        } else {
            self.buf[self.head] = t;
            self.head = (self.head + 1) % CAP;
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Newest tick.
    pub fn last(&self) -> Option<&Tick> {
        if self.buf.len() < CAP {
            self.buf.last()
        } else {
            self.buf.get((self.head + CAP - 1) % CAP)
        }
    }

    /// The tick before the newest one.
    pub fn prev(&self) -> Option<&Tick> {
        self.iter().rev().nth(1)
    }

    /// All ticks, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Tick> + '_ {
        let (a, b) = if self.buf.len() < CAP {
            (&self.buf[..], &[][..])
        } else {
            (&self.buf[self.head..], &self.buf[..self.head])
        };
        a.iter().chain(b.iter())
    }

    /// The last `n` ticks, oldest first.
    pub fn last_n(&self, n: usize) -> impl Iterator<Item = &Tick> + '_ {
        self.iter().skip(self.len().saturating_sub(n))
    }

    /// Ticks with `from_ms <= t <= to_ms`, oldest first.
    pub fn span(&self, from_ms: u64, to_ms: u64) -> impl Iterator<Item = &Tick> + '_ {
        self.iter().filter(move |t| t.t_ms() >= from_ms / 1000 * 1000 && t.t_ms() <= to_ms)
    }

    /// Ticks at or after `since_ms`, oldest first.
    #[allow(dead_code)]
    pub fn since(&self, since_ms: u64) -> impl Iterator<Item = &Tick> + '_ {
        self.span(since_ms, u64::MAX)
    }

    /// The tick closest to `t_ms`, if one is within `within_ms`.
    pub fn at(&self, t_ms: u64, within_ms: u64) -> Option<&Tick> {
        self.iter()
            .rev()
            .map(|t| (t.t_ms().abs_diff(t_ms), t))
            .filter(|(d, _)| *d <= within_ms)
            .min_by_key(|(d, _)| *d)
            .map(|(_, t)| t)
    }

    /// Field values of the last `n` ticks, oldest first (`NaN` where the tick has no data).
    pub fn series(&self, field: Field, n: usize) -> Vec<f32> {
        self.last_n(n).map(|t| field.of(t).unwrap_or(f32::NAN)).collect()
    }

    /// Interned process name.
    pub fn name(&self, i: u8) -> Option<&str> {
        if i == NONE8 {
            None
        } else {
            self.names.get(i)
        }
    }

    /// Top CPU processes over a span: (name, average CPU % over the ticks that had
    /// process data), highest first, at most `k`. Not for the per-tick path.
    pub fn top_cpu(&self, from_ms: u64, to_ms: u64, k: usize) -> Vec<(String, f32)> {
        let mut sum = [0u32; 256];
        let mut n = 0u32;
        for t in self.span(from_ms, to_ms).filter(|t| t.has(flag::PROCS)) {
            n += 1;
            for &(i, v) in &t.top {
                if i != NONE8 {
                    sum[i as usize] += v as u32;
                }
            }
        }
        self.rank(&sum, n, k)
    }

    /// Top I/O process over a span: (name, average KB/s over the ticks with process data).
    pub fn top_io(&self, from_ms: u64, to_ms: u64) -> Option<(String, f32)> {
        let mut sum = [0u32; 256];
        let mut n = 0u32;
        for t in self.span(from_ms, to_ms).filter(|t| t.has(flag::PROCS)) {
            n += 1;
            if t.io != NONE8 {
                sum[t.io as usize] = sum[t.io as usize].saturating_add(t.io_kbps.max(1));
            }
        }
        self.rank(&sum, n, 1).into_iter().next()
    }

    fn rank(&self, sum: &[u32; 256], n: u32, k: usize) -> Vec<(String, f32)> {
        if n == 0 {
            return Vec::new();
        }
        let mut v: Vec<(u8, u32)> = sum.iter().enumerate().filter(|(_, &s)| s > 0).map(|(i, &s)| (i as u8, s)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.into_iter()
            .take(k)
            .filter_map(|(i, s)| self.name(i).map(|nm| (nm.to_string(), s as f32 / n as f32)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::{ProcStat, Snapshot};

    fn ctx_push(r: &mut Ring, t_s: u64, cpu: f32, procs: &[(&str, f32)]) {
        let snap = Snapshot {
            cpu,
            ram_pct: 40.0,
            elapsed: 1.0,
            top_cpu: procs.iter().map(|(n, c)| ProcStat { name: n.to_string(), cpu: *c, ..Default::default() }).collect(),
            ..Default::default()
        };
        let c = Ctx {
            now_ms: t_s * 1000,
            snap: &snap,
            display_on: true,
            input_idle_ms: 0,
            interval_ms: 1000,
            procs_sampled: !procs.is_empty(),
        };
        r.push(&c);
    }

    #[test]
    fn wraps_and_keeps_order() {
        let mut r = Ring::new();
        for i in 0..(CAP as u32 + 10) {
            r.push_tick(Tick { t_s: i, cpu: (i % 100) as u8, ..Tick::default() });
        }
        assert_eq!(r.len(), CAP);
        assert_eq!(r.last().map(|t| t.t_s), Some(CAP as u32 + 9));
        assert_eq!(r.iter().next().map(|t| t.t_s), Some(10));
        assert_eq!(r.prev().map(|t| t.t_s), Some(CAP as u32 + 8));
        let s = r.series(Field::Cpu, 3);
        assert_eq!(s, vec![((CAP + 7) % 100) as f32, ((CAP + 8) % 100) as f32, ((CAP + 9) % 100) as f32]);
        assert_eq!(r.since((CAP as u64 + 5) * 1000).count(), 5);
        assert_eq!(r.at((CAP as u64 + 3) * 1000 + 400, 1000).map(|t| t.t_s), Some(CAP as u32 + 3));
        assert!(r.at(1, 1000).is_none());
    }

    #[test]
    fn missing_values_are_nan() {
        let mut r = Ring::new();
        r.push_tick(Tick::default());
        assert!(r.series(Field::Gpu, 1)[0].is_nan());
        assert_eq!(Field::Fan.of(&Tick::default()), None);
    }

    #[test]
    fn culprit_average_skips_first_procs_tick() {
        let mut r = Ring::new();
        ctx_push(&mut r, 100, 90.0, &[("MsMpEng", 99.0)]); // first procs tick: no rates yet
        for i in 1..=4 {
            ctx_push(&mut r, 100 + i, 90.0, &[("MsMpEng", 40.0), ("svchost", 10.0)]);
        }
        ctx_push(&mut r, 105, 90.0, &[("svchost", 50.0)]);
        let top = r.top_cpu(100_000, 105_000, 2);
        assert_eq!(top[0].0, "MsMpEng");
        assert!((top[0].1 - 32.0).abs() < 0.01); // 40*4 / 5
        assert_eq!(top[1].0, "svchost");
        assert!((top[1].1 - 18.0).abs() < 0.01); // (10*4 + 50) / 5
        assert!(r.top_cpu(0, 99_000, 3).is_empty());
    }

    #[test]
    fn interval_change_and_wall_gap() {
        let mut r = Ring::new();
        let push = |r: &mut Ring, t_s: u64, elapsed: f64, interval_ms: u32| {
            let snap = Snapshot { elapsed, ..Default::default() };
            let c = Ctx { now_ms: t_s * 1000, snap: &snap, display_on: true, input_idle_ms: 0, interval_ms, procs_sampled: false };
            r.push(&c);
        };
        push(&mut r, 100, 5.0, 5000);
        // Interval just went 5 s -> 1 s: the 5 s gap was the old interval, not a freeze.
        push(&mut r, 105, 5.0, 1000);
        assert_eq!(r.last().map(|t| t.late_ms), Some(0));
        push(&mut r, 108, 3.0, 1000);
        assert_eq!(r.last().map(|t| t.late_ms), Some(2000));
        // A long wall-clock pause the sample clock did not see still marks a gap.
        push(&mut r, 400, 1.0, 1000);
        assert!(r.last().is_some_and(|t| t.has(flag::GAP)));
    }

    #[test]
    fn name_table_reuses_oldest() {
        let mut n = Names::default();
        for i in 0..MAX_NAMES {
            assert_eq!(n.intern(&format!("p{}", i), i as u32 + 10) as usize, i);
        }
        assert_eq!(n.intern("p0", 1000), 0); // refreshes p0
        let i = n.intern("new", 1001); // evicts p1 (oldest)
        assert_eq!(i, 1);
        assert_eq!(n.get(1), Some("new"));
        assert_eq!(n.intern("p1", 1002), 2); // p1 is gone, p2 is now the oldest
        assert_eq!(n.intern("", 5), NONE8);
    }
}
