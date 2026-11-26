//! Insights: five "why" features on top of the samples the monitor already takes.
//!
//! * Incident Cards  (`incidents`) — spikes you missed, written down with their cause.
//! * Sleep Verdict   (`sleep`)     — on wake: battery lost, share awake, top culprit.
//! * Held Back       (`heldback`)  — current CPU ceiling and why (thermal / power / plan).
//! * Before / After  (`baseline`)  — daily idle fingerprint, diffed across updates.
//! * Ghost Memory    (`ghost`)     — RAM no process owns, trended over days.
//!
//! Rules (same as the achievements observer): no admin, no driver, no timers of our own.
//! Everything is fed from `App::sample()` (the existing tick), window messages the app
//! already receives, and push-based Event Log subscriptions. Nothing on the per-tick path
//! may block, allocate heavily, or touch the disk; disk writes happen in `flush()`
//! (every few minutes, on suspend and on exit).
//!
//! CONTRACT between modules (keep signatures stable; each module owns its own file):
//!   - every feature has `fn panel(&self, now_ms: u64) -> Panel` for the Insights pages of the main window,
//!   - `fn save(&self) -> Json` / `fn load(&mut self, &Json)` for `insights.json`,
//!   - per-tick work goes through `on_sample(&Ctx)`; events through `on_event` / `on_power`.

pub mod baseline;
pub mod events;
pub mod ghost;
pub mod heldback;
pub mod incidents;
pub mod ring;
pub mod sleep;
pub mod store;
pub mod view;

use crate::achievements::json::Json;
use crate::sensors::Snapshot;
use std::collections::BTreeMap;
use windows_sys::core::GUID;
use windows_sys::Win32::UI::WindowsAndMessaging::WM_APP;

/// Posted to the main window by the Event Log subscription thread.
/// `lParam` = `Box::<events::SysEvent>::into_raw` (the receiver takes ownership).
pub const WM_APP_INSIGHT_EVENT: u32 = WM_APP + 20;
/// Posted by the effective-power-mode callback. `wParam` = EFFECTIVE_POWER_MODE value.
pub const WM_APP_POWER_MODE: u32 = WM_APP + 21;

/// How a panel / item should be coloured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Good,
    Info,
    Warn,
    Bad,
    /// Still collecting data (e.g. baseline needs more days).
    Learning,
    /// Hardware or Windows does not expose the data on this PC.
    Unavailable,
}

/// One row in a panel list (an incident card, a night, a change marker...).
#[derive(Clone, Debug)]
pub struct Item {
    /// Unix ms; 0 = no time shown.
    pub t_ms: u64,
    pub tone: Tone,
    pub title: String,
    pub detail: String,
}

/// A small line chart shown in a panel.
#[derive(Clone, Debug, Default)]
pub struct Chart {
    pub label: String,
    /// Oldest first.
    pub points: Vec<f32>,
    /// Y axis top (0 = auto from the data).
    pub max: f32,
    /// Unit appended to axis labels ("%", "GB", ...).
    pub unit: &'static str,
    /// Vertical markers: (index into `points`, short label), e.g. "Update".
    pub markers: Vec<(usize, String)>,
    /// Draw one bar per point (per-night / per-day values). False = a time series line.
    pub bars: bool,
    /// Fixed x-axis length for a time series with a fixed window (e.g. 60 minutes):
    /// `points` fill the newest (rightmost) slots, the rest stays empty. 0 = `points.len()`.
    /// Non-finite points are gaps.
    pub slots: usize,
    /// Labels under the x axis: left, middle, right ("-60 min", "-30 min", "now").
    /// Empty strings are not drawn.
    pub x_labels: [String; 3],
    /// Per-bar colours (bar charts only), one per point; empty = the accent colour.
    pub tones: Vec<Tone>,
}

/// Left / middle / right x-axis labels for a chart.
pub fn x_labels(l: &str, m: &str, r: &str) -> [String; 3] {
    [l.into(), m.into(), r.into()]
}

/// The x-axis labels of a chart covering the last hour.
pub fn last_hour() -> [String; 3] {
    x_labels("-60 min", "-30 min", "now")
}

/// The x-axis labels of a per-day chart from local day `first` to `last` ("today"
/// when `last` is the day of `now_ms`).
pub fn day_span(first: u32, last: u32, now_ms: u64) -> [String; 3] {
    let end = if last == time::day(now_ms) {
        "today".into()
    } else {
        time::day_label(last)
    };
    let start = if first == last {
        String::new()
    } else {
        time::day_label(first)
    };
    [start, String::new(), end]
}

