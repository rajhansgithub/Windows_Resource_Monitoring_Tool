//! Per-process CPU / I/O / memory from a single `NtQuerySystemInformation(SystemProcessInformation)`
//! call. No `OpenProcess`, so it works without elevation for every process.

use super::{ProcStat, Snapshot};
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use windows_sys::Win32::System::Threading::GetActiveProcessorCount;

const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023u32 as i32;
const ALL_PROCESSOR_GROUPS: u16 = 0xFFFF;
const TOP: usize = 5;

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        len: u32,
        return_len: *mut u32,
    ) -> i32;
}

/// 64-bit layout of SYSTEM_PROCESS_INFORMATION (without the trailing thread array).
#[repr(C)]
struct ProcessInfo {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name_length: u16,
    image_name_max_length: u16,
    image_name_buffer: *const u16,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

// Verify the layout against the documented x64 offsets.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(offset_of!(ProcessInfo, working_set_private_size) == 8);
    assert!(offset_of!(ProcessInfo, cycle_time) == 24);
    assert!(offset_of!(ProcessInfo, create_time) == 32);
    assert!(offset_of!(ProcessInfo, user_time) == 40);
    assert!(offset_of!(ProcessInfo, kernel_time) == 48);
    assert!(offset_of!(ProcessInfo, image_name_length) == 56);
    assert!(offset_of!(ProcessInfo, image_name_buffer) == 64);
    assert!(offset_of!(ProcessInfo, unique_process_id) == 80);
    assert!(offset_of!(ProcessInfo, handle_count) == 96);
    assert!(offset_of!(ProcessInfo, virtual_size) == 120);
    assert!(offset_of!(ProcessInfo, working_set_size) == 144);
    assert!(offset_of!(ProcessInfo, private_page_count) == 200);
    assert!(offset_of!(ProcessInfo, read_operation_count) == 208);
    assert!(offset_of!(ProcessInfo, read_transfer_count) == 232);
    assert!(offset_of!(ProcessInfo, other_transfer_count) == 248);
    assert!(size_of::<ProcessInfo>() == 256);
};

struct Prev {
    create_time: i64,
    cpu_100ns: u64,
    io_bytes: u64,
    generation: u32,
}

/// One process of the current sample; `offset` locates its entry in the buffer.
struct Row {
    offset: usize,
    pid: u32,
    cpu: f32,
    io_bps: f64,
    mem: u64,
}

pub struct Procs {
    /// 8-byte aligned scratch buffer for the process list, reused between samples.
    buf: Vec<u64>,
    prev: HashMap<u32, Prev>,
    rows: Vec<Row>,
    generation: u32,
    cpus: f64,
}

