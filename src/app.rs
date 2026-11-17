//! Application state, controller window, menu and event handling.
//!
//! A hidden controller window owns the tray icon, timers and system
//! broadcasts. Each taskbar (main + secondary monitors) gets its own
//! `Overlay` widget window.

use crate::fmt::{self, SpeedUnit};
use crate::overlay::{Drag, Overlay};
use crate::render::Rgba;
use crate::sensors::{AdapterInfo, SampleOpts, Sensors, Snapshot};
use crate::settings::{Metric, Mode, Settings, Theme};
use crate::taskbar::{self, Taskbar};
use crate::tray::{Tray, WM_TRAY};
use crate::usage::{self, UsageStore};
use crate::util::*;
use crate::{popup, report};
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::time::{Instant, SystemTime};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Power::*;
use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::Controls::{WM_MOUSEHOVER, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const MAIN_CLASS: &str = "ResourceMonitor.Main";
pub const WM_APP_ACTIVATE: u32 = WM_APP + 2;
/// History length in samples (1 hour at the default 1 s interval).
pub const HIST: usize = 3600;

const T_SAMPLE: usize = 1;
/// One-shot: restart into the freshly installed version.
const T_RESTART: usize = 3;
/// Appbar callback: Windows tells us when a full-screen app opens/closes.
const WM_APPBAR: u32 = WM_APP + 5;
/// Settings file changed on disk (from the watcher thread).
const WM_SETTINGS_FILE: u32 = WM_APP + 6;

pub const C_WARN: Rgba = Rgba::rgb(255, 183, 77);
pub const C_CRIT: Rgba = Rgba::rgb(255, 82, 82);

/// Per-metric colour overrides (0 = none, else 0x01RRGGBB); set from settings.
static ACCENT_OVERRIDE: [std::sync::atomic::AtomicU32; Metric::ALL.len()] =
    [const { std::sync::atomic::AtomicU32::new(0) }; Metric::ALL.len()];

/// Load colour overrides: explicit per-metric colours, else (optionally) the
/// Windows accent colour for every metric.
pub fn set_accents(s: &Settings) {
    use std::sync::atomic::Ordering::Relaxed;
    let win = if s.windows_accent {
        let (mut c, mut opaque) = (0u32, 0i32);
        let ok = unsafe { windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut c, &mut opaque) } == 0;
        ok.then_some(c & 0xFF_FFFF)
    } else {
        None
    };
    for m in Metric::ALL {
        let own = s.colors.iter().find(|(x, _)| *x == m).map(|(_, c)| *c);
        let v = own.or(win).map_or(0, |c| 0x0100_0000 | c);
        ACCENT_OVERRIDE[m.idx()].store(v, Relaxed);
    }
}

/// Built-in colour for a metric (ignores overrides).
pub fn default_accent(m: Metric) -> Rgba {
    match m {
        Metric::Cpu => Rgba::rgb(79, 195, 247),
        Metric::Ram => Rgba::rgb(179, 157, 219),
        Metric::Gpu | Metric::GpuTemp => Rgba::rgb(255, 138, 101),
        Metric::Vram => Rgba::rgb(255, 171, 145),
        Metric::DiskRead | Metric::DiskWrite | Metric::DiskActive | Metric::DiskFree => Rgba::rgb(255, 213, 79),
        Metric::Up => Rgba::rgb(255, 145, 0),
        Metric::Down => Rgba::rgb(38, 198, 218),
        Metric::TodayUp | Metric::TodayDown => Rgba::rgb(165, 214, 167),
        Metric::TopProc => Rgba::rgb(176, 190, 197),
        Metric::Battery => Rgba::rgb(129, 199, 132),
    }
}

pub fn accent(m: Metric, dark: bool) -> Rgba {
    let o = ACCENT_OVERRIDE[m.idx()].load(std::sync::atomic::Ordering::Relaxed);
    let c = if o != 0 {
        Rgba::rgb((o >> 16) as u8, (o >> 8) as u8, o as u8)
    } else {
        default_accent(m)
    };
    #[allow(unreachable_code)]
    if dark {
        c
    } else {
        c.mix(Rgba::rgb(0, 0, 0), 0.38)
    }
}

/// Usage colour ramp. Low usage: the metric's colour, a little muted; it
/// grows more saturated as usage rises, turns amber at `warn` and red at
/// `crit`.
pub fn heat(base: Rgba, pct: f32, warn: f32, crit: f32) -> Rgba {
    let gray = Rgba::rgb(140, 140, 140);
    let lo = (warn - 25.0).max(1.0);
    if pct <= lo {
        // 0% → 45% muted, lo → full colour.
        base.mix(gray, 0.45 * (1.0 - pct / lo))
    } else if pct <= warn {
        base.mix(C_WARN, (pct - lo) / (warn - lo).max(1.0))
    } else if pct <= crit {
        C_WARN.mix(C_CRIT, (pct - warn) / (crit - warn).max(1.0))
    } else {
        C_CRIT
    }
}

/// Same ramp for text: stays the normal text colour until it gets busy.
pub fn heat_text(fg: Rgba, pct: f32, warn: f32, crit: f32) -> Rgba {
    let lo = (warn - 25.0).max(1.0);
    if pct <= lo {
        fg
    } else if pct <= warn {
        fg.mix(C_WARN, (pct - lo) / (warn - lo).max(1.0))
    } else {
        heat(C_WARN, pct, warn, crit)
    }
}

pub fn short_label(m: Metric) -> &'static str {
    match m {
        Metric::Cpu => "CPU",
        Metric::Ram => "RAM",
        Metric::Gpu => "GPU",
        Metric::Vram => "VRAM",
        Metric::GpuTemp => "TEMP",
        Metric::DiskFree => "C:",
        Metric::DiskRead => "DR",
        Metric::DiskWrite => "DW",
        Metric::DiskActive => "DISK",
        Metric::Up => "\u{25B2}",
        Metric::Down => "\u{25BC}",
        Metric::TodayUp => "T\u{25B2}",
        Metric::TodayDown => "T\u{25BC}",
        Metric::TopProc => "TOP",
        Metric::Battery => "BAT",
    }
}

pub fn effective_dark(t: Theme) -> bool {
    match t {
        Theme::Dark => true,
        Theme::Light => false,
        Theme::Auto => {
            read_dword(
                HKEY_CURRENT_USER,
                "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
                "SystemUsesLightTheme",
            )
            .unwrap_or(0)
                == 0
        }
    }
}

// ==================================================================== model

/// Everything the widgets and panels draw from.
pub struct Model {
    pub s: Settings,
    pub snap: Snapshot,
    pub hist: Vec<VecDeque<f32>>,
    pub usage: UsageStore,
    pub dark: bool,
    pub samples: u64,
    pub gpu_seen: bool,
}

impl Model {
    pub fn rows(&self) -> Vec<Metric> {
        self.s
            .order
            .iter()
            .copied()
            .filter(|&m| self.s.shown(m))
            .filter(|&m| match m {
                // Hide sensors this machine doesn't have, once we know.
                Metric::Gpu => self.gpu_seen || self.samples < 4,
                Metric::Vram => (self.gpu_seen && self.snap.vram_used.is_some()) || self.samples < 4,
                Metric::Battery => self.snap.battery.is_some(),
                Metric::DiskActive => self.snap.disk_active.is_some() || self.samples < 4,
                Metric::GpuTemp => self.snap.gpu_temp.is_some() || self.samples < 4,
                Metric::DiskFree => self.snap.disk_used_pct.is_some() || self.samples < 4,
                _ => true,
            })
            .collect()
    }

