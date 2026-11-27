//! Display "link truth": what Windows really sends to each screen, plus panel identity.
//!
//! Sources, all user-mode and admin-free:
//! * `QueryDisplayConfig` + `DisplayConfigGetDeviceInfo`: friendly name, GDI name, connector,
//!   configured mode, bits per channel, colour encoding, advanced-colour (HDR) state, SDR white.
//! * DXGI `IDXGIOutput6::GetDesc1`: HDR luminance and panel primaries, matched by GDI name.
//! * The monitor's EDID from `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\...\Device Parameters`.
//! * `GetDpiForMonitor`: the Windows scale factor per monitor.
//!
//! `DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2` (type 15, Windows 11 24H2) is not in
//! windows-sys 0.59, so it is skipped; the v1 query already gives bits and encoding.
//!
//! `screens()` is a one-shot static read (a few ms, call it from a worker thread).

#![allow(dead_code)]

use crate::util::{from_wide, wide};
use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::{LPARAM, LUID, RECT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForMonitor;

/// One EDID-derived panel identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edid {
    /// 3-letter PNP id, followed by the brand in brackets when known ("GSM (LG)").
    pub maker: String,
    /// Manufacturer product code.
    pub product: u16,
    /// 32-bit serial number from the EDID header (0 when unset).
    pub serial: u32,
    /// Monitor name from descriptor 0xFC.
    pub model: String,
    /// Week of manufacture (0 when unknown).
    pub week: u8,
    /// Year of manufacture (or model year).
    pub year: u16,
    /// Physical image width in cm (0 for projectors / unknown).
    pub width_cm: u8,
    /// Physical image height in cm.
    pub height_cm: u8,
    /// Diagonal in inches, 0.1 resolution (0 when size unknown).
    pub diag_in: f32,
    /// Vertical refresh range in Hz (min, max) from the range-limits descriptor 0xFD.
    pub range: Option<(u32, u32)>,
    /// CTA-861 HDR static metadata data block present.
    pub hdr_static: bool,
}

/// One active screen and the link Windows drives it with.
#[derive(Clone, Debug)]
pub struct Screen {
    /// Friendly monitor name.
    pub name: String,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub gdi: String,
    /// True for the desktop's primary monitor (origin 0,0).
    pub primary: bool,
    /// Desktop width in pixels.
    pub w: u32,
    /// Desktop height in pixels.
    pub h: u32,
    /// Configured refresh rate in Hz.
    pub hz: f32,
    /// "HDMI", "DisplayPort", "Internal", "DVI", "VGA", "USB-C", "Wireless" or "Other".
    pub connector: &'static str,
    /// Locally unique id of the GPU driving this screen.
    pub adapter_luid: u64,
    /// Bits per colour channel on the link.
    pub bits: Option<u32>,
    /// "RGB", "YCbCr 4:4:4", "YCbCr 4:2:2", "YCbCr 4:2:0", "Intensity" or "" when unknown.
    pub encoding: &'static str,
    /// Windows reports advanced colour (HDR) support for this screen.
    pub hdr_supported: bool,
    /// HDR is currently on (PQ / ST.2084 output).
    pub hdr_on: bool,
    /// SDR content white level in nits (only meaningful while HDR is on).
    pub sdr_white_nits: Option<f32>,
    /// Peak luminance in nits.
    pub max_nits: Option<f32>,
    /// Full-frame (sustained) luminance in nits.
    pub max_full_frame_nits: Option<f32>,
    /// Black level in nits.
    pub min_nits: Option<f32>,
    /// Red, green, blue and white point as CIE 1931 xy.
    pub primaries: Option<[(f32, f32); 4]>,
    /// Windows display scale in percent (100, 125, 150 ...).
    pub dpi_scale: Option<u32>,
    /// Parsed EDID, when the registry has one.
    pub edid: Option<Edid>,
}

// ---------------------------------------------------------------- EDID

