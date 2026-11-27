//! Frame-time probe. Off unless `RESMON_PERF=1` is in the environment; then
//! each named span (paint, sample, layout, frame interval, ...) is timed and
//! every few seconds a summary line per span goes to diagnostics.log:
//! `perf paint: n=240 avg=3.1ms p50=2.9 p95=4.8 max=7.2`.
//! The CI frame-time benchmark reads those lines. Costs one load when off.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Instant;

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("RESMON_PERF").is_ok_and(|v| v != "0"))
}

struct Store {
    since: Instant,
    spans: Vec<(&'static str, Vec<f32>)>,
}

thread_local! {
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
}

/// Record `ms` under `name`.
pub fn record(name: &'static str, ms: f32) {
    if !on() {
        return;
    }
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.get_or_insert_with(|| Store { since: Instant::now(), spans: Vec::new() });
        match st.spans.iter_mut().find(|x| x.0 == name) {
            Some(x) => x.1.push(ms),
            None => st.spans.push((name, vec![ms])),
        }
        if st.since.elapsed().as_secs_f32() >= 4.0 {
            for (n, v) in st.spans.iter_mut() {
                if v.is_empty() {
                    continue;
                }
                crate::util::diag(&summary(n, v));
                v.clear();
            }
            st.since = Instant::now();
        }
    });
}

fn summary(name: &str, v: &mut [f32]) -> String {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let avg = v.iter().sum::<f32>() / n as f32;
    let q = |p: f32| v[((n as f32 - 1.0) * p).round() as usize];
    format!("perf {name}: n={n} avg={avg:.2}ms p50={:.2} p95={:.2} max={:.2}", q(0.5), q(0.95), v[n - 1])
}

/// Times its own life; records on drop.
pub struct Span(&'static str, Instant);

impl Drop for Span {
    fn drop(&mut self) {
        record(self.0, self.1.elapsed().as_secs_f32() * 1000.0);
    }
}

pub fn span(name: &'static str) -> Option<Span> {
    on().then(|| Span(name, Instant::now()))
}
