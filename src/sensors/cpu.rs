//! Total CPU utilisation: PDH "% Processor Utility" (matches Task Manager on Win10 1709+/Win11),
//! falling back to `GetSystemTimes` deltas.

use super::pdh::{Counter, Query};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::GetSystemTimes;

#[derive(Default)]
pub struct Cpu {
    counter: Option<Counter>,
    attached: bool,
    /// (idle, kernel + user) from the previous `GetSystemTimes`.
    prev: Option<(u64, u64)>,
}

fn ticks(ft: &FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

impl Cpu {
    pub fn attach(&mut self, q: &Query) {
        if !self.attached {
            self.attached = true;
            self.counter = q.add(r"\Processor Information(_Total)\% Processor Utility");
        }
    }

    pub fn sample(&mut self) -> f32 {
        let fallback = self.system_times_pct();
        let pct = match self.counter.as_ref().and_then(Counter::value) {
            Some(v) => v as f32,
            None => fallback,
        };
        if pct.is_finite() { pct.clamp(0.0, 100.0) } else { 0.0 }
    }

    fn system_times_pct(&mut self) -> f32 {
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut idle, mut kernel, mut user) = (zero, zero, zero);
        // SAFETY: valid out pointers.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return 0.0;
        }
        // Kernel time already includes idle time.
        let now = (ticks(&idle), ticks(&kernel) + ticks(&user));
        let pct = match self.prev {
            Some((pi, pt)) if now.1 > pt => {
                let total = (now.1 - pt) as f64;
                let idle = now.0.saturating_sub(pi) as f64;
                ((total - idle) / total * 100.0) as f32
            }
            _ => 0.0,
        };
        self.prev = Some(now);
        pct
    }
}
