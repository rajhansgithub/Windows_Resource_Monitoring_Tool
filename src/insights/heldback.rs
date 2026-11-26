//! Held Back: "Am I throttled?" The current CPU ceiling and the most likely reason.
//!
//! Inputs, all already paid for or pushed to us:
//!   * PDH counters on the shared sensor query (`Snapshot::cpu_perf_*`, `thermal_passive_limit`),
//!   * the effective power mode (pushed by `PowerRegisterForEffectivePowerModeNotifications`),
//!   * AC / battery from the snapshot and `Power::Ac`,
//!   * Kernel-Processor-Power event 37 ("speed limited by system firmware") from the Event Log
//!     listener,
//!   * the foreground app's Efficiency mode (EcoQoS), read every 10 s, never per tick,
//!   * the active power plan name, read at start, on power changes and at most once a minute
//!     when the panel is drawn.
//!
//! The decision logic (`reason`, `Debounce`, `Episodes`) is plain data so it is unit-tested.

use super::events::SysEvent;
use super::store::{num, obj};
use super::{Chart, Ctx, Item, Panel, Power, Tone, Visual, WM_APP_POWER_MODE};
use crate::achievements::json::Json;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

/// A ceiling below this, held for `CAP_HOLD_MS`, counts as "capped".
const CAP_BELOW: f32 = 95.0;
const CAP_HOLD_MS: u64 = 10_000;
/// Foreground app Efficiency-mode check interval.
const FG_EVERY_MS: u64 = 10_000;
/// One chart point per 10 s, one hour kept.
const CHART_EVERY_MS: u64 = 10_000;
const CHART_POINTS: usize = 360;
const EPISODE_KEEP_MS: u64 = 24 * 3600 * 1000;
const EPISODE_CAP: usize = 50;
/// A firmware-limit event this recent still explains a cap.
const FW_RECENT_MS: u64 = 5 * 60 * 1000;
/// Event 37 is logged once per logical processor; reports this close together are one episode.
const FW_MERGE_MS: u64 = 60_000;
const PLAN_REFRESH_MS: u64 = 60_000;

const FW_PROVIDER: &str = "Microsoft-Windows-Kernel-Processor-Power";
const FW_EVENT_ID: u32 = 37;
const FW_REASON: &str = "Firmware limit";

// EFFECTIVE_POWER_MODE values (windows-sys names in brackets).
const MODE_BATTERY_SAVER: i32 = 0; // EffectivePowerModeBatterySaver
const MODE_BEST_EFFICIENCY: i32 = 1; // EffectivePowerModeBetterBattery

/// Plain name for an EFFECTIVE_POWER_MODE value, as Settings shows it on Windows 11.
pub fn mode_name(m: i32) -> &'static str {
    mode_name_for(m, true)
}

/// Like `mode_name`, for a PC with or without a battery: without one, Windows calls
/// mode 0 "Energy saver".
pub fn mode_name_for(m: i32, battery: bool) -> &'static str {
    match m {
        0 if !battery => "Energy saver",
        0 => "Battery saver",
        1 => "Best power efficiency",
        2 => "Balanced",
        3 => "Better performance",
        4 => "Best performance",
        5 => "Game mode",
        6 => "Mixed reality",
        _ => "Unknown",
    }
}

/// Why the CPU is capped, most specific first.
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    Thermal,
    BatterySaver,
    /// Mode 0 on a PC without a battery.
    EnergySaver,
    EfficiencyOnBattery,
    EfficiencyOnAc,
    /// Foreground app (image stem) runs with EcoQoS.
    AppEco(String),
    /// On battery in some other mode (`None` = mode unknown).
    OnBattery(Option<i32>),
    System,
}

impl Reason {
    /// Sentence (no trailing period) for headlines and episode details.
    pub fn text(&self) -> String {
        match self {
            Reason::Thermal => "Firmware thermal limit".into(),
            Reason::BatterySaver => "Battery saver".into(),
            Reason::EnergySaver => "Energy saver is on".into(),
            Reason::EfficiencyOnBattery => "Best power efficiency mode on battery".into(),
            Reason::EfficiencyOnAc => "Power mode is set to best efficiency".into(),
            Reason::AppEco(app) => format!("{} is in Efficiency mode", app),
            Reason::OnBattery(Some(m)) => format!("{} on battery", mode_name(*m)),
            Reason::OnBattery(None) => "On battery".into(),
            Reason::System => "Limited by the system".into(),
        }
    }

    /// A few words for a one-line hint elsewhere ("Capped at 62% (heat)").
    #[allow(dead_code)] // used by `HeldBack::ceiling`
    pub fn short(&self) -> String {
        match self {
            Reason::Thermal => "heat".into(),
            Reason::BatterySaver => "battery saver".into(),
            Reason::EnergySaver => "energy saver".into(),
            Reason::EfficiencyOnBattery | Reason::EfficiencyOnAc => "efficiency mode".into(),
            Reason::AppEco(app) => format!("{} efficiency mode", app),
            Reason::OnBattery(_) => "on battery".into(),
            Reason::System => "system".into(),
        }
    }
}

