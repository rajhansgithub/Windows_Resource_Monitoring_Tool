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

// ---- startup timing guard -------------------------------------------------------------------

/// First data later than this after launch is a regression: logged to diagnostics.log
/// even with the probe off. CI fails the build on it (startup timing step).
pub const STARTUP_BUDGET_MS: u128 = 2500;
/// One sensor sample slower than this holds every number on screen: logged (once a minute).
pub const SLOW_SAMPLE_MS: u128 = 400;

/// Process start; `mark_start` sets it first thing in `main`.
pub fn started() -> Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    *T.get_or_init(Instant::now)
}

pub fn mark_start() {
    let _ = started();
}

/// Record one startup milestone ("first sample", "first Monitor detail"). Logged once
/// per name: always when over budget, else only with the probe on.
pub fn milestone(name: &'static str) {
    use std::sync::Mutex;
    static SEEN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if seen.contains(&name) {
            return;
        }
        seen.push(name);
    }
    let ms = started().elapsed().as_millis();
    if ms > STARTUP_BUDGET_MS {
        crate::util::diag(&format!("startup {name}: {ms} ms (slow, budget {STARTUP_BUDGET_MS} ms)"));
    } else if on() {
        crate::util::diag(&format!("startup {name}: {ms} ms"));
    }
}

/// Log a slow sensor sample, at most once a minute.
pub fn slow_sample(ms: u128, detail: bool) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if ms < SLOW_SAMPLE_MS {
        return;
    }
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|t| t.elapsed().as_secs() < 60) {
        return;
    }
    *last = Some(Instant::now());
    crate::util::diag(&format!("slow: sensor sample {ms} ms (detail {detail})"));
}

/// Time from a page opening to its first data. Over budget is always logged.
pub fn page_data(page: &str, ms: u128) {
    if ms > STARTUP_BUDGET_MS {
        crate::util::diag(&format!("open {page}: first data {ms} ms (slow, budget {STARTUP_BUDGET_MS} ms)"));
    } else if on() {
        crate::util::diag(&format!("open {page}: first data {ms} ms"));
    }
}
