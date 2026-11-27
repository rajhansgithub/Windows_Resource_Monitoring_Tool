//! GPU detail page sensors: adapter inventory (DXGI, driver registry, PCIe link via SetupAPI,
//! HAGS), live WDDM perf data (D3DKMT), per-process VRAM name parsing, the wrong-GPU check and
//! optional NVIDIA clocks/power (NVML, loaded dynamically). No admin, no new crates.
//!
//! Static facts (`adapters`, `Nvml::load`) are for a background thread (a few ms to ~200 ms).
//! `perf` and `Nvml::read` are cheap enough to call once a second. The per-process counters
//! (`\GPU Process Memory(*)\...`) are owned by the caller; this module only parses their names.
#![allow(dead_code)]

use crate::util::{from_wide, read_dword, wide};
use std::collections::HashMap;
use std::ffi::c_void;
use windows_sys::core::GUID;
use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo, D3DKMT_ADAPTER_PERFDATA,
    D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO, KMTQAITYPE_ADAPTERPERFDATA,
};
use windows_sys::Win32::Foundation::{FreeLibrary, INVALID_HANDLE_VALUE, LUID};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

/// Upper bound on adapters and display devices we look at.
const MAX_ADAPTERS: u32 = 16;
/// An adapter with at most this much dedicated memory is treated as an integrated GPU.
const INTEGRATED_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Microsoft vendor id: Basic Render Driver, WARP, Remote Display Adapter.
const VENDOR_MICROSOFT: u32 = 0x1414;
const DISPLAY_CLASS_KEY: &str =
    "SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}";

/// PCIe link as negotiated now and as the device and slot allow at best.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcieLink {
    /// Current generation (1..=6). Drops at idle on many cards (power saving).
    pub gen: u8,
    /// Current lane count (x1, x4, x8, x16).
    pub width: u8,
    /// Highest generation the link supports.
    pub max_gen: u8,
    /// Widest lane count the link supports.
    pub max_width: u8,
}

/// Live WDDM adapter counters. Every field is `None` when the driver does not report it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PerfData {
    /// Core temperature, deg C.
    pub temp_c: Option<f32>,
    /// Fan speed, RPM.
    pub fan_rpm: Option<u32>,
    /// Board power as a percentage of TDP (the driver reports no watts).
    pub power_pct: Option<f32>,
    /// Current memory clock, MHz.
    pub mem_mhz: Option<f32>,
    /// Maximum memory clock, MHz.
    pub mem_max_mhz: Option<f32>,
}

/// One hardware GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adapter {
    /// `(HighPart << 32) | LowPart`; matches the `luid_0x..._0x...` part of PDH instance names.
    pub luid: u64,
    pub name: String,
    /// PCI vendor id: 0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel.
    pub vendor_id: u32,
    /// Dedicated video memory, bytes.
    pub dedicated: u64,
    /// Shared system memory the GPU may use, bytes.
    pub shared: u64,
    /// Integrated (shares system RAM): dedicated memory is at most 512 MiB.
    pub integrated: bool,
    /// User-mode driver version "a.b.c.d" (registry `DriverVersion` as a fallback).
    pub driver_version: String,
    /// Driver date "2025-10-14"; empty when unknown.
    pub driver_date: String,
    pub pcie: Option<PcieLink>,
    /// Hardware-accelerated GPU scheduling: `Some(true)` on, `Some(false)` off, `None` unknown.
    /// A system-wide setting, repeated on every adapter.
    pub hags: Option<bool>,
    /// Perf counters sampled once while enumerating; call [`perf`] for live values.
    pub perf: Option<PerfData>,
}

/// `(HighPart << 32) | LowPart`.
fn luid_u64(l: LUID) -> u64 {
    (u64::from(l.HighPart as u32) << 32) | u64::from(l.LowPart)
}

// ---------------------------------------------------------------- adapters

/// Hardware adapters in DXGI order (software and Microsoft virtual adapters are skipped).
/// Reads DXGI, the registry and SetupAPI: call from a background thread, once.
pub fn adapters() -> Vec<Adapter> {
    let mut out = dxgi_adapters();
    if out.is_empty() {
        return out;
    }
    let hags = hags();
    let links = pcie_links();
    let mut used: HashMap<String, usize> = HashMap::new();
    for a in &mut out {
        a.hags = hags;
        a.perf = perf(a.luid);
        if let Some((date, ver)) = registry_driver(&a.name) {
            a.driver_date = date;
            if a.driver_version.is_empty() {
                a.driver_version = ver;
            }
        }
        // Identical cards share a name: the nth adapter takes the nth matching device.
        let key = a.name.trim().to_lowercase();
        let nth = used.entry(key.clone()).or_insert(0);
        a.pcie = links.iter().filter(|(n, _)| *n == key).nth(*nth).and_then(|(_, l)| *l);
        *nth += 1;
    }
    out
}

