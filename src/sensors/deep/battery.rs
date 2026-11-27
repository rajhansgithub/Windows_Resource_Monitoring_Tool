//! Battery detail: capacity, health, live power flow, cycles and charge-limit hint.
//!
//! Talks straight to the battery class driver (`batclass.h`): one `\\?\...` interface per
//! battery from SetupAPI (`GUID_DEVICE_BATTERY`), opened for read with shared access, then
//! `IOCTL_BATTERY_QUERY_TAG`, `IOCTL_BATTERY_QUERY_INFORMATION` (static facts) and
//! `IOCTL_BATTERY_QUERY_STATUS` (live state). Every IOCTL here is declared with
//! `FILE_READ_ACCESS`, so a standard (non-admin) account can use it. Handles stay open
//! between calls; a stale tag (battery swapped or removed) is re-queried once, then dropped.
#![allow(dead_code)]

use crate::util::from_wide;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Power::{
    BatteryDeviceName, BatteryInformation, BatteryManufactureName, BatteryTemperature,
    BATTERY_CAPACITY_RELATIVE, BATTERY_CHARGING, BATTERY_DISCHARGING, BATTERY_INFORMATION,
    BATTERY_POWER_ON_LINE, BATTERY_QUERY_INFORMATION, BATTERY_STATUS, BATTERY_UNKNOWN_CAPACITY,
    BATTERY_UNKNOWN_RATE, BATTERY_UNKNOWN_VOLTAGE, BATTERY_WAIT_STATUS, GUID_DEVICE_BATTERY,
    IOCTL_BATTERY_QUERY_INFORMATION, IOCTL_BATTERY_QUERY_STATUS, IOCTL_BATTERY_QUERY_TAG,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

/// Upper bound on batteries we track (laptops have 1, rarely 2).
const MAX_BATTERIES: u32 = 8;
/// Health above this is a gauge artefact; clamp to it.
const HEALTH_MAX_PCT: f32 = 120.0;
/// Time estimates beyond this many minutes (100 h) are noise from a near-zero rate.
const MAX_MINUTES: u64 = 6000;
/// |rate| at or below this counts as "idle" for the charge-limit hint.
const IDLE_RATE_MW: i32 = 500;
/// Charge range in which a plugged-in, idle battery suggests a charge limit.
const LIMIT_PCT_MIN: u8 = 50;
const LIMIT_PCT_MAX: u8 = 95;
/// Minutes the battery must sit idle on AC before the hint is raised.
const LIMIT_MIN_IDLE_MINUTES: u32 = 10;

/// One battery: static facts plus the latest live reading.
#[derive(Clone, Debug, Default)]
pub struct Battery {
    /// Device name (`BatteryDeviceName`).
    pub name: String,
    /// Manufacturer (`BatteryManufactureName`).
    pub maker: String,
    /// Friendly chemistry, e.g. "Li-ion" (see [`chemistry_name`]).
    pub chemistry: String,
    /// Designed capacity, mWh.
    pub design_mwh: u32,
    /// Last full-charge capacity, mWh.
    pub full_mwh: u32,
    /// Charge cycles; `None` when the firmware reports 0 (not supported).
    pub cycles: Option<u32>,
    /// Full / design capacity, percent (0..=120). `None` when design is 0 or capacities are relative.
    pub health_pct: Option<f32>,
    /// Remaining energy, mWh.
    pub now_mwh: Option<u32>,
    /// Power flow in mW: positive while charging, negative while discharging.
    pub rate_mw: Option<i32>,
    /// Terminal voltage, mV.
    pub voltage_mv: Option<u32>,
    /// Battery is being charged.
    pub charging: bool,
    /// Mains power is connected.
    pub on_ac: bool,
    /// Battery temperature, deg C (most batteries do not report it).
    pub temp_c: Option<f32>,
    /// Minutes until empty (discharging) or full (charging), computed from `rate_mw`.
    pub minutes_left: Option<u32>,
}

/// An open battery interface. `h == 0` means closed/dead.
struct Entry {
    /// Interface path, UTF-16 with trailing NUL (kept so `statics` can reopen).
    path: Vec<u16>,
    /// Raw HANDLE (stored as isize so `Batteries` stays `Send`).
    h: isize,
    tag: u32,
    /// Capabilities reported `BATTERY_CAPACITY_RELATIVE`: mWh values are not meaningful.
    relative: bool,
    /// Temperature IOCTL worked at least once.
    temp_ok: bool,
}

/// Open battery interfaces with cached device paths and tags.
pub struct Batteries {
    entries: Vec<Entry>,
}

impl Drop for Batteries {
    fn drop(&mut self) {
        for e in &mut self.entries {
            e.close();
        }
    }
}

impl Entry {
    fn close(&mut self) {
        if self.h != 0 {
            // SAFETY: handle came from CreateFileW and is closed exactly once.
            unsafe { CloseHandle(self.h as HANDLE) };
            self.h = 0;
        }
        self.tag = 0;
    }

    /// Open the device and fetch its tag. False when no battery is present in the slot.
    fn open(&mut self) -> bool {
        self.close();
        let h = open_device(&self.path);
        if h == 0 {
            return false;
        }
        match query_tag(h) {
            Some(tag) => {
                self.h = h;
                self.tag = tag;
                true
            }
            None => {
                // SAFETY: valid handle just opened.
                unsafe { CloseHandle(h as HANDLE) };
                false
            }
        }
    }

    /// Re-query the tag after a failed status call. False means the battery is gone.
    fn retag(&mut self) -> bool {
        match query_tag(self.h) {
            Some(t) => {
                self.tag = t;
                true
            }
            None => {
                self.close();
                false
            }
        }
    }

    /// Fill the live fields of `b`. Failure clears them and, if the tag cannot be
    /// refreshed, drops the entry.
    fn live(&mut self, b: &mut Battery) {
        if self.h == 0 {
            clear_live(b);
            return;
        }
        let st = match query_status(self.h, self.tag) {
            Some(s) => s,
            None => {
                // Stale tag (battery swapped): fetch a fresh one and retry once.
                match self.retag().then(|| query_status(self.h, self.tag)).flatten() {
                    Some(s) => s,
                    None => {
                        self.close();
                        clear_live(b);
                        return;
                    }
                }
            }
        };
        apply_status(b, &st, self.relative);
        if self.temp_ok {
            b.temp_c = query_temp(self.h, self.tag);
        }
    }
}

impl Batteries {
    /// Enumerate batteries and open each one. Empty when the PC has no battery.
    pub fn open() -> Batteries {
        let mut entries = Vec::new();
        for path in enumerate_paths() {
            let mut e = Entry { path, h: 0, tag: 0, relative: false, temp_ok: false };
            if e.open() {
                entries.push(e);
            }
        }
        Batteries { entries }
    }

    /// Read the static facts of every battery (names, capacities, cycles). Run on a
    /// worker thread, rarely. Batteries that cannot be read are dropped, so afterwards the
    /// returned vector lines up with the slice later passed to [`Batteries::live`].
    pub fn statics(&mut self) -> Vec<Battery> {
        let mut out = Vec::with_capacity(self.entries.len());
        self.entries.retain_mut(|e| {
            if e.h == 0 && !e.open() {
                return false;
            }
            match read_statics(e) {
                Some(b) => {
                    out.push(b);
                    true
                }
                None => {
                    e.close();
                    false
                }
            }
        });
        out
    }

    /// Refresh the live fields (state, charge, voltage, rate, temperature when supported)
    /// of `b`, which must come from [`Batteries::statics`]. One cheap IOCTL per battery.
    pub fn live(&mut self, b: &mut [Battery]) {
        for (e, bat) in self.entries.iter_mut().zip(b.iter_mut()) {
            e.live(bat);
        }
    }
}

/// Friendly name of the 4-byte `BATTERY_INFORMATION.Chemistry` code.
pub fn chemistry_name(c: [u8; 4]) -> String {
    let raw: String = c
        .iter()
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|&b| b as char)
        .collect();
    let raw = raw.trim();
    match raw.to_ascii_uppercase().as_str() {
        "" => "Unknown".into(),
        "LION" | "LI-I" | "LIION" => "Li-ion".into(),
        "LIP" | "LIPO" | "LI-P" | "LIPL" => "Li-polymer".into(),
        "PBAC" => "Lead-acid".into(),
        "NICD" => "NiCd".into(),
        "NIMH" => "NiMH".into(),
        "NIZN" => "NiZn".into(),
        "RAM" => "Alkaline (RAM)".into(),
        _ => raw.to_string(),
    }
}

