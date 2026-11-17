//! Self-update from GitHub Releases.
//!
//! Checks once at startup (if enabled) and when the user picks "Check for
//! updates now" — never on a timer. Runs on a background thread. A newer
//! `ResourceMonitor.exe` is downloaded, its SHA-256 verified against the
//! digest GitHub publishes, then swapped in: Windows lets a running exe be
//! renamed, so the old file becomes `ResourceMonitor.old.exe` and the new one
//! takes its name. The app then restarts itself.

use crate::util::wide;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Networking::WinHttp::*;
use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

const REPO: &str = "rajhansgithub/Windows_Resource_Monitoring_Tool";
const ASSET: &str = "ResourceMonitor.exe";
const AGENT: &str = concat!("ResourceMonitor-Updater/", env!("CARGO_PKG_VERSION"));

/// wParam low byte: 0 = up to date, 1 = installed (restart now), 2 = newer
/// version but this folder isn't writable, 3 = check failed. Bit 0x100 is set
/// when the user started the check. lParam: version string (see take_version).
pub const WM_APP_UPDATE: u32 = WM_APP + 8;
pub const UP_TO_DATE: usize = 0;
pub const INSTALLED: usize = 1;
pub const MANUAL: usize = 2;
pub const FAILED: usize = 3;
/// Like MANUAL, but from the automatic startup check: notify only.
pub const MANUAL_QUIET: usize = 4;
pub const MANUAL_FLAG: usize = 0x100;

/// Progress while working: wParam = stage, lParam = percent × 10 (downloads).
pub const WM_APP_UPDATE_STAGE: u32 = WM_APP + 9;
pub const STAGE_DOWNLOAD: usize = 1;
pub const STAGE_VERIFY: usize = 2;
pub const STAGE_INSTALL: usize = 3;

pub fn releases_page() -> String {
    format!("https://github.com/{}/releases/latest", REPO)
}

/// Remove the previous exe left behind by the last update.
pub fn cleanup() {
    if let Some(old) = old_path() {
        let _ = std::fs::remove_file(old);
    }
}

fn old_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name("ResourceMonitor.old.exe"))
}

/// Start a check on a background thread; the result is posted to `notify`.
/// `manual`: the user asked, so also report "up to date" / failures.
pub fn check(notify: isize, manual: bool) {
    let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
        let stage = |st: usize, permille: isize| unsafe {
            PostMessageW(notify as _, WM_APP_UPDATE_STAGE, st, permille);
        };
        let (code, ver) = match run(&stage) {
            Ok(Some(v)) => (INSTALLED, v),
            Ok(None) => (UP_TO_DATE, String::new()),
            Err(Outcome::NotWritable(v)) => (MANUAL, v),
            // lParam string carries the reason for the user.
            Err(Outcome::Failed(why)) => (FAILED, why),
        };
        let code = if code == MANUAL && !manual { MANUAL_QUIET } else { code };
        if code == INSTALLED || code == MANUAL_QUIET || manual {
            // Version text goes through a leaked Box the UI thread takes back.
            let p = Box::into_raw(Box::new(ver)) as isize;
            let w = code | if manual { MANUAL_FLAG } else { 0 };
            unsafe { PostMessageW(notify as _, WM_APP_UPDATE, w, p) };
        }
    });
}

/// Take back the version string posted with WM_APP_UPDATE.
///
/// # Safety
/// `l` must be the lParam of a WM_APP_UPDATE message, taken exactly once.
pub unsafe fn take_version(l: isize) -> String {
    if l == 0 {
        return String::new();
    }
    *Box::from_raw(l as *mut String)
}

enum Outcome {
    NotWritable(String),
    Failed(String),
}