/// "a.b.c.d" from a packed 64-bit driver version.
fn format_version(v: u64) -> String {
    format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF)
}

fn dxgi_adapters() -> Vec<Adapter> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
    let mut out = Vec::new();
    // SAFETY: plain COM factory creation; DXGI needs no apartment.
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    for i in 0..MAX_ADAPTERS {
        // SAFETY: valid factory; an error (DXGI_ERROR_NOT_FOUND) ends the list.
        let Ok(a) = (unsafe { f.EnumAdapters1(i) }) else { break };
        // SAFETY: valid adapter.
        let Ok(d) = (unsafe { a.GetDesc1() }) else { continue };
        // DXGI_ADAPTER_FLAG_SOFTWARE = 2 (Microsoft Basic Render Driver).
        if d.Flags & 2 != 0 || d.VendorId == VENDOR_MICROSOFT {
            continue;
        }
        let len = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
        let dedicated = d.DedicatedVideoMemory as u64;
        // SAFETY: valid adapter; the "version" is the user-mode driver version.
        let driver_version = unsafe { a.CheckInterfaceSupport(&IDXGIDevice::IID) }
            .map(|v| format_version(v as u64))
            .unwrap_or_default();
        let luid = LUID { LowPart: d.AdapterLuid.LowPart, HighPart: d.AdapterLuid.HighPart };
        out.push(Adapter {
            luid: luid_u64(luid),
            name: String::from_utf16_lossy(&d.Description[..len]).trim().to_string(),
            vendor_id: d.VendorId,
            dedicated,
            shared: d.SharedSystemMemory as u64,
            integrated: dedicated <= INTEGRATED_MAX_BYTES,
            driver_version,
            ..Adapter::default()
        });
    }
    out
}

// ---------------------------------------------------------------- registry