/// Everything `reason` looks at.
#[derive(Clone, Debug, Default)]
pub struct Inputs<'a> {
    /// Lowest thermal-zone passive limit, percent.
    pub passive: Option<f32>,
    /// A firmware-limit event (37) arrived within `FW_RECENT_MS`.
    pub fw_recent: bool,
    /// This PC has a battery (a UPS that Windows reports as one counts).
    pub battery: bool,
    pub on_battery: bool,
    pub mode: Option<i32>,
    /// Foreground app name when it runs in Efficiency mode.
    pub fg_eco: Option<&'a str>,
}

/// Most likely reason for a cap, by priority: heat / firmware, battery saver, efficiency mode,
/// foreground app EcoQoS, generic battery, otherwise "the system".
///
/// `Performance Limit Flags` is deliberately not used to name a reason: its bits are not
/// documented well enough to tell thermal from power limits with confidence. A nonzero value
/// only tells us that some limit is active, which the ceiling already shows.
pub fn reason(i: &Inputs) -> Reason {
    if i.passive.is_some_and(|p| p < 100.0) || i.fw_recent {
        return Reason::Thermal;
    }
    match (i.on_battery, i.mode) {
        // Battery saver can stay on briefly after plugging in; still the cause.
        (_, Some(MODE_BATTERY_SAVER)) if i.battery => return Reason::BatterySaver,
        (_, Some(MODE_BATTERY_SAVER)) => return Reason::EnergySaver,
        (true, Some(MODE_BEST_EFFICIENCY)) => return Reason::EfficiencyOnBattery,
        (false, Some(MODE_BEST_EFFICIENCY)) => return Reason::EfficiencyOnAc,
        _ => {}
    }
    if let Some(app) = i.fg_eco {
        return Reason::AppEco(app.to_string());
    }
    if i.on_battery {
        return Reason::OnBattery(i.mode);
    }
    Reason::System
}

/// The effective ceiling: the lower of `% Performance Limit` and the thermal passive limit
/// (both are "percent of max performance allowed").
pub fn ceiling_of(limit: Option<f32>, passive: Option<f32>) -> Option<f32> {
    match (limit, passive) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// "Capped" only once the ceiling has stayed below `CAP_BELOW` for `CAP_HOLD_MS`.
#[derive(Default, Debug)]
pub struct Debounce {
    below_since: Option<u64>,
}

impl Debounce {
    /// Returns the time the current cap began while capped, `None` otherwise.
    pub fn step(&mut self, now_ms: u64, ceiling: Option<f32>) -> Option<u64> {
        match ceiling {
            Some(c) if c < CAP_BELOW => {
                let since = *self.below_since.get_or_insert(now_ms);
                (now_ms.saturating_sub(since) >= CAP_HOLD_MS).then_some(since)
            }
            _ => {
                self.below_since = None;
                None
            }
        }
    }
}

/// One stretch of throttling.
#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    pub start_ms: u64,
    pub dur_ms: u64,
    /// Lowest ceiling seen; `None` for firmware-event episodes (no counter value).
    pub min_ceiling: Option<f32>,
    pub reason: String,
}

/// Open episode plus the closed ones of the last 24 h (oldest first, at most `EPISODE_CAP`).
#[derive(Default, Debug)]
pub struct Episodes {
    pub open: Option<Episode>,
    pub done: VecDeque<Episode>,
}

impl Episodes {
    /// Advance with this tick's debounced state. `why` is only called when a reason is needed
    /// (opening an episode, or upgrading a generic one). Returns true when an episode closed.
    pub fn step(
        &mut self,
        now_ms: u64,
        capped_since: Option<u64>,
        ceiling: Option<f32>,
        why: impl Fn() -> Reason,
    ) -> bool {
        match (capped_since, &mut self.open) {
            (Some(_), Some(e)) => {
                e.dur_ms = now_ms.saturating_sub(e.start_ms);
                if let Some(c) = ceiling {
                    e.min_ceiling = Some(e.min_ceiling.map_or(c, |m| m.min(c)));
                }
                if e.reason == Reason::System.text() {
                    let r = why();
                    if r != Reason::System {
                        e.reason = r.text();
                    }
                }
                false
            }
            (Some(since), None) => {
                self.open = Some(Episode {
                    start_ms: since,
                    dur_ms: now_ms.saturating_sub(since),
                    min_ceiling: ceiling,
                    reason: why().text(),
                });
                false
            }
            (None, _) => match self.open.take() {
                Some(e) => {
                    self.push(e, now_ms);
                    true
                }
                None => false,
            },
        }
    }

    /// Add a closed episode and drop old / excess ones.
    pub fn push(&mut self, e: Episode, now_ms: u64) {
        self.done.push_back(e);
        self.prune(now_ms);
    }

    pub fn prune(&mut self, now_ms: u64) {
        let keep_from = now_ms.saturating_sub(EPISODE_KEEP_MS);
        self.done.retain(|e| e.start_ms + e.dur_ms >= keep_from);
        while self.done.len() > EPISODE_CAP {
            self.done.pop_front();
        }
    }

