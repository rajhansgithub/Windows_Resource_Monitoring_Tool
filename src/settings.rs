//! Settings: plain `key = value` file, hand-editable, hot-reloaded.

use crate::fmt::SpeedUnit;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Ram,
    Gpu,
    Vram,
    GpuTemp,
    DiskRead,
    DiskWrite,
    DiskActive,
    DiskFree,
    Up,
    Down,
    TodayUp,
    TodayDown,
    TopProc,
    Battery,
}

impl Metric {
    /// Same order as the enum (index = discriminant).
    pub const ALL: [Metric; 15] = [
        Metric::Cpu,
        Metric::Ram,
        Metric::Gpu,
        Metric::Vram,
        Metric::GpuTemp,
        Metric::DiskRead,
        Metric::DiskWrite,
        Metric::DiskActive,
        Metric::DiskFree,
        Metric::Up,
        Metric::Down,
        Metric::TodayUp,
        Metric::TodayDown,
        Metric::TopProc,
        Metric::Battery,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Metric::Cpu => "cpu",
            Metric::Ram => "ram",
            Metric::Gpu => "gpu",
            Metric::Vram => "vram",
            Metric::GpuTemp => "gpu_temp",
            Metric::DiskFree => "disk_free",
            Metric::DiskRead => "disk_read",
            Metric::DiskWrite => "disk_write",
            Metric::DiskActive => "disk_active",
            Metric::Up => "net_up",
            Metric::Down => "net_down",
            Metric::TodayUp => "today_up",
            Metric::TodayDown => "today_down",
            Metric::TopProc => "top_process",
            Metric::Battery => "battery",
        }
    }
    pub fn menu_label(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU",
            Metric::Ram => "Memory",
            Metric::Gpu => "GPU",
            Metric::Vram => "GPU memory",
            Metric::GpuTemp => "GPU temperature",
            Metric::DiskFree => "Disk space used (C:)",
            Metric::DiskRead => "Disk read",
            Metric::DiskWrite => "Disk write",
            Metric::DiskActive => "Disk active time",
            Metric::Up => "Upload speed",
            Metric::Down => "Download speed",
            Metric::TodayUp => "Uploaded today",
            Metric::TodayDown => "Downloaded today",
            Metric::TopProc => "Top process",
            Metric::Battery => "Battery",
        }
    }
    pub fn default_on(self) -> bool {
        matches!(
            self,
            Metric::Cpu | Metric::Ram | Metric::Gpu | Metric::Up | Metric::Down | Metric::TodayDown
        )
    }
    pub fn idx(self) -> usize {
        self as usize
    }
    pub fn from_key(k: &str) -> Option<Metric> {
        Metric::ALL.iter().copied().find(|m| m.key().eq_ignore_ascii_case(k.trim()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Auto,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Docked next to the system tray, follows the taskbar.
    Dock,
    /// Free-floating, draggable anywhere.
    Float,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub show: [bool; Metric::ALL.len()],
    pub visible: bool,
    pub sparklines: bool,
    pub labels: bool,
    /// Two (or three) metrics per column; off = one line, each metric its own column.
    pub stacked: bool,
    /// Per-display override of `stacked`, keyed by display ("DISPLAY2").
    pub stacked_on: Vec<(String, bool)>,
    /// Allow switching a one-line widget to stacked rows when it doesn't fit.
    pub auto_stack: bool,
    /// Check GitHub for a newer version once at startup and install it.
    pub auto_update: bool,
    /// Max widget width on the taskbar, % of taskbar width. 0 = auto (stay clear of app icons).
    pub max_width_pct: u32,
    pub unit: SpeedUnit,
    pub adapter: String,
    pub interval_ms: u32,
    pub mode: Mode,
    pub float_x: i32,
    pub float_y: i32,
    /// Dock mode: extra distance (px at 96 dpi) from the tray, set by dragging.
    pub dock_offset: i32,
    /// Same for secondary-monitor taskbars, keyed by display ("DISPLAY2").
    pub offsets: Vec<(String, i32)>,
    /// "primary", "all", or a comma list of displays ("DISPLAY1,DISPLAY3").
    pub displays: String,
    pub hide_fullscreen: bool,
    pub click_through: bool,
    pub text_scale: u32,
    pub theme: Theme,
    /// 0 = invisible, 255 = solid.
    pub bg_opacity: u8,
    pub warn_pct: f32,
    pub crit_pct: f32,
    /// Monthly data cap in GB, 0 = off.
    pub data_cap_gb: f64,
    /// Display order of metrics on the widget (all metrics, shown or not).
    pub order: Vec<Metric>,
    /// Widget font face; empty = Segoe UI Variable / Segoe UI.
    pub font: String,
    /// Per-metric colour overrides (0xRRGGBB).
    pub colors: Vec<(Metric, u32)>,
    /// Use the Windows accent colour for every metric.
    pub windows_accent: bool,
    /// Alerts (tray notifications). 0 disables a rule.
    pub alerts: bool,
    pub alert_cpu_pct: f32,
    pub alert_ram_pct: f32,
    /// Seconds a CPU/RAM level must last before alerting.
    pub alert_secs: u32,
    /// Alert when the system drive has less than this % free.
    pub alert_disk_free_pct: f32,
    /// Alert when the battery is at or below this % and not charging.
    pub alert_battery_pct: u32,
    /// Alert at 90% and 100% of the monthly data cap.
    pub alert_cap: bool,
    /// Day of month the billing cycle resets (1..28).
    pub cap_reset_day: u32,
    pub first_run: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let mut show = [false; Metric::ALL.len()];
        for m in Metric::ALL {
            show[m.idx()] = m.default_on();
        }
        Settings {
            show,
            visible: true,
            sparklines: true,
            labels: true,
            stacked: true,
            stacked_on: Vec::new(),
            auto_stack: false,
            auto_update: true,
            max_width_pct: 0,
            unit: SpeedUnit::AutoBytes,
            adapter: String::new(),
            interval_ms: 1000,
            mode: Mode::Dock,
            float_x: 100,
            float_y: 100,
            dock_offset: 0,
            offsets: Vec::new(),
            displays: "primary".into(),
            hide_fullscreen: true,
            click_through: false,
            text_scale: 100,
            theme: Theme::Auto,
            bg_opacity: 0,
            warn_pct: 75.0,
            crit_pct: 90.0,
            data_cap_gb: 0.0,
            order: Metric::ALL.to_vec(),
            font: String::new(),
            colors: Vec::new(),
            windows_accent: false,
            alerts: true,
            alert_cpu_pct: 95.0,
            alert_ram_pct: 90.0,
            alert_secs: 60,
            alert_disk_free_pct: 10.0,
            alert_battery_pct: 15,
            alert_cap: true,
            cap_reset_day: 1,
            first_run: true,
        }
    }
}

/// UTF-8 (with or without BOM), UTF-16 LE (Notepad "Unicode"), or ANSI.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let w: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&w);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn b(v: bool) -> &'static str {
    if v {
        "1"
    } else {
        "0"
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        crate::util::data_dir().join("settings.ini")
    }

    pub fn shown(&self, m: Metric) -> bool {
        self.show[m.idx()]
    }

    /// Stacked rows on this display? Per-display choice, else the default.
    pub fn stacked_for(&self, display: &str) -> bool {
        self.stacked_on.iter().find(|(k, _)| k.eq_ignore_ascii_case(display)).map_or(self.stacked, |e| e.1)
    }

    pub fn set_stacked(&mut self, display: &str, on: bool) {
        match self.stacked_on.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(display)) {
            Some(e) => e.1 = on,
            None => self.stacked_on.push((display.to_string(), on)),
        }
    }

    pub fn offset(&self, display: &str, primary: bool) -> i32 {
        if primary {
            return self.dock_offset;
        }
        self.offsets.iter().find(|(k, _)| k.eq_ignore_ascii_case(display)).map_or(0, |(_, v)| *v)
    }

    pub fn set_offset(&mut self, display: &str, primary: bool, v: i32) {
        if primary {
            self.dock_offset = v;
        } else if let Some(e) = self.offsets.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(display)) {
            e.1 = v;
        } else {
            self.offsets.push((display.to_string(), v));
        }
    }

    /// Should the taskbar on `display` get a widget?
    pub fn wants_display(&self, display: &str, primary: bool) -> bool {
        match self.displays.to_ascii_lowercase().as_str() {
            "all" => true,
            "primary" => primary,
            list => list.split(',').any(|d| d.trim().eq_ignore_ascii_case(display)),
        }
    }

    pub fn load() -> Self {
        let mut s = Settings::default();
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => {
                s.first_run = false;
                s.apply(&decode(&bytes));
            }
            Err(_) if !path.exists() => s.import_legacy(),
            Err(_) => s.first_run = false,
        }
        s
    }

    /// Reload from disk on top of `self`; None if unreadable (e.g. mid-save).
    pub fn reload(&self) -> Option<Settings> {
        let bytes = std::fs::read(Self::path()).ok()?;
        let mut s = Settings {
            first_run: false,
            offsets: Vec::new(),
            stacked_on: Vec::new(),
            colors: Vec::new(),
            ..self.clone()
        };
        s.apply(&decode(&bytes));
        Some(s)
    }

    fn apply(&mut self, txt: &str) {
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            if let Some(mk) = k.strip_prefix("color.") {
                let hex = v.trim_start_matches('#');
                if let (Some(m), Ok(rgb)) = (Metric::from_key(mk), u32::from_str_radix(hex, 16)) {
                    self.colors.retain(|(x, _)| *x != m);
                    self.colors.push((m, rgb & 0xFF_FFFF));
                }
                continue;
            }
            if let Some(d) = k.strip_prefix("stacked.") {
                if let Some(on) = parse_bool(v) {
                    self.set_stacked(d, on);
                }
                continue;
            }
            if let Some(d) = k.strip_prefix("dock_offset.") {
                if let Ok(v) = v.parse::<i32>() {
                    self.set_offset(d, false, v.max(0));
                }
                continue;
            }
            if let Some(m) = k.strip_prefix("show.") {
                if let (Some(m), Some(on)) = (Metric::ALL.iter().find(|x| x.key() == m), parse_bool(v)) {
                    self.show[m.idx()] = on;
                }
                continue;
            }
            match k {
                "visible" => self.visible = parse_bool(v).unwrap_or(self.visible),
                "sparklines" => self.sparklines = parse_bool(v).unwrap_or(self.sparklines),
                "labels" => self.labels = parse_bool(v).unwrap_or(self.labels),
                "stacked" => self.stacked = parse_bool(v).unwrap_or(self.stacked),
                "auto_stack" => self.auto_stack = parse_bool(v).unwrap_or(self.auto_stack),
                "auto_update" => self.auto_update = parse_bool(v).unwrap_or(self.auto_update),
                "order" => {
                    let mut o: Vec<Metric> = v.split(',').filter_map(Metric::from_key).collect();
                    o.dedup();
                    // Metrics missing from the list (e.g. new ones) go at the end.
                    for m in Metric::ALL {
                        if !o.contains(&m) {
                            o.push(m);
                        }
                    }
                    self.order = o;
                }
                "font" => self.font = v.to_string(),
                "windows_accent" => self.windows_accent = parse_bool(v).unwrap_or(self.windows_accent),
                "alerts" => self.alerts = parse_bool(v).unwrap_or(self.alerts),
                "alert_cpu_pct" => self.alert_cpu_pct = v.parse().unwrap_or(self.alert_cpu_pct),
                "alert_ram_pct" => self.alert_ram_pct = v.parse().unwrap_or(self.alert_ram_pct),
                "alert_secs" => self.alert_secs = v.parse().map(|x: u32| x.clamp(5, 3600)).unwrap_or(self.alert_secs),
                "alert_disk_free_pct" => self.alert_disk_free_pct = v.parse().unwrap_or(self.alert_disk_free_pct),
                "alert_battery_pct" => self.alert_battery_pct = v.parse().unwrap_or(self.alert_battery_pct),
                "alert_cap" => self.alert_cap = parse_bool(v).unwrap_or(self.alert_cap),
                "max_width_pct" => {
                    self.max_width_pct = v.parse().map(|x: u32| x.min(90)).unwrap_or(self.max_width_pct)
                }
                "speed_unit" => self.unit = SpeedUnit::from_key(v).unwrap_or(self.unit),
                "adapter" => self.adapter = v.to_string(),
                "interval_ms" => {
                    self.interval_ms = v.parse().map(|x: u32| x.clamp(250, 10_000)).unwrap_or(self.interval_ms)
                }
                "mode" => {
                    self.mode = if v.eq_ignore_ascii_case("float") { Mode::Float } else { Mode::Dock }
                }
                "float_x" => self.float_x = v.parse().unwrap_or(self.float_x),
                "float_y" => self.float_y = v.parse().unwrap_or(self.float_y),
                "dock_offset" => self.dock_offset = v.parse().map(|x: i32| x.max(0)).unwrap_or(self.dock_offset),
                "displays" => self.displays = if v.is_empty() { "primary".into() } else { v.to_string() },
                "hide_fullscreen" => self.hide_fullscreen = parse_bool(v).unwrap_or(self.hide_fullscreen),
                "click_through" => self.click_through = parse_bool(v).unwrap_or(self.click_through),
                "text_scale" => {
                    self.text_scale = v.parse().map(|x: u32| x.clamp(70, 200)).unwrap_or(self.text_scale)
                }
                "theme" => {
                    self.theme = match v.to_ascii_lowercase().as_str() {
                        "dark" => Theme::Dark,
                        "light" => Theme::Light,
                        _ => Theme::Auto,
                    }
                }
                "bg_opacity" => self.bg_opacity = v.parse().unwrap_or(self.bg_opacity),
                "warn_pct" => self.warn_pct = v.parse().unwrap_or(self.warn_pct),
                "crit_pct" => self.crit_pct = v.parse().unwrap_or(self.crit_pct),
                "data_cap_gb" => self.data_cap_gb = v.parse::<f64>().map(|x| x.max(0.0)).unwrap_or(self.data_cap_gb),
                "cap_reset_day" => {
                    self.cap_reset_day = v.parse().map(|x: u32| x.clamp(1, 28)).unwrap_or(self.cap_reset_day)
                }
                _ => {}
            }
        }
    }

    /// Carry over choices from the old .NET version (settings.json) on first run.
    fn import_legacy(&mut self) {
        let Some(dir) = crate::util::legacy_dir() else { return };
        let Ok(txt) = std::fs::read_to_string(dir.join("settings.json")) else { return };
        let get = |key: &str| -> Option<String> {
            let i = txt.find(&format!("\"{}\"", key))?;
            let rest = &txt[i + key.len() + 2..];
            let rest = rest[rest.find(':')? + 1..].trim_start();
            let end = rest.find([',', '}', '\n']).unwrap_or(rest.len());
            Some(rest[..end].trim().trim_matches('"').to_string())
        };
        let flag = |k: &str| get(k).and_then(|v| parse_bool(&v));
        let mut set = |m: Metric, k: &str| {
            if let Some(v) = flag(k) {
                self.show[m.idx()] = v;
            }
        };
        set(Metric::Cpu, "ShowCpu");
        set(Metric::Gpu, "ShowGpu");
        set(Metric::Ram, "ShowRam");
        set(Metric::TopProc, "ShowTopProcess");
        if let Some(v) = flag("ShowDisk") {
            self.show[Metric::DiskRead.idx()] = v;
            self.show[Metric::DiskWrite.idx()] = v;
        }
        if let Some(v) = flag("ShowNetwork") {
            self.show[Metric::Up.idx()] = v;
            self.show[Metric::Down.idx()] = v;
        }
        if let Some(v) = flag("ShowUsage") {
            self.show[Metric::TodayUp.idx()] = v;
            self.show[Metric::TodayDown.idx()] = v;
        }
        if let Some(v) = flag("ShowSparklines") {
            self.sparklines = v;
        }
        // .NET enum serialised as number: Mbps=0, MBs=1, Kbps=2, KBs=3
        if let Some(v) = get("NetworkSpeedUnit") {
            self.unit = match v.as_str() {
                "0" | "Mbps" => SpeedUnit::Mbps,
                "1" | "MBs" => SpeedUnit::MBs,
                "2" | "Kbps" => SpeedUnit::Kbps,
                _ => SpeedUnit::AutoBytes,
            };
        }
    }

    pub fn save(&self) {
        let mut o = String::with_capacity(1024);
        o.push_str("# Resource Monitor settings. Edit freely; changes apply within a few seconds.\n");
        o.push_str("# Booleans: 1/0. Delete this file to reset everything.\n\n");
        o.push_str("# Metrics shown on the taskbar widget\n");
        for m in Metric::ALL {
            o.push_str(&format!("show.{} = {}\n", m.key(), b(self.shown(m))));
        }
        let theme = match self.theme {
            Theme::Auto => "auto",
            Theme::Dark => "dark",
            Theme::Light => "light",
        };
        let mode = match self.mode {
            Mode::Dock => "dock",
            Mode::Float => "float",
        };
        o.push_str(&format!(
            "\nvisible = {}\nsparklines = {}\nlabels = {}\n# 1 = metrics stacked in rows, 0 = one line, each metric its own column.\n\
             # Default for every display; per display: stacked.DISPLAY2 = 0\nstacked = {}\n\
             # 1 = a one-line widget may switch to stacked rows when it doesn't fit\nauto_stack = {}\n\
             # 1 = check GitHub for a new version once at startup and install it\nauto_update = {}\n\
             # widest the widget may get, % of taskbar width; 0 = auto (never cover app icons)\nmax_width_pct = {}\n\
             # auto | autobits | KB/s | MB/s | Kbps | Mbps\nspeed_unit = {}\n\
             # empty = all connected physical adapters; otherwise the adapter name (e.g. Wi-Fi)\nadapter = {}\n\
             interval_ms = {}\n\n\
             # dock = next to the tray, float = anywhere (drag it)\nmode = {}\nfloat_x = {}\nfloat_y = {}\ndock_offset = {}\n\
             # Taskbars that get a widget: primary | all | DISPLAY1,DISPLAY2,...\ndisplays = {}\n\
             hide_fullscreen = {}\nclick_through = {}\n\n\
             # percent, 70..200\ntext_scale = {}\n# auto | dark | light\ntheme = {}\n# 0 (invisible) .. 255 (solid)\nbg_opacity = {}\n\
             # value colours: amber at warn_pct, red at crit_pct\nwarn_pct = {}\ncrit_pct = {}\n\n\
             # Monthly data cap in GB (0 = off) and the day the billing cycle resets\ndata_cap_gb = {}\ncap_reset_day = {}\n",
            b(self.visible),
            b(self.sparklines),
            b(self.labels),
            b(self.stacked),
            b(self.auto_stack),
            b(self.auto_update),
            self.max_width_pct,
            self.unit.key(),
            self.adapter,
            self.interval_ms,
            mode,
            self.float_x,
            self.float_y,
            self.dock_offset,
            self.displays,
            b(self.hide_fullscreen),
            b(self.click_through),
            self.text_scale,
            theme,
            self.bg_opacity,
            self.warn_pct,
            self.crit_pct,
            self.data_cap_gb,
            self.cap_reset_day,
        ));
        let order: Vec<&str> = self.order.iter().map(|m| m.key()).collect();
        o.push_str(&format!(
            "\n# Widget order (comma list of metric names)\norder = {}\n\
             # Widget font; empty = Segoe UI Variable\nfont = {}\n\
             # 1 = colour every metric with the Windows accent colour\nwindows_accent = {}\n\
             # Per-metric colours: color.cpu = #4FC3F7\n",
            order.join(","),
            self.font,
            b(self.windows_accent)
        ));
        for (m, rgb) in &self.colors {
            o.push_str(&format!("color.{} = #{:06X}\n", m.key(), rgb));
        }
        o.push_str(&format!(
            "\n# Alerts (tray notifications). 0 turns a rule off.\nalerts = {}\nalert_cpu_pct = {}\nalert_ram_pct = {}\n\
             # seconds a CPU/RAM level must last\nalert_secs = {}\nalert_disk_free_pct = {}\nalert_battery_pct = {}\nalert_cap = {}\n\n",
            b(self.alerts),
            self.alert_cpu_pct,
            self.alert_ram_pct,
            self.alert_secs,
            self.alert_disk_free_pct,
            self.alert_battery_pct,
            b(self.alert_cap)
        ));
        for (k, v) in &self.stacked_on {
            o.push_str(&format!("stacked.{} = {}\n", k, b(*v)));
        }
        for (k, v) in &self.offsets {
            o.push_str(&format!("dock_offset.{} = {}\n", k, v));
        }
        let _ = crate::util::write_atomic(&Self::path(), o.as_bytes());
    }

    pub fn mtime() -> Option<SystemTime> {
        std::fs::metadata(Self::path()).and_then(|m| m.modified()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_parse() {
        let mut s = Settings::default();
        s.apply("show.cpu = 0\nspeed_unit = Mbps\nmode = float\ntext_scale = 999\n# x\nbogus\n");
        assert!(!s.shown(Metric::Cpu));
        assert_eq!(s.unit, SpeedUnit::Mbps);
        assert_eq!(s.mode, Mode::Float);
        assert_eq!(s.text_scale, 200);
        s.apply("displays = DISPLAY1, DISPLAY3\ndock_offset.DISPLAY3 = 40\n");
        assert!(s.wants_display("DISPLAY3", false));
        assert!(!s.wants_display("DISPLAY2", false));
        assert_eq!(s.offset("display3", false), 40);
        assert_eq!(s.offset("DISPLAY2", false), 0);
    }
}

/// Notify `hwnd` with `msg` whenever a file in the data folder is written.
/// Kernel change notification on a sleeping thread — no polling.
pub fn watch(hwnd: isize, msg: u32) {
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
    let dir = crate::util::wide(&crate::util::data_dir().display().to_string());
    let _ = std::thread::Builder::new().name("settings-watch".into()).stack_size(64 * 1024).spawn(move || unsafe {
        let h = FindFirstChangeNotificationW(dir.as_ptr(), 0, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME);
        if h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return;
        }
        loop {
            if WaitForSingleObject(h, INFINITE) != 0 {
                return;
            }
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd as _, msg, 0, 0);
            if FindNextChangeNotification(h) == 0 {
                return;
            }
        }
    });
}