/// REG_SZ value under HKLM, `None` when missing or empty.
fn reg_string(sub: &str, name: &str) -> Option<String> {
    let (sub, name) = (wide(sub), wide(name));
    let mut buf = [0u16; 256];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: NUL-terminated strings; `buf` is writable for `size` bytes.
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if r != 0 {
        return None;
    }
    let s = from_wide(&buf);
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// "10-14-2025" (M-D-YYYY, as the display class key stores it) to "2025-10-14".
fn parse_driver_date(s: &str) -> Option<String> {
    let mut it = s.trim().split(['-', '/']);
    let m: u32 = it.next()?.trim().parse().ok()?;
    let d: u32 = it.next()?.trim().parse().ok()?;
    let y: u32 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1990..=2200).contains(&y) {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// `(date, version)` of the display class driver whose `DriverDesc` equals `name`.
fn registry_driver(name: &str) -> Option<(String, String)> {
    for i in 0..64u32 {
        let sub = format!("{DISPLAY_CLASS_KEY}\\{i:04}");
        let Some(desc) = reg_string(&sub, "DriverDesc") else { continue };
        if !desc.eq_ignore_ascii_case(name.trim()) {
            continue;
        }
        let date = reg_string(&sub, "DriverDate").and_then(|d| parse_driver_date(&d)).unwrap_or_default();
        let ver = reg_string(&sub, "DriverVersion").unwrap_or_default();
        return Some((date, ver));
    }
    None
}

/// Hardware-accelerated GPU scheduling from `HwSchMode` (2 on, 1 off).
fn hags() -> Option<bool> {
    hags_from(read_dword(HKEY_LOCAL_MACHINE, "SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers", "HwSchMode"))
}

fn hags_from(v: Option<u32>) -> Option<bool> {
    match v? {
        2 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------- PCIe (SetupAPI)

/// `DEVPROPKEY`, declared here because windows-sys' SetupAPI feature is not enabled.
#[repr(C)]
struct DevPropKey {
    fmtid: GUID,
    pid: u32,
}

/// The PCI device property set. pids VERIFIED against the `DEVPKEY_PciDevice_*` constants in the
/// windows-sys / windows crate metadata (generated from devpkey.h): CurrentLinkSpeed 9,
/// CurrentLinkWidth 10, MaxLinkSpeed 11, MaxLinkWidth 12. (7 and 8 are payload sizes.)
const PCI_DEVICE_PROPS: GUID = GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
const KEY_CUR_SPEED: DevPropKey = DevPropKey { fmtid: PCI_DEVICE_PROPS, pid: 9 };
const KEY_CUR_WIDTH: DevPropKey = DevPropKey { fmtid: PCI_DEVICE_PROPS, pid: 10 };
const KEY_MAX_SPEED: DevPropKey = DevPropKey { fmtid: PCI_DEVICE_PROPS, pid: 11 };
const KEY_MAX_WIDTH: DevPropKey = DevPropKey { fmtid: PCI_DEVICE_PROPS, pid: 12 };

const GUID_DEVCLASS_DISPLAY: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);
const DIGCF_PRESENT: u32 = 2;
const SPDRP_DEVICEDESC: u32 = 0;
const SPDRP_FRIENDLYNAME: u32 = 0xC;
const DEVPROP_TYPE_UINT32: u32 = 7;

/// `SP_DEVINFO_DATA`.
#[repr(C)]
struct SpDevinfoData {
    cb_size: u32,
    class_guid: GUID,
    dev_inst: u32,
    reserved: usize,
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(class: *const GUID, enumerator: *const u16, hwnd: *mut c_void, flags: u32) -> *mut c_void;
    fn SetupDiEnumDeviceInfo(set: *mut c_void, index: u32, data: *mut SpDevinfoData) -> i32;
    fn SetupDiGetDevicePropertyW(
        set: *mut c_void,
        data: *const SpDevinfoData,
        key: *const DevPropKey,
        prop_type: *mut u32,
        buf: *mut u8,
        size: u32,
        required: *mut u32,
        flags: u32,
    ) -> i32;
    fn SetupDiGetDeviceRegistryPropertyW(
        set: *mut c_void,
        data: *const SpDevinfoData,
        property: u32,
        reg_type: *mut u32,
        buf: *mut u8,
        size: u32,
        required: *mut u32,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(set: *mut c_void) -> i32;
}

/// Closes the device info set on drop.
struct DevSet(*mut c_void);

impl Drop for DevSet {
    fn drop(&mut self) {
        // SAFETY: handle from SetupDiGetClassDevsW, destroyed exactly once.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn dev_u32(set: &DevSet, data: &SpDevinfoData, key: &DevPropKey) -> Option<u32> {
    let (mut ty, mut v, mut req) = (0u32, 0u32, 0u32);
    // SAFETY: `v` is a writable 4-byte buffer; all pointers valid for the call.
    let ok = unsafe {
        SetupDiGetDevicePropertyW(set.0, data, key, &mut ty, (&mut v as *mut u32).cast(), 4, &mut req, 0)
    };
    (ok != 0 && ty == DEVPROP_TYPE_UINT32).then_some(v)
}

fn dev_string(set: &DevSet, data: &SpDevinfoData, prop: u32) -> Option<String> {
    let mut buf = [0u16; 256];
    let (mut ty, mut req) = (0u32, 0u32);
    // SAFETY: `buf` is writable for its byte size; all pointers valid for the call.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.0,
            data,
            prop,
            &mut ty,
            buf.as_mut_ptr().cast(),
            std::mem::size_of_val(&buf) as u32,
            &mut req,
        )
    };
    if ok == 0 {
        return None;
    }
    let s = from_wide(&buf);
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// PCIe generation from `DEVPKEY_PciDevice_*LinkSpeed` (1 = 2.5 GT/s ... 5 = 32 GT/s).
fn link_gen(code: u32) -> Option<u8> {
    (1..=6).contains(&code).then_some(code as u8)
}

fn make_link(cur_speed: u32, cur_width: u32, max_speed: Option<u32>, max_width: Option<u32>) -> Option<PcieLink> {
    let gen = link_gen(cur_speed)?;
    let width = u8::try_from(cur_width).ok().filter(|w| *w > 0)?;
    let max_gen = max_speed.and_then(link_gen).unwrap_or(gen).max(gen);
    let max_width = max_width.and_then(|w| u8::try_from(w).ok()).filter(|w| *w > 0).unwrap_or(width).max(width);
    Some(PcieLink { gen, width, max_gen, max_width })
}

/// `(lowercase device description, link)` for every present display device, in enumeration order.
fn pcie_links() -> Vec<(String, Option<PcieLink>)> {
    let mut out = Vec::new();
    // SAFETY: class GUID is valid; null enumerator and parent window.
    let h = unsafe { SetupDiGetClassDevsW(&GUID_DEVCLASS_DISPLAY, std::ptr::null(), std::ptr::null_mut(), DIGCF_PRESENT) };
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return out;
    }
    let set = DevSet(h);
    for i in 0..MAX_ADAPTERS {
        let mut data = SpDevinfoData {
            cb_size: std::mem::size_of::<SpDevinfoData>() as u32,
            class_guid: GUID::from_u128(0),
            dev_inst: 0,
            reserved: 0,
        };
        // SAFETY: `data.cb_size` is set; valid set handle.
        if unsafe { SetupDiEnumDeviceInfo(set.0, i, &mut data) } == 0 {
            break;
        }
        let Some(name) = dev_string(&set, &data, SPDRP_DEVICEDESC).or_else(|| dev_string(&set, &data, SPDRP_FRIENDLYNAME)) else {
            continue;
        };
        let link = match (dev_u32(&set, &data, &KEY_CUR_SPEED), dev_u32(&set, &data, &KEY_CUR_WIDTH)) {
            (Some(s), Some(w)) => make_link(s, w, dev_u32(&set, &data, &KEY_MAX_SPEED), dev_u32(&set, &data, &KEY_MAX_WIDTH)),
            _ => None,
        };
        out.push((name.to_lowercase(), link));
    }
    out
}

// ---------------------------------------------------------------- live perf (D3DKMT)

/// Memory clock register value to MHz. Drivers report Hz; a small value is taken as MHz already.
fn to_mhz(v: u64) -> Option<f32> {
    match v {
        0 => None,
        1..=999_999 => Some(v as f32),
        _ => Some((v as f64 / 1_000_000.0) as f32),
    }
}

fn perf_from(p: &D3DKMT_ADAPTER_PERFDATA) -> PerfData {
    PerfData {
        temp_c: (p.Temperature > 0).then(|| p.Temperature as f32 / 10.0),
        fan_rpm: (p.FanRPM > 0).then_some(p.FanRPM),
        // Tenths of a percent of TDP (see gpu_temp.rs).
        power_pct: (p.Power > 0).then(|| p.Power as f32 / 10.0),
        mem_mhz: to_mhz(p.MemoryFrequency),
        mem_max_mhz: to_mhz(if p.MaxMemoryFrequency > 0 { p.MaxMemoryFrequency } else { p.MaxMemoryFrequencyOC }),
    }
}

/// WDDM perf counters of one adapter (temperature, fan, power, memory clock). Opens and closes
/// the adapter per call (microseconds). `None` when the driver does not support the query.
pub fn perf(luid: u64) -> Option<PerfData> {
    let mut open = D3DKMT_OPENADAPTERFROMLUID {
        AdapterLuid: LUID { LowPart: luid as u32, HighPart: (luid >> 32) as u32 as i32 },
        hAdapter: 0,
    };
    // SAFETY: valid in/out struct.
    if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) } < 0 || open.hAdapter == 0 {
        return None;
    }
    // SAFETY: POD struct, zero is a valid value.
    let mut data: D3DKMT_ADAPTER_PERFDATA = unsafe { std::mem::zeroed() };
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: open.hAdapter,
        Type: KMTQAITYPE_ADAPTERPERFDATA,
        pPrivateDriverData: (&mut data as *mut D3DKMT_ADAPTER_PERFDATA).cast(),
        PrivateDriverDataSize: std::mem::size_of::<D3DKMT_ADAPTER_PERFDATA>() as u32,
    };
    // SAFETY: pPrivateDriverData points to PrivateDriverDataSize writable bytes.
    let ok = unsafe { D3DKMTQueryAdapterInfo(&mut q) } >= 0;
    let c = D3DKMT_CLOSEADAPTER { hAdapter: open.hAdapter };
    // SAFETY: handle opened above, closed exactly once.
    unsafe { D3DKMTCloseAdapter(&c) };
    ok.then(|| perf_from(&data))
}

