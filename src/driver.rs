//! Missing device drivers and the one-tap fix on the Full speed page.
//!
//! - `missing()` lists present devices Windows has no driver for (problem code 28), and
//!   graphics cards left on Microsoft's basic display driver (which caps the refresh rate
//!   and turns off the GPU's own acceleration). No admin.
//! - `search()` asks Windows Update for driver updates that match those devices' hardware
//!   ids. Works as a standard user; slow (seconds to a minute), so run it on a thread.
//! - `install_elevated()` starts a second copy of the app as administrator with
//!   `--install-drivers <ids>`, which downloads and installs only those matching updates
//!   through Windows Update (`run_install`). Windows Update checks every package's
//!   signature itself. Progress goes through a small file in %TEMP%.
//! - When Windows Update has nothing, `vendor_url` opens the GPU maker's own driver app page.

use crate::util::wide;

/// A device that runs without its proper driver.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Missing {
    pub name: String,
    /// Hardware ids, most specific first ("PCI\VEN_10DE&DEV_2684&SUBSYS_...").
    pub hwids: Vec<String>,
    /// PCI vendor id when the device is on PCI (0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel).
    pub vendor: u32,
    /// A graphics card on the basic display driver (or with no driver at all).
    pub gpu: bool,
}

/// CM problem code "The drivers for this device are not installed".
pub const PROB_NO_DRIVER: u32 = 28;

/// PCI vendor id from a hardware id ("PCI\VEN_10DE&DEV_..." -> 0x10DE).
pub fn pci_vendor(hwid: &str) -> u32 {
    let u = hwid.to_ascii_uppercase();
    u.find("VEN_")
        .and_then(|i| u.get(i + 4..i + 8))
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .unwrap_or(0)
}

/// Is this device one we flag? `problem` is its CM problem code (0 = none),
/// `service` its driver service name, `class` its setup class name.
pub fn is_missing(problem: u32, service: &str, class: &str, hwids: &[String]) -> Option<bool> {
    let vendor = hwids.iter().map(|h| pci_vendor(h)).find(|v| *v != 0).unwrap_or(0);
    let gpu_vendor = matches!(vendor, 0x10DE | 0x1002 | 0x8086);
    let display = class.eq_ignore_ascii_case("Display");
    if display && gpu_vendor && service.eq_ignore_ascii_case("BasicDisplay") {
        return Some(true);
    }
    if problem == PROB_NO_DRIVER {
        // Only hardware the user can do something about: PCI cards and USB devices.
        let real = hwids.iter().any(|h| {
            let u = h.to_ascii_uppercase();
            u.starts_with("PCI\\") || u.starts_with("USB\\")
        });
        if real {
            return Some(display || (gpu_vendor && hwids.iter().any(|h| h.to_ascii_uppercase().contains("CC_03"))));
        }
    }
    None
}

/// Short maker name for a PCI vendor id.
pub fn vendor_name(v: u32) -> &'static str {
    match v {
        0x10DE => "NVIDIA",
        0x1002 => "AMD",
        0x8086 => "Intel",
        _ => "",
    }
}

/// The maker's own driver page (their updater picks the right driver).
pub fn vendor_url(v: u32) -> Option<&'static str> {
    match v {
        0x10DE => Some("https://www.nvidia.com/en-us/software/nvidia-app/"),
        0x1002 => Some("https://www.amd.com/en/support/download/drivers.html"),
        0x8086 => Some("https://www.intel.com/content/www/us/en/support/detect.html"),
        _ => None,
    }
}

/// Does a Windows Update driver (its `DriverHardwareID`) fit one of these devices?
pub fn fits(update_hwid: &str, devices: &[Missing]) -> bool {
    let u = update_hwid.trim().to_ascii_uppercase();
    !u.is_empty() && devices.iter().flat_map(|d| d.hwids.iter()).any(|h| {
        let h = h.to_ascii_uppercase();
        // Device ids get more specific to the right: a driver for the shorter id fits.
        h == u || h.starts_with(&u)
    })
}