/// Palette role of one part of a split bar (the view maps it to the theme).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Role {
    Accent,
    /// Accent leaning toward "good" (a second, related part).
    AccentAlt,
    Warn,
    /// Neutral grey (cache, other).
    Neutral,
    /// The empty track (free space).
    Track,
}

/// A picture shown under the facts, above the chart.
#[derive(Clone, Debug, PartialEq)]
pub enum Visual {
    /// One bar split into parts that add up to a whole, with a legend
    /// ("Apps 7.2 GB"). Parts are (label, value, colour); zero parts stay in the legend.
    Split {
        parts: Vec<(&'static str, f32, Role)>,
        unit: &'static str,
    },
    /// Ring gauge (open at the bottom): the ceiling (percent of max speed allowed; None = this PC
    /// does not report one), current use in percent and the reason for the ceiling
    /// ("none" when nothing limits it).
    Gauge {
        ceiling: Option<f32>,
        used: f32,
        reason: String,
    },
}

/// Everything an Insights page needs to draw one feature.
#[derive(Clone, Debug)]
pub struct Panel {
    /// Stable id: "incidents" | "sleep" | "heldback" | "baseline" | "ghost".
    pub key: &'static str,
    pub title: &'static str,
    pub tone: Tone,
    /// One sentence verdict, e.g. "Capped at 62%. Firmware thermal limit."
    pub headline: String,
    /// Short supporting facts, one per line.
    pub lines: Vec<String>,
    /// Drawn between the facts and the chart; None = nothing to show.
    pub visual: Option<Visual>,
    pub chart: Option<Chart>,
    /// Newest first.
    pub items: Vec<Item>,
}

/// A tray notification to show (subject to the user's toast setting).
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Panel key of the feature that produced it ("incidents", "sleep", ...), so a click
    /// on the notification can open that page.
    pub key: &'static str,
    pub title: String,
    pub text: String,
}

/// Per-tick input. Built in `App::sample()` from data already collected.
pub struct Ctx<'a> {
    /// Wall clock, unix ms.
    pub now_ms: u64,
    pub snap: &'a Snapshot,
    pub display_on: bool,
    /// Milliseconds since the last keyboard/mouse input (GetLastInputInfo).
    pub input_idle_ms: u64,
    /// Sample interval in effect right now (ms); `snap.elapsed` much larger = the tick was late
    /// (system froze) or the PC slept.
    pub interval_ms: u32,
    /// `snap.top_cpu` / `top_io` / `top_mem` are filled this tick.
    pub procs_sampled: bool,
}

/// Power transitions the app already receives (WM_POWERBROADCAST + power setting GUIDs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Power {
    /// PBT_APMSUSPEND (S3 / hibernate; usually not sent for Modern Standby).
    Suspend,
    /// PBT_APMRESUMEAUTOMATIC / PBT_APMRESUMESUSPEND.
    Resume,
    DisplayOff,
    DisplayOn,
    /// Display dimmed (GUID_CONSOLE_DISPLAY_STATE = 2).
    DisplayDim,
    LidClosed,
    LidOpen,
    /// Power source changed; true = on AC.
    Ac(bool),
    /// EFFECTIVE_POWER_MODE (0 battery saver .. 5 game mode, 6 mixed reality...).
    Mode(i32),
}

// Power setting GUIDs we register for (see `register_power`).
pub const GUID_CONSOLE_DISPLAY_STATE: GUID =
    GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
pub const GUID_LIDSWITCH_STATE_CHANGE: GUID =
    GUID::from_u128(0xba3e0f4d_b817_4094_a2d1_d56379e6a0f3);
pub const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// Translate a PBT_POWERSETTINGCHANGE payload into a `Power` event.
pub fn power_from_setting(g: &GUID, data: u32) -> Option<Power> {
    if guid_eq(g, &GUID_CONSOLE_DISPLAY_STATE) {
        Some(match data {
            0 => Power::DisplayOff,
            2 => Power::DisplayDim,
            _ => Power::DisplayOn,
        })
    } else if guid_eq(g, &GUID_LIDSWITCH_STATE_CHANGE) {
        Some(if data == 0 {
            Power::LidClosed
        } else {
            Power::LidOpen
        })
    } else if guid_eq(g, &GUID_ACDC_POWER_SOURCE) {
        // 0 = AC, 1 = DC (battery), 2 = short-term (UPS).
        Some(Power::Ac(data == 0))
    } else {
        None
    }
}