    /// Record a firmware-limit report (event 37), merging the per-processor burst.
    pub fn firmware(&mut self, t_ms: u64, secs: u32) {
        let dur_ms = u64::from(secs) * 1000;
        if let Some(e) = self.done.iter_mut().rev().find(|e| e.reason == FW_REASON) {
            let end = e.start_ms + e.dur_ms;
            if t_ms.abs_diff(end) <= FW_MERGE_MS {
                let start = e.start_ms.min(t_ms.saturating_sub(dur_ms));
                e.dur_ms = end.max(t_ms) - start;
                e.start_ms = start;
                return;
            }
        }
        let e = Episode {
            start_ms: t_ms.saturating_sub(dur_ms),
            dur_ms,
            min_ceiling: None,
            reason: FW_REASON.into(),
        };
        // Keep the list in time order (events can arrive slightly late).
        let at = self
            .done
            .iter()
            .position(|x| x.start_ms > e.start_ms)
            .unwrap_or(self.done.len());
        self.done.insert(at, e);
        self.prune(t_ms);
    }
}

/// Foreground app as of the last 10 s check.
#[derive(Clone, Debug)]
struct Foreground {
    name: String,
    /// `None` = Windows did not say (Windows 10 or access denied).
    eco: Option<bool>,
}

pub struct HeldBack {
    sampled: bool,
    limit: Option<f32>,
    perf: Option<f32>,
    flags: Option<u32>,
    passive: Option<f32>,
    /// A battery is present (from the snapshot; no extra polling).
    battery: bool,
    on_battery: bool,
    /// Total CPU use now, percent.
    cpu: f32,
    mode: Option<i32>,
    fg: Option<Foreground>,
    fg_last_ms: u64,
    fw_last_ms: Option<u64>,
    deb: Debounce,
    capped_since: Option<u64>,
    eps: Episodes,
    chart: VecDeque<f32>,
    chart_min: f32,
    chart_last_ms: u64,
    /// (plan name, read at unix ms). RefCell: `panel(&self)` refreshes it at most once a minute.
    plan: RefCell<(Option<String>, u64)>,
    /// Bumped whenever an episode closes or a firmware report lands (save trigger).
    changes: usize,
}

impl HeldBack {
    pub fn new() -> HeldBack {
        HeldBack {
            sampled: false,
            limit: None,
            perf: None,
            flags: None,
            passive: None,
            battery: false,
            on_battery: false,
            cpu: 0.0,
            mode: None,
            fg: None,
            fg_last_ms: 0,
            fw_last_ms: None,
            deb: Debounce::default(),
            capped_since: None,
            eps: Episodes::default(),
            chart: VecDeque::with_capacity(CHART_POINTS),
            chart_min: f32::INFINITY,
            chart_last_ms: 0,
            plan: RefCell::new((active_plan_name(), super::unix_ms())),
            changes: 0,
        }
    }

    pub fn on_sample(&mut self, c: &Ctx) {
        let s = c.snap;
        let now = c.now_ms;
        self.sampled = true;
        self.limit = s.cpu_perf_limit;
        self.perf = s.cpu_perf_pct;
        self.flags = s.cpu_perf_flags;
        self.passive = s.thermal_passive_limit;
        self.battery = s.battery.is_some();
        self.on_battery = s.battery.is_some_and(|b| !b.on_ac);
        self.cpu = s.cpu;
        if self.mode.is_none() {
            // The callback may have fired before Insights was switched on.
            let m = LAST_MODE.load(Ordering::Relaxed);
            if m >= 0 {
                self.mode = Some(m);
            }
        }
        if c.display_on
            && (now.saturating_sub(self.fg_last_ms) >= FG_EVERY_MS || now < self.fg_last_ms)
        {
            self.fg_last_ms = now;
            // Our own window in front (e.g. the main window) says nothing; keep the last app.
            if let Some(fg) = foreground_app() {
                self.fg = Some(fg);
            }
        }

        let ceiling = ceiling_of(self.limit, self.passive);
        self.capped_since = self.deb.step(now, ceiling);
        let inputs = Inputs {
            passive: self.passive,
            fw_recent: fw_recent(self.fw_last_ms, now),
            battery: self.battery,
            on_battery: self.on_battery,
            mode: self.mode,
            fg_eco: eco_name(&self.fg),
        };
        if self
            .eps
            .step(now, self.capped_since, ceiling, || reason(&inputs))
        {
            self.changes = self.changes.wrapping_add(1);
        }

        if let Some(c) = ceiling {
            self.chart_min = self.chart_min.min(c);
        }
        if now.saturating_sub(self.chart_last_ms) >= CHART_EVERY_MS || now < self.chart_last_ms {
            self.chart_last_ms = now;
            if self.chart_min.is_finite() {
                if self.chart.len() == CHART_POINTS {
                    self.chart.pop_front();
                }
                self.chart.push_back(self.chart_min);
            }
            self.chart_min = f32::INFINITY;
        }
    }