/// Battery health: full-charge / design capacity as percent, clamped to 0..=120.
/// `None` when the design capacity is 0 (unknown).
pub fn health(design: u32, full: u32) -> Option<f32> {
    if design == 0 {
        return None;
    }
    Some((full as f64 / design as f64 * 100.0).clamp(0.0, HEALTH_MAX_PCT as f64) as f32)
}

/// Minutes until empty (negative `rate_mw`) or full (positive). `None` for a zero rate or
/// an estimate above 100 hours.
pub fn minutes_left(now: u32, full: u32, rate_mw: i32) -> Option<u32> {
    if rate_mw == 0 {
        return None;
    }
    let mag = rate_mw.unsigned_abs() as u64;
    let wh = if rate_mw < 0 { now } else { full.saturating_sub(now) } as u64;
    let mins = wh * 60 / mag;
    (mins <= MAX_MINUTES).then_some(mins as u32)
}

/// Guess that a charge limit is active: plugged in, not charging, power flow ~0 and the
/// charge sat between 50 and 95 % for at least 10 minutes. Returns the limit rounded to the
/// nearest 5 % ("limit likely active at N%"). An unknown rate counts as idle.
/// `minutes_plugged_idle` is tracked by the caller (minutes in this idle state so far).
pub fn limit_hint(
    pct_now: u8,
    on_ac: bool,
    charging: bool,
    rate_mw: Option<i32>,
    minutes_plugged_idle: u32,
) -> Option<u8> {
    let idle = rate_mw.is_none_or(|r| r.abs() <= IDLE_RATE_MW);
    if !on_ac
        || charging
        || !idle
        || !(LIMIT_PCT_MIN..=LIMIT_PCT_MAX).contains(&pct_now)
        || minutes_plugged_idle < LIMIT_MIN_IDLE_MINUTES
    {
        return None;
    }
    let rounded = (pct_now as u16 + 2) / 5 * 5;
    Some((rounded as u8).clamp(LIMIT_PCT_MIN, LIMIT_PCT_MAX))
}

