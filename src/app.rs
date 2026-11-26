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
/// One-shot, debounced re-check after window/taskbar events.
const T_CHECK: usize = 4;
/// One-shot: close an unpinned panel after the mouse left widget and panel.
const T_HOVER_HIDE: usize = 5;
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

#[derive(Default)]
struct AlertState {
    cpu_since: Option<Instant>,
    cpu_done: bool,
    ram_since: Option<Instant>,
    ram_done: bool,
    disk_done: bool,
    battery_done: bool,
    cap_level: u8,
}

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
    pub menu_open: bool,
    /// Update window and what it shows.
    pub upd: HWND,
    pub upd_text: String,
    pub upd_pct: Option<f32>,
    /// Background UI Automation watcher for taskbar app-icon positions.
    icons: Option<crate::icons::Watcher>,
    alerts: AlertState,
    /// Achievements observer (fed from the sample tick and existing events).
    pub ach: crate::achievements::observer::Observer,
    /// Insights (incident cards, sleep verdict, held back, before/after, ghost memory),
    /// fed from the same sample tick and power/event-log messages.
    pub ins: crate::insights::Insights,
    /// Set by events: re-read taskbar geometry on the next refresh.
    need_locate: bool,
    /// Set by events: re-evaluate full-screen state and z-order.
    need_check: bool,
    /// Location-change hook scoped to the taskbar's own thread.
    move_hook: HWINEVENTHOOK,
    move_hook_tid: u32,
    /// Taskbar windows created/destroyed/shown/hidden.
    life_hook: HWINEVENTHOOK,
    /// Foreground app's window moves (full-screen toggles).
    fg_hook: HWINEVENTHOOK,
    fg_hook_tid: u32,
    /// Fonts + canvas reused by the details panel between repaints.
    pub popup_cache: Option<(popup::Fonts, crate::render::Canvas)>,
    /// Clickable / hoverable regions of the details panel from its last paint
    /// (client coordinates, later entries sit on top of earlier ones).
    pub popup_hits: Vec<(Rect, popup::PopupHit)>,
    /// Chart time range of the details panel, in seconds (= samples at 1 s).
    pub popup_range: usize,
    /// Top-process list shown in the details panel: 0 CPU, 1 memory, 2 disk I/O.
    pub popup_top: u8,
    /// Mouse position over the details panel (client coordinates).
    pub popup_hover: Option<(i32, i32)>,
    /// Panel uses the native acrylic backdrop (Windows 11).
    pub popup_acrylic: bool,
    /// Theme the panel's backdrop was styled for (restyled when it changes).
    pub popup_dark: bool,
    /// A WM_MOUSELEAVE for the details panel is pending.
    pub popup_tracking: bool,
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
        crate::achievements::ui::register(hinst);
        // Compile the medal shaders in the background a little after start-up (seconds on the first run after an
        // update, then from the disk cache), so opening Achievements never freezes the window.
        let _ = std::thread::Builder::new().name("ach-gpu-warm".into()).spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(8));
            crate::achievements::gpu::warm();
        });

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
            popup_hits: Vec::new(),
            popup_range: 60,
            popup_top: 0,
            popup_hover: None,
            popup_acrylic: false,
            popup_dark: true,
            popup_tracking: false,
            icons: None,
            alerts: AlertState::default(),
            ach: crate::achievements::observer::Observer::new(),
            ins: crate::insights::Insights::new(),
            upd: std::ptr::null_mut(),
            upd_text: String::new(),
            upd_pct: None,
            need_locate: true,
            need_check: true,
            move_hook: std::ptr::null_mut(),
            move_hook_tid: 0,
            life_hook: std::ptr::null_mut(),
            fg_hook: std::ptr::null_mut(),
            fg_hook_tid: 0,
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

        // Dark/light native menus from the first right-click, not only after
        // the Settings window has been opened.
        crate::settings_ui::apply_menu_theme(a.m.s.theme, hwnd);
        a.popup = popup::create(hinst, hwnd);
        a.tray.add();
        // Moments earned but not shown last run (crash, shutdown) are shown once now.
        if a.m.s.achievements {
            let pending = crate::achievements::store::read_pending();
            if !pending.is_empty() {
                a.ach_unlocked(pending);
            }
        }
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

        crate::insights::register_power(hwnd as isize);

        let a = app()?;
        a.hook = hook;
        a.ins.set_enabled(a.m.s.insights, hwnd as isize);
        a.ins.sleep.bag_guard = a.m.s.bag_guard;
        a.sample();
        a.update_timer();
        a.refresh(true);
        if after_update {
            a.ach_event(crate::achievements::Event::UpdateInstalled { now_ms: crate::achievements::observer::unix_ms() });
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
        a.hook_foreground();
        a.schedule_check();
    }
}

