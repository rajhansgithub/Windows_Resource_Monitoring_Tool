//! Extra detail for the Monitor page: per-core CPU, memory lists, commit, GPU engines and
//! shared memory, every physical disk, every fixed volume, every adapter and the full
//! process list. Attached to the shared PDH query only while the page is on screen
//! (`SampleOpts::detail`), and dropped again when it closes. No admin, no driver.

use super::pdh::{Counter, Query};
use crate::util::wide;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformation, RelationCache, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION,
};
use windows_sys::Win32::System::Threading::GetActiveProcessorCount;

/// Re-read volumes and adapter addresses this often.
const SLOW_SECS: f64 = 10.0;
const DRIVE_FIXED: u32 = 3;

#[derive(Clone, Debug, Default)]
pub struct DiskStat {
    /// "Disk 0".
    pub name: String,
    /// Volumes on the disk ("C: E:"), may be empty.
    pub letters: String,
    pub read: f64,
    pub write: f64,
    /// Active time 0..100.
    pub active: f32,
    /// Average response time, milliseconds.
    pub latency_ms: f32,
    pub queue: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Volume {
    /// "C:".
    pub letter: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

#[derive(Clone, Debug, Default)]
pub struct NetStat {
    pub alias: String,
    pub description: String,
    pub connected: bool,
    /// Wireless (802.11) adapter.
    pub wifi: bool,
    pub down: f64,
    pub up: f64,
    /// Receive link speed, bits/s (0 = unknown).
    pub link_bps: u64,
    /// First IPv4 address, empty when none.
    pub ipv4: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProcRow {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
    pub io: f64,
    /// Busiest GPU engine this process uses, 0..100.
    pub gpu: f32,
    pub threads: u32,
}

/// Everything the Monitor page shows beyond `Snapshot`.
#[derive(Clone, Debug, Default)]
pub struct Detail {
    // ---- CPU (static)
    pub cpu_name: String,
    pub cores: u32,
    pub logical: u32,
    /// Largest L3 cache, bytes (0 = unknown).
    pub l3: u64,
    /// Nominal clock, MHz (0 = unknown).
    pub base_mhz: f32,
    // ---- CPU (live)
    /// Per logical processor utility, 0..100, in processor order.
    pub core_pct: Vec<f32>,
    /// Kernel (privileged) share of total CPU, 0..100.
    pub kernel: Option<f32>,
    pub threads: u32,
    pub handles: u32,
    pub processes: u32,
    // ---- memory
    pub commit: u64,
    pub commit_limit: u64,
    pub cached: u64,
    pub paged: u64,
    pub nonpaged: u64,
    pub modified: Option<u64>,
    pub standby: Option<u64>,
    pub free: Option<u64>,
    /// Hard faults (page reads) per second.
    pub hard_faults: Option<f32>,
    // ---- GPU
    pub gpu_name: String,
    pub vram_total: u64,
    pub shared_total: u64,
    pub vram_shared: Option<u64>,
    /// Engine type ("3D", "Copy", "VideoDecode", ...) and its busiest engine, 0..100.
    pub engines: Vec<(String, f32)>,
    // ---- disks, volumes, network, processes
    pub disks: Vec<DiskStat>,
    pub volumes: Vec<Volume>,
    pub adapters: Vec<NetStat>,
    pub procs: Vec<ProcRow>,
}

#[derive(Default)]
struct Counters {
    cores: Option<Counter>,
    kernel: Option<Counter>,
    freq: Option<Counter>,
    modified: Option<Counter>,
    standby: [Option<Counter>; 3],
    free: Option<Counter>,
    faults: Option<Counter>,
    shared: Option<Counter>,
    d_read: Option<Counter>,
    d_write: Option<Counter>,
    d_idle: Option<Counter>,
    d_lat: Option<Counter>,
    d_queue: Option<Counter>,
}

#[derive(Default)]
pub struct DetailSensors {
    attached: bool,
    statics: bool,
    c: Counters,
    buf: Vec<u64>,
    /// Seconds since volumes were read (None = never).
    slow_age: Option<f64>,
    pub d: Detail,
}

fn utf16(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

impl DetailSensors {
    /// Attach the counters when the page opens; drop them (PDH stops collecting) when it closes.
    pub fn sync(&mut self, q: &Query, on: bool) {
        if !on {
            if self.attached {
                self.attached = false;
                self.c = Counters::default();
                self.slow_age = None;
                self.d.procs = Vec::new();
            }
            return;
        }
        if self.attached {
            return;
        }
        self.attached = true;
        let c = &mut self.c;
        c.cores = q.add(r"\Processor Information(*)\% Processor Utility");
        c.kernel = q
            .add(r"\Processor Information(_Total)\% Privileged Utility")
            .or_else(|| q.add(r"\Processor Information(_Total)\% Privileged Time"));
        c.freq = q.add(r"\Processor Information(_Total)\Processor Frequency");
        c.modified = q.add(r"\Memory\Modified Page List Bytes");
        c.standby = [
            q.add(r"\Memory\Standby Cache Core Bytes"),
            q.add(r"\Memory\Standby Cache Normal Priority Bytes"),
            q.add(r"\Memory\Standby Cache Reserve Bytes"),
        ];
        c.free = q.add(r"\Memory\Free & Zero Page List Bytes");
        c.faults = q.add(r"\Memory\Page Reads/sec");
        c.shared = q.add(r"\GPU Adapter Memory(*)\Shared Usage");
        c.d_read = q.add(r"\PhysicalDisk(*)\Disk Read Bytes/sec");
        c.d_write = q.add(r"\PhysicalDisk(*)\Disk Write Bytes/sec");
        c.d_idle = q.add(r"\PhysicalDisk(*)\% Idle Time");
        c.d_lat = q.add(r"\PhysicalDisk(*)\Avg. Disk sec/Transfer");
        c.d_queue = q.add(r"\PhysicalDisk(*)\Current Disk Queue Length");
    }

    /// Fill `d` after `Query::collect`. `perf_pct` is `% Processor Performance`.
    pub fn sample(&mut self, elapsed: f64) {
        if !self.statics {
            self.statics = true;
            read_statics(&mut self.d);
        }
        let val = |c: &Option<Counter>| c.as_ref().and_then(Counter::value);
        let d = &mut self.d;

        // Per-core: instances "group,index" (skip the "_Total" ones), in processor order.
        let mut cores: Vec<(u32, u32, f32)> = Vec::with_capacity(d.logical as usize);
        if let Some(c) = &self.c.cores {
            c.for_each(&mut self.buf, |name, v| {
                let s = utf16(name);
                if let Some((g, i)) = s.split_once(',') {
                    if let (Ok(g), Ok(i)) = (g.parse::<u32>(), i.parse::<u32>()) {
                        cores.push((g, i, v.clamp(0.0, 100.0) as f32));
                    }
                }
            });
        }
        cores.sort_by_key(|c| (c.0, c.1));
        d.core_pct = cores.into_iter().map(|c| c.2).collect();
        d.kernel = val(&self.c.kernel).map(|v| v.clamp(0.0, 100.0) as f32);
        if let Some(f) = val(&self.c.freq).filter(|f| *f > 0.0) {
            d.base_mhz = f as f32;
        }

        // Memory lists and commit.
        d.modified = val(&self.c.modified).map(|v| v.max(0.0) as u64);
        let sb: Vec<f64> = self.c.standby.iter().filter_map(val).collect();
        d.standby = (!sb.is_empty()).then(|| sb.iter().sum::<f64>().max(0.0) as u64);
        d.free = val(&self.c.free).map(|v| v.max(0.0) as u64);
        d.hard_faults = val(&self.c.faults).map(|v| v.max(0.0) as f32);
        // SAFETY: plain-old-data out struct with cb set.
        let mut pi: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
        pi.cb = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
        if unsafe { GetPerformanceInfo(&mut pi, pi.cb) } != 0 {
            let pg = pi.PageSize as u64;
            d.commit = pi.CommitTotal as u64 * pg;
            d.commit_limit = pi.CommitLimit as u64 * pg;
            d.cached = pi.SystemCache as u64 * pg;
            d.paged = pi.KernelPaged as u64 * pg;
            d.nonpaged = pi.KernelNonpaged as u64 * pg;
            d.handles = pi.HandleCount;
            d.threads = pi.ThreadCount;
            d.processes = pi.ProcessCount;
        }

        // Shared GPU memory of the adapter using the most.
        d.vram_shared = self.c.shared.as_ref().and_then(|c| {
            let mut max: Option<f64> = None;
            c.for_each(&mut self.buf, |_, v| max = Some(max.map_or(v, |m| m.max(v))));
            max.map(|m| m.max(0.0) as u64)
        });

        // Physical disks: instance "0 C: E:".
        let mut disks: Vec<DiskStat> = Vec::new();
        let mut each = |c: &Option<Counter>, buf: &mut Vec<u64>, f: &mut dyn FnMut(&mut DiskStat, f64)| {
            if let Some(c) = c {
                c.for_each(buf, |name, v| {
                    let s = utf16(name);
                    if s == "_Total" {
                        return;
                    }
                    let i = match disks.iter().position(|x| x.letters == s) {
                        Some(i) => i,
                        None => {
                            disks.push(DiskStat { letters: s.clone(), ..Default::default() });
                            disks.len() - 1
                        }
                    };
                    f(&mut disks[i], v);
                });
            }
        };
        each(&self.c.d_read, &mut self.buf, &mut |x, v| x.read = v.max(0.0));
        each(&self.c.d_write, &mut self.buf, &mut |x, v| x.write = v.max(0.0));
        each(&self.c.d_idle, &mut self.buf, &mut |x, v| x.active = (100.0 - v).clamp(0.0, 100.0) as f32);
        each(&self.c.d_lat, &mut self.buf, &mut |x, v| x.latency_ms = (v * 1000.0).max(0.0) as f32);
        each(&self.c.d_queue, &mut self.buf, &mut |x, v| x.queue = v.max(0.0) as f32);
        for x in &mut disks {
            // "0 C: E:" -> "Disk 0", "C: E:"
            let (num, rest) = x.letters.split_once(' ').map_or((x.letters.clone(), String::new()), |(a, b)| (a.to_string(), b.trim().to_string()));
            x.name = format!("Disk {}", num);
            x.letters = rest;
        }
        disks.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then_with(|| a.name.cmp(&b.name)));
        d.disks = disks;

        // Volumes, every few seconds.
        let due = self.slow_age.map_or(true, |a| a + elapsed >= SLOW_SECS);
        if due {
            self.slow_age = Some(0.0);
            d.volumes = volumes();
        } else if let Some(a) = &mut self.slow_age {
            *a += elapsed;
        }
    }

    /// Volumes and addresses are due for a re-read on this sample (shared with the adapter detail).
    pub fn slow_due(&self) -> bool {
        self.slow_age == Some(0.0)
    }
}

/// Registry string under HKLM.
fn reg_string(sub: &str, name: &str) -> Option<String> {
    let (sub, name) = (wide(sub), wide(name));
    let mut buf = [0u16; 256];
    let mut sz = (buf.len() * 2) as u32;
    // SAFETY: buffer of `sz` bytes.
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut sz)
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]).split_whitespace().collect::<Vec<_>>().join(" "))
}

