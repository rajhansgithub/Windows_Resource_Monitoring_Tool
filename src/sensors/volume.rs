//! System drive capacity via GetDiskFreeSpaceExW, refreshed at most every 30 seconds.

use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

const REFRESH_SECS: f64 = 30.0;

#[derive(Default)]
pub struct SystemDrive {
    /// Seconds since the last read attempt; None = never read.
    age: Option<f64>,
    used: u64,
    total: u64,
}

impl SystemDrive {
    /// Re-reads when due. Returns (used, total, used %) from the cached values.
    pub fn sample(&mut self, elapsed: f64) -> (u64, u64, Option<f32>) {
        let due = self.age.map_or(true, |a| a + elapsed >= REFRESH_SECS);
        if due {
            self.age = Some(0.0);
            if let Some((total, free)) = query() {
                self.total = total;
                self.used = total.saturating_sub(free);
            }
        } else if let Some(a) = &mut self.age {
            *a += elapsed;
        }
        let pct = (self.total > 0)
            .then(|| (self.used as f64 / self.total as f64 * 100.0).clamp(0.0, 100.0) as f32);
        (self.used, self.total, pct)
    }
}

fn query() -> Option<(u64, u64)> {
    let mut root: Vec<u16> = Vec::with_capacity(8);
    if let Ok(d) = std::env::var("SystemDrive") {
        root.extend(d.trim_end_matches(['\\', '/']).encode_utf16());
        root.push(b'\\' as u16);
    }
    if root.len() < 3 {
        root.clear();
        root.extend(r"C:\".encode_utf16());
    }
    root.push(0);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: root is NUL-terminated; out pointers are valid.
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0 && total > 0).then_some((total, free))
}