    fn level(&self, pct: f32) -> u8 {
        if pct >= self.s.crit_pct {
            2
        } else if pct >= self.s.warn_pct {
            1
        } else {
            0
        }
    }

    /// Usage as a percentage, for metrics that have one (drives the colour ramp).
    pub fn pct_of(&self, m: Metric) -> Option<f32> {
        let s = &self.snap;
        match m {
            Metric::Cpu => Some(s.cpu),
            Metric::Ram => Some(s.ram_pct),
            Metric::Gpu => s.gpu,
            Metric::DiskActive => s.disk_active,
            Metric::TodayUp | Metric::TodayDown => self.cycle_usage().map(|(_, p)| p as f32),
            // 40 °C cool … 95 °C hot.
            Metric::GpuTemp => s.gpu_temp.map(|t| ((t - 40.0) / 55.0 * 100.0).clamp(0.0, 100.0)),
            Metric::DiskFree => s.disk_used_pct,
            _ => None,
        }
    }

    pub fn heat(&self, base: Rgba, pct: f32) -> Rgba {
        heat(base, pct, self.s.warn_pct, self.s.crit_pct)
    }

    pub fn heat_text(&self, fg: Rgba, pct: f32) -> Rgba {
        heat_text(fg, pct, self.s.warn_pct, self.s.crit_pct)
    }

    pub fn cycle_usage(&self) -> Option<(u64, f64)> {
        (self.s.data_cap_gb > 0.0).then(|| {
            let now = usage::today();
            let start = UsageStore::cycle_start(self.s.cap_reset_day, now);
            let used = self.usage.range(start, now).total();
            let cap = self.s.data_cap_gb * 1024.0 * 1024.0 * 1024.0;
            (used, used as f64 / cap * 100.0)
        })
    }

    /// Value text, colour level, and template used to reserve width.
    pub fn value(&self, m: Metric) -> (String, u8, &'static str) {
        let s = &self.snap;
        let pct = |v: f32| format!("{:.0}%", v);
        let speed_tpl = match self.s.unit {
            SpeedUnit::AutoBits | SpeedUnit::Mbps => "8.88 Mbps",
            SpeedUnit::Kbps => "888 Kbps",
            SpeedUnit::KBs => "888 KB/s",
            _ => "8.88 MB/s",
        };
        match m {
            Metric::Cpu => (pct(s.cpu), self.level(s.cpu), "100%"),
            Metric::Ram => (pct(s.ram_pct), self.level(s.ram_pct), "100%"),
            Metric::Gpu => match s.gpu {
                Some(g) => (pct(g), self.level(g), "100%"),
                None => ("\u{2013}".into(), 0, "100%"),
            },
            Metric::Vram => (s.vram_used.map_or("\u{2013}".into(), fmt::bytes), 0, "8.88 GB"),
            Metric::GpuTemp => (s.gpu_temp.map_or("\u{2013}".into(), |t| format!("{:.0}\u{00B0}C", t)), 0, "100\u{00B0}C"),
            Metric::DiskFree => (s.disk_used_pct.map_or("\u{2013}".into(), |p| format!("{:.0}%", p)), 0, "100%"),
            Metric::DiskRead => (fmt::rate(s.disk_read), 0, "8.88 MB/s"),
            Metric::DiskWrite => (fmt::rate(s.disk_write), 0, "8.88 MB/s"),
            Metric::DiskActive => {
                let v = s.disk_active.unwrap_or(0.0);
                (pct(v), self.level(v), "100%")
            }
            Metric::Up => (fmt::speed(s.net_up, self.s.unit), 0, speed_tpl),
            Metric::Down => (fmt::speed(s.net_down, self.s.unit), 0, speed_tpl),
            Metric::TodayUp | Metric::TodayDown => {
                let u = self.usage.get(usage::today());
                let v = if m == Metric::TodayUp { u.sent } else { u.recv };
                let lvl = self.cycle_usage().map_or(0, |(_, p)| self.level(p as f32));
                (fmt::bytes(v), lvl, "888 MB")
            }
            Metric::TopProc => match s.top_cpu.first() {
                Some(p) => (format!("{} {:.0}%", p.name, p.cpu), 0, "mmmmmmmmm 99%"),
                None => ("\u{2013}".into(), 0, "mmmmmmmmm 99%"),
            },
            Metric::Battery => match s.battery {
                Some(b) => {
                    let lvl = if b.on_ac || b.charging {
                        0
                    } else if b.percent <= 15 {
                        2
                    } else if b.percent <= 30 {
                        1
                    } else {
                        0
                    };
                    (format!("{}%{}", b.percent, if b.charging { "+" } else { "" }), lvl, "100%+")
                }
                None => ("\u{2013}".into(), 0, "100%"),
            },
        }
    }

    /// Normalised series (0..1) for a metric, newest last.
    pub fn series(&self, m: Metric, n: usize) -> Vec<f32> {
        let h = &self.hist[m.idx()];
        let start = h.len().saturating_sub(n);
        let data = h.iter().skip(start).copied();
        let floor = match m {
            Metric::Cpu | Metric::Ram | Metric::Gpu | Metric::DiskActive | Metric::Battery | Metric::DiskFree => {
                return data.map(|v| v / 100.0).collect()
            }
            // 30..100 °C mapped to the chart.
            Metric::GpuTemp => return data.map(|v| ((v - 30.0) / 70.0).clamp(0.0, 1.0)).collect(),
            Metric::Up | Metric::Down => 64.0 * 1024.0,
            Metric::DiskRead | Metric::DiskWrite => 1024.0 * 1024.0,
            _ => 1.0,
        };
        let max = data.clone().fold(floor, f32::max);
        data.map(|v| v / max).collect()
    }
}

// ==================================================================== app

pub struct App {
    /// Hidden controller window: tray, timers, broadcasts.
    pub hwnd: HWND,
    pub popup: HWND,
    pub report: HWND,
    /// The Settings window (one instance), null when closed.
    pub settings_win: HWND,
    pub m: Model,
    settings_mtime: Option<SystemTime>,
    sensors: Sensors,
    last_save: Instant,
    adapters: Vec<AdapterInfo>,
    pub overlays: Vec<Overlay>,
    /// All taskbars found on the last refresh (for the Displays menu).
    pub taskbars: Vec<Taskbar>,
    display_on: bool,
    tray: Tray,
    msg_taskbar_created: u32,
    pub popup_pinned: bool,
    /// Overlay the popup belongs to.
    pub popup_anchor: HWND,
    hook: HWINEVENTHOOK,
    shut_down: bool,
    detail_until: Option<Instant>,
    menu_open: bool,
    /// Item under the mouse/keyboard in the open menu (from WM_MENUSELECT).
    menu_sel: (HMENU, usize),
    /// Update window and what it shows.
    pub upd: HWND,
    pub upd_text: String,
    pub upd_pct: Option<f32>,
    /// Background UI Automation watcher for taskbar app-icon positions.
    icons: Option<crate::icons::Watcher>,
    /// Set by events: re-read taskbar geometry on the next refresh.
    need_locate: bool,
    /// Set by events: re-evaluate full-screen state and z-order.
    need_check: bool,
    /// Location-change hook scoped to the taskbar's own thread.
    move_hook: HWINEVENTHOOK,
    move_hook_tid: u32,
    /// Fonts + canvas reused by the details panel between repaints.
    pub popup_cache: Option<(popup::Fonts, crate::render::Canvas)>,
}

impl Deref for App {
    type Target = Model;
    fn deref(&self) -> &Model {
        &self.m
    }
}
impl DerefMut for App {
    fn deref_mut(&mut self) -> &mut Model {
        &mut self.m
    }
}