// ---------------------------------------------------------------- per-process memory

/// One process's GPU memory on one adapter (from `\GPU Process Memory(*)` counters).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcGpuMem {
    pub pid: u32,
    pub luid: u64,
    /// Dedicated VRAM in use, bytes.
    pub dedicated: u64,
    /// Shared system memory in use, bytes.
    pub shared: u64,
}

/// `(pid, luid)` from a PDH instance name such as
/// `pid_1234_luid_0x00000000_0x0000D1F3_phys_0` or the engine form
/// `pid_1234_luid_0x00000000_0x0000D1F3_phys_0_eng_0_engtype_3D`. `None` when it does not match.
pub fn parse_instance(name: &str) -> Option<(u32, u64)> {
    let mut it = name.split('_');
    if it.next()? != "pid" {
        return None;
    }
    let pid: u32 = it.next()?.parse().ok()?;
    if it.next()? != "luid" {
        return None;
    }
    let hex = |s: &str| {
        let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
        u32::from_str_radix(s, 16).ok()
    };
    let high = hex(it.next()?)?;
    let low = hex(it.next()?)?;
    Some((pid, (u64::from(high) << 32) | u64::from(low)))
}

/// Sums `(instance name, value)` rows per `(pid, luid)`, skipping names that do not parse.
/// Sorted by pid, then luid.
pub fn sum_by_pid(rows: impl Iterator<Item = (String, f64)>) -> Vec<(u32, u64, f64)> {
    let mut m: HashMap<(u32, u64), f64> = HashMap::new();
    for (name, v) in rows {
        if let Some(k) = parse_instance(&name) {
            *m.entry(k).or_insert(0.0) += v;
        }
    }
    let mut out: Vec<(u32, u64, f64)> = m.into_iter().map(|((p, l), v)| (p, l, v)).collect();
    out.sort_by_key(|r| (r.0, r.1));
    out
}

