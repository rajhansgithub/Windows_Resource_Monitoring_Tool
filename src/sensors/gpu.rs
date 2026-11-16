//! GPU utilisation and dedicated VRAM via PDH "GPU Engine" / "GPU Adapter Memory" counters.

use super::pdh::{Counter, Query};

const UTIL_PATH: &str = r"\GPU Engine(*)\Utilization Percentage";
const VRAM_PATH: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";

#[derive(Default)]
pub struct Gpu {
    util: Option<Counter>,
    vram: Option<Counter>,
    /// Counters have been looked up for the current "enabled" period.
    attached: bool,
    buf: Vec<u64>,
    /// (engine key hash, summed utilisation over all processes).
    engines: Vec<(u64, f64)>,
}

/// Instance names look like `pid_1234_luid_0x00000000_0x0000C3A1_phys_0_eng_0_engtype_3D`.
/// The part from `luid_` on identifies one physical engine, independent of the process.
fn engine_key(name: &[u16]) -> u64 {
    const MARK: &[u8] = b"luid_";
    let start = name
        .windows(MARK.len())
        .position(|w| w.iter().zip(MARK).all(|(&a, &b)| a == u16::from(b)))
        .unwrap_or(0);
    // FNV-1a over the UTF-16 code units.
    name[start..].iter().fold(0xcbf29ce484222325u64, |h, &c| {
        (h ^ u64::from(c)).wrapping_mul(0x100000001b3)
    })
}

impl Gpu {
    /// Adds the counters when GPU monitoring turns on and drops them (so PDH stops
    /// collecting them) when it turns off.
    pub fn sync(&mut self, q: &Query, enabled: bool) {
        if !enabled {
            *self = Gpu::default();
        } else if !self.attached {
            self.attached = true;
            self.util = q.add(UTIL_PATH);
            self.vram = q.add(VRAM_PATH);
        }
    }

    /// `(busiest engine %, busiest adapter dedicated VRAM bytes)`.
    pub fn sample(&mut self) -> (Option<f32>, Option<u64>) {
        let util = match &self.util {
            Some(c) => {
                self.engines.clear();
                let engines = &mut self.engines;
                let ok = c.for_each(&mut self.buf, |name, v| {
                    let key = engine_key(name);
                    match engines.iter_mut().find(|e| e.0 == key) {
                        Some(e) => e.1 += v,
                        None => engines.push((key, v)),
                    }
                });
                ok.then(|| {
                    let max = self.engines.iter().fold(0.0f64, |m, e| m.max(e.1));
                    max.clamp(0.0, 100.0) as f32
                })
            }
            None => None,
        };
        let vram = self.vram.as_ref().and_then(|c| {
            let mut max: Option<f64> = None;
            let ok = c.for_each(&mut self.buf, |_, v| {
                max = Some(max.map_or(v, |m| m.max(v)));
            });
            if ok { max.map(|m| m.max(0.0) as u64) } else { None }
        });
        (util, vram)
    }
}