static mut APP: *mut App = std::ptr::null_mut();

/// Single UI thread. Callers never hold the returned reference across a
/// nested message loop (menus) — they re-fetch it afterwards.
#[allow(static_mut_refs)]
pub fn app() -> Option<&'static mut App> {
    unsafe { APP.as_mut() }
}

const GUID_CONSOLE_DISPLAY_STATE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

fn guid_eq(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

pub fn create(hinst: HINSTANCE, after_update: bool) -> Option<()> {
    unsafe {
        let cls = wide(MAIN_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: hinst,
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        crate::overlay::register(hinst, Some(overlay_proc));
        popup::register(hinst);
        crate::update_ui::register(hinst);
        report::register(hinst);
        crate::settings_ui::register(hinst);

        // Hidden top-level window (not message-only: those miss broadcasts
        // such as TaskbarCreated, WM_SETTINGCHANGE and WM_DISPLAYCHANGE).
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            cls.as_ptr(),
            cls.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let s = Settings::load();
        let first_run = s.first_run;
        let dark = effective_dark(s.theme);
        set_accents(&s);
        let mut a = Box::new(App {
            hwnd,
            popup: std::ptr::null_mut(),
            report: std::ptr::null_mut(),
            settings_win: std::ptr::null_mut(),
            m: Model {
                s,
                snap: Snapshot::default(),
                hist: (0..Metric::ALL.len()).map(|_| VecDeque::with_capacity(HIST)).collect(),
                usage: UsageStore::load(),
                dark,
                samples: 0,
                gpu_seen: false,
            },
            settings_mtime: None,
            sensors: Sensors::new(),
            last_save: Instant::now(),
            adapters: Vec::new(),
            overlays: Vec::new(),
            taskbars: Vec::new(),
            display_on: true,
            tray: Tray::new(hwnd),
            msg_taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            popup_pinned: false,
            popup_anchor: std::ptr::null_mut(),
            hook: std::ptr::null_mut(),
            shut_down: false,
            detail_until: None,
            menu_open: false,
            popup_cache: None,
            icons: None,
            upd: std::ptr::null_mut(),
            upd_text: String::new(),
            upd_pct: None,
            menu_sel: (std::ptr::null_mut(), 0),
            need_locate: true,
            need_check: true,
            move_hook: std::ptr::null_mut(),
            move_hook_tid: 0,
        });
        a.icons = Some(crate::icons::Watcher::start(hwnd as isize));
        // Register as a (zero-size, space-less) appbar purely to receive
        // ABN_FULLSCREENAPP when a full-screen app opens or closes.
        let mut abd: APPBARDATA = std::mem::zeroed();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        abd.hWnd = hwnd;
        abd.uCallbackMessage = WM_APPBAR;
        SHAppBarMessage(ABM_NEW, &mut abd);
        crate::settings::watch(hwnd as isize, WM_SETTINGS_FILE);

        a.popup = popup::create(hinst, hwnd);
        a.tray.add();
        if a.m.s.auto_update && !after_update {
            // Once per start, in the background. No repeated checks.
            crate::update::check(hwnd as isize, false);
        }
        if first_run {
            a.m.s.first_run = false;
            a.save_settings();
            a.tray.balloon(
                "Resource Monitor is running",
                "Hover the taskbar widget for details. Drag it to move. Right-click for options.",
            );
        } else {
            a.settings_mtime = Settings::mtime();
        }
        crate::autostart::refresh_path();
        APP = Box::into_raw(a);

        // Event-driven z-order + fullscreen checks instead of polling.
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        // Slow down when the display is off (usage stays exact: counters are cumulative).
        RegisterPowerSettingNotification(hwnd as HANDLE, &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE);

        let a = app()?;
        a.hook = hook;
        a.sample();
        a.update_timer();
        a.refresh(true);
        if after_update {
            crate::update_ui::show(
                a,
                concat!("Updated successfully. You're now on v", env!("CARGO_PKG_VERSION"), "."),
                Some(100.0),
                false,
            );
        }
    }
    Some(())
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    if let Some(a) = app() {
        a.need_check = true;
        a.refresh(false);
    }
}

/// A taskbar window moved/resized/slid (auto-hide). Scoped to Explorer's
/// taskbar thread, so it fires only for taskbar activity.
unsafe extern "system" fn on_taskbar_move(_: HWINEVENTHOOK, _: u32, hwnd: HWND, obj: i32, _: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW {
        return;
    }
    if let Some(a) = app() {
        if a.taskbars.iter().any(|t| t.hwnd == hwnd) {
            a.need_locate = true;
            a.refresh(false);
        }
    }
}

impl App {
    /// (Re)attach the taskbar move hook to whatever thread owns the taskbar now.
    fn hook_taskbar_moves(&mut self) {
        let Some(tb) = self.taskbars.iter().find(|t| !t.hwnd.is_null()) else { return };
        let mut pid = 0u32;
        let tid = unsafe { GetWindowThreadProcessId(tb.hwnd, &mut pid) };
        if tid == 0 || tid == self.move_hook_tid {
            return;
        }
        unsafe {
            if !self.move_hook.is_null() {
                UnhookWinEvent(self.move_hook);
            }
            self.move_hook = SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_LOCATIONCHANGE,
                std::ptr::null_mut(),
                Some(on_taskbar_move),
                pid,
                tid,
                WINEVENT_OUTOFCONTEXT,
            );
        }
        self.move_hook_tid = tid;
    }

    fn opts(&self) -> SampleOpts {
        let sh = |m| self.m.s.shown(m);
        SampleOpts {
            adapter: (!self.m.s.adapter.is_empty()).then(|| self.m.s.adapter.clone()),
            // GPU engine counters are the only costly source: query them only when needed.
            // Details stay "warm" a while after hover so the panel has data instantly
            // and counters aren't attached/detached on every hover.
            gpu: sh(Metric::Gpu) || sh(Metric::Vram) || sh(Metric::GpuTemp) || self.detail_warm(),
            disk: sh(Metric::DiskRead) || sh(Metric::DiskWrite) || sh(Metric::DiskActive) || self.detail_warm(),
            procs: sh(Metric::TopProc) || self.detail_warm(),
        }
    }

    fn detail_warm(&self) -> bool {
        self.popup_open() || self.detail_until.is_some_and(|t| Instant::now() < t)
    }

    /// Mouse is near: start collecting detail data before the panel opens.
    fn warm_details(&mut self) {
        let cold = !self.detail_warm();
        self.detail_until = Some(Instant::now() + std::time::Duration::from_secs(30));
        if cold {
            // Take a baseline now so rates/percentages exist on the next tick.
            let opts = self.opts();
            let _ = self.sensors.sample(&opts);
        }
    }

    pub fn popup_open(&self) -> bool {
        !self.popup.is_null() && unsafe { IsWindowVisible(self.popup) } != 0
    }

    fn sample(&mut self) {
        let opts = self.opts();
        let snap = self.sensors.sample(&opts).clone();
        let m = &mut self.m;
        m.usage.add(snap.net_sent_delta, snap.net_recv_delta);
        let mut v = [0f32; Metric::ALL.len()];
        v[Metric::Cpu.idx()] = snap.cpu;
        v[Metric::Ram.idx()] = snap.ram_pct;
        v[Metric::Gpu.idx()] = snap.gpu.unwrap_or(0.0);
        v[Metric::Vram.idx()] = snap.vram_used.unwrap_or(0) as f32;
        v[Metric::DiskRead.idx()] = snap.disk_read as f32;
        v[Metric::DiskWrite.idx()] = snap.disk_write as f32;
        v[Metric::DiskActive.idx()] = snap.disk_active.unwrap_or(0.0);
        v[Metric::Up.idx()] = snap.net_up as f32;
        v[Metric::Down.idx()] = snap.net_down as f32;
        v[Metric::Battery.idx()] = snap.battery.map_or(0.0, |b| b.percent as f32);
        v[Metric::GpuTemp.idx()] = snap.gpu_temp.unwrap_or(0.0);
        v[Metric::DiskFree.idx()] = snap.disk_used_pct.unwrap_or(0.0);
        for (h, v) in m.hist.iter_mut().zip(v) {
            if h.len() == HIST {
                h.pop_front();
            }
            h.push_back(v);
        }
        if snap.gpu.is_some() {
            m.gpu_seen = true;
        }
        m.snap = snap;
        m.samples += 1;

        if self.last_save.elapsed().as_secs() >= 60 {
            self.m.usage.save();
            self.last_save = Instant::now();
        }
        if self.m.samples % 5 == 1 {
            self.update_tip();
        }
    }

    fn update_tip(&mut self) {
        let s = &self.m.snap;
        let mut t = format!("CPU {:.0}% \u{00B7} RAM {:.0}%", s.cpu, s.ram_pct);
        if let Some(g) = s.gpu {
            t.push_str(&format!(" \u{00B7} GPU {:.0}%", g));
        }
        t.push_str(&format!(
            "\n\u{25B2} {}  \u{25BC} {}",
            fmt::speed(s.net_up, self.m.s.unit),
            fmt::speed(s.net_down, self.m.s.unit)
        ));
        let today = self.m.usage.get(usage::today());
        t.push_str(&format!("\nToday {}", fmt::bytes(today.total())));
        self.tray.tip(&t);
    }

    fn check_settings_file(&mut self) {
        let m = Settings::mtime();
        if m == self.settings_mtime {
            return;
        }
        self.settings_mtime = m;
        match m {
            // File deleted: "delete to reset".
            None => {
                self.m.s = Settings { first_run: false, ..Settings::default() };
                self.save_settings();
            }
            Some(_) => match self.m.s.reload() {
                Some(s) => self.m.s = s,
                None => return,
            },
        }
        self.apply_settings();
    }

    pub fn save_settings(&mut self) {
        self.m.s.save();
        self.settings_mtime = Settings::mtime();
    }

    /// Network adapters currently present (refreshed on each call).
    pub fn adapter_list(&mut self) -> Vec<AdapterInfo> {
        self.adapters = self.sensors.adapters();
        self.adapters.clone()
    }

    /// Replace all settings (e.g. from the Settings window), save and apply.
    pub fn apply_new_settings(&mut self, s: Settings) {
        self.m.s = s;
        self.save_settings();
        self.apply_settings();
    }

    fn apply_settings(&mut self) {
        set_accents(&self.m.s);
        self.need_locate = true;
        self.m.dark = effective_dark(self.m.s.theme);
        let ct = self.m.s.click_through;
        for o in &mut self.overlays {
            o.set_click_through(ct);
            o.reset_widths();
        }
        self.update_timer();
        self.refresh(true);
        if self.popup_open() {
            popup::reposition(self);
        }
    }

    fn update_timer(&self) {
        let ms = if self.display_on { self.m.s.interval_ms } else { self.m.s.interval_ms.max(5000) };
        unsafe {
            // Coalescable: lets Windows batch our wakeups with others to save power.
            SetCoalescableTimer(self.hwnd, T_SAMPLE, ms, None, (ms / 10).max(32));
        }
    }

    /// Where the popup should attach: (widget rect, its taskbar, scale).
    pub fn anchor(&self) -> Option<(Rect, Taskbar, f32)> {
        let o = self
            .overlays
            .iter()
            .find(|o| o.hwnd == self.popup_anchor && o.shown)
            .or_else(|| self.overlays.iter().find(|o| o.shown));
        if let Some(o) = o {
            return Some((o.pos, o.tb, o.scale));
        }
        let tb = self.primary_taskbar()?;
        Some((tb.tray, tb, tb.dpi as f32 / 96.0))
    }

    pub fn primary_taskbar(&self) -> Option<Taskbar> {
        self.taskbars.iter().find(|t| t.primary).or(self.taskbars.first()).copied()
    }

    /// Create/destroy widgets to match the taskbars present and the
    /// "displays" setting, then update visibility/z-order and redraw.
    ///
    /// Nothing here polls: the taskbar is re-located only after an event set
    /// `need_locate`, and full-screen / z-order only after one set `need_check`.
    pub fn refresh(&mut self, redraw: bool) {
        if self.need_locate || self.taskbars.is_empty() {
            self.need_locate = false;
            self.need_check = true;
            let mut tbs = taskbar::locate_all();
            if tbs.is_empty() {
                tbs.extend(taskbar::fallback());
            }
            self.taskbars = tbs;
            self.hook_taskbar_moves();
        }
        let check = std::mem::take(&mut self.need_check);
        let tbs = self.taskbars.clone();
        let wanted: Vec<Taskbar> = if self.m.s.mode == Mode::Float {
            tbs.iter().find(|t| t.primary).or(tbs.first()).copied().into_iter().collect()
        } else {
            let mut w: Vec<Taskbar> =
                tbs.iter().filter(|t| self.m.s.wants_display(&t.key(), t.primary)).copied().collect();
            if w.is_empty() {
                // Chosen display unplugged: fall back to the main taskbar.
                w.extend(tbs.iter().find(|t| t.primary).copied());
            }
            w
        };

        // Drop widgets whose taskbar went away. Remove first, destroy after,
        // so re-entrant messages during DestroyWindow see a consistent list.
        let mut gone = Vec::new();
        let mut i = 0;
        while i < self.overlays.len() {
            let o = &self.overlays[i];
            let owner_ok = (self.m.s.mode == Mode::Dock && !o.tb.hwnd.is_null()) == !o.owner.is_null();
            if owner_ok && wanted.iter().any(|t| t.hwnd == o.tb.hwnd) {
                i += 1;
            } else {
                gone.push(self.overlays.remove(i));
            }
        }
        if gone.iter().any(|o| o.hwnd == self.popup_anchor) && !self.popup_pinned {
            popup::hide(self);
        }
        drop(gone);

        for tb in &wanted {
            if !self.overlays.iter().any(|o| o.tb.hwnd == tb.hwnd) {
                let owned = self.m.s.mode == Mode::Dock && !tb.hwnd.is_null();
                if let Some(o) = Overlay::new(*tb, self.m.s.click_through, owned) {
                    self.overlays.push(o);
                }
            }
        }

        if let Some(wt) = &self.icons {
            let targets = wanted
                .iter()
                .filter(|t| !t.hwnd.is_null() && t.edge.horizontal() && self.m.s.mode == Mode::Dock)
                .map(|t| crate::icons::Target {
                    taskbar: t.hwnd as isize,
                    rect: t.rect,
                    limit: t.tray.l,
                    secondary: !t.primary,
                })
                .collect();
            wt.set_targets(targets);
        }

        let own: Vec<HWND> =
            self.overlays.iter().map(|o| o.hwnd).chain([self.hwnd, self.popup, self.report]).collect();
        let dock = self.m.s.mode == Mode::Dock;
        let mut anchor_hidden = false;
        let m = &self.m;
        for o in &mut self.overlays {
            let Some(tb) = wanted.iter().find(|t| t.hwnd == o.tb.hwnd).copied() else { continue };
            let moved = !tb.same_geometry(&o.tb);
            o.tb = tb;
            if check || !o.shown && !o.fullscreen {
                o.fullscreen = m.s.hide_fullscreen && taskbar::fullscreen_active(&tb, &own);
            }
            let want = m.s.visible && !o.fullscreen && !(dock && tb.retracted) && self.display_on;
            if want {
                if redraw || moved || !o.shown {
                    o.render(m);
                }
            } else if o.hwnd == self.popup_anchor {
                anchor_hidden = true;
            }
            // Topmost is re-asserted only after an event (focus change etc.),
            // and never over our own open menu.
            o.show(want, check && !self.menu_open);
        }

        if self.popup_open() {
            if anchor_hidden && !self.popup_pinned {
                popup::hide(self);
            } else if redraw {
                unsafe { InvalidateRect(self.popup, std::ptr::null(), 0) };
            }
        }
        if redraw && !self.report.is_null() {
            unsafe { InvalidateRect(self.report, std::ptr::null(), 0) };
        }
    }

    // ---------------------------------------------------------------- menu

    fn menu(&mut self, x: i32, y: i32) {
        self.adapters = self.sensors.adapters();
        let s = &self.m.s;
        let hm = unsafe { CreatePopupMenu() };
        let add = |m: HMENU, id: usize, text: &str, checked: bool| unsafe {
            let t = wide(text);
            AppendMenuW(m, MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED }, id, t.as_ptr());
        };
        let radio = |m: HMENU, id: usize, text: &str, on: bool| unsafe {
            let t = wide(text);
            AppendMenuW(m, MF_STRING, id, t.as_ptr());
            if on {
                CheckMenuRadioItem(m, id as u32, id as u32, id as u32, MF_BYCOMMAND);
            }
        };
        let sub = |parent: HMENU, child: HMENU, text: &str| unsafe {
            let t = wide(text);
            AppendMenuW(parent, MF_POPUP, child as usize, t.as_ptr());
        };
        let sep = |m: HMENU| unsafe {
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
        };
        unsafe {
            let title = wide(concat!("Resource Monitor ", env!("CARGO_PKG_VERSION")));
            AppendMenuW(hm, MF_STRING | MF_GRAYED, 0, title.as_ptr());
            sep(hm);
            add(hm, ID_VISIBLE, "Show widget", s.visible);
            add(hm, ID_SETTINGS, "Settings\u{2026}", false);

            let mm = CreatePopupMenu();
            for (i, m) in Metric::ALL.iter().enumerate() {
                if matches!(m, Metric::DiskRead | Metric::Up | Metric::TodayUp | Metric::TopProc) {
                    sep(mm);
                }
                add(mm, ID_METRIC + i, m.menu_label(), s.shown(*m));
            }
            sub(hm, mm, "Metrics");

            let dm = CreatePopupMenu();
            add(dm, ID_SPARK, "Sparklines", s.sparklines);
            add(dm, ID_LABELS, "Labels", s.labels);
            // Stacked: one checkbox, or one per display when there are several.
            if self.taskbars.len() > 1 {
                let km = CreatePopupMenu();
                for (i, t) in self.taskbars.iter().take(MAX_DISPLAYS).enumerate() {
                    let label = format!("{}{}", t.label(), if t.primary { " \u{2014} main" } else { "" });
                    add(km, ID_STACK_DISP + i, &label, s.stacked_for(&t.key()));
                }
                AppendMenuW(km, MF_SEPARATOR, 0, std::ptr::null());
                add(km, ID_AUTOSTACK, "Stack automatically when crowded", s.auto_stack);
                sub(dm, km, "Stacked (2 rows)");
            } else {
                let key = self.taskbars.first().map(|t| t.key()).unwrap_or_default();
                add(dm, ID_STACKED, "Stacked (2 rows)", s.stacked_for(&key));
                add(dm, ID_AUTOSTACK, "Stack automatically when crowded", s.auto_stack);
            }
            let wm = CreatePopupMenu();
            for (i, (n, v)) in MAX_WIDTHS.iter().enumerate() {
                radio(wm, ID_MAXW + i, n, s.max_width_pct == *v);
            }
            sub(dm, wm, "Max width");
            add(dm, ID_RECHECK, "Re-check taskbar icons", false);
            sep(dm);
            let tm = CreatePopupMenu();
            for (i, (n, t)) in [("Match Windows", Theme::Auto), ("Dark", Theme::Dark), ("Light", Theme::Light)]
                .iter()
                .enumerate()
            {
                radio(tm, ID_THEME + i, n, s.theme == *t);
            }
            sub(dm, tm, "Theme");
            let zm = CreatePopupMenu();
            for (i, (n, v)) in TEXT_SIZES.iter().enumerate() {
                radio(zm, ID_SIZE + i, n, s.text_scale == *v);
            }
            sub(dm, zm, "Text size");
            let bm = CreatePopupMenu();
            for (i, (n, v)) in BACKGROUNDS.iter().enumerate() {
                radio(bm, ID_BG + i, n, s.bg_opacity == *v);
            }
            sub(dm, bm, "Background");
            sub(hm, dm, "Appearance");

            let um = CreatePopupMenu();
            for (i, u) in SpeedUnit::ALL.iter().enumerate() {
                radio(um, ID_UNIT + i, u.label(), s.unit == *u);
            }
            sub(hm, um, "Speed unit");

            let am = CreatePopupMenu();
            radio(am, ID_ADAPTER, "Automatic (all connected adapters)", s.adapter.is_empty());
            sep(am);
            for (i, a) in self.adapters.iter().take(MAX_ADAPTERS).enumerate() {
                let label = format!(
                    "{}{}  \u{2014}  {}",
                    a.alias,
                    if a.connected { "" } else { " (disconnected)" },
                    a.description
                );
                radio(am, ID_ADAPTER + 1 + i, &label, s.adapter.eq_ignore_ascii_case(&a.alias));
            }
            sub(hm, am, "Network adapter");

            let im = CreatePopupMenu();
            for (i, (n, v)) in INTERVALS.iter().enumerate() {
                radio(im, ID_INTERVAL + i, n, s.interval_ms == *v);
            }
            sub(hm, im, "Update interval");

            let pm = CreatePopupMenu();
            radio(pm, ID_DOCK, "Dock to taskbar", s.mode == Mode::Dock);
            radio(pm, ID_FLOAT, "Floating (drag anywhere)", s.mode == Mode::Float);
            sep(pm);
            add(pm, ID_RESET_POS, "Reset position", false);
            add(pm, ID_FULLSCREEN, "Hide during full-screen apps", s.hide_fullscreen);
            add(pm, ID_CLICKTHRU, "Click-through (no hover)", s.click_through);
            sub(hm, pm, "Position");

            // Displays: which taskbars get a widget.
            let xm = CreatePopupMenu();
            let mode = s.displays.to_ascii_lowercase();
            radio(xm, ID_DISP_PRIMARY, "Main taskbar only", mode == "primary");
            radio(xm, ID_DISP_ALL, "All taskbars", mode == "all");
            sep(xm);
            for (i, t) in self.taskbars.iter().take(MAX_DISPLAYS).enumerate() {
                let label = format!(
                    "{}{}  ({}\u{00D7}{})",
                    t.label(),
                    if t.primary { " \u{2014} main" } else { "" },
                    t.monitor.w(),
                    t.monitor.h()
                );
                add(xm, ID_DISP + i, &label, s.wants_display(&t.key(), t.primary));
            }
            if self.taskbars.len() < 2 {
                let t = wide("Turn on \u{201C}Show my taskbar on all displays\u{201D} for more");
                AppendMenuW(xm, MF_STRING | MF_GRAYED, 0, t.as_ptr());
            }
            sub(hm, xm, "Displays");

            sep(hm);
            add(hm, ID_REPORT, "Data usage report\u{2026}", false);
            add(hm, ID_TASKMGR, "Task Manager", false);
            sep(hm);
            add(hm, ID_AUTOSTART, "Start with Windows", crate::autostart::enabled());
            add(hm, ID_AUTOUPDATE, "Update automatically", s.auto_update);
            add(hm, ID_UPDATE_NOW, "Check for updates now", false);
            add(hm, ID_EDIT, "Edit settings file\u{2026}", false);
            add(hm, ID_DATA, "Open data folder", false);
            sep(hm);
            add(hm, ID_EXIT, "Exit", false);

            let hwnd = self.hwnd;
            self.menu_open = true;
            self.menu_sel = (std::ptr::null_mut(), 0);
            SetForegroundWindow(hwnd);
            // Lets checkbox items toggle without closing the menu.
            let hook = SetWindowsHookExW(WH_MSGFILTER, Some(menu_filter), std::ptr::null_mut(), windows_sys::Win32::System::Threading::GetCurrentThreadId());
            // `self` must not be used past this point: the menu loop re-enters.
            let cmd = TrackPopupMenuEx(hm, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, hwnd, std::ptr::null());
            if !hook.is_null() {
                UnhookWindowsHookEx(hook);
            }
            DestroyMenu(hm);
            PostMessageW(hwnd, WM_NULL, 0, 0);
            if let Some(a) = app() {
                a.menu_open = false;
                if cmd > 0 {
                    a.command(cmd as usize);
                }
            }
        }
    }

    /// Checkbox items that keep the menu open when clicked; returns new state.
    fn sticky_state(&self, id: usize) -> Option<bool> {
        let s = &self.m.s;
        match id {
            ID_SPARK => Some(s.sparklines),
            ID_LABELS => Some(s.labels),
            ID_STACKED => Some(s.stacked_for(&self.taskbars.first().map(|t| t.key()).unwrap_or_default())),
            ID_AUTOSTACK => Some(s.auto_stack),
            _ if (ID_STACK_DISP..ID_STACK_DISP + MAX_DISPLAYS).contains(&id) => {
                self.taskbars.get(id - ID_STACK_DISP).map(|t| s.stacked_for(&t.key()))
            }
            _ if (ID_METRIC..ID_METRIC + Metric::ALL.len()).contains(&id) => Some(s.show[id - ID_METRIC]),
            _ => None,
        }
    }

    fn toggle_display(&mut self, i: usize) {
        let Some(t) = self.taskbars.get(i).copied() else { return };
        let mut on: Vec<String> = self
            .taskbars
            .iter()
            .filter(|o| self.m.s.wants_display(&o.key(), o.primary))
            .map(|o| o.key())
            .collect();
        let key = t.key();
        if let Some(p) = on.iter().position(|k| *k == key) {
            on.remove(p);
        } else {
            on.push(key);
        }
        let all = self.taskbars.iter().all(|o| on.contains(&o.key()));
        let primary_only = on.len() == 1 && self.taskbars.iter().any(|o| o.primary && o.key() == on[0]);
        self.m.s.displays = if on.is_empty() || primary_only {
            "primary".into()
        } else if all {
            "all".into()
        } else {
            on.join(",")
        };
    }

    fn command(&mut self, id: usize) {
        match id {
            ID_SETTINGS => return crate::settings_ui::open(self),
            ID_REPORT => return report::open(self),
            ID_TASKMGR => return shell_open("taskmgr.exe", None),
            ID_UPDATE_NOW => {
                crate::update_ui::show(self, "Checking GitHub for a newer version\u{2026}", Some(0.0), true);
                return crate::update::check(self.hwnd as isize, true);
            }
            ID_RECHECK => {
                if let Some(w) = &self.icons {
                    w.kick();
                }
                return;
            }
            ID_AUTOSTART => return crate::autostart::set(!crate::autostart::enabled()),
            ID_EDIT => {
                self.save_settings();
                return shell_open("notepad.exe", Some(&format!("\"{}\"", Settings::path().display())));
            }
            ID_DATA => {
                self.m.usage.save();
                return shell_open(&data_dir().display().to_string(), None);
            }
            ID_EXIT => {
                unsafe { DestroyWindow(self.hwnd) };
                return;
            }
            _ => {}
        }
        if (ID_DISP..ID_DISP + MAX_DISPLAYS).contains(&id) {
            self.toggle_display(id - ID_DISP);
            self.save_settings();
            self.apply_settings();
            return;
        }
        let first = self.overlays.first().map(|o| (o.pos, o.tb));
        let s = &mut self.m.s;
        match id {
            ID_VISIBLE => s.visible = !s.visible,
            ID_SPARK => s.sparklines = !s.sparklines,
            ID_LABELS => s.labels = !s.labels,
            ID_STACKED => {
                // Single display: this is the default for every display.
                s.stacked = !s.stacked;
                s.stacked_on.clear();
            }
            ID_AUTOSTACK => s.auto_stack = !s.auto_stack,
            ID_AUTOUPDATE => s.auto_update = !s.auto_update,
            _ if (ID_STACK_DISP..ID_STACK_DISP + MAX_DISPLAYS).contains(&id) => {
                if let Some(t) = self.taskbars.get(id - ID_STACK_DISP) {
                    let k = t.key();
                    let on = !s.stacked_for(&k);
                    s.set_stacked(&k, on);
                }
            }
            ID_FULLSCREEN => s.hide_fullscreen = !s.hide_fullscreen,
            ID_CLICKTHRU => s.click_through = !s.click_through,
            ID_DOCK => s.mode = Mode::Dock,
            ID_FLOAT => {
                if s.mode != Mode::Float {
                    s.mode = Mode::Float;
                    if let Some((pos, _)) = first {
                        s.float_x = pos.l;
                        s.float_y = pos.t - pos.h() - 40;
                    }
                }
            }
            ID_RESET_POS => {
                s.dock_offset = 0;
                s.offsets.clear();
                if let Some((_, tb)) = first {
                    s.float_x = tb.work.r - 360;
                    s.float_y = tb.work.b - 80;
                }
            }
            ID_DISP_PRIMARY => s.displays = "primary".into(),
            ID_DISP_ALL => s.displays = "all".into(),
            ID_ADAPTER => s.adapter.clear(),
            _ if (ID_METRIC..ID_METRIC + Metric::ALL.len()).contains(&id) => {
                let i = id - ID_METRIC;
                s.show[i] = !s.show[i];
            }
            _ if (ID_UNIT..ID_UNIT + SpeedUnit::ALL.len()).contains(&id) => s.unit = SpeedUnit::ALL[id - ID_UNIT],
            _ if (ID_ADAPTER + 1..=ID_ADAPTER + MAX_ADAPTERS).contains(&id) => {
                if let Some(a) = self.adapters.get(id - ID_ADAPTER - 1) {
                    s.adapter = a.alias.clone();
                }
            }
            _ if (ID_INTERVAL..ID_INTERVAL + INTERVALS.len()).contains(&id) => {
                s.interval_ms = INTERVALS[id - ID_INTERVAL].1
            }
            _ if (ID_THEME..ID_THEME + 3).contains(&id) => {
                s.theme = [Theme::Auto, Theme::Dark, Theme::Light][id - ID_THEME]
            }
            _ if (ID_SIZE..ID_SIZE + TEXT_SIZES.len()).contains(&id) => s.text_scale = TEXT_SIZES[id - ID_SIZE].1,
            _ if (ID_BG..ID_BG + BACKGROUNDS.len()).contains(&id) => s.bg_opacity = BACKGROUNDS[id - ID_BG].1,
            _ if (ID_MAXW..ID_MAXW + MAX_WIDTHS.len()).contains(&id) => s.max_width_pct = MAX_WIDTHS[id - ID_MAXW].1,
            _ => return,
        }
        self.save_settings();
        self.apply_settings();
    }

    // ---------------------------------------------------------------- mouse

    fn on_mouse(&mut self, hwnd: HWND, msg: u32) {
        let Some(i) = self.overlays.iter().position(|o| o.hwnd == hwnd) else { return };
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        match msg {
            WM_MOUSEMOVE => {
                let o = &mut self.overlays[i];
                if !o.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_HOVER,
                        hwndTrack: o.hwnd,
                        dwHoverTime: 350,
                    };
                    unsafe { TrackMouseEvent(&mut t) };
                    o.tracking = true;
                    self.warm_details();
                }
                let o = &mut self.overlays[i];
                let th = (4.0 * o.scale) as i32;
                let Some(d) = o.drag.as_mut() else { return };
                let (dx, dy) = (pt.x - d.start.x, pt.y - d.start.y);
                if !d.moved && (dx.abs() > th || dy.abs() > th) {
                    d.moved = true;
                }
                if !d.moved {
                    return;
                }
                let (ws, os) = (d.win_start, d.offset_start);
                if self.popup_open() {
                    popup::hide(self);
                }
                let o = &mut self.overlays[i];
                let s = &mut self.m.s;
                if s.mode == Mode::Float {
                    s.float_x = ws.0 + dx;
                    s.float_y = ws.1 + dy;
                } else {
                    let delta = if o.tb.edge.horizontal() { dx } else { dy };
                    // Never past the taskbar start, never onto the tray.
                    let room = if o.tb.edge.horizontal() {
                        o.tb.tray.l - o.tb.rect.l - o.pos.w()
                    } else {
                        o.tb.tray.t - o.tb.rect.t - o.pos.h()
                    };
                    let max_off = (room as f32 / o.scale) as i32;
                    let off = (os - (delta as f32 / o.scale) as i32).clamp(0, max_off.max(0));
                    s.set_offset(&o.tb.key(), o.tb.primary, off);
                }
                o.render(&self.m);
            }
            WM_MOUSEHOVER => {
                let o = &mut self.overlays[i];
                o.tracking = false;
                if o.drag.is_none() && !o.no_hover && !self.popup_open() {
                    self.popup_anchor = hwnd;
                    popup::show(self, false);
                }
            }
            WM_MOUSELEAVE => {
                self.overlays[i].tracking = false;
                self.overlays[i].no_hover = false;
                if !self.popup_pinned {
                    popup::hide(self);
                }
            }
            WM_LBUTTONDOWN => {
                let o = &mut self.overlays[i];
                unsafe { SetCapture(o.hwnd) };
                let offset_start = self.m.s.offset(&o.tb.key(), o.tb.primary);
                o.drag = Some(Drag { start: pt, win_start: (o.pos.l, o.pos.t), offset_start, moved: false });
            }
            WM_LBUTTONUP => {
                // Take the drag state before ReleaseCapture (which sends WM_CAPTURECHANGED).
                let d = self.overlays[i].drag.take();
                unsafe { ReleaseCapture() };
                if let Some(d) = d {
                    if d.moved {
                        self.save_settings();
                        // Offset changed: the room next to the icons changed too.
                        self.refresh(true);
                    } else if self.popup_open() && self.popup_anchor == hwnd {
                        // Closed by click: don't let hover reopen it until the mouse leaves.
                        self.overlays[i].no_hover = true;
                        popup::hide(self);
                    } else {
                        self.popup_anchor = hwnd;
                        popup::show(self, true);
                    }
                }
            }
            WM_LBUTTONDBLCLK => {
                popup::hide(self);
                shell_open("taskmgr.exe", None);
            }
            WM_RBUTTONUP => {
                popup::hide(self);
                self.menu(pt.x, pt.y);
            }
            WM_CAPTURECHANGED => {
                if let Some(d) = self.overlays[i].drag.take() {
                    if d.moved {
                        self.save_settings();
                    }
                }
            }
            _ => {}
        }
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        self.m.usage.save();
        self.tray.remove();
        unsafe {
            if !self.hook.is_null() {
                UnhookWinEvent(self.hook);
            }

            KillTimer(self.hwnd, T_SAMPLE);
            if !self.move_hook.is_null() {
                UnhookWinEvent(self.move_hook);
            }
            let mut abd: APPBARDATA = std::mem::zeroed();
            abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            abd.hWnd = self.hwnd;
            SHAppBarMessage(ABM_REMOVE, &mut abd);
        }
        let ovs = std::mem::take(&mut self.overlays);
        drop(ovs);
        self.icons = None;
    }
}