// ---------------------------------------------------------------- wrong GPU

/// 3D load (%) a process needs on an integrated GPU to be flagged.
const WRONG_GPU_MIN_PCT: f32 = 20.0;
/// A discrete GPU counts as idle below this total 3D load (%).
const DISCRETE_IDLE_PCT: f32 = 5.0;

/// Processes with at least 20% 3D load on an integrated adapter while a discrete adapter exists
/// and sits under 5% 3D (total of `procs_3d` on it). Returns `(pid, luid)` of each offender.
pub fn wrong_gpu(procs_3d: &[(u32, u64, f32)], adapters: &[Adapter]) -> Vec<(u32, u64)> {
    let load_on = |luid: u64| procs_3d.iter().filter(|p| p.1 == luid).map(|p| p.2).sum::<f32>();
    let discrete_idle = adapters.iter().filter(|a| !a.integrated).any(|a| load_on(a.luid) < DISCRETE_IDLE_PCT);
    if !discrete_idle {
        return Vec::new();
    }
    let mut out: Vec<(u32, u64)> = Vec::new();
    for &(pid, luid, pct) in procs_3d {
        let on_igpu = adapters.iter().any(|a| a.integrated && a.luid == luid);
        if on_igpu && pct >= WRONG_GPU_MIN_PCT && !out.contains(&(pid, luid)) {
            out.push((pid, luid));
        }
    }
    out
}

// ---------------------------------------------------------------- NVML (optional, NVIDIA)

