//! Measured refresh rate of one screen: times its vertical blanks
//! (`D3DKMTWaitForVerticalBlankEvent`) on a thread of its own while that display's page is
//! open. Shows what the panel really runs at, which is not always the mode Windows reports
//! (VRR, a cable that fell back, a game that changed the mode). The thread blocks in the
//! wait, so it costs no CPU; it parks on a condition variable while no page needs it.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;
use windows_sys::Wdk::Graphics::Direct3D::*;
use windows_sys::Win32::Graphics::Gdi::{CreateDCW, DeleteDC};

/// Vblanks per measurement (about half a second at 120 Hz).
const FRAMES: u32 = 60;

#[derive(Default)]
struct State {
    /// GDI device name ("\\.\DISPLAY1") of the screen to time; None = park.
    target: Option<String>,
}

struct Shared {
    st: Mutex<State>,
    cv: Condvar,
    /// f32 bits of the last result for `target` (0 = none yet).
    hz: AtomicU32,
}

#[derive(Default)]
pub struct Meter {
    s: Option<Arc<Shared>>,
    cur: Option<String>,
}

impl Meter {
    /// Time this screen (None = stop). Cheap to call every second.
    pub fn set(&mut self, gdi: Option<&str>) {
        if self.cur.as_deref() == gdi {
            return;
        }
        if gdi.is_none() && self.s.is_none() {
            return;
        }
        self.cur = gdi.map(str::to_string);
        let s = self.s.get_or_insert_with(spawn);
        s.hz.store(0, Relaxed);
        s.st.lock().unwrap_or_else(|e| e.into_inner()).target = self.cur.clone();
        s.cv.notify_one();
    }

    /// Last measured refresh, Hz (0 = not measured yet).
    pub fn hz(&self) -> f32 {
        self.s.as_ref().map_or(0.0, |s| f32::from_bits(s.hz.load(Relaxed)))
    }
}

fn spawn() -> Arc<Shared> {
    let s = Arc::new(Shared { st: Mutex::default(), cv: Condvar::new(), hz: AtomicU32::new(0) });
    let t = s.clone();
    let _ = std::thread::Builder::new().name("refresh-meter".into()).spawn(move || run(&t));
    s
}

struct Source {
    adapter: u32,
    source: u32,
}

impl Source {
    fn open(gdi: &str) -> Option<Source> {
        let name = crate::util::wide(gdi);
        // SAFETY: NUL-terminated device name; the DC is deleted right after use.
        unsafe {
            let dc = CreateDCW(std::ptr::null(), name.as_ptr(), std::ptr::null(), std::ptr::null());
            if dc.is_null() {
                return None;
            }
            let mut o: D3DKMT_OPENADAPTERFROMHDC = std::mem::zeroed();
            o.hDc = dc;
            let st = D3DKMTOpenAdapterFromHdc(&mut o);
            DeleteDC(dc);
            (st >= 0).then_some(Source { adapter: o.hAdapter, source: o.VidPnSourceId })
        }
    }

    fn wait(&self) -> bool {
        let w = D3DKMT_WAITFORVERTICALBLANKEVENT { hAdapter: self.adapter, hDevice: 0, VidPnSourceId: self.source };
        // SAFETY: adapter handle we opened.
        unsafe { D3DKMTWaitForVerticalBlankEvent(&w) >= 0 }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let c = D3DKMT_CLOSEADAPTER { hAdapter: self.adapter };
        // SAFETY: closing our own handle.
        unsafe { D3DKMTCloseAdapter(&c) };
    }
}

fn run(s: &Shared) {
    let mut open: Option<(String, Source)> = None;
    loop {
        let target = {
            let mut st = s.st.lock().unwrap_or_else(|e| e.into_inner());
            while st.target.is_none() {
                open = None;
                st = s.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            st.target.clone().unwrap_or_default()
        };
        if open.as_ref().is_none_or(|o| o.0 != target) {
            open = Source::open(&target).map(|src| (target.clone(), src));
        }
        let Some((_, src)) = &open else {
            // No vblank for this output (remote session, unplugged): retry slowly.
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        };
        if !src.wait() {
            open = None;
            continue;
        }
        let t0 = Instant::now();
        let mut n = 0;
        while n < FRAMES && src.wait() {
            n += 1;
        }
        let secs = t0.elapsed().as_secs_f64();
        let still = s.st.lock().unwrap_or_else(|e| e.into_inner()).target.as_deref() == Some(target.as_str());
        if n == FRAMES && secs > 0.0 && still {
            s.hz.store(((f64::from(n) / secs) as f32).to_bits(), Relaxed);
        }
    }
}

/// The measured rate, snapped to the nearest 0.1 Hz for display.
pub fn tidy(hz: f32) -> f32 {
    (hz * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    #[test]
    fn tidy_rounds() {
        assert_eq!(super::tidy(143.96), 144.0);
        assert_eq!(super::tidy(59.94), 59.9);
    }
}
