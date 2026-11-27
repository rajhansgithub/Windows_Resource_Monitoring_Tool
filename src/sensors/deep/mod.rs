//! Deep per-device facts for the device detail pages (click a Monitor tile). Each module
//! reads one kind of device. Static facts are read once on a worker thread and cached
//! (`reader`); live values are cheap and read only while that device's page is on screen
//! (`live`).

pub mod battery;
pub mod cpu;
pub mod disk;
pub mod display;
pub mod gpu;
pub mod live;
pub mod net;
pub mod reader;
pub mod refresh;

#[cfg(test)]
mod tests {
    use super::Dev;

    #[test]
    fn keys() {
        assert_eq!(Dev::from_key("disk1"), Some(Dev::Disk(1)));
        assert_eq!(Dev::from_key("Display0"), Some(Dev::Display(0)));
        assert_eq!(Dev::from_key("cpu"), Some(Dev::Cpu));
        assert_eq!(Dev::from_key("diskx"), None);
    }
}

use std::sync::Arc;

/// A device with its own detail page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dev {
    Cpu,
    Mem,
    Gpu,
    /// PhysicalDrive number.
    Disk(u32),
    /// Index into `DeepStatic::screens`.
    Display(u32),
    Net,
    Battery,
}

impl Dev {
    /// From a short key ("cpu", "mem", "gpu", "disk0", "display1", "net", "battery"): CI runs
    /// open a page with `RESMON_DEV`.
    pub fn from_key(k: &str) -> Option<Dev> {
        let k = k.trim().to_ascii_lowercase();
        Some(match k.as_str() {
            "cpu" => Dev::Cpu,
            "mem" | "memory" => Dev::Mem,
            "gpu" => Dev::Gpu,
            "net" | "network" => Dev::Net,
            "battery" => Dev::Battery,
            _ => {
                if let Some(n) = k.strip_prefix("disk") {
                    Dev::Disk(n.parse().ok()?)
                } else if let Some(n) = k.strip_prefix("display") {
                    Dev::Display(n.parse().ok()?)
                } else {
                    return None;
                }
            }
        })
    }
}

/// Health and extra facts of one physical drive.
#[derive(Clone, Debug, Default)]
pub struct DiskDeep {
    pub num: u32,
    pub health: Option<disk::Health>,
    pub extra: disk::DiskExtra,
}

/// Facts that change rarely, read on the deep reader thread while a device page is open.
#[derive(Clone, Debug, Default)]
pub struct DeepStatic {
    pub topo: Option<cpu::CpuTopo>,
    pub power: Option<cpu::PowerMode>,
    pub gpus: Vec<gpu::Adapter>,
    pub screens: Vec<display::Screen>,
    pub disks: Vec<DiskDeep>,
    pub adapters: Vec<net::AdapterFacts>,
    /// The app runs elevated (admin-only reads were tried).
    pub admin: bool,
}

impl DeepStatic {
    pub fn disk(&self, num: u32) -> Option<&DiskDeep> {
        self.disks.iter().find(|d| d.num == num)
    }
}

/// Live values of the open device page, sampled with the detail once a second.
#[derive(Clone, Debug, Default)]
pub struct DeepLive {
    /// The page these values are for.
    pub dev: Option<Dev>,
    pub st: Arc<DeepStatic>,
    // ---- CPU
    /// Effective clock per logical processor, MHz.
    pub core_mhz: Vec<f32>,
    /// Parked logical processors.
    pub parked: Vec<u32>,
    pub interrupt_pct: Option<f32>,
    pub dpc_pct: Option<f32>,
    pub ctx_switches: Option<f32>,
    pub queue: Option<f32>,
    // ---- GPU
    /// (pid, adapter LUID, dedicated bytes, shared bytes).
    pub gpu_mem: Vec<(u32, u64, u64, u64)>,
    /// (pid, adapter LUID, 3D engine %).
    pub gpu_3d: Vec<(u32, u64, f32)>,
    /// (adapter LUID, sensor data).
    pub gpu_perf: Vec<(u64, gpu::PerfData)>,
    // ---- display
    /// Measured refresh of the open screen, Hz (0 = not measured yet).
    pub measured_hz: f32,
    // ---- network
    pub ping: Option<net::PingState>,
    pub conns: Vec<net::Conn>,
    pub wifi: Option<net::WifiInfo>,
    // ---- battery
    pub batteries: Vec<battery::Battery>,
}
