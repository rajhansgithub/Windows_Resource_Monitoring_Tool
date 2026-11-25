//! CPU performance ceiling ("am I throttled?") from PDH counters on the shared query:
//! `\Processor Information(_Total)\% Performance Limit`, `% Processor Performance`,
//! `Performance Limit Flags` and `\Thermal Zone Information(*)\% Passive Limit`.
//! Attached once; any counter Windows or the firmware does not provide stays `None`.

use super::pdh::{Counter, Query};
use super::Snapshot;

#[derive(Default)]
pub struct Perf {
    attached: bool,
    limit: Option<Counter>,
    perf: Option<Counter>,
    flags: Option<Counter>,
    passive: Option<Counter>,
    /// Scratch buffer for the thermal-zone wildcard counter, reused between samples.
    buf: Vec<u64>,
}

impl Perf {
    pub fn attach(&mut self, q: &Query) {
        if !self.attached {
            self.attached = true;
            self.limit = q.add(r"\Processor Information(_Total)\% Performance Limit");
            self.perf = q.add(r"\Processor Information(_Total)\% Processor Performance");
            self.flags = q.add(r"\Processor Information(_Total)\Performance Limit Flags");
            self.passive = q.add(r"\Thermal Zone Information(*)\% Passive Limit");
        }
    }

    /// Fills the `cpu_perf_*` and `thermal_passive_limit` fields after `Query::collect`.
    pub fn sample(&mut self, s: &mut Snapshot) {
        let val = |c: &Option<Counter>| c.as_ref().and_then(Counter::value);
        s.cpu_perf_limit = val(&self.limit).map(|v| v.clamp(0.0, 100.0) as f32);
        s.cpu_perf_pct = val(&self.perf).map(|v| v.max(0.0) as f32);
        s.cpu_perf_flags = val(&self.flags).map(|v| v.max(0.0) as u32);
        s.thermal_passive_limit = None;
        if let Some(c) = &self.passive {
            let mut min: Option<f64> = None;
            c.for_each(&mut self.buf, |_, v| {
                // A zone without passive cooling reports 100; some firmware reports 0 for
                // "not supported". A real 0% cap could not run any code, so 0 is ignored.
                if v > 0.0 {
                    min = Some(min.map_or(v, |m| m.min(v)));
                }
            });
            s.thermal_passive_limit = min.map(|v| v.min(100.0) as f32);
        }
    }
}