type NvmlInit = unsafe extern "C" fn() -> i32;
type NvmlShutdown = unsafe extern "C" fn() -> i32;
type NvmlHandleByIndex = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type NvmlClock = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;
type NvmlPower = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`: only look in System32, never the app directory.
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
/// `NVML_CLOCK_GRAPHICS`.
const NVML_CLOCK_GRAPHICS: u32 = 0;

/// NVIDIA Management Library, loaded from System32 on demand. Absent on non-NVIDIA PCs.
pub struct Nvml {
    module: usize,
    shutdown: Option<NvmlShutdown>,
    handle_by_index: NvmlHandleByIndex,
    clock: NvmlClock,
    power: NvmlPower,
}

impl Nvml {
    /// Loads `nvml.dll` and initialises it. `None` when the DLL or an entry point is missing.
    /// Can take 100 ms or more: call once on a background thread.
    pub fn load() -> Option<Nvml> {
        let name = wide("nvml.dll");
        // SAFETY: NUL-terminated name; System32-only search.
        let m = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if m.is_null() {
            return None;
        }
        let sym = |n: &[u8]| {
            // SAFETY: `n` is NUL-terminated; `m` is a loaded module.
            unsafe { GetProcAddress(m, n.as_ptr()) }
        };
        let (init, shutdown, h, c, p) = (
            sym(b"nvmlInit_v2\0"),
            sym(b"nvmlShutdown\0"),
            sym(b"nvmlDeviceGetHandleByIndex_v2\0"),
            sym(b"nvmlDeviceGetClockInfo\0"),
            sym(b"nvmlDeviceGetPowerUsage\0"),
        );
        let (Some(init), Some(h), Some(c), Some(p)) = (init, h, c, p) else {
            // SAFETY: module loaded above.
            unsafe { FreeLibrary(m) };
            return None;
        };
        // SAFETY: the exports have these documented signatures (nvml.h).
        let (init, handle_by_index, clock, power, shutdown) = unsafe {
            (
                std::mem::transmute::<unsafe extern "system" fn() -> isize, NvmlInit>(init),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, NvmlHandleByIndex>(h),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, NvmlClock>(c),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, NvmlPower>(p),
                shutdown.map(|s| std::mem::transmute::<unsafe extern "system" fn() -> isize, NvmlShutdown>(s)),
            )
        };
        // SAFETY: documented init call; 0 = NVML_SUCCESS.
        if unsafe { init() } != 0 {
            // SAFETY: module loaded above.
            unsafe { FreeLibrary(m) };
            return None;
        }
        Some(Nvml { module: m as usize, shutdown, handle_by_index, clock, power })
    }

    /// `(graphics clock MHz, board power W)` of NVML device `index` (NVML's own order, which
    /// can differ from DXGI's). `None` when either value is unavailable.
    pub fn read(&self, index: u32) -> Option<(u32, f32)> {
        let mut dev: *mut c_void = std::ptr::null_mut();
        let (mut mhz, mut mw) = (0u32, 0u32);
        // SAFETY: valid out pointers; the handle is only used with the same library.
        unsafe {
            if (self.handle_by_index)(index, &mut dev) != 0 || dev.is_null() {
                return None;
            }
            if (self.clock)(dev, NVML_CLOCK_GRAPHICS, &mut mhz) != 0 || (self.power)(dev, &mut mw) != 0 {
                return None;
            }
        }
        Some((mhz, mw as f32 / 1000.0))
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: shutdown pairs with the init in `load`; the module is freed exactly once and
        // its function pointers are never used afterwards.
        unsafe {
            if let Some(s) = self.shutdown {
                s();
            }
            FreeLibrary(self.module as *mut c_void);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUID_D1F3: u64 = 0xD1F3;

    #[test]
    fn parses_memory_instance() {
        assert_eq!(parse_instance("pid_1234_luid_0x00000000_0x0000D1F3_phys_0"), Some((1234, LUID_D1F3)));
    }

    #[test]
    fn parses_engine_instance() {
        let n = "pid_4_luid_0x00000001_0x0000D1F3_phys_0_eng_0_engtype_3D";
        assert_eq!(parse_instance(n), Some((4, (1u64 << 32) | LUID_D1F3)));
        let n = "pid_22136_luid_0x00000000_0x000109A5_phys_0_eng_3_engtype_VideoDecode";
        assert_eq!(parse_instance(n), Some((22136, 0x109A5)));
    }

    #[test]
    fn rejects_bad_instances() {
        assert_eq!(parse_instance(""), None);
        assert_eq!(parse_instance("_Total"), None);
        assert_eq!(parse_instance("pid_x_luid_0x00000000_0x0000D1F3_phys_0"), None);
        assert_eq!(parse_instance("pid_1_luid_00000000_0x0000D1F3_phys_0"), None);
        assert_eq!(parse_instance("pid_1_luid_0x00000000"), None);
        assert_eq!(parse_instance("pid_99999999999_luid_0x0_0x1"), None);
    }

    #[test]
    fn sums_per_pid_and_luid() {
        let rows = vec![
            ("pid_10_luid_0x00000000_0x0000D1F3_phys_0_eng_0_engtype_3D".to_string(), 10.0),
            ("pid_10_luid_0x00000000_0x0000D1F3_phys_0_eng_1_engtype_Copy".to_string(), 2.5),
            ("pid_10_luid_0x00000000_0x00001111_phys_0".to_string(), 7.0),
            ("pid_5_luid_0x00000000_0x0000D1F3_phys_0".to_string(), 1.0),
            ("garbage".to_string(), 99.0),
        ];
        let out = sum_by_pid(rows.into_iter());
        assert_eq!(out, vec![(5, LUID_D1F3, 1.0), (10, 0x1111, 7.0), (10, LUID_D1F3, 12.5)]);
    }

    fn adapter(luid: u64, integrated: bool) -> Adapter {
        Adapter { luid, integrated, ..Adapter::default() }
    }

    #[test]
    fn wrong_gpu_flags_igpu_hog_with_idle_dgpu() {
        let ads = [adapter(1, true), adapter(2, false)];
        let procs = [(100, 1, 35.0), (101, 1, 5.0), (102, 2, 2.0)];
        assert_eq!(wrong_gpu(&procs, &ads), vec![(100, 1)]);
    }

    #[test]
    fn wrong_gpu_quiet_when_dgpu_busy_or_missing() {
        let ads = [adapter(1, true), adapter(2, false)];
        assert!(wrong_gpu(&[(100, 1, 50.0), (101, 2, 40.0)], &ads).is_empty());
        assert!(wrong_gpu(&[(100, 1, 50.0)], &[adapter(1, true)]).is_empty());
        assert!(wrong_gpu(&[(100, 2, 90.0)], &ads).is_empty());
    }

    #[test]
    fn wrong_gpu_threshold_is_inclusive() {
        let ads = [adapter(1, true), adapter(2, false)];
        assert_eq!(wrong_gpu(&[(7, 1, 20.0)], &ads), vec![(7, 1)]);
        assert!(wrong_gpu(&[(7, 1, 19.9)], &ads).is_empty());
    }

    #[test]
    fn driver_date_reorders() {
        assert_eq!(parse_driver_date("10-14-2025").as_deref(), Some("2025-10-14"));
        assert_eq!(parse_driver_date("1-5-2024").as_deref(), Some("2024-01-05"));
        assert_eq!(parse_driver_date("13-1-2024"), None);
        assert_eq!(parse_driver_date("nope"), None);
    }

    #[test]
    fn version_formats() {
        assert_eq!(format_version((31u64 << 48) | (15u64 << 32) | (5222u64 << 16) | 7), "31.15.5222.7");
    }

    #[test]
    fn hags_values() {
        assert_eq!(hags_from(Some(2)), Some(true));
        assert_eq!(hags_from(Some(1)), Some(false));
        assert_eq!(hags_from(Some(0)), None);
        assert_eq!(hags_from(None), None);
    }

    #[test]
    fn link_maps_speed_codes() {
        assert_eq!(link_gen(1), Some(1));
        assert_eq!(link_gen(5), Some(5));
        assert_eq!(link_gen(0), None);
        let l = make_link(1, 16, Some(4), Some(16)).unwrap();
        assert_eq!(l, PcieLink { gen: 1, width: 16, max_gen: 4, max_width: 16 });
        assert_eq!(make_link(3, 8, None, None), Some(PcieLink { gen: 3, width: 8, max_gen: 3, max_width: 8 }));
        assert_eq!(make_link(0, 8, None, None), None);
    }

    #[test]
    fn luid_and_clock_conversion() {
        assert_eq!(luid_u64(LUID { LowPart: 0xD1F3, HighPart: 1 }), (1u64 << 32) | 0xD1F3);
        assert_eq!(luid_u64(LUID { LowPart: 5, HighPart: -1 }), 0xFFFF_FFFF_0000_0005);
        assert_eq!(to_mhz(0), None);
        assert_eq!(to_mhz(7000), Some(7000.0));
        assert_eq!(to_mhz(10_501_000_000), Some(10501.0));
    }

    #[test]
    fn perf_conversion() {
        // SAFETY: POD struct, zero is a valid value.
        let mut p: D3DKMT_ADAPTER_PERFDATA = unsafe { std::mem::zeroed() };
        assert_eq!(perf_from(&p), PerfData::default());
        p.Temperature = 655;
        p.FanRPM = 1800;
        p.Power = 473;
        p.MemoryFrequency = 9_001_000_000;
        p.MaxMemoryFrequency = 10_501_000_000;
        let d = perf_from(&p);
        assert_eq!(d.temp_c, Some(65.5));
        assert_eq!(d.fan_rpm, Some(1800));
        assert_eq!(d.power_pct, Some(47.3));
        assert_eq!(d.mem_mhz, Some(9001.0));
        assert_eq!(d.mem_max_mhz, Some(10501.0));
    }
}
