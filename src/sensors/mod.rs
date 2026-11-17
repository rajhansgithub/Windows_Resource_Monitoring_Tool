//! System sensors. No driver, no admin. All sources are user-mode Windows APIs.
//!
//! PUBLIC API CONTRACT (used by the UI — do not change signatures):
//!
//!   let mut s = Sensors::new();
//!   let snap: &Snapshot = s.sample(&SampleOpts { .. });   // call ~once per interval
//!   let list: Vec<AdapterInfo> = s.adapters();              // for the tray menu

mod cpu;
mod disk;
mod gpu;
mod gpu_temp;
mod mem;
mod net;
mod pdh;
mod power;
mod procs;
mod volume;

use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

#[derive(Clone, Debug, Default)]
pub struct ProcStat {
    pub pid: u32,
    /// Image name without ".exe" (e.g. "chrome").
    pub name: String,
    /// Percent of total machine CPU, 0..100.
    pub cpu: f32,
    /// Read+write+other I/O bytes per second.
    pub io_bps: f64,
    /// Private working set / working set bytes.
    pub mem: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Battery {
    /// 0..100
    pub percent: u8,
    pub charging: bool,
    pub on_ac: bool,
    /// Remaining seconds when known.
    pub secs_left: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct AdapterInfo {
    /// Stable key used in settings (interface alias, e.g. "Wi-Fi").
    pub alias: String,
    /// Human description (e.g. "Intel(R) Wi-Fi 6E AX211").
    pub description: String,
    pub connected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Total CPU 0..100 (matches Task Manager where possible).
    pub cpu: f32,
    pub ram_pct: f32,
    pub ram_used: u64,
    pub ram_total: u64,
    /// Busiest GPU engine utilisation 0..100. None = unavailable.
    pub gpu: Option<f32>,
    /// Dedicated VRAM in use (busiest adapter), bytes.
    pub vram_used: Option<u64>,
    /// Hottest GPU adapter temperature in deg C (WDDM driver). None = unavailable.
    pub gpu_temp: Option<f32>,
    pub gpu_fan_rpm: Option<u32>,
    /// Reserved: the driver reports power only as a fraction of TDP, so this stays None.
    pub gpu_power_w: Option<f32>,
    /// System drive space (refreshed every ~30 s).
    pub disk_used_pct: Option<f32>,
    pub disk_used: u64,
    pub disk_total: u64,
    /// Bytes/s, all physical disks.
    pub disk_read: f64,
    pub disk_write: f64,
    /// Disk active time 0..100 (max over physical disks).
    pub disk_active: Option<f32>,
    /// Bytes/s for selected adapter(s).
    pub net_up: f64,
    pub net_down: f64,
    /// Exact byte counts since previous sample (for usage accounting).
    pub net_sent_delta: u64,
    pub net_recv_delta: u64,
    /// Up to 5 each, sorted descending. Empty unless SampleOpts::procs.
    pub top_cpu: Vec<ProcStat>,
    pub top_io: Vec<ProcStat>,
    pub top_mem: Vec<ProcStat>,
    pub proc_count: u32,
    pub battery: Option<Battery>,
    pub uptime_secs: u64,
    /// Seconds since previous sample (first sample: 0).
    pub elapsed: f64,
}

#[derive(Clone, Debug, Default)]
pub struct SampleOpts {
    /// None/empty = auto (all connected physical adapters, no double counting).
    pub adapter: Option<String>,
    pub gpu: bool,
    pub disk: bool,
    pub procs: bool,
}

/// Monotonic clock based on QueryPerformanceCounter.
struct Clock {
    freq: f64,
    last: Option<i64>,
}

impl Clock {
    fn new() -> Self {
        let mut f = 0i64;
        // SAFETY: valid out pointer.
        unsafe { QueryPerformanceFrequency(&mut f) };
        Clock { freq: f.max(1) as f64, last: None }
    }

    /// Seconds since the previous call (0 on the first call).
    fn tick(&mut self) -> f64 {
        let mut now = 0i64;
        // SAFETY: valid out pointer.
        unsafe { QueryPerformanceCounter(&mut now) };
        let elapsed = self.last.map_or(0.0, |l| (now - l).max(0) as f64 / self.freq);
        self.last = Some(now);
        elapsed
    }
}

pub struct Sensors {
    snap: Snapshot,
    clock: Clock,
    cpu: cpu::Cpu,
    gpu: gpu::Gpu,
    gpu_temp: gpu_temp::GpuTemp,
    sys_drive: volume::SystemDrive,
    disk: disk::Disk,
    net: net::Net,
    procs: procs::Procs,
    // Declared last: counters above are dropped before the query that owns them.
    pdh: Option<pdh::Query>,
}

impl Sensors {
    pub fn new() -> Self {
        Sensors {
            snap: Snapshot::default(),
            clock: Clock::new(),
            cpu: cpu::Cpu::default(),
            gpu: gpu::Gpu::default(),
            gpu_temp: gpu_temp::GpuTemp::default(),
            sys_drive: volume::SystemDrive::default(),
            disk: disk::Disk::default(),
            net: net::Net::default(),
            procs: procs::Procs::new(),
            pdh: pdh::Query::open(),
        }
    }

    pub fn sample(&mut self, opts: &SampleOpts) -> &Snapshot {
        let elapsed = self.clock.tick();

        // One PDH collection per sample; counters are attached only while their metric is enabled.
        if let Some(q) = &self.pdh {
            self.cpu.attach(q);
            self.gpu.sync(q, opts.gpu);
            self.disk.sync(q, opts.disk);
            q.collect();
        }

        let s = &mut self.snap;
        s.elapsed = elapsed;
        s.cpu = self.cpu.sample();
        (s.ram_used, s.ram_total) = mem::usage();
        s.ram_pct = if s.ram_total > 0 {
            (s.ram_used as f64 / s.ram_total as f64 * 100.0) as f32
        } else {
            0.0
        };

        (s.gpu, s.vram_used) = if opts.gpu { self.gpu.sample() } else { (None, None) };

        (s.gpu_temp, s.gpu_fan_rpm) = if opts.gpu { self.gpu_temp.sample() } else { (None, None) };
        s.gpu_power_w = None;
        (s.disk_used, s.disk_total, s.disk_used_pct) = self.sys_drive.sample(elapsed);

        if opts.disk {
            let d = self.disk.sample();
            (s.disk_read, s.disk_write, s.disk_active) = (d.read, d.write, d.active);
        } else {
            (s.disk_read, s.disk_write, s.disk_active) = (0.0, 0.0, None);
        }

        self.net.sample(opts.adapter.as_deref(), elapsed, s);

        if opts.procs {
            self.procs.sample(elapsed, s);
        } else {
            self.procs.reset();
            s.top_cpu.clear();
            s.top_io.clear();
            s.top_mem.clear();
            s.proc_count = 0;
        }

        s.battery = power::battery();
        s.uptime_secs = power::uptime_secs();
        &self.snap
    }

    pub fn adapters(&self) -> Vec<AdapterInfo> {
        net::adapters()
    }
}