/// Where the elevated installer reports its step ("download", "install", "done", "none", "failed").
pub fn progress_path() -> std::path::PathBuf {
    std::env::temp_dir().join("ResourceMonitor-driver.txt")
}

fn report(step: &str) {
    let _ = std::fs::write(progress_path(), step);
}

pub fn read_progress() -> String {
    std::fs::read_to_string(progress_path()).unwrap_or_default().trim().to_string()
}

// ------------------------------------------------------------------ Windows only

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

    fn reg_prop(node: u32, prop: u32) -> Vec<u16> {
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let mut ty = 0u32;
        // SAFETY: buffer and length describe the same allocation.
        let r = unsafe { CM_Get_DevNode_Registry_PropertyW(node, prop, &mut ty, buf.as_mut_ptr().cast(), &mut len, 0) };
        if r != CR_SUCCESS {
            return Vec::new();
        }
        buf.truncate((len as usize / 2).min(buf.len()));
        buf
    }

    pub fn prop_str(node: u32, prop: u32) -> String {
        let b = reg_prop(node, prop);
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf16_lossy(&b[..end])
    }

    pub fn prop_multi(node: u32, prop: u32) -> Vec<String> {
        reg_prop(node, prop).split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }

    pub fn missing() -> Vec<Missing> {
        let mut out = Vec::new();
        // SAFETY: no class GUID with DIGCF_ALLCLASSES; the set is destroyed below.
        let set = unsafe { SetupDiGetClassDevsW(std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), DIGCF_ALLCLASSES | DIGCF_PRESENT) };
        if set == INVALID_HANDLE_VALUE as isize || set == 0 {
            return out;
        }
        for i in 0..4096u32 {
            // SAFETY: zeroed plain data with cbSize set.
            let mut d: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
            d.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            // SAFETY: valid set and out struct; failure = end of list.
            if unsafe { SetupDiEnumDeviceInfo(set, i, &mut d) } == 0 {
                break;
            }
            let node = d.DevInst;
            let (mut status, mut problem) = (0u32, 0u32);
            // SAFETY: out pointers valid.
            unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) };
            let class = prop_str(node, CM_DRP_CLASS);
            let service = prop_str(node, CM_DRP_SERVICE);
            if problem != PROB_NO_DRIVER && !class.eq_ignore_ascii_case("Display") {
                continue;
            }
            let hwids = prop_multi(node, CM_DRP_HARDWAREID);
            if let Some(gpu) = is_missing(problem, &service, &class, &hwids) {
                let mut name = prop_str(node, CM_DRP_FRIENDLYNAME);
                if name.is_empty() {
                    name = prop_str(node, CM_DRP_DEVICEDESC);
                }
                let vendor = hwids.iter().map(|h| pci_vendor(h)).find(|v| *v != 0).unwrap_or(0);
                if gpu && !vendor_name(vendor).is_empty() && name.to_ascii_lowercase().contains("basic") {
                    name = format!("{} graphics", vendor_name(vendor));
                }
                out.push(Missing { name, hwids, vendor, gpu });
            }
        }
        // SAFETY: set from SetupDiGetClassDevsW.
        unsafe { SetupDiDestroyDeviceInfoList(set) };
        out.sort_by_key(|m| !m.gpu);
        out
    }

    use windows::core::{Interface, BSTR};
    use windows::Win32::System::Com::*;
    use windows::Win32::System::UpdateAgent::*;

    struct Com;
    impl Com {
        fn init() -> Com {
            // SAFETY: balanced by Drop on this thread.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Com
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            // SAFETY: paired with init.
            unsafe { CoUninitialize() };
        }
    }

    /// Driver updates on Windows Update that fit `devices`: (title, update).
    fn matching(session: &IUpdateSession, devices: &[Missing]) -> windows::core::Result<Vec<(String, IUpdate)>> {
        // SAFETY: COM calls on live interfaces.
        unsafe {
            let searcher = session.CreateUpdateSearcher()?;
            let res = searcher.Search(&BSTR::from("IsInstalled=0 and Type='Driver' and IsHidden=0"))?;
            let ups = res.Updates()?;
            let mut out = Vec::new();
            for i in 0..ups.Count()? {
                let u = ups.get_Item(i)?;
                let Ok(d) = u.cast::<IWindowsDriverUpdate>() else { continue };
                let id = d.DriverHardwareID().map(|b| b.to_string()).unwrap_or_default();
                if fits(&id, devices) {
                    out.push((u.Title().map(|b| b.to_string()).unwrap_or_default(), u));
                }
            }
            Ok(out)
        }
    }

    fn session() -> windows::core::Result<IUpdateSession> {
        // SAFETY: standard COM activation of the Windows Update session.
        unsafe {
            let s: IUpdateSession = CoCreateInstance(&UpdateSession, None, CLSCTX_INPROC_SERVER)?;
            let _ = s.SetClientApplicationID(&BSTR::from("Resource Monitor"));
            Ok(s)
        }
    }

    /// Titles of matching driver updates (standard user is fine).
    pub fn search(devices: &[Missing]) -> Result<Vec<String>, String> {
        let _com = Com::init();
        let s = session().map_err(|e| e.message().to_string())?;
        matching(&s, devices).map(|v| v.into_iter().map(|x| x.0).collect()).map_err(|e| e.message().to_string())
    }

    /// Elevated side: download and install the matching updates. Exit code for the parent.
    pub fn run_install(devices: &[Missing]) -> i32 {
        let _com = Com::init();
        let go = || -> windows::core::Result<i32> {
            let s = session()?;
            let found = matching(&s, devices)?;
            if found.is_empty() {
                report("none");
                return Ok(3);
            }
            // SAFETY: COM calls on live interfaces.
            unsafe {
                let coll: IUpdateCollection = CoCreateInstance(&UpdateCollection, None, CLSCTX_INPROC_SERVER)?;
                for (_, u) in &found {
                    if u.EulaAccepted().map(|b| !b.as_bool()).unwrap_or(false) {
                        let _ = u.AcceptEula();
                    }
                    coll.Add(u)?;
                }
                report("download");
                let dl = s.CreateUpdateDownloader()?;
                dl.SetUpdates(&coll)?;
                let r = dl.Download()?;
                if r.ResultCode()? != orcSucceeded && r.ResultCode()? != orcSucceededWithErrors {
                    report("failed");
                    return Ok(1);
                }
                report("install");
                let inst = s.CreateUpdateInstaller()?;
                inst.SetUpdates(&coll)?;
                let r = inst.Install()?;
                let ok = matches!(r.ResultCode()?, x if x == orcSucceeded || x == orcSucceededWithErrors);
                report(if ok { "done" } else { "failed" });
                Ok(if ok { 0 } else { 1 })
            }
        };
        go().unwrap_or_else(|_| {
            report("failed");
            1
        })
    }
}

