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
    DiskRead,
    DiskWrite,
    DiskActive,
    Up,
    Down,
    TodayUp,
    TodayDown,
    TopProc,
    Battery,
}

impl Metric {
    pub const ALL: [Metric; 13] = [
        Metric::Cpu,
        Metric::Ram,
        Metric::Gpu,
        Metric::Vram,
        Metric::DiskRead,
        Metric::DiskWrite,
        Metric::DiskActive,
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
    pub unit: SpeedUnit,
    pub adapter: String,
    pub interval_ms: u32,
    pub mode: Mode,
    pub float_x: i32,
    pub float_y: i32,
    /// Dock mode: extra distance (physical px at 96 dpi) from the tray, set by dragging.
    pub dock_offset: i32,
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
            unit: SpeedUnit::AutoBytes,
            adapter: String::new(),
            interval_ms: 1000,
            mode: Mode::Dock,
            float_x: 100,
            float_y: 100,
            dock_offset: 0,
            hide_fullscreen: true,
            click_through: false,
            text_scale: 100,
            theme: Theme::Auto,
            bg_opacity: 0,
            warn_pct: 75.0,
            crit_pct: 90.0,
            data_cap_gb: 0.0,
            cap_reset_day: 1,
            first_run: true,
        }
    }
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

    pub fn load() -> Self {
        let mut s = Settings::default();
        match std::fs::read_to_string(Self::path()) {
            Ok(txt) => {
                s.first_run = false;
                s.apply(&txt);
            }
            Err(_) => s.import_legacy(),
        }
        s
    }

    fn apply(&mut self, txt: &str) {
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
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
                "dock_offset" => self.dock_offset = v.parse().unwrap_or(self.dock_offset),
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
            "\nvisible = {}\nsparklines = {}\nlabels = {}\n\
             # auto | autobits | KB/s | MB/s | Kbps | Mbps\nspeed_unit = {}\n\
             # empty = all connected physical adapters; otherwise the adapter name (e.g. Wi-Fi)\nadapter = {}\n\
             interval_ms = {}\n\n\
             # dock = next to the tray, float = anywhere (drag it)\nmode = {}\nfloat_x = {}\nfloat_y = {}\ndock_offset = {}\n\
             hide_fullscreen = {}\nclick_through = {}\n\n\
             # percent, 70..200\ntext_scale = {}\n# auto | dark | light\ntheme = {}\n# 0 (invisible) .. 255 (solid)\nbg_opacity = {}\n\
             # value colours: amber at warn_pct, red at crit_pct\nwarn_pct = {}\ncrit_pct = {}\n\n\
             # Monthly data cap in GB (0 = off) and the day the billing cycle resets\ndata_cap_gb = {}\ncap_reset_day = {}\n",
            b(self.visible),
            b(self.sparklines),
            b(self.labels),
            self.unit.key(),
            self.adapter,
            self.interval_ms,
            mode,
            self.float_x,
            self.float_y,
            self.dock_offset,
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
    }
}