    fn inputs(&self, now_ms: u64) -> Inputs<'_> {
        Inputs {
            passive: self.passive,
            fw_recent: fw_recent(self.fw_last_ms, now_ms),
            battery: self.battery,
            on_battery: self.on_battery,
            mode: self.mode,
            fg_eco: eco_name(&self.fg),
        }
    }

    pub fn on_event(&mut self, e: &SysEvent) {
        if e.id != FW_EVENT_ID || !e.provider.eq_ignore_ascii_case(FW_PROVIDER) {
            return;
        }
        let t = if e.t_ms > 0 { e.t_ms } else { super::unix_ms() };
        self.fw_last_ms = Some(self.fw_last_ms.map_or(t, |x| x.max(t)));
        self.eps.firmware(t, fw_seconds(e));
        self.changes = self.changes.wrapping_add(1);
    }

    pub fn on_power(&mut self, p: Power) {
        match p {
            Power::Mode(m) => self.mode = Some(m),
            Power::Ac(on) => self.on_battery = !on && self.battery,
            _ => return,
        }
        *self.plan.borrow_mut() = (active_plan_name(), super::unix_ms());
    }

    /// Current cap and a few words of reason, while capped (for a one-line hint elsewhere).
    #[allow(dead_code)] // for the details panel hint, not wired yet
    pub fn ceiling(&self) -> Option<(f32, String)> {
        self.capped_since?;
        let c = ceiling_of(self.limit, self.passive)?;
        Some((c, reason(&self.inputs(super::unix_ms())).short()))
    }

    pub fn save(&self) -> Json {
        let eps = self
            .eps
            .done
            .iter()
            .chain(self.eps.open.iter())
            .map(|e| {
                obj(vec![
                    ("s", Json::Num(e.start_ms as f64)),
                    ("d", Json::Num(e.dur_ms as f64)),
                    (
                        "m",
                        e.min_ceiling
                            .map_or(Json::Null, |m| Json::Num(m.round() as f64)),
                    ),
                    ("r", Json::Str(e.reason.clone())),
                ])
            })
            .collect();
        obj(vec![("episodes", Json::Arr(eps))])
    }

    /// Changes to the finished episodes (the orchestrator saves when this changes). A counter,
    /// not the list length: once the list is at `EPISODE_CAP` its length stops changing.
    pub fn episode_count(&self) -> usize {
        self.changes
    }

    pub fn load(&mut self, j: &Json) {
        let Some(list) = j.get("episodes").and_then(Json::as_arr) else {
            return;
        };
        let mut done: Vec<Episode> = list
            .iter()
            .filter_map(|x| {
                let start_ms = num(x, "s")? as u64;
                let dur_ms = num(x, "d")? as u64;
                let min_ceiling = num(x, "m").map(|m| m.clamp(0.0, 100.0) as f32);
                let reason: String = x.get("r")?.as_str()?.chars().take(80).collect();
                Some(Episode {
                    start_ms,
                    dur_ms,
                    min_ceiling,
                    reason,
                })
            })
            .collect();
        done.sort_by_key(|e| e.start_ms);
        self.eps.done = done.into();
        self.eps.prune(super::unix_ms());
    }

    pub fn panel(&self, now_ms: u64) -> Panel {
        let mut p = Panel {
            key: "heldback",
            title: "Held Back",
            tone: Tone::Learning,
            headline: "Checking CPU limits.".into(),
            lines: Vec::new(),
            visual: None,
            chart: None,
            items: Vec::new(),
        };
        if !self.sampled {
            return p;
        }
        self.refresh_plan(now_ms);
        let plan = self.plan.borrow();
        let st = State {
            ceiling: ceiling_of(self.limit, self.passive),
            capped: self.capped_since.is_some(),
            reason: reason(&self.inputs(now_ms)),
            battery: self.battery,
            on_battery: self.on_battery,
            mode: self.mode,
            plan: plan.0.as_deref(),
            front: self.fg.as_ref().map(|f| (f.name.as_str(), f.eco)),
            perf: self.perf,
            passive: self.passive,
            flags: self.flags,
            cpu: self.cpu,
        };
        let (tone, headline, lines, visual) = describe(&st);
        (p.tone, p.headline, p.lines, p.visual) = (tone, headline, lines, Some(visual));
        if !self.chart.is_empty() {
            p.chart = Some(Chart {
                label: "CPU ceiling, last hour".into(),
                points: self.chart.iter().copied().collect(),
                max: 100.0,
                unit: "%",
                markers: Vec::new(),
                bars: false,
                // The full hour stays on the x axis while it fills up.
                slots: CHART_POINTS,
                x_labels: super::last_hour(),
                tones: Vec::new(),
            });
        }
        let keep_from = now_ms.saturating_sub(EPISODE_KEEP_MS);
        p.items = self
            .eps
            .open
            .iter()
            .chain(self.eps.done.iter().rev())
            .filter(|e| e.start_ms + e.dur_ms >= keep_from)
            .map(episode_item)
            .collect();
        p
    }

    /// Re-read the plan name when the cached one is over a minute old (panel builds only).
    fn refresh_plan(&self, now_ms: u64) {
        let stale = {
            let p = self.plan.borrow();
            now_ms.saturating_sub(p.1) >= PLAN_REFRESH_MS || now_ms < p.1
        };
        if stale {
            *self.plan.borrow_mut() = (active_plan_name(), now_ms);
        }
    }
}

