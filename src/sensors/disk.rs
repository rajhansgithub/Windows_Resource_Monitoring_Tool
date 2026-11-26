//! Disk throughput and active time via PDH "PhysicalDisk" counters.

use super::pdh::{Counter, Query};

#[derive(Default)]
pub struct Disk {
    read: Option<Counter>,
    write: Option<Counter>,
    idle: Option<Counter>,
    attached: bool,
    buf: Vec<u64>,
}

pub struct DiskSample {
    pub read: f64,
    pub write: f64,
    pub active: Option<f32>,
}

fn is_total(name: &[u16]) -> bool {
    name.iter().copied().eq("_Total".encode_utf16())
}

impl Disk {
    /// Adds the counters when disk monitoring turns on and drops them when it turns off.
    pub fn sync(&mut self, q: &Query, enabled: bool) {
        if !enabled {
            *self = Disk::default();
        } else if !self.attached {
            self.attached = true;
            self.read = q.add(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec");
            self.write = q.add(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec");
            self.idle = q.add(r"\PhysicalDisk(*)\% Idle Time");
        }
    }

    pub fn sample(&mut self) -> DiskSample {
        let rate =
            |c: &Option<Counter>| c.as_ref().and_then(Counter::value).unwrap_or(0.0).max(0.0);
        let active = self.idle.as_ref().and_then(|c| {
            let mut busiest: Option<f64> = None;
            let ok = c.for_each(&mut self.buf, |name, idle| {
                if !is_total(name) {
                    let active = 100.0 - idle;
                    busiest = Some(busiest.map_or(active, |b| b.max(active)));
                }
            });
            if ok {
                busiest.map(|b| b.clamp(0.0, 100.0) as f32)
            } else {
                None
            }
        });
        DiskSample {
            read: rate(&self.read),
            write: rate(&self.write),
            active,
        }
    }
}
