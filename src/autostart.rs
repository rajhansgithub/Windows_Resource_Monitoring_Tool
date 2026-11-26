//! Run-at-login via HKCU\...\Run. No admin, no self-copying installer.

use crate::util::wide;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Registry::*;

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const NAME: &str = "ResourceMonitor";
/// Value written by the old .NET build; removed so two copies never start.
const LEGACY: &str = "WindowsNetworkMonitor";

fn exe() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn open() -> Option<HKEY> {
    let sub = wide(RUN);
    let mut k: HKEY = std::ptr::null_mut();
    let r = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            KEY_READ | KEY_WRITE,
            &mut k,
        )
    };
    (r == ERROR_SUCCESS).then_some(k)
}

fn value(k: HKEY, name: &str) -> Option<String> {
    let n = wide(name);
    let mut buf = [0u16; 1024];
    let mut sz = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            k,
            std::ptr::null(),
            n.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as _,
            &mut sz,
        )
    };
    (r == ERROR_SUCCESS).then(|| crate::util::from_wide(&buf))
}

pub fn enabled() -> bool {
    open()
        .map(|k| {
            let v = value(k, NAME);
            unsafe { RegCloseKey(k) };
            v.is_some()
        })
        .unwrap_or(false)
}

pub fn set(on: bool) {
    let Some(k) = open() else { return };
    let name = wide(NAME);
    unsafe {
        if on {
            let v = wide(&format!("\"{}\"", exe()));
            RegSetValueExW(
                k,
                name.as_ptr(),
                0,
                REG_SZ,
                v.as_ptr() as _,
                (v.len() * 2) as u32,
            );
        } else {
            RegDeleteValueW(k, name.as_ptr());
        }
        let legacy = wide(LEGACY);
        RegDeleteValueW(k, legacy.as_ptr());
        RegCloseKey(k);
    }
}

/// If the exe moved since autostart was enabled, point the entry at the new path.
pub fn refresh_path() {
    let Some(k) = open() else { return };
    let cur = value(k, NAME);
    unsafe { RegCloseKey(k) };
    if let Some(cur) = cur {
        if !cur.trim_matches('"').eq_ignore_ascii_case(&exe()) {
            set(true);
        }
    }
}