/// Everything the panel text and gauge depend on (plain data, unit-tested).
#[derive(Clone, Debug)]
pub struct State<'a> {
    /// Percent of max speed allowed; None = this PC does not report it.
    pub ceiling: Option<f32>,
    /// Debounced: below `CAP_BELOW` for `CAP_HOLD_MS`.
    pub capped: bool,
    pub reason: Reason,
    /// A battery is present. Without one, nothing about batteries is said.
    pub battery: bool,
    pub on_battery: bool,
    pub mode: Option<i32>,
    pub plan: Option<&'a str>,
    /// Foreground app and its Efficiency mode (None = Windows did not say).
    pub front: Option<(&'a str, Option<bool>)>,
    pub perf: Option<f32>,
    pub passive: Option<f32>,
    pub flags: Option<u32>,
    /// Total CPU use, percent.
    pub cpu: f32,
}

/// Tone, headline, facts and gauge for the panel.
pub fn describe(s: &State) -> (Tone, String, Vec<String>, Visual) {
    let eco_app = s.front.filter(|f| f.1 == Some(true)).map(|f| f.0);
    let (tone, headline) = match s.ceiling {
        None => (
            Tone::Unavailable,
            "This PC does not report CPU limits.".to_string(),
        ),
        Some(c) if s.capped => {
            let pct = c.round() as i32;
            let tone = if s.reason == Reason::Thermal {
                Tone::Bad
            } else {
                Tone::Warn
            };
            let h = match &s.reason {
                Reason::OnBattery(Some(m)) => {
                    format!("{} on battery. Capped at {}%.", mode_name(*m), pct)
                }
                r => format!("Capped at {}%. {}.", pct, r.text()),
            };
            (tone, h)
        }
        Some(_) => match eco_app {
            Some(app) => (
                Tone::Info,
                format!("{} is in Efficiency mode. It may feel slow.", app),
            ),
            None => (
                Tone::Good,
                "Full power. Nothing is holding this PC back.".to_string(),
            ),
        },
    };

    let mut lines = Vec::new();
    let mut first = match s.mode {
        Some(m) => format!("Power mode: {}.", mode_name_for(m, s.battery)),
        None => "Power mode: unknown.".into(),
    };
    if s.battery {
        first += if s.on_battery {
            " On battery."
        } else {
            " Plugged in."
        };
    }
    if let Some((name, eco)) = s.front {
        first += &match eco {
            Some(true) => format!(" In front: {}, in Efficiency mode.", name),
            _ => format!(" In front: {}.", name),
        };
    }
    lines.push(first);
    if let Some(name) = s.plan {
        lines.push(format!("Power plan: {}.", name));
    }
    let mut speed = String::new();
    if let Some(sp) = s.perf {
        speed = format!("Current speed {}% of normal.", sp.round() as i32);
    }
    if let Some(t) = s.passive {
        if !speed.is_empty() {
            speed.push(' ');
        }
        speed += &format!("Thermal limit {}%.", t.round() as i32);
    }
    if !speed.is_empty() {
        lines.push(speed);
    }
    if s.flags.is_some_and(|f| f != 0) && s.ceiling.is_some_and(|c| c >= CAP_BELOW) {
        lines.push("Windows reports a speed limit is active.".into());
    }

    let reason = match s.ceiling {
        Some(_) if s.capped => s.reason.text(),
        _ => "none".into(),
    };
    let gauge = Visual::Gauge {
        ceiling: s.ceiling.map(|c| c.clamp(0.0, 100.0)),
        used: s.cpu.clamp(0.0, 100.0),
        reason,
    };
    (tone, headline, lines, gauge)
}

fn fw_recent(fw_last_ms: Option<u64>, now_ms: u64) -> bool {
    fw_last_ms.is_some_and(|t| now_ms.saturating_sub(t) < FW_RECENT_MS)
}

/// Foreground app name when it is known to run in Efficiency mode.
fn eco_name(fg: &Option<Foreground>) -> Option<&str> {
    fg.as_ref()
        .filter(|f| f.eco == Some(true))
        .map(|f| f.name.as_str())
}

fn episode_item(e: &Episode) -> Item {
    let title = match e.min_ceiling {
        Some(c) => format!(
            "Capped at {}% for {}.",
            c.round() as i32,
            dur_text(e.dur_ms)
        ),
        None => format!("Firmware limit for {}.", dur_text(e.dur_ms)),
    };
    let tone = if e.min_ceiling.is_none() || e.reason == Reason::Thermal.text() {
        Tone::Bad
    } else {
        Tone::Warn
    };
    Item {
        t_ms: e.start_ms,
        tone,
        title,
        detail: format!("{}.", e.reason),
    }
}

/// "40 s", "3 min", "2 h 5 min".
pub fn dur_text(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{} s", s)
    } else if s < 3600 {
        format!("{} min", (s + 30) / 60)
    } else {
        let m = (s + 30) / 60;
        format!("{} h {} min", m / 60, m % 60)
    }
}

