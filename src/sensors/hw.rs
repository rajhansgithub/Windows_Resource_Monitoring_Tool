//! Hardware inventory for the Monitor page: Windows edition, device and board, BIOS,
//! memory sticks, displays, GPU driver, every physical drive (model, bus, SSD or HDD,
//! size, temperature) and every drive letter. Read on a worker thread (a drive or a
//! firmware call can be slow), only while the Monitor page is open. No admin: drives are
//! opened with zero access, which the storage queries used here allow.

use crate::util::{read_dword, wide};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows_sys::Win32::System::IO::DeviceIoControl;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
/// Re-read (drives plugged in, temperatures, free space) this often while open.
pub const REFRESH_SECS: f64 = 15.0;

/// One memory slot (SMBIOS type 17).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dimm {
    /// "DIMM A1", "ChannelA-DIMM0".
    pub slot: String,
    /// Bytes; 0 = empty slot.
    pub size: u64,
    pub maker: String,
    pub part: String,
    /// Configured speed, MT/s (0 = unknown).
    pub speed: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Display {
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub hz: f32,
    /// "HDMI", "DisplayPort", "Built-in", ...
    pub link: String,
    pub primary: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drive {
    /// PhysicalDrive number (matches "Disk N" of the PhysicalDisk counters).
    pub num: u32,
    pub model: String,
    /// "NVMe", "SATA", "USB", ...
    pub bus: String,
    /// Some(true) SSD, Some(false) HDD, None unknown.
    pub ssd: Option<bool>,
    pub size: u64,
    pub removable: bool,
    /// Degrees Celsius when the drive reports it.
    pub temp: Option<i16>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vol {
    /// "C:".
    pub letter: String,
    pub label: String,
    /// "NTFS".
    pub fs: String,
    /// "Fixed", "Removable", "Network", "Optical", "RAM disk".
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
    /// Physical drive it lives on.
    pub disk: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hw {
    /// "Windows 11 Pro".
    pub os: String,
    /// "24H2 · build 26100.2314".
    pub os_ver: String,
    pub host: String,
    /// "ASUS System Product Name" style maker + model (empty when generic).
    pub device: String,
    pub board: String,
    /// "American Megatrends 2803 · 04/12/2024".
    pub bios: String,
    /// "DDR5".
    pub mem_type: String,
    pub dimms: Vec<Dimm>,
    pub displays: Vec<Display>,
    pub gpu_driver: String,
    pub drives: Vec<Drive>,
    pub vols: Vec<Vol>,
}

impl Hw {
    pub fn drive(&self, num: u32) -> Option<&Drive> {
        self.drives.iter().find(|d| d.num == num)
    }
    /// Filled memory slots and total slots.
    pub fn slots(&self) -> (usize, usize) {
        (self.dimms.iter().filter(|d| d.size > 0).count(), self.dimms.len())
    }
}

/// Reads `Hw` on a worker thread; the page polls `latest`.
#[derive(Default)]
pub struct HwReader {
    out: Arc<Mutex<Option<Hw>>>,
    busy: Arc<Mutex<bool>>,
    age: Option<f64>,
    pub latest: Arc<Hw>,
}

impl HwReader {
    /// Call every sample while the page is open. Starts a read when due; picks up a finished one.
    pub fn tick(&mut self, elapsed: f64) -> bool {
        let mut changed = false;
        if let Some(h) = self.out.lock().ok().and_then(|mut o| o.take()) {
            if *self.latest != h {
                self.latest = Arc::new(h);
                changed = true;
            }
        }
        let due = self.age.is_none_or(|a| a + elapsed >= REFRESH_SECS);
        if !due {
            if let Some(a) = &mut self.age {
                *a += elapsed;
            }
            return changed;
        }
        if self.busy.lock().map(|b| *b).unwrap_or(true) {
            return changed;
        }
        self.age = Some(0.0);
        if let Ok(mut b) = self.busy.lock() {
            *b = true;
        }
        let (out, busy) = (self.out.clone(), self.busy.clone());
        let full = self.latest.os.is_empty();
        let prev = (*self.latest).clone();
        let spawned = std::thread::Builder::new().name("hw-inventory".into()).spawn(move || {
            let h = if full { read_all() } else { Hw { drives: drives(), vols: vols(), ..prev } };
            if let Ok(mut o) = out.lock() {
                *o = Some(h);
            }
            if let Ok(mut b) = busy.lock() {
                *b = false;
            }
        });
        if spawned.is_err() {
            if let Ok(mut b) = self.busy.lock() {
                *b = false;
            }
        }
        changed
    }

    /// The page closed: read again from scratch next time (cheap enough, and catches changes).
    pub fn reset(&mut self) {
        self.age = None;
    }
}

fn read_all() -> Hw {
    let mut h = Hw::default();
    os(&mut h);
    smbios(&mut h);
    h.displays = displays();
    h.gpu_driver = gpu_driver();
    h.drives = drives();
    h.vols = vols();
    h
}

// ---------------------------------------------------------------- Windows

fn os(h: &mut Hw) {
    const CV: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let get = |n: &str| super::detail::reg_string(CV, n).unwrap_or_default();
    let build = get("CurrentBuildNumber");
    let mut name = get("ProductName");
    // Windows 11 still reports "Windows 10" here.
    if build.parse::<u32>().unwrap_or(0) >= 22000 {
        name = name.replace("Windows 10", "Windows 11");
    }
    h.os = name;
    let ubr = read_dword(HKEY_LOCAL_MACHINE, CV, "UBR");
    let b = match ubr {
        Some(u) if !build.is_empty() => format!("build {build}.{u}"),
        _ if !build.is_empty() => format!("build {build}"),
        _ => String::new(),
    };
    let dv = get("DisplayVersion");
    h.os_ver = [dv, b].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" \u{00B7} ");
    h.host = std::env::var("COMPUTERNAME").unwrap_or_default();
}

// ---------------------------------------------------------------- SMBIOS

fn smbios(h: &mut Hw) {
    use windows_sys::Win32::System::SystemInformation::GetSystemFirmwareTable;
    const RSMB: u32 = u32::from_be_bytes(*b"RSMB");
    // SAFETY: sizing call with a null buffer.
    let n = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
    if n == 0 {
        return;
    }
    let mut buf = vec![0u8; n as usize];
    // SAFETY: buffer of the size Windows asked for.
    let got = unsafe { GetSystemFirmwareTable(RSMB, 0, buf.as_mut_ptr().cast(), n) };
    if got == 0 || got as usize > buf.len() || buf.len() < 8 {
        return;
    }
    let len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let data = &buf[8..(8 + len).min(buf.len())];
    parse_smbios(data, h);
}

/// Strings of one structure (1-based in the table).
fn sm_strings(tail: &[u8]) -> (Vec<String>, usize) {
    let mut v = Vec::new();
    let mut i = 0;
    if tail.len() >= 2 && tail[0] == 0 && tail[1] == 0 {
        return (v, 2);
    }
    while i < tail.len() {
        let end = tail[i..].iter().position(|&c| c == 0).map_or(tail.len(), |p| i + p);
        v.push(String::from_utf8_lossy(&tail[i..end]).trim().to_string());
        i = end + 1;
        if i < tail.len() && tail[i] == 0 {
            return (v, i + 1);
        }
    }
    (v, tail.len())
}

/// Firmware placeholders that mean "not filled in".
fn junk(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    s.is_empty()
        || ["to be filled", "default string", "system product name", "system manufacturer", "not specified", "o.e.m.", "unknown", "none", "undefined"]
            .iter()
            .any(|j| l.contains(j))
}

pub(crate) fn parse_smbios(data: &[u8], h: &mut Hw) {
    let mut p = 0usize;
    let (mut maker, mut model) = (String::new(), String::new());
    while p + 4 <= data.len() {
        let (ty, flen) = (data[p], data[p + 1] as usize);
        if flen < 4 || p + flen > data.len() {
            break;
        }
        let f = &data[p..p + flen];
        let (strs, slen) = sm_strings(&data[p + flen..]);
        let s = |off: usize| -> String {
            f.get(off).and_then(|&i| if i == 0 { None } else { strs.get(i as usize - 1) }).cloned().unwrap_or_default()
        };
        let w = |off: usize| -> u16 { if off + 1 < f.len() { u16::from_le_bytes([f[off], f[off + 1]]) } else { 0 } };
        let dw = |off: usize| -> u32 {
            if off + 3 < f.len() {
                u32::from_le_bytes([f[off], f[off + 1], f[off + 2], f[off + 3]])
            } else {
                0
            }
        };
        match ty {
            0 => {
                let parts: Vec<String> = [s(4), s(5)].into_iter().filter(|x| !junk(x)).collect();
                let date = s(8);
                h.bios = parts.join(" ");
                if !date.is_empty() {
                    h.bios = format!("{} \u{00B7} {}", h.bios, date).trim_start_matches(" \u{00B7} ").to_string();
                }
            }
            1 => {
                maker = s(4);
                model = s(5);
            }
            2 if h.board.is_empty() => {
                let parts: Vec<String> = [s(4), s(5)].into_iter().filter(|x| !junk(x)).collect();
                h.board = parts.join(" ");
            }
            17 => {
                let sz = w(0x0C);
                let size: u64 = match sz {
                    0 | 0xFFFF => 0,
                    0x7FFF => u64::from(dw(0x1C) & 0x7FFF_FFFF) << 20,
                    v if v & 0x8000 != 0 => u64::from(v & 0x7FFF) << 10,
                    v => u64::from(v) << 20,
                };
                let speed = if flen > 0x21 && w(0x20) > 0 { w(0x20) } else { w(0x15) };
                if size > 0 && h.mem_type.is_empty() {
                    h.mem_type = mem_type(f.get(0x12).copied().unwrap_or(0)).to_string();
                }
                let slot = [s(0x10)].into_iter().find(|x| !x.is_empty()).unwrap_or_else(|| s(0x11));
                let clean = |x: String| if junk(&x) { String::new() } else { x };
                h.dimms.push(Dimm { slot, size, maker: clean(s(0x17)), part: clean(s(0x1A)), speed: u32::from(speed) });
            }
            127 => break,
            _ => {}
        }
        p += flen + slen;
    }
    let dev: Vec<String> = [maker, model].into_iter().filter(|x| !junk(x)).collect();
    h.device = dev.join(" ");
}

fn mem_type(t: u8) -> &'static str {
    match t {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "",
    }
}

// ---------------------------------------------------------------- displays

fn displays() -> Vec<Display> {
    use windows_sys::Win32::Devices::Display::*;
    let mut out = Vec::new();
    let (mut np, mut nm) = (0u32, 0u32);
    // SAFETY: plain sizing call.
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) } != 0 {
        return out;
    }
    // SAFETY: zeroed plain-old-data arrays of the sizes Windows asked for.
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![unsafe { std::mem::zeroed() }; np as usize];
    let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![unsafe { std::mem::zeroed() }; nm as usize];
    // SAFETY: arrays sized above; no topology id requested.
    let r = unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) };
    if r != 0 {
        return out;
    }
    for p in &paths[..np as usize] {
        // SAFETY: zeroed out struct with the header filled in as the API requires.
        let mut tn: DISPLAYCONFIG_TARGET_DEVICE_NAME = unsafe { std::mem::zeroed() };
        tn.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        tn.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        tn.header.adapterId = p.targetInfo.adapterId;
        tn.header.id = p.targetInfo.id;
        // SAFETY: valid packet.
        let name = if unsafe { DisplayConfigGetDeviceInfo(&mut tn.header) } == 0 {
            let n = tn.monitorFriendlyDeviceName.iter().position(|&c| c == 0).unwrap_or(64);
            String::from_utf16_lossy(&tn.monitorFriendlyDeviceName[..n])
        } else {
            String::new()
        };
        let link = match p.targetInfo.outputTechnology {
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED => "Built-in",
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI => "HDMI",
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL => "DisplayPort",
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI => "DVI",
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15 => "VGA",
            _ => "",
        };
        let rr = p.targetInfo.refreshRate;
        let hz = if rr.Denominator > 0 { rr.Numerator as f32 / rr.Denominator as f32 } else { 0.0 };
        // SAFETY: union read; modeInfoIdx is valid for active paths without virtual modes.
        let mi = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let (mut w, mut h, mut primary) = (0, 0, false);
        if let Some(m) = modes.get(mi).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) {
            // SAFETY: infoType says the union holds a source mode.
            let sm = unsafe { m.Anonymous.sourceMode };
            w = sm.width;
            h = sm.height;
            primary = sm.position.x == 0 && sm.position.y == 0;
        }
        let name = if name.is_empty() { if link == "Built-in" { "Built-in display".to_string() } else { "Display".to_string() } } else { name };
        out.push(Display { name, w, h, hz, link: link.to_string(), primary });
    }
    out.sort_by_key(|d| !d.primary);
    out
}

