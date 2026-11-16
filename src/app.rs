//! Application state, overlay window, menu and event handling.

use crate::fmt::{self, SpeedUnit};
use crate::render::{create_font, ui_face, Canvas, Rgba};
use crate::sensors::{AdapterInfo, SampleOpts, Sensors, Snapshot};
use crate::settings::{Metric, Mode, Settings, Theme};
use crate::taskbar::{self, Edge, Taskbar};
use crate::tray::{Tray, WM_TRAY};
use crate::usage::{self, UsageStore};
use crate::util::*;
use crate::{popup, report};
use std::collections::VecDeque;
use std::time::{Instant, SystemTime};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Power::*;
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::Controls::{WM_MOUSEHOVER, WM_MOUSELEAVE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const OVERLAY_CLASS: &str = "ResourceMonitor.Overlay";
pub const WM_APP_ACTIVATE: u32 = WM_APP + 2;
pub const HIST: usize = 60;

const T_SAMPLE: usize = 1;
const T_FAST: usize = 2;

pub const C_WARN: Rgba = Rgba::rgb(255, 183, 77);
pub const C_CRIT: Rgba = Rgba::rgb(255, 82, 82);

pub fn accent(m: Metric, dark: bool) -> Rgba {
    let c = match m {
        Metric::Cpu => Rgba::rgb(79, 195, 247),
        Metric::Ram => Rgba::rgb(179, 157, 219),
        Metric::Gpu => Rgba::rgb(255, 138, 101),
        Metric::Vram => Rgba::rgb(255, 171, 145),
        Metric::DiskRead | Metric::DiskWrite | Metric::DiskActive => Rgba::rgb(255, 213, 79),
        Metric::Up => Rgba::rgb(255, 145, 0),
        Metric::Down => Rgba::rgb(38, 198, 218),
        Metric::TodayUp | Metric::TodayDown => Rgba::rgb(165, 214, 167),
        Metric::TopProc => Rgba::rgb(176, 190, 197),
        Metric::Battery => Rgba::rgb(129, 199, 132),
    };
    if dark {
        c
    } else {
        c.mix(Rgba::rgb(0, 0, 0), 0.38)
    }
}

pub fn short_label(m: Metric) -> &'static str {
    match m {
        Metric::Cpu => "CPU",
        Metric::Ram => "RAM",
        Metric::Gpu => "GPU",
        Metric::Vram => "VRAM",
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

/// Which metrics belong together in one column (e.g. ▲ above ▼).
fn group(m: Metric) -> u8 {
    match m {
        Metric::Cpu | Metric::Ram => 0,
        Metric::Gpu | Metric::Vram => 1,
        Metric::DiskRead | Metric::DiskWrite | Metric::DiskActive => 2,
        Metric::Up | Metric::Down => 3,
        Metric::TodayUp | Metric::TodayDown => 4,
        Metric::TopProc => 5,
        Metric::Battery => 6,
    }
}

/// Pack rows into columns of `rpc`, never splitting a group across columns
/// when it would fit whole in the next one. Order is preserved.
fn pack(rows: &[Metric], rpc: usize) -> Vec<Vec<Metric>> {
    let mut groups: Vec<Vec<Metric>> = Vec::new();
    for &m in rows {
        match groups.last_mut() {
            Some(g) if group(g[0]) == group(m) && g.len() < rpc => g.push(m),
            _ => groups.push(vec![m]),
        }
    }
    let mut cols: Vec<Vec<Metric>> = Vec::new();
    for g in groups {
        match cols.last_mut() {
            Some(c) if c.len() + g.len() <= rpc => c.extend(g),
            _ => cols.push(g),
        }
    }
    cols
}

struct Drag {
    start: POINT,
    win_start: (i32, i32),
    offset_start: i32,
    moved: bool,
}

pub struct App {
    pub hwnd: HWND,
    pub popup: HWND,
    pub report: HWND,
    pub s: Settings,
    settings_mtime: Option<SystemTime>,
    sensors: Sensors,
    pub snap: Snapshot,
    pub hist: Vec<VecDeque<f32>>,
    pub usage: UsageStore,
    last_save: Instant,
    pub adapters: Vec<AdapterInfo>,
    canvas: Canvas,
    font: HFONT,
    font_key: (u32, u32, u32),
    sticky: [i32; Metric::ALL.len()],
    pub scale: f32,
    pub tb: Option<Taskbar>,
    pub pos: Rect,
    shown: bool,
    fullscreen: bool,
    display_on: bool,
    pub dark: bool,
    tray: Tray,
    msg_taskbar_created: u32,
    tracking: bool,
    pub popup_pinned: bool,
    drag: Option<Drag>,
    samples: u64,
    gpu_seen: bool,
    hook: HWINEVENTHOOK,
}

static mut APP: *mut App = std::ptr::null_mut();

/// Single UI thread; every access goes through here and is never held across
/// a nested message loop (menus, dialogs).
#[allow(static_mut_refs)]
pub fn app() -> Option<&'static mut App> {
    unsafe { APP.as_mut() }
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
use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

pub fn create(hinst: HINSTANCE) -> Option<()> {
    unsafe {
        let cls = wide(OVERLAY_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        popup::register(hinst);
        report::register(hinst);

        let s = Settings::load();
        let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
        if s.click_through {
            ex |= WS_EX_TRANSPARENT;
        }
        let title = wide("Resource Monitor");
        let hwnd = CreateWindowExW(
            ex,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let first_run = s.first_run;
        let msg_taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let dark = effective_dark(s.theme);
        let mut a = Box::new(App {
            hwnd,
            popup: std::ptr::null_mut(),
            report: std::ptr::null_mut(),
            s,
            settings_mtime: None,
            sensors: Sensors::new(),
            snap: Snapshot::default(),
            hist: (0..Metric::ALL.len()).map(|_| VecDeque::with_capacity(HIST)).collect(),
            usage: UsageStore::load(),
            last_save: Instant::now(),
            adapters: Vec::new(),
            canvas: Canvas::new(),
            font: std::ptr::null_mut(),
            font_key: (0, 0, 0),
            sticky: [0; Metric::ALL.len()],
            scale: 1.0,
            tb: None,
            pos: Rect::default(),
            shown: false,
            fullscreen: false,
            display_on: true,
            dark,
            tray: Tray::new(hwnd),
            msg_taskbar_created,
            tracking: false,
            popup_pinned: false,
            drag: None,
            samples: 0,
            gpu_seen: false,
            hook: std::ptr::null_mut(),
        });
        a.popup = popup::create(hinst, hwnd);
        a.tray.add();
        if first_run {
            a.s.first_run = false;
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
        let guid = GUID_CONSOLE_DISPLAY_STATE;
        RegisterPowerSettingNotification(hwnd as HANDLE, &guid, DEVICE_NOTIFY_WINDOW_HANDLE);

        let a = app()?;
        a.hook = hook;
        a.sample();
        a.update_timer();
        a.refresh(true);
    }
    Some(())
}

const GUID_CONSOLE_DISPLAY_STATE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

fn guid_eq(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    if let Some(a) = app() {
        a.refresh(false);
    }
}

impl App {
    fn opts(&self) -> SampleOpts {
        let sh = |m| self.s.shown(m);
        SampleOpts {
            adapter: (!self.s.adapter.is_empty()).then(|| self.s.adapter.clone()),
            // GPU engine counters are the only costly source: query them only when needed.
            gpu: sh(Metric::Gpu) || sh(Metric::Vram) || self.popup_open(),
            disk: true,
            procs: true,
        }
    }

    pub fn popup_open(&self) -> bool {
        !self.popup.is_null() && unsafe { IsWindowVisible(self.popup) } != 0
    }

    fn sample(&mut self) {
        let opts = self.opts();
        let snap = self.sensors.sample(&opts).clone();
        self.usage.add(snap.net_sent_delta, snap.net_recv_delta);
        let vals: [f32; Metric::ALL.len()] = {
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
            v
        };
        for (h, v) in self.hist.iter_mut().zip(vals) {
            if h.len() == HIST {
                h.pop_front();
            }
            h.push_back(v);
        }
        if snap.gpu.is_some() {
            self.gpu_seen = true;
        }
        self.snap = snap;
        self.samples += 1;

        if self.last_save.elapsed().as_secs() >= 60 {
            self.usage.save();
            self.last_save = Instant::now();
        }
        if self.samples % 3 == 0 {
            self.check_settings_file();
        }
        self.update_tip();
    }

    fn update_tip(&mut self) {
        let s = &self.snap;
        let mut t = format!("CPU {:.0}% \u{00B7} RAM {:.0}%", s.cpu, s.ram_pct);
        if let Some(g) = s.gpu {
            t.push_str(&format!(" \u{00B7} GPU {:.0}%", g));
        }
        t.push_str(&format!(
            "\n\u{25B2} {}  \u{25BC} {}",
            fmt::speed(s.net_up, self.s.unit),
            fmt::speed(s.net_down, self.s.unit)
        ));
        let today = self.usage.get(usage::today());
        t.push_str(&format!("\nToday {}", fmt::bytes(today.total())));
        self.tray.tip(&t);
    }

    fn check_settings_file(&mut self) {
        let m = Settings::mtime();
        if m.is_some() && m != self.settings_mtime {
            self.settings_mtime = m;
            self.s = Settings::load();
            self.apply_settings();
        }
    }

    pub fn save_settings(&mut self) {
        self.s.save();
        self.settings_mtime = Settings::mtime();
    }

    fn apply_settings(&mut self) {
        unsafe {
            let ex = GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32;
            let ex = if self.s.click_through { ex | WS_EX_TRANSPARENT } else { ex & !WS_EX_TRANSPARENT };
            SetWindowLongW(self.hwnd, GWL_EXSTYLE, ex as i32);
        }
        self.dark = effective_dark(self.s.theme);
        self.sticky = [0; Metric::ALL.len()];
        self.update_timer();
        self.refresh(true);
        if self.popup_open() {
            popup::reposition(self);
        }
    }

    fn update_timer(&self) {
        let ms = if self.display_on { self.s.interval_ms } else { self.s.interval_ms.max(5000) };
        unsafe {
            // Coalescable: lets Windows batch our wakeups with others to save power.
            SetCoalescableTimer(self.hwnd, T_SAMPLE, ms, None, (ms / 10).max(32));
            let mut abd: APPBARDATA = std::mem::zeroed();
            abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            let auto_hide = SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32 & ABS_AUTOHIDE != 0;
            if auto_hide && self.s.mode == Mode::Dock {
                SetCoalescableTimer(self.hwnd, T_FAST, 200, None, 50);
            } else {
                KillTimer(self.hwnd, T_FAST);
            }
        }
    }

    // ---------------------------------------------------------------- layout

    fn ensure_font(&mut self, px: u32) {
        let key = (px, self.s.text_scale, 0);
        if key == self.font_key && !self.font.is_null() {
            return;
        }
        unsafe {
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
        self.font = create_font(px as i32, 600, ui_face(), true);
        self.font_key = key;
        self.sticky = [0; Metric::ALL.len()];
    }

    fn rows(&self) -> Vec<Metric> {
        Metric::ALL
            .iter()
            .copied()
            .filter(|&m| self.s.shown(m))
            .filter(|&m| match m {
                // Hide sensors this machine doesn't have, once we know.
                Metric::Gpu => self.gpu_seen || self.samples < 4,
                Metric::Vram => (self.gpu_seen && self.snap.vram_used.is_some()) || self.samples < 4,
                Metric::Battery => self.snap.battery.is_some(),
                Metric::DiskActive => self.snap.disk_active.is_some() || self.samples < 4,
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
    fn value(&self, m: Metric) -> (String, u8, &'static str) {
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

    /// Normalised sparkline series (0..1) for a metric, newest last.
    pub fn series(&self, m: Metric, n: usize) -> Vec<f32> {
        let h = &self.hist[m.idx()];
        let start = h.len().saturating_sub(n);
        let data: Vec<f32> = h.iter().skip(start).copied().collect();
        let floor = match m {
            Metric::Cpu | Metric::Ram | Metric::Gpu | Metric::DiskActive | Metric::Battery => {
                return data.iter().map(|v| v / 100.0).collect()
            }
            Metric::Up | Metric::Down => 64.0 * 1024.0,
            Metric::DiskRead | Metric::DiskWrite => 1024.0 * 1024.0,
            _ => 1.0,
        };
        let max = data.iter().copied().fold(floor, f32::max);
        data.iter().map(|v| v / max).collect()
    }

    fn has_spark(m: Metric) -> bool {
        !matches!(m, Metric::TodayUp | Metric::TodayDown | Metric::TopProc | Metric::Battery)
    }

    /// Measure, lay out and draw the widget, then position it.
    fn render(&mut self) {
        let Some(tb) = self.tb else { return };
        let dock = self.s.mode == Mode::Dock;
        let horiz = !dock || tb.edge.horizontal();
        let scale = tb.dpi as f32 / 96.0;
        self.scale = scale;
        let ts = self.s.text_scale as f32 / 100.0;
        let sc = |v: f32| (v * scale).round() as i32;

        let thickness = if tb.edge.horizontal() { tb.rect.h() } else { tb.rect.w() };
        let mut font_px = (12.0 * scale * ts).round() as u32;
        // Rows per column for a horizontal strip: fit 2 lines (or 3 on tall taskbars).
        let mut rpc = 2usize;
        if dock && horiz {
            let avail = thickness - sc(4.0);
            let line = |px: u32| (px as f32 * 1.32).ceil() as i32;
            if line(font_px) * 3 <= avail && thickness >= sc(64.0) {
                rpc = 3;
            } else if line(font_px) * 2 > avail {
                let fit = ((avail / 2) as f32 / 1.32).floor() as u32;
                if fit as f32 >= 10.0 * scale {
                    font_px = fit;
                } else {
                    rpc = 1;
                }
            }
        }
        self.ensure_font(font_px);
        let font = self.font;
        let rows = self.rows();
        let n = rows.len();

        let (_, line_h) = self.canvas.measure(font, "Ag");
        let line_h = if line_h == 0 { (font_px as f32 * 1.32) as i32 } else { line_h - sc(1.0) };
        let gap = sc(5.0);
        let col_gap = sc(12.0);
        let pad = sc(6.0);
        let spark_w = if self.s.sparklines { sc(30.0) } else { 0 };
        let spark_h = (line_h - sc(5.0)).max(4);

        let values: Vec<(String, u8, &str)> = rows.iter().map(|&m| self.value(m)).collect();
        let label_w = if self.s.labels {
            rows.iter().map(|&m| self.canvas.measure(font, short_label(m)).0).max().unwrap_or(0)
        } else {
            0
        };
        for (i, &m) in rows.iter().enumerate() {
            let (v, _, tpl) = &values[i];
            let tw = self.canvas.measure(font, tpl).0.max(self.canvas.measure(font, v).0);
            let cap = if m == Metric::TopProc { sc(110.0 * ts) } else { i32::MAX };
            self.sticky[m.idx()] = self.sticky[m.idx()].max(tw).min(cap);
        }
        let vw = |m: Metric| self.sticky[m.idx()];
        let spark_part = |m: Metric| if spark_w > 0 && Self::has_spark(m) { gap + spark_w } else { 0 };
        let lab_part = if label_w > 0 { label_w + gap } else { 0 };

        // (metric, x, y, value_right, spark rect)
        let mut cells: Vec<(usize, i32, i32, i32, Option<Rect>, bool)> = Vec::with_capacity(n);
        let (w, h);
        if n == 0 {
            (w, h) = (sc(24.0), if dock && horiz { thickness } else { line_h + 2 * pad });
        } else if horiz {
            let cols = pack(&rows, rpc);
            let hh = if dock { thickness.max(line_h + 2) } else { rpc as i32 * line_h + 2 * pad };
            let mut x = pad;
            let mut idx = 0;
            for col in &cols {
                let val_w = col.iter().map(|&m| vw(m)).max().unwrap_or(0);
                let sp = col.iter().map(|&m| spark_part(m)).max().unwrap_or(0);
                let lab_part = if label_w > 0 {
                    col.iter().map(|&m| self.canvas.measure(font, short_label(m)).0).max().unwrap_or(0) + gap
                } else {
                    0
                };
                let y0 = (hh - col.len() as i32 * line_h) / 2;
                for (j, &m) in col.iter().enumerate() {
                    let y = y0 + j as i32 * line_h;
                    let vr = x + lab_part + val_w;
                    let spark = (spark_part(m) > 0).then(|| Rect {
                        l: vr + gap,
                        t: y + (line_h - spark_h) / 2,
                        r: vr + gap + spark_w,
                        b: y + (line_h - spark_h) / 2 + spark_h,
                    });
                    cells.push((idx, x, y, vr, spark, label_w > 0));
                    idx += 1;
                    let _ = m;
                }
                x += lab_part + val_w + sp + col_gap;
            }
            (w, h) = (x - col_gap + pad, hh);
        } else {
            // Vertical taskbar: one row per metric, drop parts that don't fit.
            let width = thickness.max(sc(40.0));
            let inner = width - 2 * sc(4.0);
            let max_v = rows.iter().map(|&m| vw(m)).max().unwrap_or(0);
            let with_label = label_w > 0 && lab_part + max_v <= inner;
            let with_spark = spark_w > 0 && (if with_label { lab_part } else { 0 }) + max_v + gap + spark_w <= inner;
            let mut y = pad;
            for (i, &m) in rows.iter().enumerate() {
                let x = sc(4.0);
                let vr = if with_spark && Self::has_spark(m) { width - sc(4.0) - gap - spark_w } else { width - sc(4.0) };
                let spark = (with_spark && Self::has_spark(m)).then(|| Rect {
                    l: vr + gap,
                    t: y + (line_h - spark_h) / 2,
                    r: vr + gap + spark_w,
                    b: y + (line_h - spark_h) / 2 + spark_h,
                });
                cells.push((i, x, y, vr, spark, with_label));
                y += line_h;
            }
            (w, h) = (width, y + pad);
        }

        // ---- draw
        self.canvas.resize(w, h);
        let fg = if self.dark { Rgba::rgb(245, 245, 245) } else { Rgba::rgb(18, 18, 18) };
        let opacity = if dock { self.s.bg_opacity } else { self.s.bg_opacity.max(200) };
        // alpha 1 keeps the whole strip hoverable while staying invisible.
        self.canvas.clear(Rgba(0, 0, 0, 1));
        if opacity > 1 {
            let bg = if self.dark { Rgba(28, 28, 28, opacity) } else { Rgba(248, 248, 248, opacity) };
            let inset = if dock && horiz { sc(4.0) } else { 0 };
            self.canvas.fill_round(Rect { l: 0, t: inset, r: w, b: h - inset }, sc(6.0) as f32, bg);
        }
        for &(i, x, y, vr, spark, with_label) in &cells {
            let m = rows[i];
            let (v, lvl, _) = &values[i];
            let ac = accent(m, self.dark);
            if with_label {
                self.canvas.text(font, short_label(m), x, y, ac);
            }
            let vc = match lvl {
                2 => C_CRIT,
                1 => C_WARN,
                _ => fg,
            };
            let max_w = vw(m);
            let mut v = v.clone();
            while v.chars().count() > 2 && self.canvas.measure(font, &v).0 > max_w {
                v.pop();
                v = v.trim_end_matches('\u{2026}').to_string();
                v.pop();
                v.push('\u{2026}');
            }
            self.canvas.text_right(font, &v, vr, y, vc);
            if let Some(r) = spark {
                let data = self.series(m, 30);
                self.canvas.fill_rect(r, ac.with_a(if self.dark { 22 } else { 30 }));
                self.canvas.spark(r, &data, ac);
            }
        }

        // ---- position
        let (x, y) = if dock {
            let off = (self.s.dock_offset as f32 * scale) as i32;
            match tb.edge {
                Edge::Bottom | Edge::Top => {
                    let x = (tb.tray.l - w - sc(4.0) - off).max(tb.rect.l);
                    (x, tb.rect.t + (tb.rect.h() - h) / 2)
                }
                Edge::Left | Edge::Right => {
                    let y = (tb.tray.t - h - sc(4.0) - off).max(tb.rect.t);
                    (tb.rect.l + (tb.rect.w() - w) / 2, y)
                }
            }
        } else {
            let mon = taskbar::monitor_info(unsafe {
                MonitorFromPoint(POINT { x: self.s.float_x, y: self.s.float_y }, MONITOR_DEFAULTTONEAREST)
            })
            .map(|m| m.0)
            .unwrap_or(tb.monitor);
            (self.s.float_x.clamp(mon.l, mon.r - w), self.s.float_y.clamp(mon.t, mon.b - h))
        };
        self.pos = Rect { l: x, t: y, r: x + w, b: y + h };
        self.canvas.present(self.hwnd, x, y);
    }

    /// Re-evaluate taskbar position, visibility and z-order. Redraws only when
    /// `redraw` (new data) or the taskbar geometry changed.
    pub fn refresh(&mut self, redraw: bool) {
        let geom = |t: &Option<Taskbar>| t.map(|t| (t.rect, t.tray, t.edge, t.dpi, t.retracted));
        let before = geom(&self.tb);
        self.tb = taskbar::locate().or(self.tb).or_else(|| {
            // No taskbar (e.g. Explorer not running): pretend one at the primary monitor bottom.
            let (mon, work) = taskbar::monitor_info(unsafe {
                MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY)
            })?;
            let rect = Rect { t: mon.b - 48, ..mon };
            Some(Taskbar {
                hwnd: std::ptr::null_mut(),
                rect,
                edge: Edge::Bottom,
                tray: Rect { l: rect.r - 200, ..rect },
                monitor: mon,
                work,
                dpi: 96,
                retracted: false,
            })
        });
        let Some(tb) = self.tb else { return };
        let redraw = redraw || before != geom(&self.tb);
        let own = [self.hwnd, self.popup, self.report];
        self.fullscreen = self.s.hide_fullscreen && taskbar::fullscreen_active(&tb, &own);
        let dock = self.s.mode == Mode::Dock;
        let want = self.s.visible && !self.fullscreen && !(dock && tb.retracted) && self.display_on;
        if want {
            if redraw || !self.shown {
                self.render();
            }
            unsafe {
                if !self.shown {
                    ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                // Win11 raises the taskbar over other topmost windows when clicked; re-assert.
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
            self.shown = true;
        } else if self.shown {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.shown = false;
        }
        if self.popup_open() {
            if self.fullscreen && !self.popup_pinned {
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
        let s = &self.s;
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
            for (i, a) in self.adapters.iter().take(40).enumerate() {
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

            sep(hm);
            add(hm, ID_REPORT, "Data usage report\u{2026}", false);
            add(hm, ID_TASKMGR, "Task Manager", false);
            sep(hm);
            add(hm, ID_AUTOSTART, "Start with Windows", crate::autostart::enabled());
            add(hm, ID_EDIT, "Edit settings file\u{2026}", false);
            add(hm, ID_DATA, "Open data folder", false);
            sep(hm);
            add(hm, ID_EXIT, "Exit", false);

            let hwnd = self.hwnd;
            SetForegroundWindow(hwnd);
            // No borrow of App is held across the modal menu loop.
            let cmd = TrackPopupMenuEx(
                hm,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                x,
                y,
                hwnd,
                std::ptr::null(),
            );
            DestroyMenu(hm);
            PostMessageW(hwnd, WM_NULL, 0, 0);
            if cmd > 0 {
                if let Some(a) = app() {
                    a.command(cmd as usize);
                }
            }
        }
    }

    fn command(&mut self, id: usize) {
        let s = &mut self.s;
        match id {
            ID_VISIBLE => s.visible = !s.visible,
            ID_SPARK => s.sparklines = !s.sparklines,
            ID_LABELS => s.labels = !s.labels,
            ID_FULLSCREEN => s.hide_fullscreen = !s.hide_fullscreen,
            ID_CLICKTHRU => s.click_through = !s.click_through,
            ID_DOCK => s.mode = Mode::Dock,
            ID_FLOAT => {
                if s.mode != Mode::Float {
                    s.mode = Mode::Float;
                    s.float_x = self.pos.l;
                    s.float_y = self.pos.t - self.pos.h() - 40;
                }
            }
            ID_RESET_POS => {
                s.dock_offset = 0;
                if let Some(tb) = self.tb {
                    s.float_x = tb.work.r - 360;
                    s.float_y = tb.work.b - 80;
                }
            }
            ID_ADAPTER => s.adapter.clear(),
            ID_REPORT => {
                report::open(self);
                return;
            }
            ID_TASKMGR => {
                shell_open("taskmgr.exe", None);
                return;
            }
            ID_AUTOSTART => {
                crate::autostart::set(!crate::autostart::enabled());
                return;
            }
            ID_EDIT => {
                self.save_settings();
                shell_open("notepad.exe", Some(&format!("\"{}\"", Settings::path().display())));
                return;
            }
            ID_DATA => {
                self.usage.save();
                shell_open(&data_dir().display().to_string(), None);
                return;
            }
            ID_EXIT => {
                unsafe { DestroyWindow(self.hwnd) };
                return;
            }
            _ if (ID_METRIC..ID_METRIC + Metric::ALL.len()).contains(&id) => {
                let i = id - ID_METRIC;
                s.show[i] = !s.show[i];
            }
            _ if (ID_UNIT..ID_UNIT + SpeedUnit::ALL.len()).contains(&id) => s.unit = SpeedUnit::ALL[id - ID_UNIT],
            _ if (ID_ADAPTER + 1..ID_ADAPTER + 41).contains(&id) => {
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
            _ => return,
        }
        self.save_settings();
        self.apply_settings();
    }

    // ---------------------------------------------------------------- mouse

    fn on_mouse(&mut self, msg: u32, l: isize) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        match msg {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_HOVER,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 350,
                    };
                    unsafe { TrackMouseEvent(&mut t) };
                    self.tracking = true;
                }
                let th = (4.0 * self.scale) as i32;
                let Some(d) = self.drag.as_mut() else { return };
                let (dx, dy) = (pt.x - d.start.x, pt.y - d.start.y);
                if !d.moved && (dx.abs() > th || dy.abs() > th) {
                    d.moved = true;
                }
                if !d.moved {
                    return;
                }
                let (ws, os) = (d.win_start, d.offset_start);
                popup::hide(self);
                if self.s.mode == Mode::Float {
                    self.s.float_x = ws.0 + dx;
                    self.s.float_y = ws.1 + dy;
                } else {
                    let horiz = self.tb.map_or(true, |t| t.edge.horizontal());
                    let delta = if horiz { dx } else { dy };
                    self.s.dock_offset = (os - (delta as f32 / self.scale) as i32).max(0);
                }
                self.render();
            }
            WM_MOUSEHOVER => {
                self.tracking = false;
                if self.drag.is_none() && !self.popup_open() {
                    popup::show(self, false);
                }
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if !self.popup_pinned {
                    popup::hide(self);
                }
            }
            WM_LBUTTONDOWN => {
                unsafe { SetCapture(self.hwnd) };
                self.drag = Some(Drag {
                    start: pt,
                    win_start: (self.pos.l, self.pos.t),
                    offset_start: self.s.dock_offset,
                    moved: false,
                });
            }
            WM_LBUTTONUP => {
                unsafe { ReleaseCapture() };
                if let Some(d) = self.drag.take() {
                    if d.moved {
                        self.save_settings();
                    } else if self.popup_pinned {
                        popup::hide(self);
                    } else {
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
                let _ = l;
                self.menu(pt.x, pt.y);
            }
            _ => {}
        }
    }

    fn shutdown(&mut self) {
        self.usage.save();
        self.tray.remove();
        unsafe {
            if !self.hook.is_null() {
                UnhookWinEvent(self.hook);
            }
        }
    }
}

const NIN_KEYSELECT_: u32 = NIN_SELECT | 1;
const ID_METRIC: usize = 100;
const ID_UNIT: usize = 200;
const ID_ADAPTER: usize = 300;
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
const ID_REPORT: usize = 700;
const ID_TASKMGR: usize = 701;
const ID_AUTOSTART: usize = 702;
const ID_EDIT: usize = 703;
const ID_DATA: usize = 704;
const ID_EXIT: usize = 705;

const INTERVALS: [(&str, u32); 4] = [("0.5 s", 500), ("1 s", 1000), ("2 s", 2000), ("5 s", 5000)];
const TEXT_SIZES: [(&str, u32); 4] = [("Small", 90), ("Normal", 100), ("Large", 115), ("Extra large", 130)];
const BACKGROUNDS: [(&str, u8); 4] = [("None", 0), ("Subtle", 90), ("Strong", 180), ("Solid", 255)];

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(a) = app() else { return DefWindowProcW(hwnd, msg, w, l) };
    if msg == a.msg_taskbar_created && msg != 0 {
        // Explorer restarted: tray icon is gone and the taskbar moved/resized.
        a.tray.add();
        a.update_timer();
        a.refresh(true);
        return 0;
    }
    match msg {
        WM_TIMER => {
            let sampled = w == T_SAMPLE;
            if sampled {
                a.sample();
            }
            a.refresh(sampled);
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_MOUSEMOVE | WM_MOUSEHOVER | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK
        | WM_RBUTTONUP => {
            a.on_mouse(msg, l);
            0
        }
        WM_CAPTURECHANGED => {
            if let Some(d) = a.drag.take() {
                if d.moved {
                    a.save_settings();
                }
            }
            0
        }
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lParam), anchor point in wParam.
            let ev = loword(l as usize) as u32;
            let (x, y) = (get_x(w as isize), get_y(w as isize));
            match ev {
                WM_CONTEXTMENU | WM_RBUTTONUP => a.menu(x, y),
                NIN_SELECT | NIN_KEYSELECT_ | WM_LBUTTONUP => {
                    if a.popup_open() {
                        popup::hide(a);
                    } else {
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
            if !a.s.visible {
                a.s.visible = true;
                a.save_settings();
            }
            a.refresh(true);
            popup::show(a, true);
            0
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            a.refresh(true);
            0
        }
        WM_SETTINGCHANGE => {
            a.dark = effective_dark(a.s.theme);
            a.update_timer();
            a.refresh(true);
            0
        }
        WM_POWERBROADCAST => {
            match w as u32 {
                PBT_APMSUSPEND => a.usage.save(),
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
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => a.refresh(true),
                _ => {}
            }
            1
        }
        WM_QUERYENDSESSION => {
            a.usage.save();
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
    use super::*;
    #[test]
    fn packing() {
        use Metric::*;
        let c = pack(&[Cpu, Ram, DiskRead, Up, Down, TodayDown, TopProc], 2);
        assert_eq!(c, vec![vec![Cpu, Ram], vec![DiskRead], vec![Up, Down], vec![TodayDown, TopProc]]);
        let c = pack(&[Cpu, Gpu, Up, Down], 2);
        assert_eq!(c, vec![vec![Cpu, Gpu], vec![Up, Down]]);
    }
}