/// Known PNP ids worth a brand name.
const MAKERS: &[(&str, &str)] = &[
    ("DEL", "Dell"),
    ("SAM", "Samsung"),
    ("GSM", "LG"),
    ("ACR", "Acer"),
    ("AUS", "ASUS"),
    ("BNQ", "BenQ"),
    ("HWP", "HP"),
    ("LEN", "Lenovo"),
    ("PHL", "Philips"),
    ("VSC", "ViewSonic"),
    ("GBT", "Gigabyte"),
    ("LGD", "LG Display"),
    ("SDC", "Samsung Display"),
    ("BOE", "BOE"),
    ("AUO", "AU Optronics"),
    ("CMN", "Chimei Innolux"),
    ("NEC", "NEC"),
    ("SNY", "Sony"),
];

/// "GSM" -> "GSM (LG)"; unknown ids and ids that are their own brand stay bare.
fn maker_label(id: &str) -> String {
    match MAKERS.iter().find(|(k, _)| *k == id) {
        Some((_, name)) if *name != id => format!("{id} ({name})"),
        _ => id.to_string(),
    }
}

/// Decode the 5-bit-per-letter manufacturer id from EDID bytes 8-9.
fn pnp_id(hi: u8, lo: u8) -> String {
    let v = ((hi as u16) << 8) | lo as u16;
    [(v >> 10) & 0x1F, (v >> 5) & 0x1F, v & 0x1F]
        .iter()
        .map(|&c| if (1..=26).contains(&c) { (b'A' + c as u8 - 1) as char } else { '?' })
        .collect()
}

/// Text of a display descriptor: 13 bytes, ended by 0x0A and padded with spaces.
fn descriptor_text(d: &[u8]) -> String {
    let t = &d[5..18];
    let end = t.iter().position(|&c| c == 0x0A).unwrap_or(t.len());
    String::from_utf8_lossy(&t[..end]).trim().to_string()
}

/// Does this 128-byte CTA-861 extension hold an HDR static metadata block (extended tag 6)?
fn cta_has_hdr_static(blk: &[u8]) -> bool {
    if blk.len() < 128 || blk[0] != 0x02 {
        return false;
    }
    let end = (blk[2] as usize).min(127);
    let mut i = 4;
    while i < end {
        let tag = blk[i] >> 5;
        let len = (blk[i] & 0x1F) as usize;
        if i + 1 + len > end {
            break;
        }
        if tag == 7 && len >= 1 && blk[i + 1] == 6 {
            return true;
        }
        i += 1 + len;
    }
    false
}