/// Extra power notifications on top of the display-state one the app already has.
/// Called once from `app::create`. Cheap: Windows pushes them; we never poll.
pub fn register_power(hwnd: isize) {
    use windows_sys::Win32::System::Power::RegisterPowerSettingNotification;
    use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;
    // SAFETY: valid window handle and static GUIDs; registrations live as long as the window.
    unsafe {
        for g in [&GUID_LIDSWITCH_STATE_CHANGE, &GUID_ACDC_POWER_SOURCE] {
            RegisterPowerSettingNotification(hwnd as _, g, DEVICE_NOTIFY_WINDOW_HANDLE);
        }
    }
    heldback::register_power_mode(hwnd);
}

/// Wall clock in unix ms.
pub fn unix_ms() -> u64 {
    crate::achievements::observer::unix_ms()
}

/// Milliseconds since the last user input (keyboard / mouse), session-wide.
pub fn input_idle_ms() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    // SAFETY: valid, sized out pointer.
    unsafe {
        let mut li = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut li) == 0 {
            return 0;
        }
        GetTickCount().wrapping_sub(li.dwTime) as u64
    }
}

/// How often `flush()` writes `insights.json` when something changed.
const SAVE_EVERY_MS: u64 = 5 * 60 * 1000;

pub struct Insights {
    pub ring: ring::Ring,
    pub incidents: incidents::Incidents,
    pub sleep: sleep::Sleep,
    pub held: heldback::HeldBack,
    pub baseline: baseline::Baseline,
    pub ghost: ghost::Ghost,
    events: Option<events::Listener>,
    last_save_ms: u64,
    held_eps: usize,
    dirty: bool,
    enabled: bool,
}

impl Insights {
    pub fn new() -> Insights {
        let mut s = Insights {
            ring: ring::Ring::new(),
            incidents: incidents::Incidents::new(),
            sleep: sleep::Sleep::new(),
            held: heldback::HeldBack::new(),
            baseline: baseline::Baseline::new(),
            ghost: ghost::Ghost::new(),
            events: None,
            last_save_ms: unix_ms(),
            held_eps: 0,
            dirty: false,
            enabled: false,
        };
        if let Some(j) = store::load() {
            s.load(&j);
        }
        s
    }