#[cfg(windows)]
pub use win::{missing, prop_multi as prop_multi_pub, prop_str as prop_str_pub, run_install, search};

#[cfg(not(windows))]
pub fn missing() -> Vec<Missing> {
    Vec::new()
}
#[cfg(not(windows))]
pub fn search(_: &[Missing]) -> Result<Vec<String>, String> {
    Err("Windows only".into())
}
#[cfg(not(windows))]
pub fn run_install(_: &[Missing]) -> i32 {
    1
}

/// Hardware ids on the command line: joined with '|'.
pub fn encode_ids(devices: &[Missing]) -> String {
    devices.iter().flat_map(|d| d.hwids.first()).cloned().collect::<Vec<_>>().join("|")
}

pub fn decode_ids(s: &str) -> Vec<Missing> {
    s.split('|').filter(|x| !x.trim().is_empty()).map(|h| Missing { hwids: vec![h.trim().to_string()], ..Default::default() }).collect()
}

/// Start the elevated installer and wait for it. Returns its exit code, or None when the
/// user said no to the admin prompt. Call from a worker thread.
#[cfg(windows)]
pub fn install_elevated(devices: &[Missing]) -> Option<u32> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    let exe = std::env::current_exe().ok()?;
    let exe = wide(&exe.to_string_lossy());
    let args = wide(&format!("--install-drivers \"{}\"", encode_ids(devices)));
    let verb = wide("runas");
    let _ = std::fs::remove_file(progress_path());
    // SAFETY: zeroed plain struct; every pointer outlives the call.
    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS;
    sei.lpVerb = verb.as_ptr();
    sei.lpFile = exe.as_ptr();
    sei.lpParameters = args.as_ptr();
    sei.nShow = 0;
    // SAFETY: sei filled above.
    if unsafe { ShellExecuteExW(&mut sei) } == 0 || sei.hProcess.is_null() {
        return None;
    }
    let mut code = 1u32;
    // SAFETY: process handle we own.
    unsafe {
        WaitForSingleObject(sei.hProcess, INFINITE);
        GetExitCodeProcess(sei.hProcess, &mut code);
        CloseHandle(sei.hProcess);
    }
    Some(code)
}