/// Parse an EDID blob (128-byte base block plus optional extensions). `None` when it is too
/// short, lacks the fixed header or fails the base block checksum.
pub fn parse_edid(b: &[u8]) -> Option<Edid> {
    if b.len() < 128 || b[..8] != [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0] {
        return None;
    }
    if b[..128].iter().fold(0u8, |a, &x| a.wrapping_add(x)) != 0 {
        return None;
    }
    let week = if b[16] == 0xFF { 0 } else { b[16] };
    let (w, h) = (b[21], b[22]);
    let diag = if w > 0 && h > 0 { (((w as f32).hypot(h as f32) / 2.54) * 10.0).round() / 10.0 } else { 0.0 };
    let mut e = Edid {
        maker: maker_label(&pnp_id(b[8], b[9])),
        product: u16::from_le_bytes([b[10], b[11]]),
        serial: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        model: String::new(),
        week,
        year: 1990 + b[17] as u16,
        width_cm: w,
        height_cm: h,
        diag_in: diag,
        range: None,
        hdr_static: false,
    };
    for off in [54usize, 72, 90, 108] {
        let d = &b[off..off + 18];
        if d[0] != 0 || d[1] != 0 || d[2] != 0 {
            continue; // a detailed timing, not a descriptor
        }
        match d[3] {
            0xFC => e.model = descriptor_text(d),
            0xFD => {
                // Offset flags in byte 4: bit0 raises the vertical max by 255, bit1 also the min.
                let add_max = if d[4] & 0x01 != 0 { 255 } else { 0 };
                let add_min = if d[4] & 0x02 != 0 { 255 } else { 0 };
                let (lo, hi) = (d[5] as u32 + add_min, d[6] as u32 + add_max);
                if lo > 0 && hi >= lo {
                    e.range = Some((lo, hi));
                }
            }
            _ => {}
        }
    }
    let ext = b[126] as usize;
    for i in 1..=ext {
        match b.get(i * 128..(i + 1) * 128) {
            Some(blk) if cta_has_hdr_static(blk) => {
                e.hdr_static = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    Some(e)
}

/// `\\?\DISPLAY#GSM5B7F#5&1a2b3c&0&UID4352#{guid}` -> registry path of its EDID value's key.
fn edid_key_from_path(dev_path: &str) -> Option<String> {
    let mut it = dev_path.split('#');
    let head = it.next()?;
    if !head.to_ascii_uppercase().ends_with("DISPLAY") {
        return None;
    }
    let hwid = it.next().filter(|s| !s.is_empty())?;
    let inst = it.next().filter(|s| !s.is_empty())?;
    Some(format!(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{hwid}\{inst}\Device Parameters"))
}

/// Read the raw EDID bytes for a monitor device path from the registry.
fn read_edid(dev_path: &str) -> Option<Vec<u8>> {
    let key = wide(&edid_key_from_path(dev_path)?);
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: NUL-terminated key; `h` receives a handle closed below.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut h) } != 0 {
        return None;
    }
    let name = wide("EDID");
    let mut len = 0u32;
    // SAFETY: size query only (null data pointer).
    let r = unsafe { RegQueryValueExW(h, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), &mut len) };
    let mut out = None;
    if r == 0 && (128..=64 * 1024).contains(&len) {
        let mut buf = vec![0u8; len as usize];
        let mut n = len;
        // SAFETY: buffer holds `n` bytes as announced.
        let r = unsafe { RegQueryValueExW(h, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), buf.as_mut_ptr(), &mut n) };
        if r == 0 {
            buf.truncate(n as usize);
            out = Some(buf);
        }
    }
    // SAFETY: handle opened above, closed once.
    unsafe { RegCloseKey(h) };
    out
}

// ---------------------------------------------------------------- DisplayConfig helpers

/// Call `DisplayConfigGetDeviceInfo` for a packet type `T` whose first field is the header.
fn get_info<T: Copy>(ty: DISPLAYCONFIG_DEVICE_INFO_TYPE, adapter: LUID, id: u32) -> Option<T> {
    // SAFETY: the display structs are plain-old-data; all-zero is valid.
    let mut v: T = unsafe { std::mem::zeroed() };
    // SAFETY: every DISPLAYCONFIG_* packet starts with DISPLAYCONFIG_DEVICE_INFO_HEADER.
    let hdr = unsafe { &mut *(&mut v as *mut T as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER) };
    hdr.r#type = ty;
    hdr.size = std::mem::size_of::<T>() as u32;
    hdr.adapterId = adapter;
    hdr.id = id;
    // SAFETY: valid, correctly sized packet.
    if unsafe { DisplayConfigGetDeviceInfo(hdr) } == 0 {
        Some(v)
    } else {
        None
    }
}

/// Map `DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY` to a short connector name.
fn connector_name(t: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> &'static str {
    match t {
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
        | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
        | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
        | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS => "Internal",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI => "HDMI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EXTERNAL => "DisplayPort",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI => "DVI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15 => "VGA",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL => "USB-C",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST => "Wireless",
        _ => "Other",
    }
}

fn encoding_name(e: DISPLAYCONFIG_COLOR_ENCODING) -> &'static str {
    match e {
        DISPLAYCONFIG_COLOR_ENCODING_RGB => "RGB",
        DISPLAYCONFIG_COLOR_ENCODING_YCBCR444 => "YCbCr 4:4:4",
        DISPLAYCONFIG_COLOR_ENCODING_YCBCR422 => "YCbCr 4:2:2",
        DISPLAYCONFIG_COLOR_ENCODING_YCBCR420 => "YCbCr 4:2:0",
        DISPLAYCONFIG_COLOR_ENCODING_INTENSITY => "Intensity",
        _ => "",
    }
}

