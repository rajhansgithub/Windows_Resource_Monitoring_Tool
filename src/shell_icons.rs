//! Keeps Windows showing the current app icon.
//!
//! Explorer caches icons by file path (icon cache, taskbar pins, Start menu,
//! desktop). The in-app updater swaps ResourceMonitor.exe in place, so after
//! an icon change Windows kept showing the previous logo. Once per version, on
//! first start, in the background:
//!  * every shortcut to this app (Start menu, desktop, taskbar pins, Startup)
//!    gets an explicit icon from the current exe, and is repointed if it still
//!    targets an old install folder;
//!  * Explorer is told the exe and those shortcuts changed, and to drop its
//!    cached icons (SHCNE_ASSOCCHANGED, then `ie4uinit -show`).

use crate::util::{data_dir, diag, wide};
use std::path::{Path, PathBuf};
use windows::core::{Interface, PCWSTR};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};
use windows_sys::Win32::UI::Shell::*;

const EXE: &str = "ResourceMonitor.exe";
const MARK: &str = "shell-icons.txt";

/// Run the refresh once for this version (background thread, a few seconds after start).
pub fn refresh_once() {
    let mark = data_dir().join(MARK);
    let ver = env!("CARGO_PKG_VERSION");
    if std::fs::read_to_string(&mark).is_ok_and(|s| s.trim() == ver) {
        return;
    }
    let _ = std::thread::Builder::new().name("shell-icons".into()).spawn(move || {
        // Let Explorer settle after an update restart.
        std::thread::sleep(std::time::Duration::from_secs(4));
        let Ok(exe) = std::env::current_exe() else { return };
        let n = refresh(&exe);
        diag(&format!("shell icons refreshed for v{ver}: {n} shortcut(s)"));
        let _ = std::fs::write(&mark, ver);
    });
}

/// Fix every shortcut to the app and flush Explorer's icons. Returns shortcuts touched.
fn refresh(exe: &Path) -> usize {
    let mut n = 0;
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        for lnk in shortcuts() {
            if let Ok(true) = fix(&lnk, exe) {
                n += 1;
                let p = wide(&lnk.to_string_lossy());
                SHChangeNotify(SHCNE_UPDATEITEM as i32, SHCNF_PATHW, p.as_ptr() as _, std::ptr::null());
            }
        }
        if com {
            CoUninitialize();
        }
        let p = wide(&exe.to_string_lossy());
        SHChangeNotify(SHCNE_UPDATEITEM as i32, SHCNF_PATHW, p.as_ptr() as _, std::ptr::null());
        SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST | SHCNF_FLUSH, std::ptr::null(), std::ptr::null());
    }
    // Rebuilds the per-user icon cache (Windows 10/11).
    if let Some(sys) = std::env::var_os("SystemRoot") {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let tool = PathBuf::from(sys).join("System32").join("ie4uinit.exe");
        if tool.exists() {
            let _ = std::process::Command::new(tool).arg("-show").creation_flags(CREATE_NO_WINDOW).status();
        }
    }
    n
}

/// Folders where Windows or the installer keep shortcuts to the app.
fn shortcuts() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(ad) = std::env::var_os("APPDATA").map(PathBuf::from) {
        let programs = ad.join("Microsoft\\Windows\\Start Menu\\Programs");
        dirs.push(programs.join("Emulsion Labs"));
        dirs.push(programs.join("Resource Monitor"));
        dirs.push(programs.join("Startup"));
        dirs.push(programs.clone());
        let pinned = ad.join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned");
        dirs.push(pinned.join("TaskBar"));
        dirs.push(pinned.join("StartMenu"));
        if let Ok(rd) = std::fs::read_dir(pinned.join("ImplicitAppShortcuts")) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    let mut buf = [0u16; 260];
    unsafe {
        if SHGetFolderPathW(std::ptr::null_mut(), CSIDL_DESKTOPDIRECTORY as i32, std::ptr::null_mut(), 0, buf.as_mut_ptr()) == 0 {
            dirs.push(PathBuf::from(crate::util::from_wide(&buf)));
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(&d) {
            out.extend(
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk"))),
            );
        }
    }
    out
}

/// If `lnk` launches this app, point it (and its icon) at `exe` and save it.
/// Ok(false): a shortcut to something else, left alone.
pub(crate) fn fix(lnk: &Path, exe: &Path) -> windows::core::Result<bool> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        let l = wide(&lnk.to_string_lossy());
        file.Load(PCWSTR(l.as_ptr()), STGM_READWRITE)?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)?;
        let target = PathBuf::from(crate::util::from_wide(&buf));
        let ours = target.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(EXE));
        if !ours {
            return Ok(false);
        }
        let e = wide(&exe.to_string_lossy());
        if !same_file(&target, exe) {
            // Pinned before the app moved folders: the old target is gone.
            link.SetPath(PCWSTR(e.as_ptr()))?;
            if let Some(dir) = exe.parent() {
                let d = wide(&dir.to_string_lossy());
                link.SetWorkingDirectory(PCWSTR(d.as_ptr()))?;
            }
        }
        // Resource id 1 (negative index = id), not "first icon": a new cache key,
        // so Explorer has to read the icon again instead of reusing the old one.
        link.SetIconLocation(PCWSTR(e.as_ptr()), -1)?;
        file.Save(PCWSTR::null(), true)?;
        Ok(true)
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shortcut left at a pre-2.13 folder is repointed at the running exe and
    /// gets the explicit icon; a shortcut to another program is not touched.
    #[test]
    fn repoints_old_shortcut_and_skips_others() {
        let dir = std::env::temp_dir().join(format!("resmon-lnk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("new").join(EXE);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"x").unwrap();
        let old = dir.join("Resource Monitor").join(EXE);
        let other = std::env::var("SystemRoot").map(|s| PathBuf::from(s).join("notepad.exe")).unwrap();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let make = |name: &str, target: &Path| {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                let t = wide(&target.to_string_lossy());
                link.SetPath(PCWSTR(t.as_ptr())).unwrap();
                let p = dir.join(name);
                let w = wide(&p.to_string_lossy());
                link.cast::<IPersistFile>().unwrap().Save(PCWSTR(w.as_ptr()), true).unwrap();
                p
            };
            let ours = make("Resource Monitor.lnk", &old);
            let theirs = make("Notepad.lnk", &other);
            assert!(fix(&ours, &exe).unwrap());
            assert!(!fix(&theirs, &exe).unwrap());

            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let w = wide(&ours.to_string_lossy());
            link.cast::<IPersistFile>().unwrap().Load(PCWSTR(w.as_ptr()), STGM_READ).unwrap();
            let mut buf = [0u16; 1024];
            link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).unwrap();
            assert!(same_file(Path::new(&crate::util::from_wide(&buf)), &exe));
            let mut icon = [0u16; 1024];
            let mut idx = 0;
            link.GetIconLocation(&mut icon, &mut idx).unwrap();
            assert!(same_file(Path::new(&crate::util::from_wide(&icon)), &exe));
            assert_eq!(idx, -1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