    /// Turn the whole feature set on/off (Overview → Insights). Off = no work at all,
    /// event subscriptions closed.
    pub fn set_enabled(&mut self, on: bool, hwnd: isize) {
        if on == self.enabled {
            return;
        }
        self.enabled = on;
        if on {
            self.events = events::Listener::start(hwnd);
        } else {
            self.events = None;
            self.sleep.abandon();
            self.flush();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The app should collect top-process lists this tick (an incident or an awake burst
    /// is in progress and needs a culprit). Keeps process enumeration off otherwise.
    pub fn wants_procs(&self) -> bool {
        self.enabled
            && (self.incidents.wants_procs()
                || self.sleep.wants_procs()
                || self.baseline.wants_procs())
    }

    /// Called once per sample, right after `Sensors::sample`.
    pub fn on_sample(&mut self, c: &Ctx) -> Vec<Notice> {
        if !self.enabled {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ring.push(c);
        out.extend(self.incidents.on_sample(c, &self.ring));
        out.extend(self.sleep.on_sample(c));
        self.held.on_sample(c);
        self.baseline.on_sample(c);
        self.ghost.on_sample(c);
        let held_eps = self.held.episode_count();
        if held_eps != self.held_eps {
            self.held_eps = held_eps;
            self.dirty = true;
        }
        if self.incidents.dirty()
            || self.sleep.dirty()
            || self.baseline.dirty()
            || self.ghost.dirty()
        {
            self.dirty = true;
        }
        if self.dirty && c.now_ms.saturating_sub(self.last_save_ms) >= SAVE_EVERY_MS {
            self.flush();
        }
        out
    }

    pub fn on_power(&mut self, p: Power, now_ms: u64) -> Vec<Notice> {
        if !self.enabled {
            return Vec::new();
        }
        self.held.on_power(p);
        let out = self.sleep.on_power(p, now_ms);
        if p == Power::Suspend {
            self.flush();
        }
        out
    }

    /// An Event Log record arrived from the subscription thread.
    pub fn on_event(&mut self, e: &events::SysEvent, now_ms: u64) -> Vec<Notice> {
        if !self.enabled {
            return Vec::new();
        }
        let mut out = Vec::new();
        out.extend(self.incidents.on_event(e, &self.ring, now_ms));
        self.sleep.on_event(e);
        self.held.on_event(e);
        self.baseline.on_event(e);
        self.dirty = true;
        out
    }

    /// Panels for the Insights pages, in display order.
    pub fn panels(&self, now_ms: u64) -> Vec<Panel> {
        vec![
            self.incidents.panel(now_ms),
            self.sleep.panel(now_ms),
            self.held.panel(now_ms),
            self.baseline.panel(now_ms),
            self.ghost.panel(now_ms),
        ]
    }

    /// Write `insights.json` (small, atomic). Called every few minutes when dirty,
    /// on suspend and on exit.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let _ = store::save(&self.save());
        // Sleep and Baseline keep plain flags that `save(&self)` cannot clear; without this
        // the file was rewritten every 5 minutes forever after their first change.
        self.sleep.mark_saved();
        self.baseline.mark_saved();
        self.dirty = false;
        self.last_save_ms = unix_ms();
    }

    /// Forget everything (Overview → Clear history).
    pub fn clear(&mut self) {
        self.incidents = incidents::Incidents::new();
        // Keep the user's Bag Guard setting (only re-applied on the next settings change).
        let bag_guard = self.sleep.bag_guard;
        self.sleep = sleep::Sleep::new();
        self.sleep.bag_guard = bag_guard;
        self.baseline = baseline::Baseline::new();
        self.ghost = ghost::Ghost::new();
        self.held = heldback::HeldBack::new();
        self.ring = ring::Ring::new();
        self.dirty = true;
        self.flush();
    }

    fn save(&self) -> Json {
        let mut m = BTreeMap::new();
        m.insert("v".into(), Json::Num(1.0));
        m.insert("incidents".into(), self.incidents.save());
        m.insert("sleep".into(), self.sleep.save());
        m.insert("baseline".into(), self.baseline.save());
        m.insert("ghost".into(), self.ghost.save());
        m.insert("held".into(), self.held.save());
        Json::Obj(m)
    }

    fn load(&mut self, j: &Json) {
        if let Some(x) = j.get("incidents") {
            self.incidents.load(x);
        }
        if let Some(x) = j.get("sleep") {
            self.sleep.load(x);
        }
        if let Some(x) = j.get("baseline") {
            self.baseline.load(x);
        }
        if let Some(x) = j.get("ghost") {
            self.ghost.load(x);
        }
        if let Some(x) = j.get("held") {
            self.held.load(x);
        }
    }
}

/// Local-time helpers shared by the feature modules (formatting card times, day keys).
pub mod time {
    /// "14:02" in local time for a unix-ms timestamp.
    pub fn hhmm(unix_ms: u64) -> String {
        let (_, _, _, h, m) = local(unix_ms);
        format!("{:02}:{:02}", h, m)
    }

    /// Local calendar day number (days since 1970-01-01 in local time) — the key for
    /// per-day rows. Same scheme everywhere so modules agree on "today".
    pub fn day(unix_ms: u64) -> u32 {
        let off = tz_offset_ms(unix_ms);
        ((unix_ms as i64 + off).max(0) / 86_400_000) as u32
    }

    /// (year, month 1-12, day 1-31, hour, minute) in local time.
    pub fn local(unix_ms: u64) -> (i32, u32, u32, u32, u32) {
        let t = (unix_ms as i64 + tz_offset_ms(unix_ms)).max(0) / 1000;
        let days = t / 86_400;
        let secs = t % 86_400;
        let (y, mo, d) = civil(days);
        (y, mo, d, (secs / 3600) as u32, ((secs % 3600) / 60) as u32)
    }

    /// "12 Nov" for a local day number from `day()`.
    pub fn day_label(day: u32) -> String {
        const M: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let (_, mo, d) = civil(day as i64);
        format!("{} {}", d, M[(mo - 1) as usize])
    }

    /// Days since epoch -> (y, m, d). Howard Hinnant's algorithm.
    fn civil(z: i64) -> (i32, u32, u32) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y } as i32, m, d)
    }

    #[cfg(windows)]
    fn tz_offset_ms(_unix_ms: u64) -> i64 {
        use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        // SAFETY: plain out struct.
        unsafe {
            let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
            let r = GetTimeZoneInformation(&mut tz);
            // 2 = TIME_ZONE_ID_DAYLIGHT
            let bias = tz.Bias
                + if r == 2 {
                    tz.DaylightBias
                } else {
                    tz.StandardBias
                };
            -(bias as i64) * 60_000
        }
    }

    #[cfg(not(windows))]
    fn tz_offset_ms(_unix_ms: u64) -> i64 {
        0
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn civil_dates() {
            assert_eq!(super::civil(0), (1970, 1, 1));
            assert_eq!(super::civil(20_417), (2025, 11, 25));
        }
    }
}