/// Turn a raw status into the live fields of `b`.
fn apply_status(b: &mut Battery, st: &BATTERY_STATUS, relative: bool) {
    let charging = st.PowerState & BATTERY_CHARGING != 0;
    let discharging = st.PowerState & BATTERY_DISCHARGING != 0;
    b.charging = charging;
    b.on_ac = st.PowerState & BATTERY_POWER_ON_LINE != 0;
    b.voltage_mv = (st.Voltage != BATTERY_UNKNOWN_VOLTAGE).then_some(st.Voltage);
    b.now_mwh = (st.Capacity != BATTERY_UNKNOWN_CAPACITY && !relative).then_some(st.Capacity);
    b.rate_mw = (st.Rate as u32 != BATTERY_UNKNOWN_RATE).then(|| normalise_rate(st.Rate, charging, discharging));
    b.minutes_left = match (b.now_mwh, b.rate_mw) {
        (Some(now), Some(r)) if charging || discharging => minutes_left(now, b.full_mwh, r),
        _ => None,
    };
}

/// Some firmware reports the rate unsigned; trust the state flags for the sign.
fn normalise_rate(rate: i32, charging: bool, discharging: bool) -> i32 {
    match (charging, discharging) {
        (true, false) => rate.saturating_abs(),
        (false, true) => -rate.saturating_abs(),
        _ => rate,
    }
}

