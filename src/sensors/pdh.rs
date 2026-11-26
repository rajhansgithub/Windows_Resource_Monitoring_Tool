//! Minimal safe wrapper over the PDH (Performance Data Helper) API.
//!
//! One `Query` owns the PDH query handle; `Counter`s are added lazily and
//! removed on drop. All counter paths are added with the English API so they
//! work on any Windows display language.

use std::ptr::{null, null_mut};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PdhRemoveCounter, PDH_CSTATUS_NEW_DATA,
    PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_MORE_DATA,
};

/// `PDH_FMT_NOCAP100`: do not clamp percentage counters at 100 (not exported by windows-sys).
const PDH_FMT_NOCAP100: u32 = 0x8000;
const FORMAT: u32 = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn status_ok(cstatus: u32) -> bool {
    cstatus == PDH_CSTATUS_VALID_DATA || cstatus == PDH_CSTATUS_NEW_DATA
}

pub struct Query(isize);

impl Query {
    pub fn open() -> Option<Query> {
        let mut h = 0isize;
        // SAFETY: valid out pointer; null data source = real-time data.
        let r = unsafe { PdhOpenQueryW(null(), 0, &mut h) };
        (r == 0 && h != 0).then_some(Query(h))
    }

    /// Adds a counter by its English path. `None` if it does not exist on this system.
    pub fn add(&self, path: &str) -> Option<Counter> {
        let w = wide(path);
        let mut c = 0isize;
        // SAFETY: `w` is NUL-terminated and outlives the call.
        let r = unsafe { PdhAddEnglishCounterW(self.0, w.as_ptr(), 0, &mut c) };
        (r == 0 && c != 0).then_some(Counter(c))
    }

    /// Collects one sample for every counter in the query.
    pub fn collect(&self) -> bool {
        // SAFETY: plain handle call.
        unsafe { PdhCollectQueryData(self.0) == 0 }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: handle is owned by us and closed exactly once.
        unsafe { PdhCloseQuery(self.0) };
    }
}

pub struct Counter(isize);

impl Counter {
    /// Formatted value of a single-instance counter. `None` while no valid data yet
    /// (rate counters need two collections).
    pub fn value(&self) -> Option<f64> {
        // SAFETY: PDH_FMT_COUNTERVALUE is plain old data.
        let mut v: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
        // SAFETY: valid out pointers.
        let r = unsafe { PdhGetFormattedCounterValue(self.0, FORMAT, null_mut(), &mut v) };
        if r != 0 || !status_ok(v.CStatus) {
            return None;
        }
        // SAFETY: we asked for PDH_FMT_DOUBLE.
        let d = unsafe { v.Anonymous.doubleValue };
        d.is_finite().then_some(d)
    }

    /// Calls `f(instance_name_utf16, value)` for every valid instance of a wildcard counter.
    /// `buf` is a caller-owned scratch buffer reused between calls (grown on PDH_MORE_DATA).
    /// Returns false if the counter has no valid data yet.
    pub fn for_each(&self, buf: &mut Vec<u64>, mut f: impl FnMut(&[u16], f64)) -> bool {
        let mut size = (buf.len() * 8) as u32;
        let mut count = 0u32;
        let mut ok = false;
        // The instance list can change between the sizing and the data call, so retry.
        for _ in 0..4 {
            let ptr = if buf.is_empty() {
                null_mut()
            } else {
                buf.as_mut_ptr().cast()
            };
            // SAFETY: `ptr` is null with size 0 (sizing call) or points to `size` writable
            // bytes, 8-aligned (Vec<u64>).
            let r =
                unsafe { PdhGetFormattedCounterArrayW(self.0, FORMAT, &mut size, &mut count, ptr) };
            if r == PDH_MORE_DATA {
                let words = (size as usize).div_ceil(8) + 32;
                buf.resize(words.max(buf.len() + 1), 0);
                size = (buf.len() * 8) as u32;
                continue;
            }
            ok = r == 0;
            break;
        }
        if !ok || count == 0 {
            return false;
        }
        let items = buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        for i in 0..count as usize {
            // SAFETY: PDH wrote `count` items at the start of the buffer; the name
            // pointers point to NUL-terminated strings stored later in the same buffer.
            let item = unsafe { &*items.add(i) };
            if !status_ok(item.FmtValue.CStatus) || item.szName.is_null() {
                continue;
            }
            // SAFETY: as above; we asked for PDH_FMT_DOUBLE.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            if !value.is_finite() {
                continue;
            }
            // SAFETY: NUL-terminated string inside `buf`.
            let name = unsafe {
                let mut len = 0;
                while *item.szName.add(len) != 0 {
                    len += 1;
                }
                std::slice::from_raw_parts(item.szName, len)
            };
            f(name, value);
        }
        true
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        // SAFETY: handle is owned by us; failure (query already closed) is harmless.
        unsafe { PdhRemoveCounter(self.0) };
    }
}