fn run(stage: &dyn Fn(usize, isize)) -> Result<Option<String>, Outcome> {
    let fail = |m: &str| Outcome::Failed(m.to_string());
    let api = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let json = get(&api, "application/vnd.github+json", &|_, _| {}).map_err(|e| match e.as_str() {
        "HTTP 404" => fail("GitHub answered 404: the repository is private (or has no releases), so it can't be read without signing in."),
        "HTTP 403" => fail("GitHub refused the request (rate limit). Try again in an hour."),
        _ => Outcome::Failed(e),
    })?;
    let json = String::from_utf8_lossy(&json);
    let tag = field(&json, "tag_name").ok_or_else(|| fail("Unexpected reply from GitHub (no version)."))?;
    if !newer(&tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let (url, digest) = asset(&json, ASSET).ok_or_else(|| fail("The latest release has no ResourceMonitor.exe."))?;
    if !url.starts_with("https://github.com/") {
        return Err(fail("Unexpected download address."));
    }
    let exe = std::env::current_exe().map_err(|_| fail("Can't find the running program."))?;
    let dir = exe.parent().ok_or_else(|| fail("Can't find the program folder."))?;
    if !writable(dir) {
        return Err(Outcome::NotWritable(tag));
    }
    stage(STAGE_DOWNLOAD, 0);
    let bin = get(&url, "application/octet-stream", &|got, total| {
        if total > 0 {
            stage(STAGE_DOWNLOAD, (got * 1000 / total) as isize);
        }
    })
    .map_err(|e| Outcome::Failed(format!("Download failed ({}).", e)))?;
    stage(STAGE_VERIFY, 1000);
    // Sanity: a Windows executable, and the exact bytes GitHub says it published.
    if bin.len() < 64 * 1024 || !bin.starts_with(b"MZ") {
        return Err(fail("The downloaded file isn't a valid program."));
    }
    if let Some(want) = digest {
        if !sha256_hex(&bin).eq_ignore_ascii_case(&want) {
            return Err(fail("The download didn't match GitHub's checksum, so it wasn't installed."));
        }
    }
    stage(STAGE_INSTALL, 1000);
    install(&exe, &bin).map_err(|_| Outcome::NotWritable(tag.clone()))?;
    Ok(Some(tag))
}

/// Write next to the exe, then swap names. The running exe can be renamed
/// but not overwritten.
fn install(exe: &Path, bin: &[u8]) -> std::io::Result<()> {
    let new = exe.with_file_name("ResourceMonitor.new.exe");
    let old = exe.with_file_name("ResourceMonitor.old.exe");
    std::fs::write(&new, bin)?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(&new, exe) {
        // Put the original back so the app still starts.
        let _ = std::fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".resmon-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// "v2.10.0" > "2.9.3"
fn newer(tag: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.trim_start_matches(['v', 'V']).split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (a, b) = (parse(tag), parse(current));
    a > b
}

/// Value of the first `"key": "value"` in `json` (string values only).
fn field(json: &str, key: &str) -> Option<String> {
    let k = format!("\"{}\"", key);
    let i = json.find(&k)? + k.len();
    let rest = json[i..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

/// (download URL, sha256 hex) of the asset called `name`.
fn asset(json: &str, name: &str) -> Option<(String, Option<String>)> {
    let pats = [format!("\"name\":\"{}\"", name), format!("\"name\": \"{}\"", name)];
    let start = pats.iter().filter_map(|p| json.find(p.as_str())).min()?;
    let body = &json[start + 8..];
    // This asset's fields end where the next asset's "name" begins.
    let end = body.find("\"name\"").unwrap_or(body.len());
    let body = &body[..end];
    let url = field(body, "browser_download_url")?;
    let digest = field(body, "digest").and_then(|d| d.strip_prefix("sha256:").map(str::to_string));
    Some((url, digest))
}

fn sha256_hex(data: &[u8]) -> String {
    let mut out = [0u8; 32];
    let st = unsafe {
        BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), data.len() as u32, out.as_mut_ptr(), 32)
    };
    if st != 0 {
        return String::new();
    }
    out.iter().map(|b| format!("{:02x}", b)).collect()
}

/// HTTPS GET via WinHTTP (follows redirects, uses the system proxy).
/// `progress(received, total)`; total is 0 when unknown.
fn get(url: &str, accept: &str, progress: &dyn Fn(u64, u64)) -> Result<Vec<u8>, String> {
    let net = |what: &str| format!("{}: no connection to GitHub (error {})", what, unsafe {
        windows_sys::Win32::Foundation::GetLastError()
    });
    let rest = url.strip_prefix("https://").ok_or("bad address")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = if path.is_empty() { "/" } else { path };
    struct H(*mut core::ffi::c_void);
    impl Drop for H {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }
    unsafe {
        let agent = wide(AGENT);
        let s = H(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0));
        if s.0.is_null() {
            return Err(net("open"));
        }
        WinHttpSetTimeouts(s.0, 10_000, 10_000, 15_000, 30_000);
        let h = wide(host);
        let c = H(WinHttpConnect(s.0, h.as_ptr(), 443, 0));
        if c.0.is_null() {
            return Err(net("connect"));
        }
        let verb = wide("GET");
        let p = wide(path);
        let r = H(WinHttpOpenRequest(
            c.0,
            verb.as_ptr(),
            p.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if r.0.is_null() {
            return Err(net("request"));
        }
        let hdr = wide(&format!("Accept: {}\r\n", accept));
        if WinHttpSendRequest(r.0, hdr.as_ptr(), u32::MAX, std::ptr::null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(r.0, std::ptr::null_mut()) == 0
        {
            return Err(net("send"));
        }
        let num = |q: u32| -> u64 {
            let mut v: u32 = 0;
            let mut len: u32 = 4;
            let ok = WinHttpQueryHeaders(
                r.0,
                q | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                &mut v as *mut u32 as *mut _,
                &mut len,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                0
            } else {
                v as u64
            }
        };
        let status = num(WINHTTP_QUERY_STATUS_CODE);
        if status != 200 {
            return Err(format!("HTTP {}", status));
        }
        let total = num(WINHTTP_QUERY_CONTENT_LENGTH);
        let mut body = Vec::with_capacity(total.min(64 << 20) as usize);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n: u32 = 0;
            if WinHttpReadData(r.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n) == 0 {
                return Err(net("read"));
            }
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n as usize]);
            progress(body.len() as u64, total);
            if body.len() > 64 * 1024 * 1024 {
                return Err("file too large".into());
            }
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_and_json() {
        assert!(newer("v2.10.0", "2.9.3"));
        assert!(!newer("v2.5.0", "2.5.0"));
        assert!(!newer("v2.4.9", "2.5.0"));
        let j = r#"{"tag_name": "v2.6.0", "assets": [
            {"name": "ResourceMonitor.exe", "uploader": {"login": "x"}, "digest": "sha256:abc",
             "browser_download_url": "https://github.com/a/b/releases/download/v2.6.0/ResourceMonitor.exe"},
            {"name": "Setup_ResourceMonitor.exe", "browser_download_url": "https://github.com/x/setup.exe"}]}"#;
        assert_eq!(field(j, "tag_name").as_deref(), Some("v2.6.0"));
        let (u, d) = asset(j, "ResourceMonitor.exe").unwrap();
        assert!(u.ends_with("/v2.6.0/ResourceMonitor.exe"));
        assert_eq!(d.as_deref(), Some("abc"));
    }
}