fn clear_live(b: &mut Battery) {
    b.now_mwh = None;
    b.rate_mw = None;
    b.voltage_mv = None;
    b.temp_c = None;
    b.minutes_left = None;
    b.charging = false;
}

/// Read names, capacities, chemistry and cycles through `IOCTL_BATTERY_QUERY_INFORMATION`.
fn read_statics(e: &mut Entry) -> Option<Battery> {
    // SAFETY: BATTERY_INFORMATION is plain old data; zero is a valid value.
    let mut info: BATTERY_INFORMATION = unsafe { std::mem::zeroed() };
    query_info(e.h, e.tag, BatteryInformation, &mut info)?;
    e.relative = info.Capabilities & BATTERY_CAPACITY_RELATIVE != 0;
    e.temp_ok = query_temp(e.h, e.tag).is_some();

    let known = |v: u32| if v == BATTERY_UNKNOWN_CAPACITY { 0 } else { v };
    let (design, full) = (known(info.DesignedCapacity), known(info.FullChargedCapacity));
    let mut b = Battery {
        name: query_string(e.h, e.tag, BatteryDeviceName),
        maker: query_string(e.h, e.tag, BatteryManufactureName),
        chemistry: chemistry_name(info.Chemistry),
        design_mwh: design,
        full_mwh: full,
        cycles: (info.CycleCount != 0).then_some(info.CycleCount),
        health_pct: if e.relative { None } else { health(design, full) },
        ..Battery::default()
    };
    b.temp_c = if e.temp_ok { query_temp(e.h, e.tag) } else { None };
    Some(b)
}

/// Interface paths (NUL-terminated UTF-16) of all present batteries.
fn enumerate_paths() -> Vec<Vec<u16>> {
    let mut out = Vec::new();
    // SAFETY: valid GUID pointer; null enumerator and parent window.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVICE_BATTERY,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE as isize {
        return out;
    }
    for i in 0..MAX_BATTERIES {
        // SAFETY: POD struct; cbSize set before use.
        let mut ifd: SP_DEVICE_INTERFACE_DATA = unsafe { std::mem::zeroed() };
        ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        // SAFETY: `set` is a live device info set and `ifd` is writable.
        if unsafe { SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &GUID_DEVICE_BATTERY, i, &mut ifd) } == 0 {
            break;
        }
        if let Some(p) = interface_path(set, &ifd) {
            out.push(p);
        }
    }
    // SAFETY: `set` was returned by SetupDiGetClassDevsW and is destroyed once.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    out
}

/// Device path of one enumerated interface.
fn interface_path(set: isize, ifd: &SP_DEVICE_INTERFACE_DATA) -> Option<Vec<u16>> {
    let mut need = 0u32;
    // SAFETY: a null buffer asks only for the required size (the call is expected to fail).
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(set, ifd, std::ptr::null_mut(), 0, &mut need, std::ptr::null_mut())
    };
    // cbSize field (4 bytes) plus at least one UTF-16 unit.
    if !(6..=4096).contains(&need) {
        return None;
    }
    // u32 storage keeps the buffer aligned for the detail struct.
    let mut buf = vec![0u32; (need as usize).div_ceil(4)];
    let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // SAFETY: `buf` holds at least `need` bytes and is suitably aligned.
    unsafe {
        (*det).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        if SetupDiGetDeviceInterfaceDetailW(set, ifd, det, need, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
            return None;
        }
    }
    // DevicePath starts right after the 4-byte cbSize.
    let units = (need as usize - 4) / 2;
    // SAFETY: offset 4 is u16-aligned inside `buf`, which holds `need` bytes.
    let chars = unsafe { std::slice::from_raw_parts((buf.as_ptr() as *const u8).add(4) as *const u16, units) };
    let n = chars.iter().position(|&c| c == 0)?;
    if n == 0 {
        return None;
    }
    let mut path = chars[..n].to_vec();
    path.push(0);
    Some(path)
}