const ID_METRIC: usize = 100;
const ID_UNIT: usize = 200;
const ID_ADAPTER: usize = 300;
const MAX_ADAPTERS: usize = 40;
const ID_INTERVAL: usize = 400;
const ID_THEME: usize = 450;
const ID_SIZE: usize = 500;
const ID_BG: usize = 550;
const ID_VISIBLE: usize = 600;
const ID_SPARK: usize = 601;
const ID_LABELS: usize = 602;
const ID_FULLSCREEN: usize = 603;
const ID_CLICKTHRU: usize = 604;
const ID_DOCK: usize = 605;
const ID_FLOAT: usize = 606;
const ID_RESET_POS: usize = 607;
const ID_STACKED: usize = 608;
const ID_AUTOSTACK: usize = 610;
const ID_AUTOUPDATE: usize = 611;
const ID_UPDATE_NOW: usize = 612;
const ID_STACK_DISP: usize = 1000;
const ID_MAXW: usize = 900;
const ID_RECHECK: usize = 609;
const MAX_WIDTHS: [(&str, u32); 5] =
    [("Auto (don't cover app icons)", 0), ("20% of taskbar", 20), ("30% of taskbar", 30), ("40% of taskbar", 40), ("50% of taskbar", 50)];
const ID_REPORT: usize = 700;
const ID_TASKMGR: usize = 701;
const ID_AUTOSTART: usize = 702;
const ID_EDIT: usize = 703;
const ID_DATA: usize = 704;
const ID_EXIT: usize = 705;
const ID_SETTINGS: usize = 706;
const ID_DISP_PRIMARY: usize = 800;
const ID_DISP_ALL: usize = 801;
const ID_DISP: usize = 810;
const MAX_DISPLAYS: usize = 16;

