//! Small Win32 helpers shared by the UI modules.

use std::path::PathBuf;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// UTF-16, NUL-terminated.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    let d = base.join("ResourceMonitor");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn legacy_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|b| PathBuf::from(b).join("WindowsNetworkMonitor"))
}

/// Write file atomically (tmp + rename) so a crash/power loss never corrupts data.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

pub fn read_dword(root: HKEY, sub: &str, name: &str) -> Option<u32> {
    let sub = wide(sub);
    let name = wide(name);
    let mut val: u32 = 0;
    let mut sz: u32 = 4;
    let r = unsafe {
        RegGetValueW(
            root,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut val as *mut u32 as *mut _,
            &mut sz,
        )
    };
    (r == ERROR_SUCCESS).then_some(val)
}

pub fn shell_open(target: &str, args: Option<&str>) {
    let t = wide(target);
    let a = args.map(wide);
    let op = wide("open");
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            t.as_ptr(),
            a.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
    pub fn is_empty(&self) -> bool {
        self.w() <= 0 || self.h() <= 0
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect { l: r.left, t: r.top, r: r.right, b: r.bottom }
    }
}

impl From<Rect> for RECT {
    fn from(r: Rect) -> Self {
        RECT { left: r.l, top: r.t, right: r.r, bottom: r.b }
    }
}

pub fn window_rect(h: HWND) -> Option<Rect> {
    if h.is_null() {
        return None;
    }
    let mut r: RECT = unsafe { std::mem::zeroed() };
    (unsafe { GetWindowRect(h, &mut r) } != 0).then(|| r.into())
}

pub fn find_window(class: &str) -> HWND {
    let c = wide(class);
    unsafe { FindWindowW(c.as_ptr(), std::ptr::null()) }
}

pub fn find_child(parent: HWND, class: &str) -> HWND {
    let c = wide(class);
    unsafe { FindWindowExW(parent, std::ptr::null_mut(), c.as_ptr(), std::ptr::null()) }
}

pub fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn local_time() -> windows_sys::Win32::Foundation::SYSTEMTIME {
    let mut st = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st) };
    st
}

#[inline]
pub fn loword(x: usize) -> u16 {
    (x & 0xffff) as u16
}
#[inline]
pub fn get_x(l: isize) -> i32 {
    (l & 0xffff) as i16 as i32
}
#[inline]
pub fn get_y(l: isize) -> i32 {
    ((l >> 16) & 0xffff) as i16 as i32
}

/// Append a line to %LOCALAPPDATA%\ResourceMonitor\diagnostics.log (kept
/// under ~256 KB). Used only for rare events (visibility changes, crashes).
pub fn diag(msg: &str) {
    use std::io::Write;
    let path = data_dir().join("diagnostics.log");
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        if let Ok(old) = std::fs::read(&path) {
            let _ = std::fs::write(&path, &old[old.len() / 2..]);
        }
    }
    let st = local_time();
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}  {}\n",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, msg
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}