/// Seconds from an event 37 record. The field names are not documented; the message is
/// "processor %2 in group %1 ... for %3 seconds", so prefer a field named like seconds,
/// else the third value.
fn fw_seconds(e: &SysEvent) -> u32 {
    let named = e.data.iter().find(|(k, _)| {
        let k = k.to_ascii_lowercase();
        k.contains("sec") || k.contains("duration")
    });
    named
        .or_else(|| e.data.get(2))
        .and_then(|(_, v)| v.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(86_400)
}

// ---- Win32 -------------------------------------------------------------------------------

/// Registration handle; lives for the process lifetime (never unregistered).
static MODE_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
/// Last mode the callback saw (-1 = none yet).
static LAST_MODE: AtomicI32 = AtomicI32::new(-1);

type RegisterFn = unsafe extern "system" fn(
    version: u32,
    callback: windows_sys::Win32::System::Power::EFFECTIVE_POWER_MODE_CALLBACK,
    context: *const c_void,
    handle: *mut *mut c_void,
) -> i32;

/// Called by Windows on a worker thread (once right after registering, then on each change).
unsafe extern "system" fn on_power_mode(mode: i32, context: *const c_void) {
    LAST_MODE.store(mode, Ordering::Relaxed);
    // SAFETY: `context` is the main window handle given at registration; PostMessageW is
    // thread-safe and harmless if the window is gone.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
            context as _,
            WM_APP_POWER_MODE,
            mode as u32 as usize,
            0,
        );
    }
}

