//! Facts for the Full speed page: is each part running as fast as it can?
//!
//! Read once when the page opens (and on Scan) on a worker thread. Reuses the device-page
//! readers (GPU, screens, drives, network, power, memory sticks) and adds what only this
//! page needs: the fastest refresh Windows offers per screen, the USB ports devices run at,
//! the fastest speed each Ethernet adapter can do, AC power and devices without a driver.
//! No admin, no driver of our own.

use super::{cpu, disk, display, gpu, net};
use crate::sensors::hw;
use std::sync::{Arc, Mutex};

/// A screen and the fastest refresh Windows offers for it at the current resolution.
#[derive(Clone, Debug)]
pub struct ScreenSpeed {
    pub s: display::Screen,
    /// Highest refresh in the mode list at the current size, Hz (0 = unknown).
    pub offered: u32,
}

/// What a USB device is, as far as speed matters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsbKind {
    Storage,
    Camera,
    Net,
    #[default]
    Other,
}

/// A device on a USB port, with the speed it can do and the speed it runs at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsbDev {
    pub name: String,
    pub kind: UsbKind,
    /// The device can do 5 Gbps (USB 3) or more.
    pub can_ss: bool,
    /// Running at 5 Gbps or more now.
    pub runs_ss: bool,
    /// Can do 10 Gbps or more.
    pub can_ssp: bool,
    /// Running at 10 Gbps or more now.
    pub runs_ssp: bool,
    /// UASP storage driver (fast mode) is in use.
    pub uas: bool,
}

/// An Ethernet or Wi-Fi adapter and the fastest link its driver lists (0 = unknown).
#[derive(Clone, Debug, Default)]
pub struct NetSpeed {
    pub a: net::AdapterFacts,
    pub max_bps: u64,
}

/// A drive and its PCIe link (NVMe only).
#[derive(Clone, Debug, Default)]
pub struct DriveSpeed {
    pub d: hw::Drive,
    /// (gen, width, max gen, max width).
    pub pcie: Option<(u8, u8, u8, u8)>,
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub gpus: Vec<gpu::Adapter>,
    pub screens: Vec<ScreenSpeed>,
    pub hw: hw::Hw,
    pub drives: Vec<DriveSpeed>,
    pub nets: Vec<NetSpeed>,
    pub power: Option<cpu::PowerMode>,
    /// Some(true) on mains power, Some(false) on battery, None unknown.
    pub on_ac: Option<bool>,
    /// The machine has a battery (a laptop or tablet).
    pub battery: bool,
    pub usb: Vec<UsbDev>,
    pub missing: Vec<crate::driver::Missing>,
}

/// Read everything. Slow (a second or two): call from a worker thread.
pub fn probe() -> Facts {
    let hw = hw::read_all();
    let drives = hw
        .drives
        .iter()
        .map(|d| DriveSpeed { d: d.clone(), pcie: if d.bus == "NVMe" { disk::extra(d.num).pcie } else { None } })
        .collect();
    let (on_ac, battery) = power_source();
    Facts {
        gpus: gpu::adapters(),
        screens: display::screens().into_iter().map(|s| ScreenSpeed { offered: offered_hz(&s.gdi, s.w, s.h), s }).collect(),
        drives,
        nets: net::adapters()
            .into_iter()
            .filter(|a| a.kind == "Ethernet" || a.kind == "Wi-Fi")
            .map(|a| NetSpeed { max_bps: nic_max_bps(&a.description), a })
            .collect(),
        power: cpu::power_mode(),
        on_ac,
        battery,
        usb: usb::devices(),
        missing: crate::driver::missing(),
        hw,
    }
}

/// Background reader: `start` kicks a read, `latest` hands out the newest one.
#[derive(Default)]
pub struct Probe {
    s: Arc<Mutex<(bool, Option<Arc<Facts>>)>>,
    pub started: bool,
}