fn luid_u64(l: LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

// ---------------------------------------------------------------- DXGI

/// HDR/colour facts for one DXGI output.
struct DxgiOut {
    name: String,
    bits: u32,
    pq: bool,
    min: f32,
    max: f32,
    full: f32,
    prim: [(f32, f32); 4],
}

/// `DXGI_OUTPUT_DESC1` by hand: the `windows` crate gates `GetDesc1` behind its `Win32_Graphics_Gdi`
/// feature, which this project does not enable.
#[repr(C)]
struct OutDesc1 {
    name: [u16; 32],
    rect: [i32; 4],
    attached: i32,
    rotation: i32,
    monitor: isize,
    bits: u32,
    color_space: i32,
    red: [f32; 2],
    green: [f32; 2],
    blue: [f32; 2],
    white: [f32; 2],
    min: f32,
    max: f32,
    full: f32,
}

/// vtable slot of `IDXGIOutput6::GetDesc1`: IUnknown 3 + IDXGIObject 4 + IDXGIOutput 12 +
/// Output1 4 + Output2..5 1 each = 27.
const GETDESC1_SLOT: usize = 27;

/// Call `IDXGIOutput6::GetDesc1` through the vtable.
fn output_desc1(o: &windows::Win32::Graphics::Dxgi::IDXGIOutput6) -> Option<OutDesc1> {
    use windows::core::Interface;
    type GetDesc1Fn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut OutDesc1) -> i32;
    // SAFETY: zeroed POD that the callee fills.
    let mut d: OutDesc1 = unsafe { std::mem::zeroed() };
    let raw = o.as_raw();
    // SAFETY: a live COM object's first word is its vtable; IDXGIOutput6 has at least 29 slots and
    // slot 27 has the GetDesc1 signature (HRESULT GetDesc1(DXGI_OUTPUT_DESC1*)).
    let hr = unsafe {
        let vtbl = *(raw as *const *const usize);
        let f: GetDesc1Fn = std::mem::transmute(*vtbl.add(GETDESC1_SLOT));
        f(raw, &mut d)
    };
    (hr >= 0).then_some(d)
}

/// Every attached DXGI output with its `DXGI_OUTPUT_DESC1`.
fn dxgi_outputs() -> Vec<DxgiOut> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    let mut out = Vec::new();
    // SAFETY: plain COM factory creation.
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    for ai in 0..16u32 {
        // SAFETY: valid factory; an error ends the list.
        let Ok(a) = (unsafe { f.EnumAdapters1(ai) }) else { break };
        for oi in 0..16u32 {
            // SAFETY: valid adapter; an error ends the list.
            let Ok(o) = (unsafe { a.EnumOutputs(oi) }) else { break };
            let Ok(o6) = o.cast::<IDXGIOutput6>() else { continue };
            let Some(d) = output_desc1(&o6) else { continue };
            // 12 FULL_G2084_P2020, 13 YCBCR_STUDIO_G2084_LEFT_P2020, 14 STUDIO_G2084_P2020, 16 ..._TOPLEFT_P2020.
            let pq = matches!(d.color_space, 12 | 13 | 14 | 16);
            let p = |v: [f32; 2]| (v[0], v[1]);
            out.push(DxgiOut {
                name: from_wide(&d.name),
                bits: d.bits,
                pq,
                min: d.min,
                max: d.max,
                full: d.full,
                prim: [p(d.red), p(d.green), p(d.blue), p(d.white)],
            });
        }
    }
    out
}

// ---------------------------------------------------------------- DPI

unsafe extern "system" fn dpi_cb(m: HMONITOR, _dc: HDC, _r: *mut RECT, lp: LPARAM) -> i32 {
    // SAFETY: `lp` is the address of the Vec passed by `dpi_by_gdi`, alive during the call.
    let v = unsafe { &mut *(lp as *mut Vec<(String, u32)>) };
    // SAFETY: zeroed POD with cbSize set as the API requires.
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: valid monitor handle and correctly sized struct.
    if unsafe { GetMonitorInfoW(m, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) } != 0 {
        let (mut x, mut y) = (0u32, 0u32);
        // SAFETY: valid handle; MDT_EFFECTIVE_DPI = 0.
        if unsafe { GetDpiForMonitor(m, 0, &mut x, &mut y) } == 0 && x > 0 {
            v.push((from_wide(&mi.szDevice), (x * 100 + 48) / 96));
        }
    }
    1
}