impl Procs {
    pub fn new() -> Self {
        // SAFETY: plain query.
        let cpus = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1);
        Procs {
            buf: Vec::new(),
            prev: HashMap::new(),
            rows: Vec::new(),
            generation: 0,
            cpus: f64::from(cpus),
        }
    }

    /// Forget history (called while process monitoring is disabled so deltas never span the gap).
    pub fn reset(&mut self) {
        if !self.buf.is_empty() {
            self.prev = HashMap::new();
            self.rows = Vec::new();
            self.buf = Vec::new();
        }
    }

    /// Reads the process list into `self.buf`; returns the number of valid bytes.
    fn query(&mut self) -> Option<usize> {
        if self.buf.is_empty() {
            self.buf.resize(256 * 1024 / 8, 0);
        }
        for _ in 0..8 {
            let mut needed = 0u32;
            // SAFETY: the buffer is writable for `len` bytes.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION_CLASS,
                    self.buf.as_mut_ptr().cast(),
                    (self.buf.len() * 8) as u32,
                    &mut needed,
                )
            };
            if status >= 0 {
                let valid = (needed as usize).min(self.buf.len() * 8);
                return (valid >= size_of::<ProcessInfo>()).then_some(valid);
            }
            if status != STATUS_INFO_LENGTH_MISMATCH && status != STATUS_BUFFER_TOO_SMALL {
                return None;
            }
            // The list may grow before the next call, so leave some slack.
            let bytes = (needed as usize + 64 * 1024).max(self.buf.len() * 8 * 2);
            self.buf.resize(bytes.div_ceil(8), 0);
        }
        None
    }

    pub fn sample(&mut self, elapsed: f64, snap: &mut Snapshot) {
        snap.top_cpu.clear();
        snap.top_io.clear();
        snap.top_mem.clear();
        snap.proc_count = 0;

        let Some(valid) = self.query() else {
            self.prev.clear();
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let cpu_scale = if elapsed > 0.0 {
            100.0 / (elapsed * 1e7 * self.cpus)
        } else {
            0.0
        };
        let io_scale = if elapsed > 0.0 { 1.0 / elapsed } else { 0.0 };

        self.rows.clear();
        let base = self.buf.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            if offset + size_of::<ProcessInfo>() > valid {
                break;
            }
            // SAFETY: bounds checked above; entries are 8-byte aligned inside a u64 buffer.
            let p = unsafe { &*base.add(offset).cast::<ProcessInfo>() };
            let pid = p.unique_process_id as u32;
            if pid != 0 {
                snap.proc_count += 1;
                let cpu_100ns =
                    (p.user_time.max(0) as u64).wrapping_add(p.kernel_time.max(0) as u64);
                let io_bytes = (p.read_transfer_count.max(0) as u64)
                    .wrapping_add(p.write_transfer_count.max(0) as u64)
                    .wrapping_add(p.other_transfer_count.max(0) as u64);
                let (mut cpu, mut io_bps) = (0.0f32, 0.0f64);
                match self.prev.get_mut(&pid) {
                    // Same create time: same process instance (otherwise the pid was reused).
                    Some(prev) if prev.create_time == p.create_time => {
                        let d_cpu = cpu_100ns.saturating_sub(prev.cpu_100ns) as f64;
                        let d_io = io_bytes.saturating_sub(prev.io_bytes) as f64;
                        cpu = (d_cpu * cpu_scale).clamp(0.0, 100.0) as f32;
                        io_bps = d_io * io_scale;
                        prev.cpu_100ns = cpu_100ns;
                        prev.io_bytes = io_bytes;
                        prev.generation = generation;
                    }
                    _ => {
                        self.prev.insert(
                            pid,
                            Prev {
                                create_time: p.create_time,
                                cpu_100ns,
                                io_bytes,
                                generation,
                            },
                        );
                    }
                }
                let mem = if p.working_set_private_size > 0 {
                    p.working_set_private_size as u64
                } else {
                    p.working_set_size as u64
                };
                self.rows.push(Row {
                    offset,
                    pid,
                    cpu,
                    io_bps,
                    mem,
                });
            }
            if p.next_entry_offset == 0 {
                break;
            }
            offset += p.next_entry_offset as usize;
        }
        self.prev.retain(|_, p| p.generation == generation);

        let fill = |out: &mut Vec<ProcStat>, (idx, n): ([usize; TOP], usize)| {
            out.extend(idx[..n].iter().map(|&i| self.stat(&self.rows[i], valid)));
        };
        fill(&mut snap.top_cpu, top_n(&self.rows, |r| f64::from(r.cpu)));
        fill(&mut snap.top_io, top_n(&self.rows, |r| r.io_bps));
        fill(&mut snap.top_mem, top_n(&self.rows, |r| r.mem as f64));
    }

    fn stat(&self, row: &Row, valid: usize) -> ProcStat {
        ProcStat {
            pid: row.pid,
            name: self.image_name(row.offset, valid),
            cpu: row.cpu,
            io_bps: row.io_bps,
            mem: row.mem,
        }
    }

    /// Image name of the entry at `offset`, without ".exe".
    fn image_name(&self, offset: usize, valid: usize) -> String {
        // SAFETY: `offset` was validated while scanning.
        let p = unsafe {
            &*self
                .buf
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<ProcessInfo>()
        };
        let start = self.buf.as_ptr() as usize;
        let ptr = p.image_name_buffer as usize;
        let len = usize::from(p.image_name_length) / 2;
        // The string lives inside the same buffer; refuse anything pointing elsewhere.
        if len == 0 || ptr < start || ptr & 1 != 0 || ptr + len * 2 > start + valid {
            return String::new();
        }
        // SAFETY: range checked against the buffer above.
        let wide = unsafe { std::slice::from_raw_parts(ptr as *const u16, len) };
        let mut name = String::from_utf16_lossy(wide);
        let stem = name
            .len()
            .checked_sub(4)
            .filter(|&i| name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe"));
        if let Some(i) = stem {
            name.truncate(i);
        }
        name
    }
}

/// One-shot lookup for `sensors::find_process`: scans a fresh process list once.
pub fn find_pid(name: &str) -> Option<u32> {
    let mut p = Procs::new();
    let valid = p.query()?;
    let base = p.buf.as_ptr().cast::<u8>();
    let mut offset = 0usize;
    while offset + size_of::<ProcessInfo>() <= valid {
        // SAFETY: bounds checked by the loop condition; entries are 8-byte aligned in a u64 buffer.
        let e = unsafe { &*base.add(offset).cast::<ProcessInfo>() };
        let pid = e.unique_process_id as u32;
        if pid != 0 && p.image_name(offset, valid).eq_ignore_ascii_case(name) {
            return Some(pid);
        }
        if e.next_entry_offset == 0 {
            break;
        }
        offset += e.next_entry_offset as usize;
    }
    None
}

/// Indices (and their count) of the `TOP` rows with the largest positive key, descending.
fn top_n(rows: &[Row], key: impl Fn(&Row) -> f64) -> ([usize; TOP], usize) {
    let mut best: [(f64, usize); TOP] = [(0.0, 0); TOP];
    let mut n = 0;
    for (i, r) in rows.iter().enumerate() {
        let k = key(r);
        if k.is_nan() || k <= 0.0 || (n == TOP && k <= best[TOP - 1].0) {
            continue;
        }
        // Insertion into the small sorted array.
        let mut j = n.min(TOP - 1);
        while j > 0 && best[j - 1].0 < k {
            best[j] = best[j - 1];
            j -= 1;
        }
        best[j] = (k, i);
        n = (n + 1).min(TOP);
    }
    (best.map(|b| b.1), n)
}