impl Probe {
    pub fn start(&mut self) {
        let mut g = self.s.lock().unwrap_or_else(|e| e.into_inner());
        if g.0 {
            return;
        }
        g.0 = true;
        drop(g);
        self.started = true;
        let s = self.s.clone();
        let _ = std::thread::Builder::new().name("full-speed".into()).spawn(move || {
            let p = crate::perf::span("full-speed-probe");
            let f = Arc::new(probe());
            drop(p);
            let mut g = s.lock().unwrap_or_else(|e| e.into_inner());
            *g = (false, Some(f));
        });
    }

    pub fn running(&self) -> bool {
        self.s.lock().map(|g| g.0).unwrap_or(false)
    }

    pub fn latest(&self) -> Option<Arc<Facts>> {
        self.s.lock().ok().and_then(|g| g.1.clone())
    }
}

/// Fastest link speed in a driver's "Speed & Duplex" choice ("1.0 Gbps Full Duplex"), bit/s.
pub fn parse_speed_text(s: &str) -> Option<u64> {
    let l = s.to_ascii_lowercase();
    let (unit, mul) = if let Some(i) = l.find("gbps").or_else(|| l.find("gb/s")).or_else(|| l.find(" gb")) {
        (i, 1_000_000_000f64)
    } else if let Some(i) = l.find("mbps").or_else(|| l.find("mb/s")).or_else(|| l.find(" mb")) {
        (i, 1_000_000f64)
    } else {
        return None;
    };
    let head = l[..unit].trim_end();
    let start = head.rfind(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).map_or(0, |i| i + 1);
    let v: f64 = head[start..].replace(',', ".").parse().ok()?;
    (v > 0.0).then(|| (v * mul).round() as u64)
}

/// Best guess from the adapter name when the driver lists nothing ("Realtek PCIe 2.5GbE").
pub fn speed_from_name(desc: &str) -> u64 {
    let d = desc.to_ascii_lowercase();
    if d.contains("10gbe") || d.contains("10 gigabit") || d.contains("aqc1") || d.contains("x550") {
        10_000_000_000
    } else if d.contains("2.5g") || d.contains("i225") || d.contains("i226") || d.contains("rtl8125") || d.contains("e3100") || d.contains("e3000") {
        // Before 5G: "2.5gbe" contains "5gbe".
        2_500_000_000
    } else if d.contains("5gbe") || d.contains("5 gigabit") {
        5_000_000_000
    } else if d.contains("gbe") || d.contains("gigabit") || d.contains("1000") || d.contains("i219") || d.contains("i211") || d.contains("i210") || d.contains("rtl8168") || d.contains("e2600") || d.contains("e2500") {
        1_000_000_000
    } else {
        0
    }
}

#[cfg(windows)]
fn nic_max_bps(desc: &str) -> u64 {
    let listed = win::nic_choices(desc).iter().filter_map(|s| parse_speed_text(s)).max().unwrap_or(0);
    listed.max(speed_from_name(desc))
}

#[cfg(not(windows))]
fn nic_max_bps(desc: &str) -> u64 {
    speed_from_name(desc)
}

#[cfg(windows)]
fn offered_hz(gdi: &str, w: u32, h: u32) -> u32 {
    win::offered_hz(gdi, w, h)
}

#[cfg(not(windows))]
fn offered_hz(_: &str, _: u32, _: u32) -> u32 {
    0
}

