//! Display-synced frame clock for animations (smooth scroll, transitions).
//!
//! A `SetTimer` tick lands every 15.6 ms at best, drifts against the
//! display's refresh and is the lowest-priority message, so motion driven by it
//! judders. This thread instead waits for the vertical blank of the monitor the
//! window is on (`D3DKMTWaitForVerticalBlankEvent`, the call browsers use;
//! `DwmFlush` or a plain sleep if that fails) and posts one `WM_APP_FRAME`
//! per refresh to the window, never more than one in its queue.
//! While nothing animates the thread sleeps on a condition variable: no CPU.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::*};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Wdk::Graphics::Direct3D::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Posted to the window once per display refresh while running.
pub const WM_APP_FRAME: u32 = WM_APP + 40;

struct Shared {
    run: AtomicBool,
    /// A frame message is in the window's queue (not yet handled).
    queued: AtomicBool,
    hwnd: AtomicIsize,
    gate: Mutex<()>,
    wake: Condvar,
}

fn shared() -> &'static Arc<Shared> {
    static S: OnceLock<Arc<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        let s = Arc::new(Shared { run: AtomicBool::new(false), queued: AtomicBool::new(false), hwnd: AtomicIsize::new(0), gate: Mutex::new(()), wake: Condvar::new() });
        let t = s.clone();
        let _ = std::thread::Builder::new().name("vsync".into()).spawn(move || run(&t));
        s
    })
}

/// Start sending frames to `hwnd`.
pub fn start(hwnd: HWND) {
    let s = shared();
    s.hwnd.store(hwnd as isize, Relaxed);
    if !s.run.swap(true, AcqRel) {
        s.queued.store(false, Release);
        let _g = s.gate.lock();
        s.wake.notify_all();
    }
}

/// Stop sending frames (the thread goes back to sleep).
pub fn stop() {
    shared().run.store(false, Release);
}

/// The window handled a frame message: the next one may be posted.
pub fn ack() {
    shared().queued.store(false, Release);
}

/// The vertical blank of one monitor's video output.
struct Vblank {
    adapter: u32,
    source: u32,
    monitor: HMONITOR,
}

impl Vblank {
    fn open(monitor: HMONITOR) -> Option<Vblank> {
        unsafe {
            let mut mi: MONITORINFOEXW = std::mem::zeroed();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, &mut mi as *mut _ as *mut MONITORINFO) == 0 {
                return None;
            }
            let dc = CreateDCW(std::ptr::null(), mi.szDevice.as_ptr(), std::ptr::null(), std::ptr::null());
            if dc.is_null() {
                return None;
            }
            let mut o: D3DKMT_OPENADAPTERFROMHDC = std::mem::zeroed();
            o.hDc = dc;
            let st = D3DKMTOpenAdapterFromHdc(&mut o);
            DeleteDC(dc);
            (st >= 0).then_some(Vblank { adapter: o.hAdapter, source: o.VidPnSourceId, monitor })
        }
    }

    fn wait(&self) -> bool {
        let w = D3DKMT_WAITFORVERTICALBLANKEVENT { hAdapter: self.adapter, hDevice: 0, VidPnSourceId: self.source };
        unsafe { D3DKMTWaitForVerticalBlankEvent(&w) >= 0 }
    }
}

impl Drop for Vblank {
    fn drop(&mut self) {
        let c = D3DKMT_CLOSEADAPTER { hAdapter: self.adapter };
        unsafe { D3DKMTCloseAdapter(&c) };
    }
}

fn run(s: &Shared) {
    let mut vb: Option<Vblank> = None;
    let mut last = Instant::now();
    loop {
        if !s.run.load(Acquire) {
            let mut g = s.gate.lock().unwrap_or_else(|e| e.into_inner());
            while !s.run.load(Acquire) {
                g = s.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            drop(g);
        }
        let hwnd = s.hwnd.load(Relaxed) as HWND;
        // Follow the window to the monitor it is on (refresh rates differ).
        let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
        if vb.as_ref().is_none_or(|v| v.monitor != mon) {
            vb = Vblank::open(mon);
        }
        let ok = vb.as_ref().is_some_and(|v| v.wait());
        if !ok {
            vb = None;
            if unsafe { DwmFlush() } < 0 {
                std::thread::sleep(Duration::from_millis(8));
            }
        }
        // Never spin: a wait that returned at once (display off, odd driver) sleeps a little.
        let now = Instant::now();
        if now - last < Duration::from_millis(2) {
            std::thread::sleep(Duration::from_millis(4));
        }
        last = Instant::now();
        if s.run.load(Acquire) && !s.queued.swap(true, AcqRel) && unsafe { PostMessageW(hwnd, WM_APP_FRAME, 0, 0) } == 0 {
            s.queued.store(false, Release);
        }
    }
}