/// (GDI device name, scale percent) for each monitor.
fn dpi_by_gdi() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = Vec::new();
    // SAFETY: callback only dereferences the Vec we pass, which outlives the call.
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(dpi_cb), &mut v as *mut _ as LPARAM) };
    v
}

// ---------------------------------------------------------------- screens()

/// Read every active screen: link, colour, HDR, panel identity. One-shot, a few ms; run it on a
/// worker thread. Never panics; failures leave the affected fields empty.
pub fn screens() -> Vec<Screen> {
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
    let dxgi = dxgi_outputs();
    let dpis = dpi_by_gdi();
    for p in &paths[..(np as usize).min(paths.len())] {
        let (tl, tid) = (p.targetInfo.adapterId, p.targetInfo.id);
        let tn = get_info::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, tl, tid);
        let sn = get_info::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, p.sourceInfo.adapterId, p.sourceInfo.id);
        let gdi = sn.map(|s| from_wide(&s.viewGdiDeviceName)).unwrap_or_default();
        let connector = connector_name(p.targetInfo.outputTechnology);
        let mut name = tn.map(|t| from_wide(&t.monitorFriendlyDeviceName)).unwrap_or_default();
        if name.is_empty() {
            name = if connector == "Internal" { "Built-in display" } else { "Display" }.to_string();
        }

        // Mode: desktop size and position from the source mode, exact vSync from the target mode.
        let (mut w, mut h, mut primary) = (0, 0, false);
        // SAFETY: union read; modeInfoIdx is valid for active paths without virtual modes.
        let si = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
        if let Some(m) = modes.get(si).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) {
            // SAFETY: infoType says the union holds a source mode.
            let sm = unsafe { m.Anonymous.sourceMode };
            (w, h) = (sm.width, sm.height);
            primary = sm.position.x == 0 && sm.position.y == 0;
        }
        let rate = |r: DISPLAYCONFIG_RATIONAL| if r.Denominator > 0 { r.Numerator as f32 / r.Denominator as f32 } else { 0.0 };
        let mut hz = rate(p.targetInfo.refreshRate);
        // SAFETY: union read, as above.
        let ti = unsafe { p.targetInfo.Anonymous.modeInfoIdx } as usize;
        if let Some(m) = modes.get(ti).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET) {
            // SAFETY: infoType says the union holds a target mode.
            let v = rate(unsafe { m.Anonymous.targetMode.targetVideoSignalInfo.vSyncFreq });
            if v > 0.0 {
                hz = v;
            }
        }

        // Advanced colour: bit 0 supported, bit 1 enabled.
        let ac = get_info::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, tl, tid);
        let (mut ac_sup, mut ac_on, mut bits, mut encoding) = (false, false, None, "");
        if let Some(a) = ac {
            // SAFETY: union read of the plain u32 view of the bit field.
            let v = unsafe { a.Anonymous.value };
            (ac_sup, ac_on) = (v & 1 != 0, v & 2 != 0);
            bits = Some(a.bitsPerColorChannel).filter(|&b| b > 0);
            encoding = encoding_name(a.colorEncoding);
        }
        let sdr_white_nits = get_info::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, tl, tid)
            .map(|s| s.SDRWhiteLevel as f32 / 1000.0 * 80.0)
            .filter(|&n| n > 0.0);

        let dx = dxgi.iter().find(|d| !gdi.is_empty() && d.name.eq_ignore_ascii_case(&gdi));
        let pos = |v: f32| Some(v).filter(|&x| x > 0.0);
        let edid = tn.and_then(|t| read_edid(&from_wide(&t.monitorDevicePath))).and_then(|b| parse_edid(&b));
        let dpi_scale = dpis.iter().find(|(n, _)| !gdi.is_empty() && n.eq_ignore_ascii_case(&gdi)).map(|d| d.1);
        let hdr_static = edid.as_ref().is_some_and(|e| e.hdr_static);
        out.push(Screen {
            name,
            primary,
            w,
            h,
            hz,
            connector,
            adapter_luid: luid_u64(p.targetInfo.adapterId),
            bits: bits.or_else(|| dx.map(|d| d.bits).filter(|&b| b > 0)),
            encoding,
            hdr_supported: ac_sup || hdr_static,
            hdr_on: dx.map(|d| d.pq).unwrap_or(ac_on),
            sdr_white_nits,
            max_nits: dx.and_then(|d| pos(d.max)),
            max_full_frame_nits: dx.and_then(|d| pos(d.full)),
            min_nits: dx.and_then(|d| pos(d.min)),
            primaries: dx.map(|d| d.prim).filter(|p| p[0].0 > 0.0 && p[1].1 > 0.0),
            dpi_scale,
            edid,
            gdi,
        });
    }
    out.sort_by_key(|s| !s.primary);
    out
}