#[cfg(windows)]
fn power_source() -> (Option<bool>, bool) {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: zeroed plain out struct.
    let mut p: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: valid out pointer.
    if unsafe { GetSystemPowerStatus(&mut p) } == 0 {
        return (None, false);
    }
    let ac = match p.ACLineStatus {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    (ac, p.BatteryFlag != 128 && p.BatteryFlag != 255)
}

#[cfg(not(windows))]
fn power_source() -> (Option<bool>, bool) {
    (None, false)
}

/// Set a screen's refresh rate (keeps its size). True when Windows took it.
#[cfg(windows)]
pub fn set_refresh(gdi: &str, hz: u32) -> bool {
    win::set_refresh(gdi, hz)
}

#[cfg(not(windows))]
pub fn set_refresh(_: &str, _: u32) -> bool {
    false
}

#[cfg(windows)]
mod win {
    use crate::util::wide;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::Registry::*;

    pub fn offered_hz(gdi: &str, w: u32, h: u32) -> u32 {
        let name = wide(gdi);
        let mut best = 0;
        for i in 0..2000u32 {
            // SAFETY: zeroed DEVMODEW with dmSize set.
            let mut m: DEVMODEW = unsafe { std::mem::zeroed() };
            m.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            // SAFETY: valid device name and out struct; 0 = end of the list.
            if unsafe { EnumDisplaySettingsExW(name.as_ptr(), i, &mut m, 0) } == 0 {
                break;
            }
            if m.dmPelsWidth == w && m.dmPelsHeight == h && m.dmDisplayFrequency > 1 {
                best = best.max(m.dmDisplayFrequency);
            }
        }
        best
    }

    pub fn set_refresh(gdi: &str, hz: u32) -> bool {
        let name = wide(gdi);
        // SAFETY: zeroed DEVMODEW with dmSize set.
        let mut m: DEVMODEW = unsafe { std::mem::zeroed() };
        m.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        // SAFETY: valid name and out struct.
        if unsafe { EnumDisplaySettingsExW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut m, 0) } == 0 {
            return false;
        }
        m.dmDisplayFrequency = hz;
        m.dmFields = DM_DISPLAYFREQUENCY | DM_PELSWIDTH | DM_PELSHEIGHT;
        // SAFETY: mode filled from the current one; saved to the registry like Settings does.
        let r = unsafe { ChangeDisplaySettingsExW(name.as_ptr(), &m, std::ptr::null_mut(), CDS_UPDATEREGISTRY, std::ptr::null()) };
        r == DISP_CHANGE_SUCCESSFUL
    }

    fn key(root: HKEY, path: &str) -> Option<HKEY> {
        let p = wide(path);
        let mut k: HKEY = std::ptr::null_mut();
        // SAFETY: valid path and out key.
        (unsafe { RegOpenKeyExW(root, p.as_ptr(), 0, KEY_READ, &mut k) } == 0).then_some(k)
    }

    fn string(k: HKEY, name: &str) -> Option<String> {
        let n = wide(name);
        let mut buf = [0u16; 260];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: buffer and length agree.
        let r = unsafe { RegGetValueW(k, std::ptr::null(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        (r == 0).then(|| crate::util::from_wide(&buf))
    }

    fn subkeys(k: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..512u32 {
            let mut buf = [0u16; 64];
            let mut len = buf.len() as u32;
            // SAFETY: buffer and length agree.
            let r = unsafe { RegEnumKeyExW(k, i, buf.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if r != 0 {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }

    /// The texts of the adapter's "Speed & Duplex" choices, from its driver's registry key.
    pub fn nic_choices(desc: &str) -> Vec<String> {
        const NET: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";
        let Some(class) = key(HKEY_LOCAL_MACHINE, NET) else { return Vec::new() };
        let mut out = Vec::new();
        for sk in subkeys(class) {
            let Some(k) = key(class, &sk) else { continue };
            let same = string(k, "DriverDesc").is_some_and(|d| d.eq_ignore_ascii_case(desc.trim()));
            if same {
                if let Some(e) = key(k, r"Ndi\params\*SpeedDuplex\enum") {
                    for i in 0..64u32 {
                        let mut nb = [0u16; 64];
                        let mut nl = nb.len() as u32;
                        let mut db = [0u16; 128];
                        let mut dl = (db.len() * 2) as u32;
                        let mut ty = 0u32;
                        // SAFETY: buffers and lengths agree.
                        let r = unsafe { RegEnumValueW(e, i, nb.as_mut_ptr(), &mut nl, std::ptr::null(), &mut ty, db.as_mut_ptr().cast(), &mut dl) };
                        if r != 0 {
                            break;
                        }
                        if ty == REG_SZ {
                            out.push(crate::util::from_wide(&db));
                        }
                    }
                    // SAFETY: key we opened.
                    unsafe { RegCloseKey(e) };
                }
            }
            // SAFETY: key we opened.
            unsafe { RegCloseKey(k) };
            if same {
                break;
            }
        }
        // SAFETY: key we opened.
        unsafe { RegCloseKey(class) };
        out
    }
}

/// USB devices and the speed their port runs them at (hub port queries, no admin).
mod usb {
    use super::{UsbDev, UsbKind};

    /// Flags of USB_NODE_CONNECTION_INFORMATION_EX_V2.
    pub const OP_SS: u32 = 1;
    pub const CAN_SS: u32 = 2;
    pub const OP_SSP: u32 = 4;
    pub const CAN_SSP: u32 = 8;

    /// Speed-sensitive kind from the services and classes on the device and its children.
    pub fn kind(services: &[String], classes: &[String]) -> (UsbKind, bool) {
        let has_s = |n: &str| services.iter().any(|s| s.eq_ignore_ascii_case(n));
        let has_c = |n: &str| classes.iter().any(|s| s.eq_ignore_ascii_case(n));
        let uas = has_s("UASPStor");
        if uas || has_s("USBSTOR") || has_c("DiskDrive") {
            (UsbKind::Storage, uas)
        } else if has_c("Camera") || has_c("Image") {
            (UsbKind::Camera, false)
        } else if has_c("Net") {
            (UsbKind::Net, false)
        } else {
            (UsbKind::Other, false)
        }
    }

    pub fn dev_from(name: String, flags: u32, services: &[String], classes: &[String]) -> UsbDev {
        let (kind, uas) = kind(services, classes);
        UsbDev {
            name,
            kind,
            can_ss: flags & CAN_SS != 0,
            runs_ss: flags & OP_SS != 0,
            can_ssp: flags & CAN_SSP != 0,
            runs_ssp: flags & OP_SSP != 0,
            uas,
        }
    }

    #[cfg(not(windows))]
    pub fn devices() -> Vec<UsbDev> {
        Vec::new()
    }

    #[cfg(windows)]
    pub fn devices() -> Vec<UsbDev> {
        win::devices()
    }

    #[cfg(windows)]
    mod win {
        use super::*;
        use crate::driver::{prop_multi_pub as multi, prop_str_pub as pstr};
        use crate::util::wide;
        use std::collections::HashMap;
        use windows_sys::core::GUID;
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
        use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_WRITE, OPEN_EXISTING};
        use windows_sys::Win32::System::IO::DeviceIoControl;

        const HUB: GUID = GUID::from_u128(0xf18a0e88_c30c_11d0_8815_00a0c906bed8);
        const NODE_INFO: u32 = 0x0022_0408;
        const CONN_INFO_EX: u32 = 0x0022_0448;
        const DRIVERKEY: u32 = 0x0022_0420;
        const CONN_INFO_V2: u32 = 0x0022_045C;

        /// Friendly name, services and classes of every present USB device, by driver key.
        fn by_driver_key() -> HashMap<String, (String, Vec<String>, Vec<String>)> {
            let mut m = HashMap::new();
            let usb = wide("USB");
            // SAFETY: enumerator string valid; set destroyed below.
            let set = unsafe { SetupDiGetClassDevsW(std::ptr::null(), usb.as_ptr(), std::ptr::null_mut(), DIGCF_ALLCLASSES | DIGCF_PRESENT) };
            if set == INVALID_HANDLE_VALUE as isize || set == 0 {
                return m;
            }
            for i in 0..1024u32 {
                // SAFETY: zeroed with cbSize set.
                let mut d: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
                d.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
                // SAFETY: valid set; failure = end.
                if unsafe { SetupDiEnumDeviceInfo(set, i, &mut d) } == 0 {
                    break;
                }
                let node = d.DevInst;
                let drv = pstr(node, CM_DRP_DRIVER);
                if drv.is_empty() {
                    continue;
                }
                let mut name = pstr(node, CM_DRP_FRIENDLYNAME);
                if name.is_empty() {
                    name = pstr(node, CM_DRP_DEVICEDESC);
                }
                let (mut sv, mut cl) = (vec![pstr(node, CM_DRP_SERVICE)], vec![pstr(node, CM_DRP_CLASS)]);
                // Children: interfaces of a composite device, the disk of a drive.
                let mut child = 0u32;
                // SAFETY: out pointer valid.
                if unsafe { CM_Get_Child(&mut child, node, 0) } == CR_SUCCESS {
                    for _ in 0..16 {
                        sv.push(pstr(child, CM_DRP_SERVICE));
                        cl.push(pstr(child, CM_DRP_CLASS));
                        if name.to_ascii_lowercase().contains("composite") || name.to_ascii_lowercase().contains("mass storage") {
                            let f = pstr(child, CM_DRP_FRIENDLYNAME);
                            if !f.is_empty() {
                                name = f;
                            }
                        }
                        let mut next = 0u32;
                        // SAFETY: out pointer valid.
                        if unsafe { CM_Get_Sibling(&mut next, child, 0) } != CR_SUCCESS {
                            break;
                        }
                        child = next;
                    }
                }
                let _ = multi;
                m.insert(drv.to_ascii_lowercase(), (name, sv, cl));
            }
            // SAFETY: set from SetupDiGetClassDevsW.
            unsafe { SetupDiDestroyDeviceInfoList(set) };
            m
        }

        fn hub_paths() -> Vec<Vec<u16>> {
            let mut out = Vec::new();
            // SAFETY: GUID valid; set destroyed below.
            let set = unsafe { SetupDiGetClassDevsW(&HUB, std::ptr::null(), std::ptr::null_mut(), DIGCF_DEVICEINTERFACE | DIGCF_PRESENT) };
            if set == INVALID_HANDLE_VALUE as isize || set == 0 {
                return out;
            }
            for i in 0..128u32 {
                // SAFETY: zeroed with cbSize set.
                let mut ifd: SP_DEVICE_INTERFACE_DATA = unsafe { std::mem::zeroed() };
                ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
                // SAFETY: valid set; failure = end.
                if unsafe { SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &HUB, i, &mut ifd) } == 0 {
                    break;
                }
                let mut buf = vec![0u8; 1024];
                // cbSize of the detail struct is 8 on 64-bit (DWORD + one WCHAR, aligned).
                buf[..4].copy_from_slice(&(if cfg!(target_pointer_width = "64") { 8u32 } else { 6u32 }).to_le_bytes());
                // SAFETY: buffer large enough for the stated size.
                let ok = unsafe { SetupDiGetDeviceInterfaceDetailW(set, &ifd, buf.as_mut_ptr().cast(), buf.len() as u32, std::ptr::null_mut(), std::ptr::null_mut()) };
                if ok == 0 {
                    continue;
                }
                let w: Vec<u16> = buf[4..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).chain([0]).collect();
                out.push(w);
            }
            // SAFETY: set from SetupDiGetClassDevsW.
            unsafe { SetupDiDestroyDeviceInfoList(set) };
            out
        }

        fn ioctl(h: windows_sys::Win32::Foundation::HANDLE, code: u32, buf: &mut [u8]) -> Option<usize> {
            let mut got = 0u32;
            // SAFETY: same buffer for in and out, length stated.
            let ok = unsafe { DeviceIoControl(h, code, buf.as_ptr().cast(), buf.len() as u32, buf.as_mut_ptr().cast(), buf.len() as u32, &mut got, std::ptr::null_mut()) };
            (ok != 0).then_some(got as usize)
        }

        pub fn devices() -> Vec<UsbDev> {
            let names = by_driver_key();
            let mut out = Vec::new();
            for path in hub_paths() {
                // SAFETY: path is NUL-terminated.
                let h = unsafe { CreateFileW(path.as_ptr(), GENERIC_WRITE, FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
                if h == INVALID_HANDLE_VALUE {
                    continue;
                }
                let mut ni = [0u8; 80];
                let ports = if ioctl(h, NODE_INFO, &mut ni).is_some() { ni[6] as u32 } else { 0 };
                for port in 1..=ports {
                    // USB_NODE_CONNECTION_INFORMATION_EX (packed): ConnectionStatus at 31, DeviceIsHub at 24.
                    let mut ci = [0u8; 512];
                    ci[..4].copy_from_slice(&port.to_le_bytes());
                    if ioctl(h, CONN_INFO_EX, &mut ci).is_none() {
                        continue;
                    }
                    let status = u32::from_le_bytes([ci[31], ci[32], ci[33], ci[34]]);
                    if status != 1 || ci[24] != 0 {
                        continue;
                    }
                    let mut v2 = [0u8; 16];
                    v2[..4].copy_from_slice(&port.to_le_bytes());
                    v2[4..8].copy_from_slice(&16u32.to_le_bytes());
                    v2[8..12].copy_from_slice(&4u32.to_le_bytes());
                    let flags = if ioctl(h, CONN_INFO_V2, &mut v2).is_some() { u32::from_le_bytes([v2[12], v2[13], v2[14], v2[15]]) } else { 0 };
                    let mut dk = [0u8; 600];
                    dk[..4].copy_from_slice(&port.to_le_bytes());
                    let key = if ioctl(h, DRIVERKEY, &mut dk).is_some() {
                        let w: Vec<u16> = dk[8..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                        String::from_utf16_lossy(&w).to_ascii_lowercase()
                    } else {
                        String::new()
                    };
                    let (name, sv, cl) = names.get(&key).cloned().unwrap_or_default();
                    out.push(dev_from(name, flags, &sv, &cl));
                }
                // SAFETY: handle we opened.
                unsafe { CloseHandle(h) };
            }
            out
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn v(x: &[&str]) -> Vec<String> {
            x.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn kinds() {
            assert_eq!(kind(&v(&["UASPStor"]), &v(&["SCSIAdapter"])), (UsbKind::Storage, true));
            assert_eq!(kind(&v(&["USBSTOR", "disk"]), &v(&["USB", "DiskDrive"])), (UsbKind::Storage, false));
            assert_eq!(kind(&v(&["usbccgp", "usbvideo"]), &v(&["USB", "Camera"])), (UsbKind::Camera, false));
            assert_eq!(kind(&v(&["HidUsb"]), &v(&["HIDClass"])), (UsbKind::Other, false));
        }

        #[test]
        fn flags() {
            let d = dev_from("SSD".into(), CAN_SS | CAN_SSP, &v(&["UASPStor"]), &v(&[]));
            assert!(d.can_ss && !d.runs_ss && d.can_ssp && !d.runs_ssp && d.uas);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_texts() {
        assert_eq!(parse_speed_text("1.0 Gbps Full Duplex"), Some(1_000_000_000));
        assert_eq!(parse_speed_text("2.5 Gbps Full Duplex"), Some(2_500_000_000));
        assert_eq!(parse_speed_text("100 Mbps Full Duplex"), Some(100_000_000));
        assert_eq!(parse_speed_text("10 Mbps Half Duplex"), Some(10_000_000));
        assert_eq!(parse_speed_text("Auto Negotiation"), None);
        assert_eq!(parse_speed_text("10Gbps Full Duplex"), Some(10_000_000_000));
    }

    #[test]
    fn names() {
        assert_eq!(speed_from_name("Realtek PCIe GbE Family Controller"), 1_000_000_000);
        assert_eq!(speed_from_name("Intel(R) Ethernet Controller I226-V"), 2_500_000_000);
        assert_eq!(speed_from_name("Realtek PCIe 2.5GbE Family Controller"), 2_500_000_000);
        assert_eq!(speed_from_name("Some USB Ethernet"), 0);
    }
}