/// Open the battery interface for reading; 0 on failure.
fn open_device(path: &[u16]) -> isize {
    // Every battery IOCTL is FILE_READ_ACCESS, so read-only is enough (and works for a
    // standard user). Read+write is a fallback for drivers that insist on it.
    for access in [GENERIC_READ, GENERIC_READ | GENERIC_WRITE] {
        // SAFETY: `path` is NUL-terminated; other pointer arguments are null.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE && !h.is_null() {
            return h as isize;
        }
    }
    0
}

/// Synchronous DeviceIoControl with typed in/out buffers. Returns bytes written.
fn ioctl<I, O>(h: isize, code: u32, input: &I, out: &mut O) -> Option<u32> {
    let mut ret = 0u32;
    // SAFETY: both buffers are valid for their sizes; the handle is not overlapped.
    let ok = unsafe {
        DeviceIoControl(
            h as HANDLE,
            code,
            input as *const I as *const _,
            std::mem::size_of::<I>() as u32,
            out as *mut O as *mut _,
            std::mem::size_of::<O>() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(ret)
}

/// `IOCTL_BATTERY_QUERY_TAG` with a zero timeout; a tag of 0 means no battery.
fn query_tag(h: isize) -> Option<u32> {
    let timeout = 0u32;
    let mut tag = 0u32;
    ioctl(h, IOCTL_BATTERY_QUERY_TAG, &timeout, &mut tag)?;
    (tag != 0).then_some(tag)
}

/// `IOCTL_BATTERY_QUERY_STATUS` (timeout 0: returns immediately).
fn query_status(h: isize, tag: u32) -> Option<BATTERY_STATUS> {
    // SAFETY: POD structs; zero is a valid value.
    let mut st: BATTERY_STATUS = unsafe { std::mem::zeroed() };
    let mut wait: BATTERY_WAIT_STATUS = unsafe { std::mem::zeroed() };
    wait.BatteryTag = tag;
    let n = ioctl(h, IOCTL_BATTERY_QUERY_STATUS, &wait, &mut st)?;
    (n as usize >= std::mem::size_of::<BATTERY_STATUS>()).then_some(st)
}

/// `IOCTL_BATTERY_QUERY_INFORMATION` into a typed output; returns bytes written.
fn query_info<O>(h: isize, tag: u32, level: i32, out: &mut O) -> Option<u32> {
    let q = BATTERY_QUERY_INFORMATION { BatteryTag: tag, InformationLevel: level, AtRate: 0 };
    ioctl(h, IOCTL_BATTERY_QUERY_INFORMATION, &q, out)
}

/// A UTF-16 string level (`BatteryDeviceName`, `BatteryManufactureName`); empty on failure.
fn query_string(h: isize, tag: u32, level: i32) -> String {
    let mut buf = [0u16; 128];
    match query_info(h, tag, level, &mut buf) {
        Some(n) => from_wide(&buf[..(n as usize / 2).min(buf.len())]).trim().to_string(),
        None => String::new(),
    }
}

/// `BatteryTemperature` (tenths of Kelvin) in deg C; `None` when unsupported or implausible.
fn query_temp(h: isize, tag: u32) -> Option<f32> {
    let mut tenths_k = 0u32;
    query_info(h, tag, BatteryTemperature, &mut tenths_k)?;
    kelvin_tenths_to_c(tenths_k)
}

/// Convert tenths of Kelvin to deg C, rejecting 0 and values outside -40..=150.
fn kelvin_tenths_to_c(tenths_k: u32) -> Option<f32> {
    if tenths_k == 0 {
        return None;
    }
    let c = tenths_k as f32 / 10.0 - 273.15;
    (-40.0..=150.0).contains(&c).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chemistry_codes() {
        assert_eq!(chemistry_name(*b"LION"), "Li-ion");
        assert_eq!(chemistry_name(*b"Li-I"), "Li-ion");
        assert_eq!(chemistry_name([b'L', b'i', b'P', 0]), "Li-polymer");
        assert_eq!(chemistry_name(*b"PbAc"), "Lead-acid");
        assert_eq!(chemistry_name(*b"NiMH"), "NiMH");
        assert_eq!(chemistry_name([0; 4]), "Unknown");
        assert_eq!(chemistry_name(*b"XYZ "), "XYZ");
    }

    #[test]
    fn health_maths() {
        assert_eq!(health(0, 5000), None);
        assert_eq!(health(50_000, 40_000), Some(80.0));
        assert_eq!(health(50_000, 0), Some(0.0));
        assert_eq!(health(10_000, 99_000), Some(120.0));
    }

    #[test]
    fn minutes_discharging_and_charging() {
        // 30 Wh at 15 W out = 2 h.
        assert_eq!(minutes_left(30_000, 50_000, -15_000), Some(120));
        // 20 Wh to fill at 40 W in = 30 min.
        assert_eq!(minutes_left(30_000, 50_000, 40_000), Some(30));
        assert_eq!(minutes_left(50_000, 50_000, 40_000), Some(0));
        assert_eq!(minutes_left(60_000, 50_000, 40_000), Some(0));
        assert_eq!(minutes_left(30_000, 50_000, 0), None);
        // Tiny rate: absurd estimate is dropped.
        assert_eq!(minutes_left(30_000, 50_000, -10), None);
    }

    #[test]
    fn limit_hint_cases() {
        assert_eq!(limit_hint(80, true, false, Some(0), 10), Some(80));
        assert_eq!(limit_hint(79, true, false, Some(120), 30), Some(80));
        assert_eq!(limit_hint(80, true, false, None, 12), Some(80));
        assert_eq!(limit_hint(80, true, false, Some(0), 9), None);
        assert_eq!(limit_hint(80, false, false, Some(0), 60), None);
        assert_eq!(limit_hint(80, true, true, Some(0), 60), None);
        assert_eq!(limit_hint(80, true, false, Some(8000), 60), None);
        assert_eq!(limit_hint(49, true, false, Some(0), 60), None);
        assert_eq!(limit_hint(96, true, false, Some(0), 60), None);
        assert_eq!(limit_hint(95, true, false, Some(0), 60), Some(95));
    }

    #[test]
    fn rate_sign_follows_flags() {
        assert_eq!(normalise_rate(5000, false, true), -5000);
        assert_eq!(normalise_rate(-5000, true, false), 5000);
        assert_eq!(normalise_rate(-5000, false, true), -5000);
        assert_eq!(normalise_rate(0, false, false), 0);
    }

    #[test]
    fn temperature_conversion() {
        assert_eq!(kelvin_tenths_to_c(0), None);
        let c = kelvin_tenths_to_c(3000).unwrap_or(0.0);
        assert!((c - 26.85).abs() < 0.01);
        assert_eq!(kelvin_tenths_to_c(100), None);
    }

    #[test]
    fn status_application() {
        let mut b = Battery { full_mwh: 50_000, ..Battery::default() };
        let st = BATTERY_STATUS {
            PowerState: BATTERY_DISCHARGING,
            Capacity: 30_000,
            Voltage: 11_800,
            Rate: -15_000,
        };
        apply_status(&mut b, &st, false);
        assert!(!b.on_ac && !b.charging);
        assert_eq!(b.rate_mw, Some(-15_000));
        assert_eq!(b.minutes_left, Some(120));
        assert_eq!(b.voltage_mv, Some(11_800));

        let st = BATTERY_STATUS {
            PowerState: BATTERY_POWER_ON_LINE | BATTERY_CHARGING,
            Capacity: BATTERY_UNKNOWN_CAPACITY,
            Voltage: BATTERY_UNKNOWN_VOLTAGE,
            Rate: BATTERY_UNKNOWN_RATE as i32,
        };
        apply_status(&mut b, &st, false);
        assert!(b.on_ac && b.charging);
        assert_eq!((b.now_mwh, b.rate_mw, b.voltage_mv, b.minutes_left), (None, None, None, None));
    }
}