/// Register for effective power mode changes (PowerRegisterForEffectivePowerModeNotifications);
/// the callback posts `WM_APP_POWER_MODE` to `hwnd`. Looked up at run time because the export
/// only exists on Windows 10 1809+; older builds simply get no power mode.
pub fn register_power_mode(hwnd: isize) {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    use windows_sys::Win32::System::Power::{EFFECTIVE_POWER_MODE_V1, EFFECTIVE_POWER_MODE_V2};
    if !MODE_HANDLE.load(Ordering::Relaxed).is_null() {
        return;
    }
    let dll: Vec<u16> = "powrprof.dll\0".encode_utf16().collect();
    // SAFETY: NUL-terminated strings; the module stays loaded for the process lifetime; the
    // transmuted pointer has the documented signature of the export.
    unsafe {
        let m = LoadLibraryExW(dll.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        if m.is_null() {
            return;
        }
        let Some(f) = GetProcAddress(
            m,
            b"PowerRegisterForEffectivePowerModeNotifications\0".as_ptr(),
        ) else {
            return;
        };
        let register: RegisterFn = std::mem::transmute(f);
        // V2 adds game mode / mixed reality; fall back to V1 on builds that reject it.
        for v in [EFFECTIVE_POWER_MODE_V2, EFFECTIVE_POWER_MODE_V1] {
            let mut h: *mut c_void = null_mut();
            if register(v, Some(on_power_mode), hwnd as *const c_void, &mut h) >= 0 && !h.is_null()
            {
                MODE_HANDLE.store(h, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// Friendly name of the active power plan ("Balanced"). Startup / transitions / once a minute.
fn active_plan_name() -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::System::Power::{PowerGetActiveScheme, PowerReadFriendlyName};
    let mut guid: *mut windows_sys::core::GUID = null_mut();
    // SAFETY: valid out pointer; the GUID is freed with LocalFree below as documented.
    if unsafe { PowerGetActiveScheme(null_mut(), &mut guid) } != 0 || guid.is_null() {
        return None;
    }
    let mut buf = [0u16; 128];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `guid` is valid; `buf` is writable for `size` bytes.
    let r = unsafe {
        PowerReadFriendlyName(
            null_mut(),
            guid,
            null(),
            null(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    // SAFETY: allocated by PowerGetActiveScheme, freed exactly once.
    unsafe { LocalFree(guid as _) };
    if r != 0 {
        return None;
    }
    let n = (size as usize / 2).min(buf.len());
    let end = buf[..n].iter().position(|&c| c == 0).unwrap_or(n);
    let s = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Foreground app name and whether Windows runs it in Efficiency mode (EcoQoS).
/// `None` when there is no foreground window, it is ours, or the process cannot be opened.
fn foreground_app() -> Option<Foreground> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetProcessInformation, OpenProcess, ProcessPowerThrottling,
        QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    // SAFETY: plain calls with valid out pointers; the process handle is closed before returning.
    unsafe {
        let w = GetForegroundWindow();
        if w.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, &mut pid);
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut st = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        };
        let ok = GetProcessInformation(
            h,
            ProcessPowerThrottling,
            (&mut st as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0;
        let exec = PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
        let eco = ok.then_some(st.ControlMask & exec != 0 && st.StateMask & exec != 0);
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let named =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        if !named {
            return None;
        }
        let full = String::from_utf16_lossy(&path[..(len as usize).min(path.len())]);
        let name = std::path::Path::new(&full)
            .file_stem()?
            .to_string_lossy()
            .into_owned();
        Some(Foreground { name, eco })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs<'static> {
        Inputs {
            passive: None,
            fw_recent: false,
            battery: true,
            on_battery: false,
            mode: Some(2),
            fg_eco: None,
        }
    }

    /// A desktop: no battery, nothing limiting, 6% CPU.
    fn desktop() -> State<'static> {
        State {
            ceiling: Some(100.0),
            capped: false,
            reason: Reason::System,
            battery: false,
            on_battery: false,
            mode: Some(2),
            plan: Some("Balanced"),
            front: Some(("Code", Some(false))),
            perf: Some(64.0),
            passive: None,
            flags: Some(0),
            cpu: 6.0,
        }
    }

    fn no_battery_words(lines: &[String], headline: &str) {
        for l in lines.iter().map(String::as_str).chain([headline]) {
            let l = l.to_ascii_lowercase();
            assert!(!l.contains("battery") && !l.contains("plugged"), "{}", l);
        }
    }

    #[test]
    fn desktop_without_battery_runs_free() {
        let (tone, head, lines, vis) = describe(&desktop());
        assert_eq!(tone, Tone::Good);
        assert_eq!(head, "Full power. Nothing is holding this PC back.");
        assert_eq!(
            vis,
            Visual::Gauge {
                ceiling: Some(100.0),
                used: 6.0,
                reason: "none".into()
            }
        );
        assert_eq!(lines[0], "Power mode: Balanced. In front: Code.");
        no_battery_words(&lines, &head);
        // Energy saver on a desktop is named as such, never "battery saver".
        let i = Inputs {
            battery: false,
            mode: Some(0),
            ..base()
        };
        assert_eq!(reason(&i), Reason::EnergySaver);
        let st = State {
            ceiling: Some(70.0),
            capped: true,
            reason: reason(&i),
            mode: Some(0),
            ..desktop()
        };
        let (tone, head, lines, vis) = describe(&st);
        assert_eq!(tone, Tone::Warn);
        assert_eq!(head, "Capped at 70%. Energy saver is on.");
        assert_eq!(lines[0], "Power mode: Energy saver. In front: Code.");
        no_battery_words(&lines, &head);
        assert_eq!(
            vis,
            Visual::Gauge {
                ceiling: Some(70.0),
                used: 6.0,
                reason: "Energy saver is on".into()
            }
        );
    }

    #[test]
    fn limits_not_reported_still_show_cpu_use() {
        let st = State {
            ceiling: None,
            perf: None,
            ..desktop()
        };
        let (tone, head, lines, vis) = describe(&st);
        assert_eq!(tone, Tone::Unavailable);
        assert_eq!(head, "This PC does not report CPU limits.");
        assert_eq!(
            vis,
            Visual::Gauge {
                ceiling: None,
                used: 6.0,
                reason: "none".into()
            }
        );
        no_battery_words(&lines, &head);
    }

    #[test]
    fn laptop_wording() {
        let st = State {
            battery: true,
            on_battery: true,
            ..desktop()
        };
        let (tone, head, lines, _) = describe(&st);
        assert_eq!(tone, Tone::Good);
        assert_eq!(head, "Full power. Nothing is holding this PC back.");
        assert_eq!(
            lines[0],
            "Power mode: Balanced. On battery. In front: Code."
        );
        let st = State {
            battery: true,
            ceiling: Some(62.0),
            capped: true,
            reason: Reason::Thermal,
            passive: Some(62.0),
            cpu: 140.0,
            ..desktop()
        };
        let (tone, head, lines, vis) = describe(&st);
        assert_eq!(tone, Tone::Bad);
        assert_eq!(head, "Capped at 62%. Firmware thermal limit.");
        assert_eq!(
            lines[0],
            "Power mode: Balanced. Plugged in. In front: Code."
        );
        assert!(lines
            .iter()
            .any(|l| l == "Current speed 64% of normal. Thermal limit 62%."));
        assert_eq!(
            vis,
            Visual::Gauge {
                ceiling: Some(62.0),
                used: 100.0,
                reason: "Firmware thermal limit".into()
            }
        );
        let st = State {
            front: Some(("Code", Some(true))),
            ..desktop()
        };
        let (tone, head, ..) = describe(&st);
        assert_eq!(
            (tone, head.as_str()),
            (Tone::Info, "Code is in Efficiency mode. It may feel slow.")
        );
    }

    #[test]
    fn reason_priority() {
        let all = Inputs {
            passive: Some(80.0),
            fw_recent: true,
            battery: true,
            on_battery: true,
            mode: Some(0),
            fg_eco: Some("x"),
        };
        assert_eq!(reason(&all), Reason::Thermal);
        assert_eq!(
            reason(&Inputs {
                passive: Some(100.0),
                fw_recent: true,
                ..base()
            }),
            Reason::Thermal
        );
        assert_eq!(
            reason(&Inputs {
                passive: Some(100.0),
                ..all.clone()
            }),
            Reason::Thermal
        ); // fw_recent
        let no_heat = Inputs {
            passive: Some(100.0),
            fw_recent: false,
            ..all
        };
        assert_eq!(reason(&no_heat), Reason::BatterySaver);
        assert_eq!(
            reason(&Inputs {
                mode: Some(1),
                ..no_heat.clone()
            }),
            Reason::EfficiencyOnBattery
        );
        assert_eq!(
            reason(&Inputs {
                mode: Some(1),
                on_battery: false,
                ..no_heat.clone()
            }),
            Reason::EfficiencyOnAc
        );
        assert_eq!(
            reason(&Inputs {
                mode: Some(2),
                on_battery: false,
                ..no_heat.clone()
            }),
            Reason::AppEco("x".into())
        );
        assert_eq!(
            reason(&Inputs {
                mode: Some(2),
                fg_eco: None,
                ..no_heat.clone()
            }),
            Reason::OnBattery(Some(2))
        );
        assert_eq!(reason(&base()), Reason::System);
        assert_eq!(
            Reason::AppEco("chrome".into()).text(),
            "chrome is in Efficiency mode"
        );
        assert_eq!(Reason::OnBattery(Some(2)).text(), "Balanced on battery");
    }

    #[test]
    fn ceiling_takes_lowest() {
        assert_eq!(ceiling_of(Some(80.0), Some(60.0)), Some(60.0));
        assert_eq!(ceiling_of(None, Some(60.0)), Some(60.0));
        assert_eq!(ceiling_of(Some(70.0), None), Some(70.0));
        assert_eq!(ceiling_of(None, None), None);
    }

    #[test]
    fn debounce_needs_ten_seconds_below_95() {
        let mut d = Debounce::default();
        assert_eq!(d.step(0, Some(90.0)), None);
        assert_eq!(d.step(9_000, Some(90.0)), None);
        assert_eq!(d.step(10_000, Some(80.0)), Some(0));
        // Back above the line resets.
        assert_eq!(d.step(11_000, Some(96.0)), None);
        assert_eq!(d.step(12_000, Some(90.0)), None);
        assert_eq!(d.step(21_000, Some(90.0)), None);
        assert_eq!(d.step(22_000, Some(90.0)), Some(12_000));
        // Unknown counter resets too.
        assert_eq!(d.step(23_000, None), None);
        assert_eq!(d.step(24_000, Some(95.0)), None);
    }

    #[test]
    fn episodes_open_update_close() {
        let mut e = Episodes::default();
        let mut d = Debounce::default();
        let mut t = 0;
        for c in [90.0, 80.0, 70.0, 62.0, 75.0] {
            let since = d.step(t, Some(c));
            assert!(!e.step(t, since, Some(c), || Reason::System));
            t += 5_000;
        }
        let open = e.open.clone().unwrap();
        assert_eq!(open.start_ms, 0);
        assert_eq!(open.min_ceiling, Some(62.0));
        assert_eq!(open.reason, "Limited by the system");
        // A specific reason replaces the generic one.
        let since = d.step(t, Some(70.0));
        e.step(t, since, Some(70.0), || Reason::Thermal);
        assert_eq!(e.open.as_ref().unwrap().reason, "Firmware thermal limit");
        t += 5_000;
        let since = d.step(t, Some(100.0));
        assert!(e.step(t, since, Some(100.0), || Reason::System));
        assert!(e.open.is_none());
        assert_eq!(e.done.len(), 1);
        assert_eq!(e.done[0].dur_ms, 25_000);
        assert_eq!(e.done[0].min_ceiling, Some(62.0));
    }

    #[test]
    fn episodes_capped_and_pruned() {
        let mut e = Episodes::default();
        for i in 0..60u64 {
            e.push(
                Episode {
                    start_ms: i * 1000,
                    dur_ms: 10,
                    min_ceiling: Some(50.0),
                    reason: "x".into(),
                },
                60_000,
            );
        }
        assert_eq!(e.done.len(), EPISODE_CAP);
        assert_eq!(e.done[0].start_ms, 10_000);
        e.prune(EPISODE_KEEP_MS + 30_000);
        assert!(e.done.iter().all(|x| x.start_ms + x.dur_ms >= 30_000));
    }

    #[test]
    fn firmware_burst_merges() {
        let mut e = Episodes::default();
        let t = 1_000_000_000;
        for k in 0..8 {
            e.firmware(t + k * 10, 30);
        }
        assert_eq!(e.done.len(), 1);
        assert_eq!(e.done[0].start_ms, t - 30_000);
        assert!(e.done[0].min_ceiling.is_none());
        e.firmware(t + 10 * 60_000, 5);
        assert_eq!(e.done.len(), 2);
    }

    #[test]
    fn fw_seconds_parsing() {
        let mut ev = SysEvent {
            provider: FW_PROVIDER.into(),
            id: 37,
            ..Default::default()
        };
        ev.data = vec![
            ("Group".into(), "0".into()),
            ("Number".into(), "3".into()),
            ("Seconds".into(), "41".into()),
        ];
        assert_eq!(fw_seconds(&ev), 41);
        ev.data = vec![
            ("0".into(), "0".into()),
            ("1".into(), "3".into()),
            ("2".into(), "17".into()),
        ];
        assert_eq!(fw_seconds(&ev), 17);
        ev.data.clear();
        assert_eq!(fw_seconds(&ev), 0);
    }

    #[test]
    fn durations() {
        assert_eq!(dur_text(40_000), "40 s");
        assert_eq!(dur_text(180_000), "3 min");
        assert_eq!(dur_text(7_500_000), "2 h 5 min");
    }
}