/// The foreground app's own window moved/resized — e.g. a browser or player
/// going full-screen (F11, full-screen video) without a focus change.
unsafe extern "system" fn on_foreground_move(_: HWINEVENTHOOK, _: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW || child != 0 {
        return;
    }
    let fg = GetForegroundWindow();
    if fg.is_null() || (hwnd != fg && hwnd != GetAncestor(fg, GA_ROOT)) {
        return;
    }
    if let Some(a) = app() {
        a.schedule_check();
    }
}

/// Taskbar windows created/destroyed/shown/hidden/moved (auto-hide slide,
/// second-monitor taskbar recreated after sleep or a display change…).
/// Scoped to Explorer's taskbar thread, so it fires only for taskbar activity.
unsafe extern "system" fn on_taskbar_event(_: HWINEVENTHOOK, ev: u32, hwnd: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW || child != 0 || hwnd.is_null() {
        return;
    }
    let Some(a) = app() else { return };
    let ours = a.taskbars.iter().any(|t| t.hwnd == hwnd);
    let relevant = match ev {
        EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW => {
            ours || matches!(class_name(hwnd).as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
        }
        // The tray area inside a taskbar grew or shrank (icons added, the
        // "Show hidden icons" arrow appeared): re-place the widget.
        EVENT_OBJECT_LOCATIONCHANGE if !ours => {
            let root = GetAncestor(hwnd, GA_ROOT);
            a.taskbars.iter().any(|t| t.hwnd == root) && class_name(hwnd) == "TrayNotifyWnd"
        }
        _ => ours,
    };
    if relevant {
        a.schedule_locate();
    }
}

impl App {
    /// (Re)attach the taskbar hooks to whatever thread owns the taskbar now.
    fn hook_taskbar_moves(&mut self) {
        let Some(tb) = self.taskbars.iter().find(|t| !t.hwnd.is_null()) else { return };
        let mut pid = 0u32;
        let tid = unsafe { GetWindowThreadProcessId(tb.hwnd, &mut pid) };
        if tid == 0 || tid == self.move_hook_tid {
            return;
        }
        unsafe {
            for h in [self.move_hook, self.life_hook] {
                if !h.is_null() {
                    UnhookWinEvent(h);
                }
            }
            self.move_hook = SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_LOCATIONCHANGE,
                std::ptr::null_mut(),
                Some(on_taskbar_event),
                pid,
                tid,
                WINEVENT_OUTOFCONTEXT,
            );
            self.life_hook = SetWinEventHook(
                EVENT_OBJECT_CREATE,
                EVENT_OBJECT_HIDE,
                std::ptr::null_mut(),
                Some(on_taskbar_event),
                pid,
                tid,
                WINEVENT_OUTOFCONTEXT,
            );
        }
        self.move_hook_tid = tid;
    }

    /// Follow the foreground app's window moves (scoped to its thread).
    fn hook_foreground(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_null() {
            return;
        }
        let mut pid = 0u32;
        let tid = unsafe { GetWindowThreadProcessId(fg, &mut pid) };
        if tid == 0 || tid == self.fg_hook_tid || pid == std::process::id() {
            return;
        }
        unsafe {
            if !self.fg_hook.is_null() {
                UnhookWinEvent(self.fg_hook);
            }
            self.fg_hook = SetWinEventHook(
                EVENT_OBJECT_LOCATIONCHANGE,
                EVENT_OBJECT_LOCATIONCHANGE,
                std::ptr::null_mut(),
                Some(on_foreground_move),
                pid,
                tid,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
        }
        self.fg_hook_tid = tid;
    }

    /// Close an unpinned panel shortly, unless the mouse is then over the
    /// panel or a widget (lets you move from the widget into the panel).
    pub fn hide_popup_soon(&mut self) {
        unsafe { SetTimer(self.hwnd, T_HOVER_HIDE, 300, None) };
    }

    fn mouse_over_ours(&self) -> bool {
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            let h = WindowFromPoint(pt);
            !h.is_null() && (h == self.popup || self.overlays.iter().any(|o| o.hwnd == h))
        }
    }

    /// Re-check full-screen state / z-order shortly. Bursts of events (window
    /// animations, Alt-Tab) collapse into one check, so nothing blinks.
    pub fn schedule_check(&mut self) {
        self.need_check = true;
        unsafe { SetTimer(self.hwnd, T_CHECK, 250, None) };
    }

    /// Re-read taskbar geometry shortly (coalesced).
    pub fn schedule_locate(&mut self) {
        self.need_locate = true;
        unsafe { SetTimer(self.hwnd, T_CHECK, 150, None) };
    }

    fn opts(&self) -> SampleOpts {
        let sh = |m| self.m.s.shown(m);
        SampleOpts {
            adapter: (!self.m.s.adapter.is_empty()).then(|| self.m.s.adapter.clone()),
            // GPU engine counters are the only costly source: query them only when needed.
            // Details stay "warm" a while after hover so the panel has data instantly
            // and counters aren't attached/detached on every hover.
            gpu: sh(Metric::Gpu) || sh(Metric::Vram) || sh(Metric::GpuTemp) || self.detail_warm(),
            // Disk counters are cheap; Insights needs them for disk incidents and the idle baseline.
            disk: sh(Metric::DiskRead) || sh(Metric::DiskWrite) || sh(Metric::DiskActive) || self.detail_warm() || self.ins.enabled(),
            // Insights asks for process lists only while an incident or awake burst needs a culprit.
            procs: sh(Metric::TopProc) || self.detail_warm() || self.ins.wants_procs(),
            // The Monitor page is on screen (no extra timer: it rides this tick).
            detail: crate::settings_ui::monitor_visible(),
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

    /// Monitor page detail from the last sample (empty unless the page is on screen).
    pub fn detail(&self) -> &crate::sensors::detail::Detail {
        self.sensors.detail()
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
        self.check_alerts();
        self.insights_sample(opts.procs);
        if self.m.s.achievements {
            self.ach_sample(opts.gpu);
        } else {
            self.ach.pause();
        }
    }

    /// Feed Insights from the sample just taken (no extra timers).
    fn insights_sample(&mut self, procs_sampled: bool) {
        if !self.ins.enabled() {
            // Still lets the window follow a battery appearing or going away.
            crate::settings_ui::insights_tick(self);
            return;
        }
        let interval_ms = if self.display_on { self.m.s.interval_ms } else { self.m.s.interval_ms.max(5000) };
        let ctx = crate::insights::Ctx {
            now_ms: crate::insights::unix_ms(),
            snap: &self.m.snap,
            display_on: self.display_on,
            input_idle_ms: crate::insights::input_idle_ms(),
            interval_ms,
            procs_sampled,
        };
        let notes = self.ins.on_sample(&ctx);
        self.insight_notices(notes);
        crate::settings_ui::insights_tick(self);
    }

    /// Show Insights notices as tray notifications (when the user wants them).
    pub fn insight_notices(&mut self, notes: Vec<crate::insights::Notice>) {
        for n in notes {
            crate::util::diag(&format!("insight: {} \u{2014} {}", n.title, n.text));
            if self.m.s.insight_toasts {
                self.tray.balloon_from(&n.title, &n.text, n.key);
            }
        }
    }

    pub fn open_insights(&mut self) {
        crate::settings_ui::open(self, crate::nav::Page::Overview);
    }

    /// Feed the achievements observer from the sample just taken (no extra timers).
    fn ach_sample(&mut self, gpu_queried: bool) {
        use crate::achievements::observer as ob;
        let now_ms = ob::unix_ms();
        let snap = &self.m.snap;
        let vis = self.m.s.visible && self.display_on;
        let present = |o: &&Overlay| vis && (o.shown || o.fullscreen);
        let input = ob::SampleInput {
            now_ms,
            cpu: snap.cpu,
            gpu: snap.gpu,
            gpu_temp: snap.gpu_temp,
            net_down: snap.net_recv_delta,
            net_up: snap.net_sent_delta,
            disk_bytes: ((snap.disk_read + snap.disk_write) * snap.elapsed) as u64,
            ram_pct: snap.ram_pct,
            disk_used_pct: snap.disk_used_pct,
            battery: snap.battery.map(|b| (b.percent, b.on_ac)),
            uptime_s: snap.uptime_secs,
            taskbars: self.overlays.iter().filter(present).count().min(255) as u8,
            fullscreen: vis && self.overlays.iter().any(|o| o.fullscreen),
            input_recent: ob::input_recent(),
            gpu_queried,
            cap_set: self.m.s.data_cap_gb > 0.0,
        };
        let mut moments = self.ach.on_sample(&input);
        if self.ach.cycle_due(now_ms) {
            let reset = self.m.s.cap_reset_day;
            let start = UsageStore::cycle_start(reset, usage::today());
            if let Some(old) = self.ach.cycle_seen(start, reset) {
                if self.m.s.data_cap_gb > 0.0 {
                    let used = self.m.usage.range(old, usage::prev_day(start)).total();
                    let cap = self.m.s.data_cap_gb * 1024.0 * 1024.0 * 1024.0;
                    moments.extend(self.ach.event(crate::achievements::Event::CycleClosed {
                        under_cap: (used as f64) < cap,
                        now_ms,
                    }));
                }
            }
        }
        if !moments.is_empty() {
            self.ach_unlocked(moments);
        }
    }

    /// Send one event to the achievements observer (does nothing when switched off).
    pub fn ach_event(&mut self, ev: crate::achievements::Event) {
        if !self.m.s.achievements {
            return;
        }
        let moments = self.ach.event(ev);
        if !moments.is_empty() {
            self.ach_unlocked(moments);
        }
    }

    /// End task attempt finished (`ok` false = Windows refused).
    pub fn ach_task_ended(&mut self, ok: bool) {
        self.ach_event(crate::achievements::Event::TaskEnded { ok, now_ms: crate::achievements::observer::unix_ms() });
    }

    /// New achievement moments are ready to show.
    fn ach_unlocked(&mut self, moments: Vec<crate::achievements::Moment>) {
        let st = self.ach.state().clone();
        crate::achievements::ui::refresh(&st);
        crate::achievements::ui::show_moments(self.hwnd, &moments, &st, self.m.dark);
        crate::settings_ui::ach_changed(self);
    }

    /// Alert rules evaluated on the samples we already take (no extra work).
    /// Each alert fires once and re-arms only after the condition clears.
    fn check_alerts(&mut self) {
        let s = &self.m.s;
        if !s.alerts {
            return;
        }
        let snap = &self.m.snap;
        let now = Instant::now();
        let secs = s.alert_secs as u64;
        let mut fire: Vec<(String, String)> = Vec::new();
        // Achievement events: (alert kind, fired?) in the order they happened.
        let mut ach: Vec<(u8, bool)> = Vec::new();
        let mut sustained = |kind: u8, v: f32, lim: f32, since: &mut Option<Instant>, done: &mut bool, what: &str| {
            if lim <= 0.0 {
                return;
            }
            if v >= lim {
                let t = *since.get_or_insert(now);
                if !*done && now.duration_since(t).as_secs() >= secs {
                    *done = true;
                    ach.push((kind, true));
                    fire.push((
                        format!("{} is at {:.0}%", what, v),
                        format!("It has stayed above {:.0}% for {}.", lim, fmt::duration(secs)),
                    ));
                }
            } else if v < lim - 5.0 {
                *since = None;
                if std::mem::take(done) {
                    ach.push((kind, false));
                }
            }
        };
        let al = &mut self.alerts;
        sustained(crate::achievements::observer::kind::CPU, snap.cpu, s.alert_cpu_pct, &mut al.cpu_since, &mut al.cpu_done, "CPU");
        sustained(crate::achievements::observer::kind::RAM, snap.ram_pct, s.alert_ram_pct, &mut al.ram_since, &mut al.ram_done, "Memory");
        if let Some(used) = snap.disk_used_pct {
            let free = 100.0 - used;
            if s.alert_disk_free_pct > 0.0 && free < s.alert_disk_free_pct {
                if !al.disk_done {
                    al.disk_done = true;
                    ach.push((crate::achievements::observer::kind::DISK, true));
                    fire.push((
                        format!("System drive almost full ({:.0}% free)", free),
                        format!("{} free of {}.", fmt::bytes(snap.disk_total - snap.disk_used), fmt::bytes(snap.disk_total)),
                    ));
                }
            } else if free > s.alert_disk_free_pct + 2.0 {
                if std::mem::take(&mut al.disk_done) {
                    ach.push((crate::achievements::observer::kind::DISK, false));
                }
            }
        }
        if let Some(b) = snap.battery {
            let low = !b.on_ac && !b.charging && s.alert_battery_pct > 0 && (b.percent as u32) <= s.alert_battery_pct;
            if low && !al.battery_done {
                al.battery_done = true;
                ach.push((crate::achievements::observer::kind::BATTERY, true));
                fire.push((format!("Battery low ({}%)", b.percent), "Plug in your charger.".into()));
            } else if !low && std::mem::take(&mut al.battery_done) {
                ach.push((crate::achievements::observer::kind::BATTERY, false));
            }
        }
        if s.alert_cap {
            if let Some((used, pct)) = self.m.cycle_usage() {
                let level = if pct >= 100.0 { 2 } else if pct >= 90.0 { 1 } else { 0 };
                if level > al.cap_level {
                    ach.push((crate::achievements::observer::kind::CAP, true));
                    fire.push((
                        if level == 2 { "Monthly data cap reached".into() } else { "90% of your data cap used".into() },
                        format!("{} of {:.0} GB this billing cycle.", fmt::bytes(used), s.data_cap_gb),
                    ));
                }
                if level == 0 && al.cap_level > 0 {
                    ach.push((crate::achievements::observer::kind::CAP, false));
                }
                al.cap_level = level;
            }
        }
        for (title, text) in fire {
            crate::util::diag(&format!("alert: {} \u{2014} {}", title, text));
            self.tray.balloon(&title, &text);
        }
        for (kind, fired) in ach {
            let now_ms = crate::achievements::observer::unix_ms();
            self.ach_event(if fired {
                crate::achievements::Event::AlertFired { kind, now_ms }
            } else {
                crate::achievements::Event::AlertCleared { kind, now_ms }
            });
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
        if self.m.s.achievements {
            t.push('\n');
            t.push_str(&crate::achievements::tip_line(self.ach.state()));
        }
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
        crate::settings_ui::apply_menu_theme(self.m.s.theme, self.hwnd);
        self.need_locate = true;
        self.m.dark = effective_dark(self.m.s.theme);
        crate::achievements::ui::set_dark(self.m.dark);
        self.ins.set_enabled(self.m.s.insights, self.hwnd as isize);
        self.ins.sleep.bag_guard = self.m.s.bag_guard;
        // After set_enabled, so an open main window shows the new on/off state.
        crate::settings_ui::app_changed(self.m.dark, self);
        if !self.m.s.achievements {
            crate::achievements::ui::close_all();
        }
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
        // A taskbar Windows recreated takes our owned widget down with it.
        let dead = self.overlays.iter().any(|o| unsafe {
            IsWindow(o.hwnd) == 0 || (!o.tb.hwnd.is_null() && IsWindow(o.tb.hwnd) == 0)
        });
        if dead {
            crate::util::diag("taskbar or widget window was destroyed by Windows; recreating");
            self.overlays.retain(|o| unsafe { IsWindow(o.hwnd) != 0 && (o.tb.hwnd.is_null() || IsWindow(o.tb.hwnd) != 0) });
            self.need_locate = true;
        }
        if self.need_locate || self.taskbars.is_empty() {
            self.need_locate = false;
            self.need_check = true;
            let mut tbs = taskbar::locate_all();
            if tbs.is_empty() {
                tbs.extend(taskbar::fallback());
            }
            // Tray area changed size: measure where its buttons start again
            // (event-driven, not a timer).
            let tray_moved = tbs.iter().any(|t| {
                self.taskbars.iter().any(|o| o.hwnd == t.hwnd && o.tray != t.tray)
            });
            self.taskbars = tbs;
            self.hook_taskbar_moves();
            if tray_moved {
                if let Some(w) = &self.icons {
                    w.kick();
                }
            }
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
                let (fs, why) = if m.s.hide_fullscreen {
                    taskbar::fullscreen_active(&tb, &own)
                } else {
                    (false, String::new())
                };
                if fs != o.fullscreen {
                    crate::util::diag(&format!(
                        "{}: {} \u{2014} {}",
                        tb.label(),
                        if fs { "hidden for full-screen app" } else { "full-screen ended, shown" },
                        why
                    ));
                }
                o.fullscreen = fs;
            }
            let want = m.s.visible && !o.fullscreen && !(dock && tb.retracted) && self.display_on;
            if want != o.shown {
                let why = if !m.s.visible {
                    "hidden in menu"
                } else if o.fullscreen {
                    "full-screen app"
                } else if dock && tb.retracted {
                    "auto-hide taskbar retracted"
                } else if !self.display_on {
                    "display off"
                } else {
                    "visible"
                };
                crate::util::diag(&format!("{}: {} ({})", tb.label(), if want { "show" } else { "hide" }, why));
            }
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
        let s = &self.m.s;
        let hm = unsafe { CreatePopupMenu() };
        let add = |m: HMENU, id: usize, text: &str, checked: bool| unsafe {
            let t = wide(text);
            AppendMenuW(m, MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED }, id, t.as_ptr());
        };
        let sep = |m: HMENU| unsafe {
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
        };
        // Short on purpose: everything else lives in the Settings window.
        unsafe {
            // Header (bold, as the default item): opens the Emulsion Labs website.
            add(hm, ID_WEBSITE, "Resource Monitor by Emulsion Labs", false);
            SetMenuDefaultItem(hm, ID_WEBSITE as u32, 0);
            sep(hm);
            add(hm, ID_VISIBLE, "Show widget", s.visible);
            add(hm, ID_SETTINGS, "Settings\u{2026}", false);
            if s.achievements {
                add(hm, ID_ACHIEVE, "Achievements\u{2026}", false);
            }
            add(hm, ID_TASKMGR, "Task Manager", false);
            add(hm, ID_UPDATE_NOW, "Check for updates", false);
            sep(hm);
            add(hm, ID_EXIT, "Exit", false);

            let hwnd = self.hwnd;
            self.menu_open = true;
            SetForegroundWindow(hwnd);
            // `self` must not be used past this point: the menu loop re-enters.
            let cmd = TrackPopupMenuEx(hm, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, hwnd, std::ptr::null());
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

    /// The Achievements page of the main window (tray menu entry, or `--achievements` on the command line).
    pub fn open_achievements(&mut self) {
        self.command(ID_ACHIEVE);
    }

    fn command(&mut self, id: usize) {
        match id {
            ID_WEBSITE => shell_open(WEBSITE, None),
            ID_SETTINGS => crate::settings_ui::open_settings(self),
            ID_ACHIEVE => crate::settings_ui::open(self, crate::nav::Page::Achievements),
            ID_TASKMGR => shell_open("taskmgr.exe", None),
            ID_UPDATE_NOW => self.run(Cmd::CheckUpdates),
            ID_EXIT => unsafe {
                DestroyWindow(self.hwnd);
            },
            ID_VISIBLE => {
                self.m.s.visible = !self.m.s.visible;
                self.save_settings();
                self.apply_settings();
            }
            _ => {}
        }
    }

    /// Actions of the Settings window that act on the app rather than on its
    /// copy of the settings.
    pub fn run(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Website => return shell_open(WEBSITE, None),
            Cmd::Report => return report::open(self),
            Cmd::CheckUpdates => {
                crate::update_ui::show(self, "Checking GitHub for a newer version\u{2026}", Some(0.0), true);
                return crate::update::check(self.hwnd as isize, true);
            }
            Cmd::RecheckIcons => {
                if let Some(w) = &self.icons {
                    w.kick();
                }
                return;
            }
            Cmd::EditSettings => {
                self.save_settings();
                return shell_open("notepad.exe", Some(&format!("\"{}\"", Settings::path().display())));
            }
            Cmd::DataFolder => {
                self.m.usage.save();
                return shell_open(&data_dir().display().to_string(), None);
            }
            _ => {}
        }
        let first = self.overlays.first().map(|o| (o.pos, o.tb));
        let s = &mut self.m.s;
        match cmd {
            Cmd::Dock => s.mode = Mode::Dock,
            Cmd::Float => {
                if s.mode != Mode::Float {
                    s.mode = Mode::Float;
                    if let Some((pos, _)) = first {
                        s.float_x = pos.l;
                        s.float_y = pos.t - pos.h() - 40;
                    }
                }
            }
            Cmd::ResetPosition => {
                s.dock_offset = 0;
                s.offsets.clear();
                if let Some((_, tb)) = first {
                    s.float_x = tb.work.r - 360;
                    s.float_y = tb.work.b - 80;
                }
            }
            _ => return,
        }
        self.save_settings();
        self.apply_settings();
    }

    /// Taskbars that can get a widget: (settings key, label, primary), for the Settings window.
    pub fn displays(&self) -> Vec<(String, String, bool)> {
        self.taskbars
            .iter()
            .take(MAX_DISPLAYS)
            .map(|t| {
                let label = format!(
                    "{}{}  ({}\u{00D7}{})",
                    t.label(),
                    if t.primary { " \u{2014} main" } else { "" },
                    t.monitor.w(),
                    t.monitor.h()
                );
                (t.key(), label, t.primary)
            })
            .collect()
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
                    // Give the mouse a moment to reach the panel before closing it.
                    self.hide_popup_soon();
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
        // The main window applies pending edits and saves its placement.
        crate::settings_ui::close();
        self.m.usage.save();
        self.ach.flush();
        self.ins.flush();
        self.tray.remove();
        unsafe {
            if !self.hook.is_null() {
                UnhookWinEvent(self.hook);
            }

            KillTimer(self.hwnd, T_SAMPLE);
            for h in [self.move_hook, self.life_hook, self.fg_hook] {
                if !h.is_null() {
                    UnhookWinEvent(h);
                }
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

/// Emulsion Labs website, opened from the tray menu header and Settings > About.
pub const WEBSITE: &str = "https://emulsionlabs.in/";

/// What the Settings window can ask the app to do.
#[derive(Clone, Copy, PartialEq)]
pub enum Cmd {
    Website,
    Report,
    CheckUpdates,
    RecheckIcons,
    EditSettings,
    DataFolder,
    Dock,
    Float,
    ResetPosition,
}

// Tray menu items.
const ID_VISIBLE: usize = 600;
const ID_TASKMGR: usize = 701;
const ID_EXIT: usize = 705;
const ID_SETTINGS: usize = 706;
const ID_WEBSITE: usize = 707;
const ID_UPDATE_NOW: usize = 708;
const ID_ACHIEVE: usize = 799;
const MAX_DISPLAYS: usize = 16;

const NIN_KEYSELECT_: u32 = NIN_SELECT | 1;

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
        WM_TIMER if w == T_HOVER_HIDE => {
            KillTimer(hwnd, T_HOVER_HIDE);
            if !a.popup_pinned && !a.mouse_over_ours() {
                popup::hide(a);
            }
            0
        }
        WM_TIMER if w == T_CHECK => {
            KillTimer(hwnd, T_CHECK);
            a.refresh(false);
            0
        }
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
                a.schedule_check();
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
                // A click on an Insights notification opens the page that wrote it.
                NIN_BALLOONUSERCLICK => {
                    if let Some(p) = a.tray.balloon_source().and_then(crate::nav::Page::of_panel_key) {
                        crate::settings_ui::open(a, p);
                    }
                }
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
            if w == 2 {
                a.open_achievements();
                return 0;
            }
            if w == 4 {
                crate::settings_ui::open(a, crate::nav::Page::Monitor);
                return 0;
            }
            if w == 3 {
                a.open_insights();
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
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            a.need_locate = true;
            a.refresh(true);
            0
        }
        WM_SETTINGCHANGE => {
            // Theme switch, taskbar moved/resized (work area), auto-hide toggled…
            a.m.dark = effective_dark(a.m.s.theme);
            crate::achievements::ui::set_dark(a.m.dark);
            crate::settings_ui::app_changed(a.m.dark, a);
            crate::settings_ui::apply_menu_theme(a.m.s.theme, hwnd);
            a.update_timer();
            a.need_locate = true;
            a.refresh(true);
            0
        }
        WM_POWERBROADCAST => {
            use crate::insights::Power;
            let now_ms = crate::insights::unix_ms();
            match w as u32 {
                PBT_APMSUSPEND => {
                    a.m.usage.save();
                    a.ach.flush();
                    let n = a.ins.on_power(Power::Suspend, now_ms);
                    a.insight_notices(n);
                }
                PBT_POWERSETTINGCHANGE => {
                    let p = l as *const POWERBROADCAST_SETTING;
                    if !p.is_null() && (*p).DataLength >= 4 {
                        let data = u32::from_le_bytes([(*p).Data[0], *(*p).Data.as_ptr().add(1), *(*p).Data.as_ptr().add(2), *(*p).Data.as_ptr().add(3)]);
                        if let Some(ev) = crate::insights::power_from_setting(&(*p).PowerSetting, data) {
                            let n = a.ins.on_power(ev, now_ms);
                            a.insight_notices(n);
                        }
                    }
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
                    // Both messages arrive after a real resume; Insights de-duplicates.
                    let n = a.ins.on_power(Power::Resume, now_ms);
                    a.insight_notices(n);
                    a.refresh(true)
                }
                _ => {}
            }
            1
        }
        crate::insights::WM_APP_INSIGHT_EVENT => {
            if l != 0 {
                // SAFETY: posted by insights::events with Box::into_raw; we take ownership once.
                let e = Box::from_raw(l as *mut crate::insights::events::SysEvent);
                let n = a.ins.on_event(&e, crate::insights::unix_ms());
                a.insight_notices(n);
            }
            0
        }
        crate::insights::WM_APP_POWER_MODE => {
            let n = a.ins.on_power(crate::insights::Power::Mode(w as i32), crate::insights::unix_ms());
            a.insight_notices(n);
            0
        }
        WM_QUERYENDSESSION => {
            a.m.usage.save();
            a.ach.flush();
            a.ins.flush();
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
