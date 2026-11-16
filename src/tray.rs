//! Notification-area icon.

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 1;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    added: bool,
    last_tip: String,
}

fn copy(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.encode_utf16().take(dst.len() - 1) {
        dst[n] = c;
        n += 1;
    }
    dst[n] = 0;
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        let mut icon: HICON = std::ptr::null_mut();
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            // DPI-correct small icon from resource id 1.
            if LoadIconMetric(hinst, 1 as _, LIM_SMALL, &mut icon) != 0 || icon.is_null() {
                icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
            }
        }
        Tray { hwnd, icon, added: false, last_tip: String::new() }
    }

    fn base(&self) -> NOTIFYICONDATAW {
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = self.hwnd;
        d.uID = 1;
        d
    }

    /// (Re)create the icon — also called after Explorer restarts.
    pub fn add(&mut self) {
        let mut d = self.base();
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = self.icon;
        copy(&mut d.szTip, if self.last_tip.is_empty() { "Resource Monitor" } else { &self.last_tip });
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &d);
            self.added = Shell_NotifyIconW(NIM_ADD, &d) != 0;
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
    }

    pub fn tip(&mut self, s: &str) {
        if s == self.last_tip {
            return;
        }
        self.last_tip = s.to_string();
        let mut d = self.base();
        d.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy(&mut d.szTip, s);
        unsafe {
            if Shell_NotifyIconW(NIM_MODIFY, &d) == 0 {
                self.add();
            }
        }
    }

    pub fn balloon(&self, title: &str, text: &str) {
        let mut d = self.base();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
        copy(&mut d.szInfoTitle, title);
        copy(&mut d.szInfo, text);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn remove(&mut self) {
        if self.added {
            let d = self.base();
            unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
            self.added = false;
        }
    }
}
