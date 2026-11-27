//! Friendly app names for the Monitor page's grouped process list ("Google Chrome"
//! for chrome.exe), read from the executable's version resource like Task Manager.

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn image_path(pid: u32) -> Option<Vec<u16>> {
    // SAFETY: plain handle query; the handle is closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) != 0;
        CloseHandle(h);
        if !ok || n == 0 {
            return None;
        }
        buf.truncate((n as usize).min(1023));
        buf.push(0);
        Some(buf)
    }
}

/// `FileDescription` of the executable running as `pid` (None: no access, no resource,
/// or a blank description).
pub fn describe(pid: u32) -> Option<String> {
    let path = image_path(pid)?;
    // SAFETY: `path` is NUL-terminated; the version block outlives every pointer read from it,
    // and each pointer is bounds checked against the block before use.
    unsafe {
        let mut dummy = 0u32;
        let size = GetFileVersionInfoSizeW(path.as_ptr(), &mut dummy);
        if size == 0 || size > 1 << 20 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, block.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let base = block.as_ptr() as usize;
        let end = base + block.len();
        let inside = |p: usize, bytes: usize| p >= base && p + bytes <= end;

        // Languages the file has; fall back to US English / Unicode.
        let mut langs: Vec<(u16, u16)> = Vec::new();
        let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut n = 0u32;
        let tr = wide("\\VarFileInfo\\Translation");
        if VerQueryValueW(block.as_ptr().cast(), tr.as_ptr(), &mut p, &mut n) != 0 && inside(p as usize, n as usize) {
            let pairs = std::slice::from_raw_parts(p as *const u16, (n as usize / 2) & !1);
            langs.extend(pairs.chunks_exact(2).map(|c| (c[0], c[1])));
        }
        langs.extend([(0x0409, 0x04B0), (0x0409, 0x04E4), (0x0000, 0x04B0)]);
        for (lang, cp) in langs {
            let key = wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"));
            let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut n = 0u32;
            if VerQueryValueW(block.as_ptr().cast(), key.as_ptr(), &mut p, &mut n) == 0 || n == 0 {
                continue;
            }
            if !inside(p as usize, n as usize * 2) {
                continue;
            }
            let s = std::slice::from_raw_parts(p as *const u16, n as usize);
            let s = String::from_utf16_lossy(s.split(|&c| c == 0).next().unwrap_or(&[]));
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
        None
    }
}

/// "chrome.exe" → "chrome" (shown when there is no description).
pub fn bare(name: &str) -> &str {
    let n = name.len();
    if n > 4 && name[n - 4..].eq_ignore_ascii_case(".exe") {
        &name[..n - 4]
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bare_strips_exe() {
        assert_eq!(super::bare("chrome.exe"), "chrome");
        assert_eq!(super::bare("Code.EXE"), "Code");
        assert_eq!(super::bare("System"), "System");
        assert_eq!(super::bare(".exe"), ".exe");
    }
}