#[cfg(not(windows))]
pub fn install_elevated(_: &[Missing]) -> Option<u32> {
    let _ = wide;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn vendor_from_hwid() {
        assert_eq!(pci_vendor("PCI\\VEN_10DE&DEV_2684&SUBSYS_16F310DE"), 0x10DE);
        assert_eq!(pci_vendor("pci\\ven_1002&dev_744c"), 0x1002);
        assert_eq!(pci_vendor("USB\\VID_046D&PID_C52B"), 0);
    }

    #[test]
    fn basic_display_gpu_is_missing() {
        let h = ids(&["PCI\\VEN_10DE&DEV_2684&SUBSYS_16F310DE&REV_A1", "PCI\\VEN_10DE&DEV_2684"]);
        assert_eq!(is_missing(0, "BasicDisplay", "Display", &h), Some(true));
        // Its own driver: fine.
        assert_eq!(is_missing(0, "nvlddmkm", "Display", &h), None);
        // A virtual adapter on the basic driver (no PCI vendor we know): not flagged.
        assert_eq!(is_missing(0, "BasicDisplay", "Display", &ids(&["ROOT\\BasicDisplay"])), None);
    }

    #[test]
    fn no_driver_devices() {
        let h = ids(&["PCI\\VEN_8086&DEV_51F0&CC_0280"]);
        assert_eq!(is_missing(PROB_NO_DRIVER, "", "", &h), Some(false));
        assert_eq!(is_missing(PROB_NO_DRIVER, "", "", &ids(&["ACPI\\VEN_INT&DEV_33A0"])), None);
        assert_eq!(is_missing(0, "", "", &h), None);
    }

    #[test]
    fn update_fits_device() {
        let d = vec![Missing { hwids: ids(&["PCI\\VEN_10DE&DEV_2684&SUBSYS_16F310DE&REV_A1", "PCI\\VEN_10DE&DEV_2684"]), ..Default::default() }];
        assert!(fits("pci\\ven_10de&dev_2684", &d));
        assert!(fits("PCI\\VEN_10DE&DEV_2684&SUBSYS_16F310DE", &d));
        assert!(!fits("PCI\\VEN_10DE&DEV_2704", &d));
        assert!(!fits("", &d));
    }

    #[test]
    fn ids_round_trip() {
        let d = vec![
            Missing { hwids: ids(&["PCI\\VEN_10DE&DEV_2684", "x"]), ..Default::default() },
            Missing { hwids: ids(&["USB\\VID_0BDA&PID_8153"]), ..Default::default() },
        ];
        let back = decode_ids(&encode_ids(&d));
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].hwids[0], "PCI\\VEN_10DE&DEV_2684");
        assert!(fits("USB\\VID_0BDA&PID_8153", &back));
    }
}