// ---------------------------------------------------------------- DSC estimate

/// Payload bandwidth ceilings in Gbit/s (after line coding).
const DP14_HBR3: f64 = 25.92;
const DP21_UHBR13_5: f64 = 52.4;
const HDMI20: f64 = 14.4;
const HDMI21_FRL: f64 = 42.6;

/// Required uncompressed video rate in Gbit/s: pixels x refresh x ~8% blanking x bits/pixel.
fn required_gbps(s: &Screen) -> f64 {
    let ch = match s.encoding {
        "YCbCr 4:2:2" => 2.0,
        "YCbCr 4:2:0" => 1.5,
        _ => 3.0,
    };
    let bits = s.bits.unwrap_or(8) as f64;
    s.w as f64 * s.h as f64 * s.hz as f64 * 1.08 * bits * ch / 1e9
}

/// Estimate whether Display Stream Compression must be active on this link. `Some(true)` only
/// when the mode clearly exceeds the best uncompressed link for the connector (DP 1.4 / UHBR13.5,
/// HDMI 2.1 FRL), `Some(false)` when it fits DP 1.4 or HDMI 2.0, else `None` (ambiguous or not
/// applicable). A DP 2.1 UHBR20 link could carry up to ~77 Gbit/s uncompressed, which this
/// estimate cannot tell apart, so treat `Some(true)` as "very likely".
pub fn dsc_guess(s: &Screen) -> Option<bool> {
    if s.w == 0 || s.h == 0 || s.hz <= 0.0 {
        return None;
    }
    let need = required_gbps(s);
    match s.connector {
        "DisplayPort" | "USB-C" => {
            if need <= DP14_HBR3 {
                Some(false)
            } else if need > DP21_UHBR13_5 {
                Some(true)
            } else {
                None
            }
        }
        "HDMI" => {
            if need <= HDMI20 {
                Some(false)
            } else if need > HDMI21_FRL {
                Some(true)
            } else {
                None
            }
        }
        "Internal" => (need <= DP14_HBR3).then_some(false),
        "DVI" | "VGA" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------- gamut coverage

const SRGB: [(f32, f32); 3] = [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)];
const DCI_P3: [(f32, f32); 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];

/// Signed shoelace area of a polygon (positive when counter-clockwise).
fn signed_area(p: &[(f32, f32)]) -> f32 {
    let n = p.len();
    (0..n).map(|i| p[i].0 * p[(i + 1) % n].1 - p[(i + 1) % n].0 * p[i].1).sum::<f32>() / 2.0
}

/// Reorder a triangle counter-clockwise.
fn ccw(t: [(f32, f32); 3]) -> [(f32, f32); 3] {
    if signed_area(&t) < 0.0 {
        [t[0], t[2], t[1]]
    } else {
        t
    }
}

/// Sutherland-Hodgman: clip `subject` by the convex counter-clockwise polygon `clip`.
fn clip_polygon(subject: &[(f32, f32)], clip: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let side = |a: (f32, f32), b: (f32, f32), p: (f32, f32)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
    let mut out = subject.to_vec();
    for i in 0..clip.len() {
        let (a, b) = (clip[i], clip[(i + 1) % clip.len()]);
        let input = std::mem::take(&mut out);
        for j in 0..input.len() {
            let (cur, prev) = (input[j], input[(j + input.len() - 1) % input.len()]);
            let (sc, sp) = (side(a, b, cur), side(a, b, prev));
            if sc >= 0.0 {
                if sp < 0.0 {
                    out.push(intersect(prev, cur, sp, sc));
                }
                out.push(cur);
            } else if sp >= 0.0 {
                out.push(intersect(prev, cur, sp, sc));
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out
}

/// Point where segment p->q crosses the clip edge, given the signed sides of p and q.
fn intersect(p: (f32, f32), q: (f32, f32), sp: f32, sq: f32) -> (f32, f32) {
    let t = sp / (sp - sq);
    (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t)
}

/// Percent of `reference` triangle covered by the `display` triangle (both xy).
fn coverage(display: [(f32, f32); 3], reference: [(f32, f32); 3]) -> f32 {
    let (d, r) = (ccw(display), ccw(reference));
    let ra = signed_area(&r);
    if ra <= 0.0 || signed_area(&d) <= 0.0 {
        return 0.0;
    }
    let inter = clip_polygon(&d, &r);
    if inter.len() < 3 {
        return 0.0;
    }
    (signed_area(&inter).abs() / ra * 100.0).clamp(0.0, 100.0)
}

/// How much of the sRGB and DCI-P3 gamuts (CIE 1931 xy triangles) the panel covers, in percent.
/// Uses the red, green and blue entries of `p`; the white point is ignored.
pub fn gamut_cover(p: &[(f32, f32); 4]) -> (f32, f32) {
    let t = [p[0], p[1], p[2]];
    (coverage(t, SRGB), coverage(t, DCI_P3))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn fix_checksum(b: &mut [u8]) {
        b[127] = 0;
        let s = b[..127].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        b[127] = 0u8.wrapping_sub(s);
    }

    /// 27" LG 5B7F-ish: GSM, product 0x5B7F, 60 x 34 cm, 2021 week 12, 48-144 Hz.
    fn base_edid() -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        // G=7, S=19, M=13 -> 0b0_00111_10011_01101
        let id: u16 = (7 << 10) | (19 << 5) | 13;
        b[8] = (id >> 8) as u8;
        b[9] = id as u8;
        b[10] = 0x7F;
        b[11] = 0x5B;
        b[12..16].copy_from_slice(&0x0004_1234u32.to_le_bytes());
        b[16] = 12;
        b[17] = 31;
        b[18] = 1;
        b[19] = 4;
        b[21] = 60;
        b[22] = 34;
        // descriptor 1: detailed timing (non-zero pixel clock)
        b[54] = 0x01;
        b[55] = 0x01;
        // descriptor 2: name
        b[72..77].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
        b[77..90].copy_from_slice(b"LG ULTRAGEAR\n");
        // descriptor 3: range limits 48-144 Hz
        b[90..95].copy_from_slice(&[0, 0, 0, 0xFD, 0]);
        b[95] = 48;
        b[96] = 144;
        fix_checksum(&mut b);
        b
    }

    fn with_cta_hdr(mut b: Vec<u8>) -> Vec<u8> {
        b[126] = 1;
        fix_checksum(&mut b);
        let mut e = vec![0u8; 128];
        e[0] = 0x02;
        e[1] = 3;
        e[2] = 12; // data blocks end / DTDs begin
        e[4] = 0x23; // audio data block, 3 bytes
        e[8] = (7 << 5) | 3; // extended tag block, 3 bytes
        e[9] = 6; // HDR static metadata
        e[10] = 0x05;
        e[11] = 0x01;
        fix_checksum(&mut e);
        b.extend_from_slice(&e);
        b
    }

    #[test]
    fn parses_base_block() {
        let e = parse_edid(&base_edid()).expect("valid edid");
        assert_eq!(e.maker, "GSM (LG)");
        assert_eq!(e.product, 0x5B7F);
        assert_eq!(e.serial, 0x0004_1234);
        assert_eq!(e.model, "LG ULTRAGEAR");
        assert_eq!((e.week, e.year), (12, 2021));
        assert_eq!((e.width_cm, e.height_cm), (60, 34));
        assert!((e.diag_in - 27.15).abs() < 0.06);
        assert_eq!(e.range, Some((48, 144)));
        assert!(!e.hdr_static);
    }

    #[test]
    fn parses_cta_hdr_block() {
        let e = parse_edid(&with_cta_hdr(base_edid())).expect("valid edid");
        assert!(e.hdr_static);
        // Same extension without the extended-tag block: no HDR.
        let mut b = with_cta_hdr(base_edid());
        b[128 + 9] = 5;
        let mut ext = b[128..].to_vec();
        fix_checksum(&mut ext);
        b.truncate(128);
        b.extend_from_slice(&ext);
        assert!(!parse_edid(&b).expect("valid edid").hdr_static);
    }

    #[test]
    fn rejects_bad_edid() {
        let mut b = base_edid();
        b[40] ^= 1; // breaks the checksum
        assert!(parse_edid(&b).is_none());
        assert!(parse_edid(&b[..100]).is_none());
        let mut c = base_edid();
        c[0] = 1;
        assert!(parse_edid(&c).is_none());
    }

    #[test]
    fn edid_registry_key() {
        let p = r"\\?\DISPLAY#GSM5B7F#5&1a2b3c4d&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        assert_eq!(
            edid_key_from_path(p).as_deref(),
            Some(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\GSM5B7F\5&1a2b3c4d&0&UID4352\Device Parameters")
        );
        assert!(edid_key_from_path("garbage").is_none());
    }

    fn screen(conn: &'static str, w: u32, h: u32, hz: f32, bits: u32, enc: &'static str) -> Screen {
        Screen {
            name: String::new(),
            gdi: String::new(),
            primary: true,
            w,
            h,
            hz,
            connector: conn,
            adapter_luid: 0,
            bits: Some(bits),
            encoding: enc,
            hdr_supported: false,
            hdr_on: false,
            sdr_white_nits: None,
            max_nits: None,
            max_full_frame_nits: None,
            min_nits: None,
            primaries: None,
            dpi_scale: None,
            edid: None,
        }
    }

    #[test]
    fn dsc_estimates() {
        assert_eq!(dsc_guess(&screen("DisplayPort", 3840, 2160, 240.0, 10, "RGB")), Some(true));
        assert_eq!(dsc_guess(&screen("DisplayPort", 2560, 1440, 144.0, 8, "RGB")), Some(false));
        assert_eq!(dsc_guess(&screen("DisplayPort", 3840, 2160, 144.0, 10, "RGB")), None);
        assert_eq!(dsc_guess(&screen("HDMI", 3840, 2160, 60.0, 8, "RGB")), Some(false));
        assert_eq!(dsc_guess(&screen("HDMI", 3840, 2160, 120.0, 10, "RGB")), None);
        assert_eq!(dsc_guess(&screen("HDMI", 7680, 4320, 120.0, 10, "RGB")), Some(true));
        assert_eq!(dsc_guess(&screen("DisplayPort", 0, 0, 0.0, 8, "RGB")), None);
        // 4:2:0 shrinks the requirement by half versus RGB.
        assert_eq!(dsc_guess(&screen("HDMI", 3840, 2160, 60.0, 10, "YCbCr 4:2:0")), Some(false));
    }

    #[test]
    fn gamut_math() {
        let srgb = [SRGB[0], SRGB[1], SRGB[2], (0.3127, 0.3290)];
        let (s, p3) = gamut_cover(&srgb);
        assert!((s - 100.0).abs() < 0.1, "{s}");
        assert!(p3 > 60.0 && p3 < 80.0, "{p3}");
        let p3p = [DCI_P3[0], DCI_P3[1], DCI_P3[2], (0.3127, 0.3290)];
        let (s2, p32) = gamut_cover(&p3p);
        assert!(s2 > 85.0 && s2 < 100.0, "{s2}");
        assert!((p32 - 100.0).abs() < 0.1, "{p32}");
        // Winding order does not matter.
        let rev = [srgb[0], srgb[2], srgb[1], srgb[3]];
        assert!((gamut_cover(&rev).0 - 100.0).abs() < 0.1);
        // Degenerate input yields zero.
        assert_eq!(gamut_cover(&[(0.0, 0.0); 4]), (0.0, 0.0));
    }
}
