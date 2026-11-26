//! Physical memory usage.

use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// Returns `(used, total)` in bytes; `(0, 0)` on failure.
pub fn usage() -> (u64, u64) {
    // SAFETY: MEMORYSTATUSEX is plain old data; dwLength must be set before the call.
    let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut st) } == 0 {
        return (0, 0);
    }
    (
        st.ullTotalPhys.saturating_sub(st.ullAvailPhys),
        st.ullTotalPhys,
    )
}