// ---------------------------------------------------------------- GPU driver

fn gpu_driver() -> String {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
    // SAFETY: plain COM factory creation.
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return String::new() };
    let mut best: Option<(u64, String)> = None;
    for i in 0..8u32 {
        // SAFETY: valid factory; an error ends the list.
        let Ok(a) = (unsafe { f.EnumAdapters1(i) }) else { break };
        // SAFETY: valid adapter.
        let Ok(d) = (unsafe { a.GetDesc1() }) else { continue };
        if d.Flags & 2 != 0 {
            continue;
        }
        // SAFETY: valid adapter; returns the user-mode driver version.
        if let Ok(v) = unsafe { a.CheckInterfaceSupport(&IDXGIDevice::IID) } {
            let v = v as u64;
            let s = format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF);
            let ded = d.DedicatedVideoMemory as u64;
            if best.as_ref().is_none_or(|b| ded > b.0) {
                best = Some((ded, s));
            }
        }
    }
    best.map(|b| b.1).unwrap_or_default()
}

// ---------------------------------------------------------------- drives

struct Dev(HANDLE);
impl Drop for Dev {
    fn drop(&mut self) {
        // SAFETY: handle we opened.
        unsafe { CloseHandle(self.0) };
    }
}

/// Open a device for queries only (no read access needed, so no admin).
fn open(path: &str) -> Option<Dev> {
    let p = wide(path);
    // SAFETY: NUL-terminated path; zero access.
    let h = unsafe { CreateFileW(p.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Dev(h))
}

fn ioctl(d: &Dev, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut got = 0u32;
    // SAFETY: buffers valid for their lengths; synchronous call.
    let ok = unsafe {
        DeviceIoControl(
            d.0,
            code,
            if input.is_empty() { std::ptr::null() } else { input.as_ptr().cast() },
            input.len() as u32,
            out.as_mut_ptr().cast(),
            out.len() as u32,
            &mut got,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(got as usize)
}

/// STORAGE_PROPERTY_QUERY { PropertyId, QueryType = standard }.
fn prop(d: &Dev, id: u32, out: &mut [u8]) -> Option<usize> {
    let mut q = [0u8; 12];
    q[..4].copy_from_slice(&id.to_le_bytes());
    ioctl(d, IOCTL_STORAGE_QUERY_PROPERTY, &q, out)
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn cstr_at(b: &[u8], o: usize) -> String {
    if o == 0 || o >= b.len() {
        return String::new();
    }
    let end = b[o..].iter().position(|&c| c == 0).map_or(b.len(), |p| o + p);
    String::from_utf8_lossy(&b[o..end]).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bus(t: u32) -> &'static str {
    match t {
        0x11 => "NVMe",
        0x0B | 0x03 => "SATA",
        0x07 => "USB",
        0x0A => "SAS",
        0x01 => "SCSI",
        0x08 => "RAID",
        0x0C | 0x0D => "SD card",
        0x0E | 0x0F => "Virtual",
        0x10 => "Storage Spaces",
        0x13 => "UFS",
        0x09 => "iSCSI",
        _ => "",
    }
}

fn drives() -> Vec<Drive> {
    let mut out = Vec::new();
    let mut misses = 0;
    for num in 0..64u32 {
        let Some(d) = open(&format!(r"\\.\PhysicalDrive{num}")) else {
            misses += 1;
            if misses > 8 {
                break;
            }
            continue;
        };
        misses = 0;
        let mut b = vec![0u8; 1024];
        let mut drv = Drive { num, ..Default::default() };
        if let Some(n) = prop(&d, 0, &mut b) {
            let b = &b[..n.min(b.len())];
            drv.removable = b.get(10).is_some_and(|&x| x != 0);
            let vendor = cstr_at(b, u32_at(b, 12) as usize);
            let product = cstr_at(b, u32_at(b, 16) as usize);
            drv.model = if vendor.is_empty() || product.starts_with(&vendor) { product } else { format!("{vendor} {product}") };
            drv.bus = bus(u32_at(b, 28)).to_string();
        }
        let mut sp = [0u8; 16];
        // StorageDeviceSeekPenaltyProperty: IncursSeekPenalty at byte 8.
        if prop(&d, 7, &mut sp).is_some_and(|n| n > 8) {
            drv.ssd = Some(sp[8] == 0);
        }
        if drv.bus == "NVMe" {
            drv.ssd = Some(true);
        }
        let mut g = [0u8; 256];
        if ioctl(&d, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut g).is_some_and(|n| n >= 32) {
            drv.size = u64::from_le_bytes(g[24..32].try_into().unwrap_or([0; 8]));
        }
        // StorageDeviceTemperatureProperty: first sensor's Temperature at byte 26.
        let mut t = [0u8; 128];
        if prop(&d, 10, &mut t).is_some_and(|n| n >= 28) && u16::from_le_bytes([t[12], t[13]]) > 0 {
            let c = i16::from_le_bytes([t[26], t[27]]);
            drv.temp = (c > -40 && c < 150).then_some(c);
        }
        out.push(drv);
    }
    out
}

fn vols() -> Vec<Vol> {
    let mut out = Vec::new();
    // SAFETY: plain query.
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = format!("{}:", char::from(b'A' + i as u8));
        let root = wide(&format!("{letter}\\"));
        // SAFETY: NUL-terminated root path.
        let kind = match unsafe { GetDriveTypeW(root.as_ptr()) } {
            2 => "Removable",
            3 => "Fixed",
            4 => "Network",
            5 => "Optical",
            6 => "RAM disk",
            _ => continue,
        };
        let mut v = Vol { letter: letter.clone(), kind, ..Default::default() };
        // A network share can stall for seconds when offline: list it, don't touch it.
        if kind != "Network" {
            let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
            // SAFETY: valid out pointers.
            if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
                // No media (empty card reader or disc drive).
                if kind != "Fixed" {
                    continue;
                }
            }
            v.total = total;
            v.free = avail;
            let (mut label, mut fs) = ([0u16; 64], [0u16; 32]);
            // SAFETY: buffers of the stated lengths.
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    label.as_mut_ptr(),
                    label.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                )
            } != 0;
            if ok {
                let s = |b: &[u16]| String::from_utf16_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]);
                v.label = s(&label);
                v.fs = s(&fs);
            }
            if let Some(d) = open(&format!(r"\\.\{letter}")) {
                let mut e = [0u8; 256];
                if ioctl(&d, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut e).is_some_and(|n| n >= 12) && u32_at(&e, 0) > 0 {
                    v.disk = Some(u32_at(&e, 8));
                }
            }
        }
        if v.total == 0 && kind == "Fixed" {
            continue;
        }
        out.push(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one SMBIOS structure: formatted bytes after the 4-byte header, then strings.
    fn st(ty: u8, body: &[u8], strs: &[&str]) -> Vec<u8> {
        let mut v = vec![ty, (body.len() + 4) as u8, 0, 0];
        v.extend_from_slice(body);
        if strs.is_empty() {
            v.extend_from_slice(&[0, 0]);
        } else {
            for s in strs {
                v.extend_from_slice(s.as_bytes());
                v.push(0);
            }
            v.push(0);
        }
        v
    }

    #[test]
    fn smbios_board_bios_and_sticks() {
        let mut d = Vec::new();
        // BIOS: vendor 1, version 2, date 3.
        d.extend(st(0, &[1, 2, 0, 0, 3], &["AMI", "2803", "04/12/2024"]));
        d.extend(st(1, &[1, 2], &["System manufacturer", "System Product Name"]));
        d.extend(st(2, &[1, 2], &["ASUSTeK", "ROG STRIX B550-F"]));
        // Memory device: 16 GB DDR4 3200 in DIMM_A1, then an empty slot.
        let mut m = vec![0u8; 0x22 - 4];
        let put = |m: &mut Vec<u8>, off: usize, b: &[u8]| m[off - 4..off - 4 + b.len()].copy_from_slice(b);
        put(&mut m, 0x0C, &16384u16.to_le_bytes());
        put(&mut m, 0x10, &[1]);
        put(&mut m, 0x12, &[0x1A]);
        put(&mut m, 0x15, &3200u16.to_le_bytes());
        put(&mut m, 0x17, &[2]);
        put(&mut m, 0x1A, &[3]);
        put(&mut m, 0x20, &3200u16.to_le_bytes());
        d.extend(st(17, &m, &["DIMM_A1", "G.Skill", "F4-3200C16"]));
        let mut e = vec![0u8; 0x22 - 4];
        put(&mut e, 0x10, &[1]);
        d.extend(st(17, &e, &["DIMM_A2"]));
        d.extend(st(127, &[], &[]));
        let mut h = Hw::default();
        parse_smbios(&d, &mut h);
        assert_eq!(h.bios, "AMI 2803 \u{00B7} 04/12/2024");
        assert_eq!(h.device, "");
        assert_eq!(h.board, "ASUSTeK ROG STRIX B550-F");
        assert_eq!(h.mem_type, "DDR4");
        assert_eq!(h.dimms.len(), 2);
        assert_eq!(h.dimms[0], Dimm { slot: "DIMM_A1".into(), size: 16 << 30, maker: "G.Skill".into(), part: "F4-3200C16".into(), speed: 3200 });
        assert_eq!(h.slots(), (1, 2));
    }
}
