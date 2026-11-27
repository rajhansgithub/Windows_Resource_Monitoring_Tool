//! Disk detail page: NVMe health log (no admin), firmware, serial, TRIM, PCIe link speed,
//! a wear-based life forecast, and an admin-only SATA SMART read. Everything is read with
//! storage IOCTLs on `\\.\PhysicalDriveN`, opened with zero access where possible.
//! Static facts (`extra`) are for a worker thread; `nvme_health` is cheap (one IOCTL).
#![allow(dead_code)]

use crate::util::wide;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Parent, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, CR_SUCCESS, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
};
use windows_sys::Win32::Devices::Properties::DEVPROPKEY;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, SMART_RCV_DRIVE_DATA,
};

/// STORAGE_PROPERTY_ID values used here.
const PROP_DEVICE: u32 = 0;
const PROP_TRIM: u32 = 8;
const PROP_PROTOCOL_SPECIFIC: u32 = 50;
/// STORAGE_BUS_TYPE of NVMe.
const BUS_NVME: u32 = 0x11;
/// Bytes of one NVMe SMART / Health Information log page.
const NVME_LOG_LEN: usize = 512;
/// sizeof(STORAGE_PROTOCOL_SPECIFIC_DATA) in current SDKs (ten u32).
const PSD_LEN: usize = 40;
/// offsetof(STORAGE_PROPERTY_QUERY, AdditionalParameters).
const QUERY_HDR: usize = 8;
/// One NVMe "data unit" is 1000 blocks of 512 bytes.
const NVME_UNIT_BYTES: u128 = 512_000;

/// DEVPKEY_PciDevice_* property set; ids 9..12 read from the Windows SDK metadata that
/// windows-sys is generated from (CurrentLinkSpeed 9, CurrentLinkWidth 10, MaxLinkSpeed 11,
/// MaxLinkWidth 12).
const PCI_FMTID: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
const PCI_CUR_SPEED: u32 = 9;
const PCI_CUR_WIDTH: u32 = 10;
const PCI_MAX_SPEED: u32 = 11;
const PCI_MAX_WIDTH: u32 = 12;

/// Health and wear of one drive, from the NVMe log or (admin) SATA SMART.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    /// NVMe "Percentage Used" (vendor estimate of life consumed); may exceed 100.
    pub pct_used: u8,
    /// 100 - min(pct_used, 100): percent of rated life left.
    pub life_left: u8,
    /// Available spare capacity, percent (0 when the source does not report it).
    pub spare: u8,
    /// Spare level below which the drive raises a warning, percent.
    pub spare_threshold: u8,
    /// NVMe critical warning bit mask (0 = fine; SMART path always 0).
    pub critical_warning: u8,
    /// Composite temperature, degrees Celsius.
    pub temp_c: Option<i16>,
    /// Host data written, bytes.
    pub written_bytes: u128,
    /// Host data read, bytes.
    pub read_bytes: u128,
    pub power_on_hours: u64,
    pub power_cycles: u64,
    pub unsafe_shutdowns: u64,
    /// NVMe media and data integrity errors; SATA reallocated sector count.
    pub media_errors: u64,
    /// "NVMe" or "SMART (admin)".
    pub source: &'static str,
}

/// Static drive facts for the detail page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiskExtra {
    pub firmware: String,
    pub serial: String,
    /// TRIM enabled (None when the drive does not say).
    pub trim: Option<bool>,
    /// (current gen, current width, max gen, max width) of the PCIe link; NVMe drives only.
    pub pcie: Option<(u8, u8, u8, u8)>,
    pub nvme: bool,
}

// ---------------------------------------------------------------- handles