fn read_statics(d: &mut Detail) {
    const CPU0: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    d.cpu_name = reg_string(CPU0, "ProcessorNameString").unwrap_or_default();
    if let Some(mhz) = crate::util::read_dword(HKEY_LOCAL_MACHINE, CPU0, "~MHz") {
        d.base_mhz = mhz as f32;
    }
    // SAFETY: plain query.
    d.logical = unsafe { GetActiveProcessorCount(0xFFFF) };
    let mut len = 0u32;
    // SAFETY: sizing call with a null buffer.
    unsafe { GetLogicalProcessorInformation(std::ptr::null_mut(), &mut len) };
    let n = len as usize / std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
    if n > 0 {
        // SAFETY: plain-old-data array of the size Windows asked for.
        let mut v: Vec<SYSTEM_LOGICAL_PROCESSOR_INFORMATION> = vec![unsafe { std::mem::zeroed() }; n];
        if unsafe { GetLogicalProcessorInformation(v.as_mut_ptr(), &mut len) } != 0 {
            let n = (len as usize / std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>()).min(v.len());
            for e in &v[..n] {
                if e.Relationship == RelationProcessorCore {
                    d.cores += 1;
                } else if e.Relationship == RelationCache {
                    // SAFETY: the union holds a cache descriptor for RelationCache.
                    let c = unsafe { e.Anonymous.Cache };
                    if c.Level == 3 {
                        d.l3 = d.l3.max(u64::from(c.Size));
                    }
                }
            }
        }
    }
    if let Some((name, ded, shared)) = gpu_adapter() {
        d.gpu_name = name;
        d.vram_total = ded;
        d.shared_total = shared;
    }
}