const INTERVALS: [(&str, u32); 4] = [("0.5 s", 500), ("1 s", 1000), ("2 s", 2000), ("5 s", 5000)];
const TEXT_SIZES: [(&str, u32); 4] = [("Small", 90), ("Normal", 100), ("Large", 115), ("Extra large", 130)];
const BACKGROUNDS: [(&str, u8); 4] = [("None", 0), ("Subtle", 90), ("Strong", 180), ("Solid", 255)];
const NIN_KEYSELECT_: u32 = NIN_SELECT | 1;

/// Runs inside the menu's modal loop. A click (or Space/Enter) on a checkbox
/// item toggles it in place and is swallowed, so the menu stays open.
unsafe extern "system" fn menu_filter(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == MSGF_MENU as i32 && l != 0 {
        let msg = &*(l as *const MSG);
        let key = msg.message == WM_KEYDOWN && (msg.wParam == VK_RETURN as usize || msg.wParam == VK_SPACE as usize);
        if msg.message == WM_LBUTTONUP || key {
            if let Some(a) = app() {
                let (hm, id) = a.menu_sel;
                let over = key || {
                    let pos = MenuItemFromPoint(std::ptr::null_mut(), hm, msg.pt);
                    pos >= 0 && GetMenuItemID(hm, pos) as usize == id
                };
                if !hm.is_null() && over && a.sticky_state(id).is_some() {
                    a.command(id);
                    let on = a.sticky_state(id).unwrap_or(false);
                    CheckMenuItem(hm, id as u32, MF_BYCOMMAND | if on { MF_CHECKED } else { MF_UNCHECKED });
                    if !msg.hwnd.is_null() {
                        InvalidateRect(msg.hwnd, std::ptr::null(), 1);
                    }
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, w, l)
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_MOUSEMOVE | WM_MOUSEHOVER | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK
        | WM_RBUTTONUP | WM_CAPTURECHANGED => {
            if let Some(a) = app() {
                a.on_mouse(hwnd, msg);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(a) = app() else { return DefWindowProcW(hwnd, msg, w, l) };
    if msg == a.msg_taskbar_created && msg != 0 {
        // Explorer restarted: tray icon is gone and taskbars were recreated.
        a.tray.add();
        a.update_timer();
        a.need_locate = true;
        a.refresh(true);
        return 0;
    }
    match msg {
        WM_TIMER if w == T_RESTART => {
            KillTimer(hwnd, T_RESTART);
            if let Ok(exe) = std::env::current_exe() {
                shell_open(&exe.display().to_string(), Some("--after-update"));
            }
            DestroyWindow(hwnd);
            0
        }
        WM_TIMER => {
            // The only periodic work: read the metrics and redraw them.
            if w == T_SAMPLE {
                a.sample();
                a.refresh(true);
            }
            0
        }
        WM_APPBAR => {
            if w as u32 == ABN_FULLSCREENAPP {
                a.need_check = true;
                a.refresh(false);
            }
            0
        }
        WM_SETTINGS_FILE => {
            a.check_settings_file();
            0
        }
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lParam), anchor point in wParam.
            let ev = loword(l as usize) as u32;
            let (x, y) = (get_x(w as isize), get_y(w as isize));
            match ev {
                // v4 also sends raw WM_*BUTTONUP; handling both would fire twice.
                WM_CONTEXTMENU => a.menu(x, y),
                NIN_SELECT | NIN_KEYSELECT_ => {
                    if a.popup_open() {
                        popup::hide(a);
                    } else {
                        a.popup_anchor = a.overlays.iter().find(|o| o.tb.primary).map_or(std::ptr::null_mut(), |o| o.hwnd);
                        popup::show(a, true);
                    }
                }
                WM_LBUTTONDBLCLK => report::open(a),
                _ => {}
            }
            0
        }
        WM_APP_ACTIVATE => {
            // Second launch: make sure the user sees us.
            if w == 1 {
                report::open(a);
                return 0;
            }
            if !a.m.s.visible {
                a.m.s.visible = true;
                a.save_settings();
            }
            a.refresh(true);
            popup::show(a, true);
            0
        }
        crate::update::WM_APP_UPDATE_STAGE => {
            let pct = l as f32 / 10.0;
            let text = match w {
                crate::update::STAGE_DOWNLOAD => format!("A new version was found. Downloading\u{2026} {:.0}%", pct),
                crate::update::STAGE_VERIFY => "Verifying the download\u{2026}".to_string(),
                _ => "Installing\u{2026}".to_string(),
            };
            // Shown for automatic updates too, without stealing focus.
            crate::update_ui::show(a, &text, Some(pct), false);
            0
        }
        crate::update::WM_APP_UPDATE => {
            let ver = crate::update::take_version(l);
            let asked = w & crate::update::MANUAL_FLAG != 0;
            match w & 0xFF {
                crate::update::INSTALLED => {
                    crate::update_ui::show(a, &format!("Updated to {}. Restarting\u{2026}", ver), Some(100.0), false);
                    a.m.usage.save();
                    a.save_settings();
                    // Let the user see it, then restart into the new version.
                    SetTimer(hwnd, T_RESTART, 1500, None);
                }
                crate::update::MANUAL => {
                    crate::update_ui::show(
                        a,
                        &format!("Version {} is available, but this folder can't be written to, so the app can't update itself. Opening the download page\u{2026}", ver),
                        None,
                        asked,
                    );
                    shell_open(&crate::update::releases_page(), None);
                }
                crate::update::MANUAL_QUIET => crate::update_ui::show(
                    a,
                    &format!("Version {} is available. Right-click \u{2192} Check for updates now to get it.", ver),
                    None,
                    false,
                ),
                crate::update::UP_TO_DATE => crate::update_ui::show(
                    a,
                    concat!("No updates available. You're on the latest version (v", env!("CARGO_PKG_VERSION"), ")."),
                    Some(100.0),
                    asked,
                ),
                _ => crate::update_ui::show(a, &format!("Couldn't check for updates. {}", ver), None, asked),
            }
            0
        }
        crate::icons::WM_APP_TRAY => {
            let mut changed = false;
            for o in &mut a.overlays {
                if o.tb.hwnd as usize == w && o.tray_l != Some(l as i32) {
                    o.tray_l = Some(l as i32);
                    changed = true;
                }
            }
            if changed {
                a.refresh(true);
            }
            0
        }
        crate::icons::WM_APP_ICONS => {
            let mut changed = false;
            for o in &mut a.overlays {
                if o.tb.hwnd as usize == w && o.icons != Some(l as i32) {
                    o.icons = Some(l as i32);
                    changed = true;
                }
            }
            if changed {
                a.refresh(true);
            }
            0
        }
        WM_MENUSELECT => {
            // Remember the highlighted item for the menu filter (popups have no id).
            let flags = (w >> 16) as u32 & 0xFFFF;
            a.menu_sel = if flags & MF_POPUP == 0 && flags != 0xFFFF {
                (l as HMENU, loword(w) as usize)
            } else {
                (std::ptr::null_mut(), 0)
            };
            0
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            a.need_locate = true;
            a.refresh(true);
            0
        }
        WM_SETTINGCHANGE => {
            // Theme switch, taskbar moved/resized (work area), auto-hide toggled…
            a.m.dark = effective_dark(a.m.s.theme);
            a.update_timer();
            a.need_locate = true;
            a.refresh(true);
            0
        }
        WM_POWERBROADCAST => {
            match w as u32 {
                PBT_APMSUSPEND => a.m.usage.save(),
                PBT_POWERSETTINGCHANGE => {
                    let p = l as *const POWERBROADCAST_SETTING;
                    if !p.is_null() && guid_eq(&(*p).PowerSetting, &GUID_CONSOLE_DISPLAY_STATE) {
                        let on = (*p).Data[0] != 0;
                        if on != a.display_on {
                            a.display_on = on;
                            a.update_timer();
                            a.refresh(true);
                        }
                    }
                }
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    a.need_locate = true;
                    a.refresh(true)
                }
                _ => {}
            }
            1
        }
        WM_QUERYENDSESSION => {
            a.m.usage.save();
            1
        }
        WM_ENDSESSION => {
            if w != 0 {
                a.shutdown();
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            a.shutdown();
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

#[cfg(test)]
mod tests {
    use crate::overlay::pack;
    use crate::settings::Metric::*;
    #[test]
    fn packing() {
        let c = pack(&[Cpu, Ram, DiskRead, Up, Down, TodayDown, TopProc], 2);
        assert_eq!(c, vec![vec![Cpu, Ram], vec![DiskRead], vec![Up, Down], vec![TodayDown, TopProc]]);
        let c = pack(&[Cpu, Gpu, Up, Down], 2);
        assert_eq!(c, vec![vec![Cpu, Gpu], vec![Up, Down]]);
    }
}
