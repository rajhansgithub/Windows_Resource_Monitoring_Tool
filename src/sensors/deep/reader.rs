//! Reads `DeepStatic` (topology, power mode, GPUs, screens, drive health, adapters) on a
//! thread of its own: DXGI, SetupAPI and the drive IOCTLs can take tens of milliseconds,
//! and the sensor thread must stay on its one-second beat. Asked once when a device page
//! opens and again every minute while one stays open.

use super::{cpu, disk, display, gpu, net, DeepStatic, DiskDeep};
use std::sync::{Arc, Condvar, Mutex};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Default)]
struct State {
    job: Option<Vec<u32>>,
    done: Option<Arc<DeepStatic>>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// The background reader. Cheap until the first `request`.
#[derive(Default)]
pub struct Reader {
    s: Option<Shared>,
    latest: Arc<DeepStatic>,
}

impl Reader {
    /// Ask for a fresh read of everything, with these PhysicalDrive numbers. Never blocks;
    /// a request made while one is being read replaces the queued one.
    pub fn request(&mut self, disks: &[u32]) {
        let s = self.s.get_or_insert_with(spawn);
        let (m, cv) = &**s;
        m.lock().unwrap_or_else(|e| e.into_inner()).job = Some(disks.to_vec());
        cv.notify_one();
    }

    /// The newest finished read (empty until the first one lands).
    pub fn latest(&mut self) -> Arc<DeepStatic> {
        if let Some(s) = &self.s {
            if let Some(d) = s.0.lock().unwrap_or_else(|e| e.into_inner()).done.take() {
                self.latest = d;
            }
        }
        self.latest.clone()
    }
}

fn spawn() -> Shared {
    let s: Shared = Arc::default();
    let t = s.clone();
    let _ = std::thread::Builder::new().name("deep-reader".into()).spawn(move || {
        let (m, cv) = &*t;
        loop {
            let disks = {
                let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(j) = st.job.take() {
                        break j;
                    }
                    st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
                }
            };
            let p = crate::perf::span("deep-static");
            let d = read_all(&disks);
            drop(p);
            m.lock().unwrap_or_else(|e| e.into_inner()).done = Some(Arc::new(d));
        }
    });
    s
}

fn read_all(disks: &[u32]) -> DeepStatic {
    let admin = elevated();
    let disks = disks
        .iter()
        .map(|&num| {
            let health = disk::nvme_health(num).or_else(|| if admin { disk::smart_sata(num) } else { None });
            DiskDeep { num, health, extra: disk::extra(num) }
        })
        .collect();
    DeepStatic {
        topo: Some(cpu::topology()),
        power: cpu::power_mode(),
        gpus: gpu::adapters(),
        screens: display::screens(),
        disks,
        adapters: net::adapters(),
        admin,
    }
}

/// The process runs elevated (UAC admin token).
pub fn elevated() -> bool {
    let mut tok: HANDLE = std::ptr::null_mut();
    // SAFETY: current-process pseudo handle; out pointer is valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
        return false;
    }
    let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    let sz = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
    // SAFETY: plain-old-data out struct of `sz` bytes.
    let ok = unsafe { GetTokenInformation(tok, TokenElevation, (&mut e as *mut TOKEN_ELEVATION).cast(), sz, &mut len) } != 0;
    // SAFETY: we own the token handle.
    unsafe { CloseHandle(tok) };
    ok && e.TokenIsElevated != 0
}