struct Dev(HANDLE);
impl Drop for Dev {
    fn drop(&mut self) {
        // SAFETY: handle we opened.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_access(path: &str, access: u32) -> Option<Dev> {
    let p = wide(path);
    // SAFETY: NUL-terminated path; sharing allows the system to keep using the drive.
    let h = unsafe { CreateFileW(p.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Dev(h))
}

fn drive_path(num: u32) -> String {
    format!(r"\\.\PhysicalDrive{num}")
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

/// STORAGE_PROPERTY_QUERY { PropertyId, PropertyStandardQuery }.
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

// ---------------------------------------------------------------- NVMe health

/// Little-endian 128-bit counter at `o` (0 when out of range).
fn u128_at(b: &[u8], o: usize) -> u128 {
    b.get(o..o + 16).map_or(0, |x| {
        let mut a = [0u8; 16];
        a.copy_from_slice(x);
        u128::from_le_bytes(a)
    })
}

fn sat64(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// Parse the 512-byte NVMe SMART / Health Information log (log page 2).
pub fn parse_nvme_health(log: &[u8]) -> Option<Health> {
    if log.len() < 176 {
        return None;
    }
    let kelvin = i32::from(u16::from_le_bytes([log[1], log[2]]));
    let c = kelvin - 273;
    // 0 K means "not reported".
    let temp_c = (kelvin > 0 && c > -40 && c < 150).then_some(c as i16);
    let pct_used = log[5];
    Some(Health {
        pct_used,
        life_left: 100 - pct_used.min(100),
        spare: log[3],
        spare_threshold: log[4],
        critical_warning: log[0],
        temp_c,
        read_bytes: u128_at(log, 32).saturating_mul(NVME_UNIT_BYTES),
        written_bytes: u128_at(log, 48).saturating_mul(NVME_UNIT_BYTES),
        power_cycles: sat64(u128_at(log, 112)),
        power_on_hours: sat64(u128_at(log, 128)),
        unsafe_shutdowns: sat64(u128_at(log, 144)),
        media_errors: sat64(u128_at(log, 160)),
        source: "NVMe",
    })
}

/// Ask the drive for its NVMe health log (no admin on Windows 10+; falls back to a read
/// handle). None for non-NVMe drives or when the driver refuses.
pub fn nvme_health(num: u32) -> Option<Health> {
    let path = drive_path(num);
    for access in [0, GENERIC_READ] {
        let Some(d) = open_access(&path, access) else { continue };
        if let Some(h) = nvme_health_on(&d) {
            return Some(h);
        }
    }
    None
}

fn nvme_health_on(d: &Dev) -> Option<Health> {
    // STORAGE_PROPERTY_QUERY header, STORAGE_PROTOCOL_SPECIFIC_DATA, then room for the log.
    let mut buf = [0u8; QUERY_HDR + PSD_LEN + NVME_LOG_LEN];
    let put = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, PROP_PROTOCOL_SPECIFIC); // PropertyId
    put(&mut buf, 4, 0); // PropertyStandardQuery
    put(&mut buf, 8, 3); // ProtocolTypeNvme
    put(&mut buf, 12, 2); // NVMeDataTypeLogPage
    put(&mut buf, 16, 2); // ProtocolDataRequestValue = NVME_LOG_PAGE_HEALTH_INFO
    put(&mut buf, 20, 0); // ProtocolDataRequestSubValue
    put(&mut buf, 24, PSD_LEN as u32); // ProtocolDataOffset
    put(&mut buf, 28, NVME_LOG_LEN as u32); // ProtocolDataLength
    let input = buf;
    let n = ioctl(d, IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut buf)?;
    // Output is a STORAGE_PROTOCOL_DATA_DESCRIPTOR (8 bytes) + specific data, then the log.
    let off = QUERY_HDR + u32_at(&buf, 24) as usize;
    let len = (u32_at(&buf, 28) as usize).min(NVME_LOG_LEN);
    let end = (off + len).min(n.min(buf.len()));
    if off >= end {
        return None;
    }
    parse_nvme_health(&buf[off..end])
}

// ---------------------------------------------------------------- forecast

/// Years of life left at `daily_bytes` written per day, from the drive's own wear figure:
/// bytes per percent used = written / pct_used. None without data (no wear yet, nothing
/// written, or no write rate). Already worn out gives Some(0.0).
pub fn forecast(h: &Health, daily_bytes: f64) -> Option<f64> {
    if h.pct_used == 0 || h.written_bytes == 0 || !daily_bytes.is_finite() || daily_bytes <= 0.0 {
        return None;
    }
    let left_pct = 100u32.saturating_sub(u32::from(h.pct_used));
    let per_pct = h.written_bytes as f64 / f64::from(h.pct_used);
    Some(f64::from(left_pct) * per_pct / daily_bytes / 365.0)
}

// ---------------------------------------------------------------- extra facts

/// Firmware, serial, TRIM and PCIe link of PhysicalDriveN. Static: call once per drive.
pub fn extra(num: u32) -> DiskExtra {
    let mut x = DiskExtra::default();
    let Some(d) = open_access(&drive_path(num), 0) else { return x };
    let mut b = vec![0u8; 1024];
    if let Some(n) = prop(&d, PROP_DEVICE, &mut b) {
        let b = &b[..n.min(b.len())];
        x.firmware = cstr_at(b, u32_at(b, 20) as usize);
        x.serial = cstr_at(b, u32_at(b, 24) as usize).trim_end_matches('.').to_string();
        x.nvme = u32_at(b, 28) == BUS_NVME;
    }
    // DEVICE_TRIM_DESCRIPTOR { Version, Size, TrimEnabled } at byte 8.
    let mut t = [0u8; 16];
    if prop(&d, PROP_TRIM, &mut t).is_some_and(|n| n > 8) {
        x.trim = Some(t[8] != 0);
    }
    drop(d);
    if x.nvme {
        x.pcie = pcie_link(num);
    }
    x
}

/// PCIe link of the NVMe controller behind PhysicalDriveN.
fn pcie_link(num: u32) -> Option<(u8, u8, u8, u8)> {
    let mut node = disk_devnode(num)?;
    // Disk -> storage miniport -> PCI function; the property sits on the PCI device.
    for _ in 0..8 {
        if let Some(cur_speed) = dev_u32(node, PCI_CUR_SPEED) {
            let cur_width = dev_u32(node, PCI_CUR_WIDTH).unwrap_or(0);
            let max_speed = dev_u32(node, PCI_MAX_SPEED).unwrap_or(cur_speed);
            let max_width = dev_u32(node, PCI_MAX_WIDTH).unwrap_or(cur_width);
            let c = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
            return Some((c(cur_speed), c(cur_width), c(max_speed), c(max_width)));
        }
        let mut parent = 0u32;
        // SAFETY: valid out pointer; a failure ends the walk.
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
            return None;
        }
        node = parent;
    }
    None
}

/// One unsigned device property (any integer width up to 4 bytes) of a devnode.
fn dev_u32(node: u32, pid: u32) -> Option<u32> {
    let key = DEVPROPKEY { fmtid: PCI_FMTID, pid };
    let (mut ty, mut size) = (0u32, 8u32);
    let mut buf = [0u8; 8];
    // SAFETY: key and buffer valid for the stated sizes.
    let r = unsafe { CM_Get_DevNode_PropertyW(node, &key, &mut ty, buf.as_mut_ptr(), &mut size, 0) };
    if r != CR_SUCCESS || size == 0 || size > 4 {
        return None;
    }
    let mut a = [0u8; 4];
    a[..size as usize].copy_from_slice(&buf[..size as usize]);
    Some(u32::from_le_bytes(a))
}

/// Device instance (DevInst) of the disk whose PhysicalDrive number is `num`.
fn disk_devnode(num: u32) -> Option<u32> {
    let guid = GUID_DEVINTERFACE_DISK;
    // SAFETY: class GUID pointer valid; present disk interfaces only.
    let set = unsafe { SetupDiGetClassDevsW(&guid, std::ptr::null(), std::ptr::null_mut(), DIGCF_DEVICEINTERFACE | DIGCF_PRESENT) };
    if set == INVALID_HANDLE_VALUE as isize || set == 0 {
        return None;
    }
    let mut found = None;
    for i in 0..64u32 {
        // SAFETY: zeroed plain data with cbSize set as the API requires.
        let mut ifd: SP_DEVICE_INTERFACE_DATA = unsafe { std::mem::zeroed() };
        ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        // SAFETY: valid set and out struct; failure (no more items) ends the search.
        if unsafe { SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &guid, i, &mut ifd) } == 0 {
            break;
        }
        found = interface_devnode(set, &ifd, num);
        if found.is_some() {
            break;
        }
    }
    // SAFETY: set came from SetupDiGetClassDevsW above.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    found
}

/// DevInst of one disk interface when it is PhysicalDrive `num`.
fn interface_devnode(set: isize, ifd: &SP_DEVICE_INTERFACE_DATA, num: u32) -> Option<u32> {
    // 4-byte cbSize + the NUL-terminated path; 1 KiB is far more than a disk path needs.
    let mut buf = [0u8; 1024];
    let cb = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    buf[..4].copy_from_slice(&cb.to_le_bytes());
    // SAFETY: zeroed plain data with cbSize set.
    let mut info: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
    // SAFETY: buffer valid for its length; its first u32 holds cbSize as required.
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set, ifd, buf.as_mut_ptr().cast(), buf.len() as u32, std::ptr::null_mut(), &mut info)
    };
    if ok == 0 {
        return None;
    }
    let chars: Vec<u16> = buf[4..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    let mut path = chars;
    path.push(0);
    // SAFETY: NUL-terminated path; zero access.
    let h = unsafe { CreateFileW(path.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return None;
    }
    let d = Dev(h);
    // STORAGE_DEVICE_NUMBER { DeviceType, DeviceNumber, PartitionNumber }.
    let mut n = [0u8; 12];
    let got = ioctl(&d, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut n)?;
    (got >= 8 && u32_at(&n, 4) == num).then_some(info.DevInst)
}

// ---------------------------------------------------------------- SATA SMART (admin)

/// Raw 48-bit value of an attribute entry.
fn raw48(e: &[u8]) -> u64 {
    e[5..11].iter().rev().fold(0u64, |a, &b| (a << 8) | u64::from(b))
}

/// Parse the 512-byte SMART "read attribute values" sector (30 twelve-byte entries after a
/// 2-byte revision). Best effort across vendors; `pct_used` is 0 when no wear attribute is
/// present. None when the sector holds no attribute at all.
pub fn parse_smart(attrs: &[u8]) -> Option<Health> {
    let table = attrs.get(2..362)?;
    let mut h = Health { source: "SMART (admin)", ..Health::default() };
    let (mut any, mut temp194, mut temp190) = (false, None, None);
    let mut wear: [Option<u8>; 3] = [None; 3]; // 231, 233, 177: normalized value
    let mut life_known = false;
    for e in table.chunks_exact(12) {
        let (id, cur, raw) = (e[0], e[3], raw48(e));
        if id == 0 {
            continue;
        }
        any = true;
        let low32 = raw & 0xFFFF_FFFF;
        match id {
            5 => h.media_errors = raw,
            9 => h.power_on_hours = low32,
            12 => h.power_cycles = low32,
            174 => h.unsafe_shutdowns = raw,
            192 if h.unsafe_shutdowns == 0 => h.unsafe_shutdowns = raw,
            194 => temp194 = Some(raw & 0xFF),
            190 => temp190 = Some(raw & 0xFF),
            231 => wear[0] = Some(cur),
            233 => wear[1] = Some(cur),
            177 => wear[2] = Some(cur),
            241 => h.written_bytes = u128::from(raw) * 512,
            242 => h.read_bytes = u128::from(raw) * 512,
            _ => {}
        }
    }
    if !any {
        return None;
    }
    // Normalized wear values count down from 100 (life left); 1..=100 is the valid range.
    if let Some(left) = wear.iter().flatten().copied().find(|v| (1..=100).contains(v)) {
        h.pct_used = 100 - left;
        life_known = true;
    }
    h.life_left = if life_known { 100 - h.pct_used.min(100) } else { 100 };
    h.temp_c = temp194.or(temp190).and_then(|t| i16::try_from(t).ok()).filter(|&t| t > 0 && t < 150);
    Some(h)
}

/// Read SATA SMART attributes. Needs admin (the drive handle needs read and write access);
/// returns None at once when that is denied, for NVMe drives, or when the driver refuses.
pub fn smart_sata(num: u32) -> Option<Health> {
    let d = open_access(&drive_path(num), GENERIC_READ | GENERIC_WRITE)?;
    // SENDCMDINPARAMS (packed), without its trailing 1-byte buffer.
    let mut inp = [0u8; 32];
    inp[..4].copy_from_slice(&512u32.to_le_bytes()); // cBufferSize
    inp[4] = 0xD0; // bFeaturesReg: SMART READ DATA
    inp[5] = 1; // bSectorCountReg
    inp[6] = 1; // bSectorNumberReg
    inp[7] = 0x4F; // bCylLowReg
    inp[8] = 0xC2; // bCylHighReg
    inp[9] = 0xA0 | ((num as u8 & 1) << 4); // bDriveHeadReg
    inp[10] = 0xB0; // bCommandReg: SMART
    inp[12] = num as u8; // bDriveNumber
    // SENDCMDOUTPARAMS: cBufferSize (4) + DRIVERSTATUS (12), then the 512-byte sector.
    let mut out = [0u8; 16 + 512];
    let n = ioctl(&d, SMART_RCV_DRIVE_DATA, &inp, &mut out)?;
    if n < 16 + 362 || out[4] != 0 {
        return None;
    }
    parse_smart(&out[16..16 + 512])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put128(log: &mut [u8], o: usize, v: u128) {
        log[o..o + 16].copy_from_slice(&v.to_le_bytes());
    }

    fn sample_log() -> Vec<u8> {
        let mut l = vec![0u8; 512];
        l[0] = 0;
        l[1..3].copy_from_slice(&(310u16).to_le_bytes()); // 310 K = 37 C
        l[3] = 100;
        l[4] = 10;
        l[5] = 7;
        put128(&mut l, 32, 1_000);
        put128(&mut l, 48, 2_000);
        put128(&mut l, 112, 321);
        put128(&mut l, 128, 4_321);
        put128(&mut l, 144, 12);
        put128(&mut l, 160, 3);
        l
    }

    #[test]
    fn nvme_log_parses() {
        let h = parse_nvme_health(&sample_log()).expect("parses");
        assert_eq!(h.pct_used, 7);
        assert_eq!(h.life_left, 93);
        assert_eq!((h.spare, h.spare_threshold, h.critical_warning), (100, 10, 0));
        assert_eq!(h.temp_c, Some(37));
        assert_eq!(h.read_bytes, 1_000 * 512_000);
        assert_eq!(h.written_bytes, 2_000 * 512_000);
        assert_eq!((h.power_cycles, h.power_on_hours, h.unsafe_shutdowns, h.media_errors), (321, 4_321, 12, 3));
        assert_eq!(h.source, "NVMe");
    }

    #[test]
    fn nvme_log_edge_cases() {
        assert!(parse_nvme_health(&[0u8; 100]).is_none());
        let mut l = sample_log();
        l[5] = 130; // worn past rated life
        l[1] = 0;
        l[2] = 0; // temperature not reported
        let h = parse_nvme_health(&l).expect("parses");
        assert_eq!((h.pct_used, h.life_left, h.temp_c), (130, 0, None));
        put128(&mut l, 128, u128::MAX);
        assert_eq!(parse_nvme_health(&l).map(|h| h.power_on_hours), Some(u64::MAX));
    }

    #[test]
    fn forecast_from_wear() {
        let h = Health { pct_used: 10, written_bytes: 10 * 1_000_000_000_000, ..Health::default() };
        // 1 TB per percent, 90 percent left, 1 TB per day => 90 days.
        let y = forecast(&h, 1e12).expect("some");
        assert!((y - 90.0 / 365.0).abs() < 1e-9);
        assert_eq!(forecast(&h, 0.0), None);
        assert_eq!(forecast(&h, f64::NAN), None);
        assert_eq!(forecast(&Health::default(), 1e9), None);
        let worn = Health { pct_used: 120, written_bytes: 1 << 40, ..Health::default() };
        assert_eq!(forecast(&worn, 1e9), Some(0.0));
    }

    fn entry(t: &mut [u8], i: usize, id: u8, cur: u8, raw: u64) {
        let e = &mut t[2 + i * 12..2 + (i + 1) * 12];
        e[0] = id;
        e[3] = cur;
        e[5..11].copy_from_slice(&raw.to_le_bytes()[..6]);
    }

    #[test]
    fn smart_attributes_parse() {
        let mut t = vec![0u8; 512];
        entry(&mut t, 0, 5, 100, 4);
        entry(&mut t, 1, 9, 99, 12_345);
        entry(&mut t, 2, 12, 99, 678);
        entry(&mut t, 3, 194, 60, 0x0000_0022_0000_0028); // low byte 40 C
        entry(&mut t, 4, 231, 88, 0);
        entry(&mut t, 5, 241, 100, 1_000_000);
        let h = parse_smart(&t).expect("parses");
        assert_eq!((h.media_errors, h.power_on_hours, h.power_cycles), (4, 12_345, 678));
        assert_eq!(h.temp_c, Some(40));
        assert_eq!((h.pct_used, h.life_left), (12, 88));
        assert_eq!(h.written_bytes, 1_000_000 * 512);
        assert_eq!(h.source, "SMART (admin)");
    }

    #[test]
    fn smart_empty_and_unknown_wear() {
        assert!(parse_smart(&vec![0u8; 512]).is_none());
        assert!(parse_smart(&[0u8; 10]).is_none());
        let mut t = vec![0u8; 512];
        entry(&mut t, 0, 9, 99, 10);
        let h = parse_smart(&t).expect("parses");
        assert_eq!((h.pct_used, h.life_left), (0, 100));
    }
}
