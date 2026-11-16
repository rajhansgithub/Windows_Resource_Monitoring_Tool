//! Battery state and uptime.

use super::Battery;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

const FLAG_CHARGING: u8 = 8;
const FLAG_NO_BATTERY: u8 = 128;
const UNKNOWN_U8: u8 = 255;

pub fn battery() -> Option<Battery> {
    // SAFETY: SYSTEM_POWER_STATUS is plain old data.
    let mut st: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut st) } == 0 {
        return None;
    }
    // 128 = no battery; percent 255 (or any value > 100) = unknown.
    if st.BatteryFlag == FLAG_NO_BATTERY || st.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery {
        percent: st.BatteryLifePercent,
        charging: st.BatteryFlag != UNKNOWN_U8 && st.BatteryFlag & FLAG_CHARGING != 0,
        on_ac: st.ACLineStatus == 1,
        secs_left: (st.BatteryLifeTime != u32::MAX).then_some(st.BatteryLifeTime),
    })
}

pub fn uptime_secs() -> u64 {
    // SAFETY: no arguments.
    unsafe { GetTickCount64() / 1000 }
}