/// Name, dedicated and shared memory of the hardware adapter with the most dedicated memory.
fn gpu_adapter() -> Option<(String, u64, u64)> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    // SAFETY: plain COM factory creation; no apartment needed for DXGI.
    let f: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let mut best: Option<(String, u64, u64)> = None;
    for i in 0..8u32 {
        // SAFETY: valid factory; an error ends the list.
        let Ok(a) = (unsafe { f.EnumAdapters1(i) }) else { break };
        // SAFETY: valid adapter.
        let Ok(desc) = (unsafe { a.GetDesc1() }) else { continue };
        // DXGI_ADAPTER_FLAG_SOFTWARE = 2 (Microsoft Basic Render Driver).
        if desc.Flags & 2 != 0 {
            continue;
        }
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len]).trim().to_string();
        let ded = desc.DedicatedVideoMemory as u64;
        if best.as_ref().map_or(true, |b| ded > b.1) {
            best = Some((name, ded, desc.SharedSystemMemory as u64));
        }
    }
    best
}

/// Fixed volumes with a drive letter.
fn volumes() -> Vec<Volume> {
    let mut out = Vec::new();
    // SAFETY: plain query.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from(b'A' + i as u8);
        let root = wide(&format!("{}:\\", letter));
        // SAFETY: NUL-terminated root path.
        if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: valid out pointers.
        if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free) } == 0 || total == 0 {
            continue;
        }
        let mut label = [0u16; 64];
        // SAFETY: label buffer of the stated length; other outputs not requested.
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        } != 0;
        let label = if ok {
            let n = label.iter().position(|&c| c == 0).unwrap_or(label.len());
            String::from_utf16_lossy(&label[..n])
        } else {
            String::new()
        };
        out.push(Volume { letter: format!("{}:", letter), label, total, free: avail });
    }
    out
}

/// "3.84 GHz" style clock from MHz.
pub fn ghz(mhz: f32) -> String {
    format!("{:.2} GHz", mhz / 1000.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ghz_formats() {
        assert_eq!(super::ghz(4200.0), "4.20 GHz");
    }
}
