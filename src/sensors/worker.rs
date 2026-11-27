//! Sensor reads on their own thread. A PDH collection (GPU engine counters
//! above all), the process list and the Monitor page's detail can take tens of
//! milliseconds; on the UI thread that stalls a scroll or an animation once a
//! second. The UI asks for a sample, this thread takes it and posts
//! `WM_APP_SAMPLED` back with the result.

use super::detail::Detail;
use super::{SampleOpts, Sensors, Snapshot};
use std::sync::{Arc, Condvar, Mutex};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the app window when a sample is ready (`Worker::take`).
pub const WM_APP_SAMPLED: u32 = WM_APP + 41;

pub struct Sample {
    pub opts: SampleOpts,
    pub snap: Snapshot,
    /// The Monitor page's detail, when `opts.detail`.
    pub detail: Option<Detail>,
    /// Only primes rates (the popup is about to open): not a tick.
    pub baseline: bool,
}

#[derive(Default)]
struct State {
    job: Option<(SampleOpts, bool)>,
    busy: bool,
    done: Option<Sample>,
}

pub struct Worker {
    s: Arc<(Mutex<State>, Condvar)>,
}

impl Worker {
    /// Start the thread; results are posted to `hwnd`.
    pub fn new(hwnd: HWND) -> Worker {
        let s: Arc<(Mutex<State>, Condvar)> = Arc::default();
        let t = s.clone();
        let target = hwnd as isize;
        let _ = std::thread::Builder::new().name("sensors".into()).spawn(move || {
            let mut sensors = Sensors::new();
            let (m, cv) = &*t;
            loop {
                let (opts, baseline) = {
                    let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if let Some(j) = st.job.take() {
                            st.busy = true;
                            break j;
                        }
                        st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let p = crate::perf::span("sample");
                let snap = sensors.sample(&opts).clone();
                let detail = opts.detail.then(|| sensors.detail().clone());
                drop(p);
                {
                    let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
                    st.busy = false;
                    // A baseline never replaces a real tick still waiting.
                    if !(baseline && st.done.as_ref().is_some_and(|d| !d.baseline)) {
                        st.done = Some(Sample { opts, snap, detail, baseline });
                    }
                }
                unsafe { PostMessageW(target as HWND, WM_APP_SAMPLED, 0, 0) };
            }
        });
        Worker { s }
    }

    /// Ask for a sample. Ignored while one is being taken (the next tick asks again).
    pub fn request(&self, opts: SampleOpts, baseline: bool) {
        let (m, cv) = &*self.s;
        let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
        if st.busy || st.job.is_some() {
            return;
        }
        st.job = Some((opts, baseline));
        cv.notify_one();
    }

    /// The sample the last `WM_APP_SAMPLED` announced.
    pub fn take(&self) -> Option<Sample> {
        self.s.0.lock().unwrap_or_else(|e| e.into_inner()).done.take()
    }
}

/// Network adapters present now (no sensor state needed).
pub fn adapters() -> Vec<super::AdapterInfo> {
    super::net::adapters()
}
