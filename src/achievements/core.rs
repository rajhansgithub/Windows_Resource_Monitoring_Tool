//! Achievements core: catalog, scoring, state, `apply`, `merge`, `normalize`.
//!
//! Pure and deterministic: no clock, no files, no Win32. The observer feeds
//! `Event`s (with their own timestamps); `apply` returns the next `State` plus
//! the `Unlock`s to show. See docs/ACHIEVEMENTS.md for the rules.

use std::collections::BTreeMap;

use super::json::{self, Json};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const STATE_VERSION: u32 = 1;
/// Flat XP (hours, handled alerts, ended tasks, clean day) stops at this per day.
pub const FLAT_CAP: u32 = 40;
/// Active minutes that make a "day of real use".
pub const DAY_MINUTES: u32 = 30;
/// An alert counts as "handled" when it clears within this window.
pub const ALERT_WINDOW_MS: u64 = 30 * 60_000;
/// Marathon: 24 h of continuous active watching.
pub const MARATHON_MINUTES: u64 = 24 * 60;
/// Largest gap between two active minutes that still counts as continuous.
pub const MARATHON_GAP_MINUTES: u64 = 5;
/// Gone Dark: consecutive active minutes with no network traffic.
pub const GONE_DARK_MINUTES: u32 = 10;
/// Night Owl: active minutes between 02:00 and 03:59 (cumulative).
pub const NIGHT_OWL_MINUTES: u64 = 15;
/// Cool Customer: days of real use in a row without an alert.
pub const COOL_DAYS: u32 = 7;

pub const GB: u64 = 1_000_000_000;
pub const TB: u64 = 1_000 * GB;

pub const LADDER_XP: [u32; 7] = [25, 50, 100, 250, 500, 1000, 2000];
pub const RECORD_XP: [u32; 7] = [100, 200, 400, 800, 1500, 3000, 6000];
pub const TIER_NAMES: [&str; 7] = [
    "Bronze", "Silver", "Gold", "Platinum", "Diamond", "Ruby", "Emulsion",
];

const MAX_LEVEL: u32 = 999;

// Emulsion gate (FREE mode).
pub const GATE_LEVEL: u32 = 30;
pub const GATE_DAYS: u64 = 180;
pub const GATE_MONTHS: u64 = 6;
pub const GATE_RUBY_LADDERS: u64 = 3;
pub const GATE_ACTIVE_HOURS: u64 = 2000;
pub const GATE_TRAFFIC_GB: u64 = 5000; // 5 TB

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Ladder,
    Start,
    Mastery,
    Accident,
    Secret,
}

/// One badge. Ladders have 7 `tiers` thresholds (in `unit`s) and no `xp`;
/// everything else is a single with `xp` and no tiers.
#[derive(Clone, Copy, Debug)]
pub struct Badge {
    pub id: &'static str,
    pub name: &'static str,
    /// One sentence: what it is for.
    pub line: &'static str,
    pub category: Category,
    /// XP of a single (0 for ladders: they pay `LADDER_XP` per tier).
    pub xp: u32,
    /// Bronze..Emulsion thresholds in `unit` (all 0 for singles).
    pub tiers: [u64; 7],
    /// Ladders only: "hours", "GB", "alerts", ...
    pub unit: &'static str,
    /// Set (Certified Build plate) this ladder belongs to.
    #[allow(dead_code)]
    pub set: Option<&'static str>,
    /// Ladders only: counter name (see `counter`) and raw units per `unit`.
    counter: &'static str,
    per: u64,
}

impl Badge {
    pub fn is_ladder(&self) -> bool {
        self.category == Category::Ladder
    }
}

const fn ladder(
    id: &'static str,
    name: &'static str,
    line: &'static str,
    unit: &'static str,
    counter: &'static str,
    per: u64,
    tiers: [u64; 7],
    set: Option<&'static str>,
) -> Badge {
    Badge {
        id,
        name,
        line,
        category: Category::Ladder,
        xp: 0,
        tiers,
        unit,
        set,
        counter,
        per,
    }
}

const fn single(
    id: &'static str,
    name: &'static str,
    line: &'static str,
    category: Category,
    xp: u32,
) -> Badge {
    Badge {
        id,
        name,
        line,
        category,
        xp,
        tiers: [0; 7],
        unit: "",
        set: None,
        counter: "",
        per: 1,
    }
}

use Category::{Accident, Mastery, Secret, Start};

pub static BADGES: &[Badge] = &[
    // Ladders (13). Counters: see `counter()`.
    ladder(
        "watch",
        "Watchkeeper",
        "Hours you really watched your PC with the widget on your taskbar.",
        "hours",
        "min",
        60,
        [5, 50, 250, 1000, 2500, 6000, 12000],
        Some("rack_watch"),
    ),
    ladder(
        "heavy",
        "Heavy Lifting",
        "Hours your CPU ran at 80% or more while you were watching.",
        "hours",
        "cpu_min",
        60,
        [1, 10, 50, 200, 600, 1500, 3000],
        Some("rack_watch"),
    ),
    ladder(
        "render",
        "Render Farm",
        "Hours your GPU ran at 50% or more while you were watching.",
        "hours",
        "gpu_min",
        60,
        [1, 10, 50, 200, 600, 1500, 3000],
        Some("rack_watch"),
    ),
    ladder(
        "mover",
        "Data Mover",
        "Gigabytes read and written on your disks.",
        "GB",
        "disk_b",
        GB,
        [50, 500, 2000, 10000, 40000, 100000, 250000],
        Some("rack_watch"),
    ),
    ladder(
        "bandwidth",
        "Bandwidth",
        "Gigabytes of network traffic tracked.",
        "GB",
        "net_b",
        GB,
        [10, 100, 500, 2000, 8000, 20000, 50000],
        Some("rack_flow"),
    ),
    ladder(
        "down",
        "Downloader",
        "Gigabytes downloaded.",
        "GB",
        "down_b",
        GB,
        [10, 100, 400, 1500, 6000, 15000, 40000],
        Some("rack_flow"),
    ),
    ladder(
        "up",
        "Uploader",
        "Gigabytes uploaded.",
        "GB",
        "up_b",
        GB,
        [1, 10, 50, 250, 1000, 3000, 8000],
        Some("rack_flow"),
    ),
    ladder(
        "gameon",
        "Game On",
        "Hours of full-screen apps on your screen.",
        "hours",
        "game_min",
        60,
        [5, 50, 200, 600, 1500, 3500, 7000],
        Some("rack_flow"),
    ),
    ladder(
        "responder",
        "First Responder",
        "Alerts you handled: fired, then cleared within 30 minutes.",
        "alerts",
        "alerts",
        1,
        [1, 5, 25, 100, 300, 800, 2000],
        Some("rack_care"),
    ),
    ladder(
        "taskmaster",
        "Task Master",
        "Tasks you ended from the panel.",
        "tasks",
        "tasks",
        1,
        [1, 5, 25, 100, 300, 800, 2000],
        Some("rack_care"),
    ),
    ladder(
        "regular",
        "Regular",
        "Days of real use (30 active minutes or more).",
        "days",
        "days",
        1,
        [3, 15, 50, 150, 365, 730, 1460],
        Some("rack_care"),
    ),
    ladder(
        "ritual",
        "Weekly Ritual",
        "Weeks (Mon-Sun) with 3 or more days of real use.",
        "weeks",
        "weeks3",
        1,
        [2, 8, 26, 52, 104, 208, 416],
        Some("rack_care"),
    ),
    ladder(
        "stillhere",
        "Still Here",
        "Months with a day of real use.",
        "months",
        "months",
        1,
        [2, 6, 12, 24, 36, 60, 120],
        None,
    ),
    // Getting started (25 XP)
    single(
        "power_on",
        "Power On",
        "Watched your first full active hour.",
        Start,
        25,
    ),
    single(
        "second_screen",
        "Second Screen",
        "Had widgets on two or more taskbars for a full active hour.",
        Start,
        25,
    ),
    single(
        "up_to_date",
        "Up to Date",
        "An automatic update installed itself.",
        Start,
        25,
    ),
    single(
        "under_cap",
        "Under the Cap",
        "Finished a whole billing cycle under your data cap.",
        Start,
        25,
    ),
    single(
        "first_gb",
        "First Gigabyte",
        "Tracked your first gigabyte of traffic.",
        Start,
        25,
    ),
    // Mastery
    single(
        "marathon",
        "Marathon",
        "Watched for 24 hours of continuous active use.",
        Mastery,
        250,
    ),
    single(
        "cool_week",
        "Cool Customer",
        "Seven days of real use in a row with no alert.",
        Mastery,
        250,
    ),
    single(
        "tb_month",
        "Terabyte Club",
        "Tracked 1 TB of traffic in one calendar month.",
        Mastery,
        300,
    ),
    single(
        "completionist",
        "Completionist",
        "Earned every badge available in this build.",
        Mastery,
        500,
    ),
    // Happy accidents (10 XP)
    single(
        "running_hot",
        "Running Hot",
        "Your GPU hit 90 degrees C or more.",
        Accident,
        10,
    ),
    single(
        "red_line",
        "Red Line",
        "Memory use reached 98% or more.",
        Accident,
        10,
    ),
    single(
        "full_house",
        "Full House",
        "Your system drive was 98% full.",
        Accident,
        10,
    ),
    single(
        "gone_dark",
        "Gone Dark",
        "No network traffic for 10 active minutes.",
        Accident,
        10,
    ),
    single(
        "access_denied",
        "Access Denied",
        "Windows refused to end a task.",
        Accident,
        10,
    ),
    single(
        "fumes",
        "Running on Fumes",
        "Battery at 5% or less, unplugged.",
        Accident,
        10,
    ),
    // Secrets (50 XP, shown as "?" until earned)
    single(
        "night_owl",
        "Night Owl",
        "Real use between 2 and 4 in the morning.",
        Secret,
        50,
    ),
    single(
        "one_year",
        "One Year",
        "365 days since your first day of real use.",
        Secret,
        50,
    ),
    single(
        "spring_clean",
        "Spring Cleaning",
        "Your system drive use dropped 10 points within a day.",
        Secret,
        50,
    ),
    single(
        "five_nines",
        "Five Nines",
        "Windows ran for 30 days without a restart.",
        Secret,
        50,
    ),
];

/// Word for a set record in the UI ("Gold rack").
pub const RECORD_NOUN: &str = "rack";

/// A set plate (1U rack): four ladders, one record tier (the weakest piece).
#[derive(Clone, Copy, Debug)]
pub struct Set {
    pub id: &'static str,
    pub name: &'static str,
    pub members: [&'static str; 4],
}

pub static SETS: &[Set] = &[
    Set {
        id: "rack_watch",
        name: "Rack 01 · Watch",
        members: ["watch", "heavy", "render", "mover"],
    },
    Set {
        id: "rack_flow",
        name: "Rack 02 · Flow",
        members: ["bandwidth", "down", "up", "gameon"],
    },
    Set {
        id: "rack_care",
        name: "Rack 03 · Care",
        members: ["responder", "taskmaster", "regular", "ritual"],
    },
];

pub struct Catalog;

impl Catalog {
    pub fn all() -> &'static [Badge] {
        BADGES
    }
    pub fn get(id: &str) -> Option<&'static Badge> {
        BADGES.iter().find(|b| b.id == id)
    }
    pub fn ladders() -> impl Iterator<Item = &'static Badge> {
        BADGES.iter().filter(|b| b.is_ladder())
    }
    #[allow(dead_code)]
    pub fn sets() -> &'static [Set] {
        SETS
    }
    pub fn set(id: &str) -> Option<&'static Set> {
        SETS.iter().find(|s| s.id == id)
    }
}

/// Tier (0..=7) a ladder reaches at `count` (in the badge's `unit`).
/// Emulsion (7) only when `gate_open`; otherwise the ladder stops at Ruby (6).
pub fn tier_for(badge: &Badge, count: u64, gate_open: bool) -> u8 {
    if !badge.is_ladder() {
        return 0;
    }
    let cap = if gate_open { 7 } else { 6 };
    let reached = badge.tiers.iter().filter(|&&t| count >= t).count() as u8;
    reached.min(cap)
}

/// Threshold of the next tier above `tier` (None at Emulsion).
pub fn next_threshold(badge: &Badge, tier: u8) -> Option<u64> {
    badge.tiers.get(tier as usize).copied().filter(|&t| t > 0)
}

// ---------------------------------------------------------------------------
// Scoring (shared by every tool: never tune per tool)
// ---------------------------------------------------------------------------

/// XP needed to go from `level` to `level + 1`.
pub fn xp_to_next(level: u32) -> u64 {
    (60.0 + 40.0 * (level as f64).powf(1.25)).round() as u64
}

/// Level (starting at 1) for a total XP.
pub fn level_of(total_xp: u64) -> u32 {
    level_progress(total_xp).0
}

/// `(level, xp into this level, xp this level needs)`.
pub fn level_progress(total_xp: u64) -> (u32, u64, u64) {
    let mut level = 1;
    let mut rest = total_xp;
    loop {
        let need = xp_to_next(level);
        if rest < need || level >= MAX_LEVEL {
            return (level, rest, need);
        }
        rest -= need;
        level += 1;
    }
}

pub fn title_of(level: u32) -> &'static str {
    match level {
        0..=2 => "Rookie",
        3..=5 => "Tinkerer",
        6..=7 => "Builder",
        8..=11 => "Tuner",
        12..=15 => "Overclocker",
        16..=19 => "Engineer",
        _ => "Architect",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Bronze,
    Silver,
    Gold,
    Platinum,
    Diamond,
    Ruby,
    Emulsion,
}

impl Rank {
    pub fn name(self) -> &'static str {
        TIER_NAMES[self as usize]
    }
}

/// Bronze 1-4, Silver 5-9, Gold 10-14, Platinum 15-19, Diamond 20-29, Ruby 30-39,
/// Emulsion 40+ only when the gate is open (else Ruby).
pub fn rank_of(level: u32, gate_open: bool) -> Rank {
    match level {
        0..=4 => Rank::Bronze,
        5..=9 => Rank::Silver,
        10..=14 => Rank::Gold,
        15..=19 => Rank::Platinum,
        20..=29 => Rank::Diamond,
        30..=39 => Rank::Ruby,
        _ if gate_open => Rank::Emulsion,
        _ => Rank::Ruby,
    }
}

// ---------------------------------------------------------------------------
// Calendar helpers (no time zone logic: the observer supplies local time)
// ---------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Local wall-clock time of an event (the observer reads it from Windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    /// 0 = Monday .. 6 = Sunday.
    pub weekday: u8,
}

impl Default for LocalTime {
    fn default() -> Self {
        LocalTime {
            year: 2026,
            month: 1,
            day: 5,
            hour: 12,
            weekday: 0,
        }
    }
}

impl LocalTime {
    /// Days since 1970-01-01 of this local date.
    pub fn day_number(&self) -> i64 {
        days_from_civil(self.year as i64, self.month as i64, self.day as i64)
    }
    pub fn month_key(&self) -> u32 {
        self.year.max(0) as u32 * 100 + self.month as u32
    }
    /// Build from unix milliseconds plus a UTC offset (handy for tests and tools).
    pub fn from_unix_ms(ms: u64, tz_offset_min: i32) -> LocalTime {
        let local = ms as i64 / 1000 + tz_offset_min as i64 * 60;
        let days = local.div_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        LocalTime {
            year: y as i32,
            month: m as u8,
            day: d as u8,
            hour: (local.rem_euclid(86_400) / 3600) as u8,
            weekday: (days + 3).rem_euclid(7) as u8,
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Everything scoped to the current local day.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayState {
    /// Day number (days since 1970-01-01) of the last active minute; 0 = none yet.
    pub k: u32,
    /// Flat XP granted today (cap `FLAT_CAP`).
    pub flat: u32,
    /// Active minutes today.
    pub m: u32,
    /// An alert fired today.
    pub a: bool,
    /// Highest system-drive use today, in tenths of a percent (Spring Cleaning).
    pub dm: u32,
}

/// Current uninterrupted run of active minutes (Marathon, Gone Dark).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Run {
    /// Last active minute seen (unix minutes). Also the replay guard.
    pub l: u64,
    /// First minute of the current continuous run.
    pub s: u64,
    /// Consecutive active minutes with no network traffic.
    pub i: u32,
}

/// Aging of one medal (achievements plugin v4 `ag`): key "badgeId:tier" -> [startMs, dustLeft, webClearedMs].
/// The dust clock runs from `start` (when that tier was earned, or the last spotless polish); a rub only scales it
/// (`left`). The cobweb regrows from `web` (the last rub).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Age {
    pub start: u64,
    /// 0..=1: how much of the dust curve is still on it.
    pub left: f32,
    pub web: u64,
}
// `left` is always finite (normalize and from_json make sure), so equality is total.
impl Eq for Age {}

/// The age clock. Look = the owner's saved tuning (preview "Save for Claude" at 366 days, 2026-11-22), timed so that look
/// arrives at a year: dust = min(1, 2 * (1 - e^(-days/527))) is full at 365; the cobweb starts at day 20 and is full at 350.
const DUST_AMT: f32 = 2.0;
const DUST_DAYS: f32 = 527.0;
const WEB_START: f32 = 20.0;
const WEB_SPAN: f32 = 330.0;
/// One rub keeps 70% of the dust (a few rubs to get it spotless).
const RUB_KEEP: f32 = 0.7;

/// Dust on a medal left alone for `days`: min(1, 2(1 - e^(-days/527))), full at a year.
pub fn dust_curve(days: f32) -> f32 {
    (DUST_AMT * (1.0 - (-days.max(0.0) / DUST_DAYS).exp())).min(1.0)
}

/// The cobweb, `days` after it was last cleared: none before day 20, full at day 350.
pub fn web_curve(days: f32) -> f32 {
    if days < WEB_START {
        0.0
    } else {
        ((days - WEB_START) / WEB_SPAN).min(1.0).powf(0.8)
    }
}

/// How worn a medal looks now. Medals age with real time, 1 real hour = 1 day on the shelf: dust settles over months
/// (a grey layer over the whole medal at a year), cobwebs start after 20 days and keep growing for most of a year. Polishing: every rub clears the webs and
/// takes 30% of the dust. `scratch` stays 0 (scratches were dropped).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wear {
    pub dust: f32,
    pub scratch: f32,
    pub web: f32,
    /// Days (real hours) since the last rub, or since it was struck if never polished.
    pub days: f32,
    /// 0..=100.
    pub condition: u8,
    pub clean: bool,
    pub polishes: u32,
}

/// One "day" of wear in real time.
pub const WEAR_DAY_MS: u64 = 3_600_000;
/// Shine points for one rub, and the bonus when the medal is spotless.
pub const SHINE_RUB: u64 = 1;
pub const SHINE_SPOTLESS: u64 = 10;

/// The saved achievements. `to_json`/`from_json` keep it under 4 KB for normal use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub v: u32,
    /// Unlocked badges: id -> first earned (unix ms).
    pub u: BTreeMap<String, u64>,
    /// Ladder tier 1..=7.
    pub k: BTreeMap<String, u8>,
    /// Ladder: time each tier was earned (index 0 = Bronze).
    pub kd: BTreeMap<String, Vec<u64>>,
    /// Set id -> highest certified record tier.
    pub ps: BTreeMap<String, u8>,
    /// Kept XP (migrations).
    pub lx: u64,
    /// Counters (only real, checked work). See `counter`.
    pub c: BTreeMap<String, u64>,
    /// Flat XP total.
    pub f: u64,
    pub d: DayState,
    /// Month key (year*100+month) -> [days of real use, weeks with 3+ days].
    pub days: BTreeMap<u32, [u32; 2]>,
    /// Day number of the first day of real use (0 = none).
    pub first: u32,
    /// Current week: (day number of its Monday, bitmask of days of real use).
    pub w: (u32, u8),
    pub r: Run,
    /// Days of real use in a row without an alert (Cool Customer).
    pub cw: u32,
    /// Traffic this calendar month: (month key, bytes) (Terabyte Club).
    pub tm: (u32, u64),
    /// Alerts fired and not yet cleared: kind -> fired at (unix ms).
    pub al: BTreeMap<u8, u64>,
    /// Emulsion gate is open (FREE mode). Never closes again.
    pub g: bool,
    /// Aging per medal: "badgeId:tier" -> its clocks (see `Age`).
    pub ag: BTreeMap<String, Age>,
    /// Shine points from polishing.
    pub sh: u64,
}

impl Default for State {
    fn default() -> Self {
        State {
            v: STATE_VERSION,
            u: BTreeMap::new(),
            k: BTreeMap::new(),
            kd: BTreeMap::new(),
            ps: BTreeMap::new(),
            lx: 0,
            c: BTreeMap::new(),
            f: 0,
            d: DayState::default(),
            days: BTreeMap::new(),
            first: 0,
            w: (0, 0),
            r: Run::default(),
            cw: 0,
            tm: (0, 0),
            al: BTreeMap::new(),
            g: false,
            ag: BTreeMap::new(),
            sh: 0,
        }
    }
}

impl State {
    /// When the medal on show was struck: the date of the current ladder tier, else the first-earned date.
    fn struck_ms(&self, id: &str) -> u64 {
        self.kd
            .get(id)
            .and_then(|v| v.last().copied())
            .filter(|&t| t > 0)
            .or_else(|| self.u.get(id).copied())
            .unwrap_or(0)
    }

    /// The aging key of the medal on show: "badgeId:tier".
    fn age_key(&self, id: &str) -> String {
        format!("{id}:{}", self.tier(id))
    }

    /// The clocks of the medal on show: (start, dust left, web cleared). A new tier is a new medal: it starts clean.
    fn age_of(&self, id: &str) -> (u64, f32, u64) {
        let struck = self.struck_ms(id);
        match self.ag.get(&self.age_key(id)) {
            Some(a) if a.start >= struck => (a.start, a.left, a.web.max(a.start)),
            _ => (struck, 1.0, struck),
        }
    }

    /// How worn the medal `id` looks at `now_ms` (see `Wear`). The Emulsion bubble never ages.
    pub fn wear(&self, id: &str, now_ms: u64) -> Wear {
        if !self.earned(id) || self.tier(id) >= 7 {
            return Wear {
                condition: 100,
                clean: true,
                ..Wear::default()
            };
        }
        let (start, left, webc) = self.age_of(id);
        let day = |t: u64| now_ms.saturating_sub(t) as f32 / WEAR_DAY_MS as f32;
        let dust = left * dust_curve(day(start));
        let web = web_curve(day(webc));
        let polished = webc > start;
        let condition = (100.0 * (1.0 - (0.7 * dust + 0.3 * web)))
            .round()
            .clamp(0.0, 100.0) as u8;
        Wear {
            dust,
            scratch: 0.0,
            web,
            days: day(webc),
            condition,
            clean: dust < 0.02 && web < 0.01,
            polishes: polished as u32,
        }
    }

    /// One rub of the polishing cloth. Returns the new look and the shine points it earned (0 when already spotless).
    /// A rub clears the cobweb (it regrows from now) and keeps 70% of the dust. Spotless: the clocks start again, + bonus.
    pub fn polish(&mut self, id: &str, now_ms: u64) -> (Wear, u64) {
        let w = self.wear(id, now_ms);
        if !self.earned(id) || w.clean {
            return (w, 0);
        }
        let (start, left, _) = self.age_of(id);
        let left = left * RUB_KEEP;
        let spotless = left < 0.05 || w.dust * RUB_KEEP < 0.02;
        let a = if spotless {
            Age {
                start: now_ms,
                left: 1.0,
                web: now_ms,
            }
        } else {
            Age {
                start,
                left,
                web: now_ms,
            }
        };
        let key = self.age_key(id);
        self.ag.insert(key, a);
        let gain = SHINE_RUB + if spotless { SHINE_SPOTLESS } else { 0 };
        self.sh = self.sh.saturating_add(gain);
        (self.wear(id, now_ms), gain)
    }

    pub fn tier(&self, id: &str) -> u8 {
        self.k.get(id).copied().unwrap_or(0)
    }
    pub fn earned(&self, id: &str) -> bool {
        self.u.contains_key(id)
    }
    pub fn counter(&self, name: &str) -> u64 {
        counter(self, name)
    }
    fn bump(&mut self, name: &str, n: u64) {
        let e = self.c.entry(name.to_string()).or_insert(0);
        *e = e.saturating_add(n);
    }
    fn add_flat(&mut self, n: u32) {
        if self.d.k == 0 {
            return; // no day known yet: nothing to cap against
        }
        let g = n.min(FLAT_CAP.saturating_sub(self.d.flat));
        self.d.flat += g;
        self.f += g as u64;
    }
}

/// Counter value in raw units (minutes, bytes, counts). `days`, `weeks3` and
/// `months` are derived from the `days` map.
pub fn counter(s: &State, name: &str) -> u64 {
    match name {
        "days" => s.days.values().map(|v| v[0] as u64).sum(),
        "weeks3" => s.days.values().map(|v| v[1] as u64).sum(),
        "months" => s.days.values().filter(|v| v[0] > 0).count() as u64,
        _ => s.c.get(name).copied().unwrap_or(0),
    }
}

/// A ladder's count in its own unit (hours, GB, ...). 0 for singles.
pub fn count_of(s: &State, badge: &Badge) -> u64 {
    if !badge.is_ladder() {
        return 0;
    }
    counter(s, badge.counter) / badge.per.max(1)
}

/// Total XP: kept + flat + ladder tiers + records + singles.
pub fn total_xp(s: &State) -> u64 {
    let mut t = s.lx.saturating_add(s.f);
    for (id, &tier) in &s.k {
        if Catalog::get(id).map_or(false, |b| b.is_ladder()) {
            t += LADDER_XP[..tier.min(7) as usize]
                .iter()
                .map(|&x| x as u64)
                .sum::<u64>();
        }
    }
    for (id, &tier) in &s.ps {
        if Catalog::set(id).is_some() {
            t += RECORD_XP[..tier.min(7) as usize]
                .iter()
                .map(|&x| x as u64)
                .sum::<u64>();
        }
    }
    for id in s.u.keys() {
        if let Some(b) = Catalog::get(id) {
            if !b.is_ladder() {
                t += b.xp as u64;
            }
        }
    }
    t
}

pub fn level(s: &State) -> u32 {
    level_of(total_xp(s))
}

#[allow(dead_code)]
pub fn rank(s: &State) -> Rank {
    rank_of(level(s), s.g)
}

// ---------------------------------------------------------------------------
// Emulsion gate (FREE mode)
// ---------------------------------------------------------------------------

/// `(have, need)` for each gate condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateProgress {
    pub level: (u64, u64),
    pub days: (u64, u64),
    pub months: (u64, u64),
    pub ruby_ladders: (u64, u64),
    pub active_hours: (u64, u64),
    pub traffic_gb: (u64, u64),
    /// Already open (sticky).
    pub open: bool,
}

impl GateProgress {
    pub fn conditions(&self) -> [(u64, u64); 6] {
        [
            self.level,
            self.days,
            self.months,
            self.ruby_ladders,
            self.active_hours,
            self.traffic_gb,
        ]
    }
    /// Every condition holds right now.
    pub fn all_met(&self) -> bool {
        self.conditions().iter().all(|&(h, n)| h >= n)
    }
}

pub fn gate_progress(s: &State) -> GateProgress {
    let ruby = Catalog::ladders().filter(|b| s.tier(b.id) >= 6).count() as u64;
    GateProgress {
        level: (level(s) as u64, GATE_LEVEL as u64),
        days: (counter(s, "days"), GATE_DAYS),
        months: (counter(s, "months"), GATE_MONTHS),
        ruby_ladders: (ruby, GATE_RUBY_LADDERS),
        active_hours: (counter(s, "min") / 60, GATE_ACTIVE_HOURS),
        traffic_gb: (counter(s, "net_b") / GB, GATE_TRAFFIC_GB),
        open: s.g,
    }
}

// ---------------------------------------------------------------------------
// Events, context, unlocks
// ---------------------------------------------------------------------------

/// One sample minute in which the user was really there: the widget is shown on
/// at least one taskbar AND there was input in the last 5 minutes. The observer
/// sends it only for such minutes; `widget_taskbars == 0` is ignored anyway.
/// Byte fields are the bytes moved during that minute.
#[derive(Clone, Debug)]
pub struct Minute {
    /// Taskbars currently showing the widget (>= 1 for a real minute).
    pub widget_taskbars: u8,
    pub cpu: f32,
    pub gpu: Option<f32>,
    pub fullscreen: bool,
    /// All network bytes (down + up). Zero feeds Gone Dark.
    pub net_bytes: u64,
    pub down_bytes: u64,
    pub up_bytes: u64,
    /// Disk bytes read + written.
    pub disk_bytes: u64,
    pub gpu_temp: Option<f32>,
    pub ram_pct: f32,
    /// System drive used, percent.
    pub disk_used_pct: Option<f32>,
    /// (percent, plugged in)
    pub battery: Option<(u8, bool)>,
    pub uptime_s: u64,
    pub local: LocalTime,
    pub now_ms: u64,
}

impl Default for Minute {
    fn default() -> Self {
        Minute {
            widget_taskbars: 1,
            cpu: 0.0,
            gpu: None,
            fullscreen: false,
            net_bytes: 1,
            down_bytes: 0,
            up_bytes: 0,
            disk_bytes: 0,
            gpu_temp: None,
            ram_pct: 0.0,
            disk_used_pct: None,
            battery: None,
            uptime_s: 0,
            local: LocalTime::default(),
            now_ms: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// A real minute (see `Minute`). Fed once per minute. A minute at or before
    /// the last counted minute, or from an earlier day, is ignored (replays,
    /// restored logs and clock changes count nothing).
    ActiveMinute(Minute),
    /// An alert of `kind` (observer-defined, e.g. 0 = CPU) started.
    AlertFired { kind: u8, now_ms: u64 },
    /// The alert of `kind` ended. Counts as handled when it was fired < 30 min ago.
    AlertCleared { kind: u8, now_ms: u64 },
    /// End task from the panel. `ok == false` means Windows refused (Access Denied).
    TaskEnded { ok: bool, now_ms: u64 },
    /// An automatic update installed.
    UpdateInstalled { now_ms: u64 },
    /// A billing cycle closed; `under_cap` when usage stayed below the data cap.
    CycleClosed { under_cap: bool, now_ms: u64 },
}

impl Event {
    pub fn now_ms(&self) -> u64 {
        match self {
            Event::ActiveMinute(m) => m.now_ms,
            Event::AlertFired { now_ms, .. }
            | Event::AlertCleared { now_ms, .. }
            | Event::TaskEnded { now_ms, .. }
            | Event::UpdateInstalled { now_ms }
            | Event::CycleClosed { now_ms, .. } => *now_ms,
        }
    }
}

/// Build options. `unavailable` = badge ids that cannot be earned in this build
/// ("Coming soon"): they never unlock, are skipped by Completionist, and a set
/// containing one never gets a record.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ctx<'a> {
    pub unavailable: &'a [&'a str],
}

impl Ctx<'_> {
    pub fn available(&self, id: &str) -> bool {
        !self.unavailable.contains(&id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unlock {
    /// A badge unlocked or climbed. Singles: `from == to == 0`. Ladders:
    /// `from` -> `to` (tier skips possible; `xp` sums every tier passed).
    /// `quiet`: Emulsion climb right after the gate opened (no moment of its own).
    Badge {
        id: &'static str,
        from: u8,
        to: u8,
        xp: u32,
        quiet: bool,
    },
    /// A certified record rose to `to` (sum of record XP of tiers passed).
    Record {
        set: &'static str,
        from: u8,
        to: u8,
        xp: u32,
    },
    /// Level up. `rank` = the new rank when it changed; `title` = new title when it changed.
    Level {
        from: u32,
        to: u32,
        rank: Option<Rank>,
        title: Option<&'static str>,
    },
    /// The Emulsion gate just opened (its own moment).
    Gate,
}

// ---------------------------------------------------------------------------
// apply
// ---------------------------------------------------------------------------

/// Feed one event. Returns the next state and the unlocks it caused, in order:
/// badges, gate, records, level. Ignored events return `prev` unchanged.
pub fn apply(prev: &State, ev: &Event, ctx: &Ctx) -> (State, Vec<Unlock>) {
    let mut s = prev.clone();
    let mut out: Vec<Unlock> = Vec::new();
    let mut hits: Vec<&'static str> = Vec::new();
    let now = ev.now_ms();
    let xp_before = total_xp(&s);

    match ev {
        Event::ActiveMinute(m) => {
            if !on_minute(&mut s, m, &mut hits) {
                return (prev.clone(), out);
            }
        }
        Event::AlertFired { kind, now_ms } => {
            if s.d.k != 0 {
                s.d.a = true;
            }
            let fresh =
                s.al.get(kind)
                    .map_or(true, |&t| now_ms.saturating_sub(t) > ALERT_WINDOW_MS);
            if fresh {
                s.al.insert(*kind, *now_ms);
            }
            s.al.retain(|_, t| now_ms.saturating_sub(*t) <= 24 * 3_600_000);
        }
        Event::AlertCleared { kind, now_ms } => {
            if let Some(t) = s.al.remove(kind) {
                if now_ms.saturating_sub(t) <= ALERT_WINDOW_MS {
                    s.bump("alerts", 1);
                    s.add_flat(1);
                }
            }
        }
        Event::TaskEnded { ok, .. } => {
            if *ok {
                s.bump("tasks", 1);
                s.add_flat(1);
            } else {
                hits.push("access_denied");
            }
        }
        Event::UpdateInstalled { .. } => hits.push("up_to_date"),
        Event::CycleClosed { under_cap, .. } => {
            if *under_cap {
                hits.push("under_cap");
            }
        }
    }

    derive_hits(&s, &mut hits);
    for id in hits {
        grant_single(&mut s, id, now, ctx, &mut out);
    }
    sync_ladders(&mut s, now, ctx, false, &mut out);
    sync_completionist(&mut s, now, ctx, &mut out);

    // Gate: checked after real events only (this function), never by a timer.
    if !s.g && gate_progress(&s).all_met() {
        s.g = true;
        out.push(Unlock::Gate);
        sync_ladders(&mut s, now, ctx, true, &mut out); // quiet climb
    }
    sync_records(&mut s, ctx, &mut out);

    let xp_after = total_xp(&s);
    let (l0, l1) = (level_of(xp_before), level_of(xp_after));
    if l1 > l0 {
        let r0 = rank_of(l0, prev.g);
        let r1 = rank_of(l1, s.g);
        let t0 = title_of(l0);
        let t1 = title_of(l1);
        out.push(Unlock::Level {
            from: l0,
            to: l1,
            rank: (r1 != r0).then_some(r1),
            title: (t1 != t0).then_some(t1),
        });
    }
    (normalize(s), out)
}

/// Returns false when the minute counts nothing.
fn on_minute(s: &mut State, m: &Minute, hits: &mut Vec<&'static str>) -> bool {
    if m.widget_taskbars == 0 {
        return false; // not a real minute
    }
    let day = m.local.day_number();
    if day <= 0 || day > u32::MAX as i64 {
        return false;
    }
    let day = day as u32;
    let minute_no = m.now_ms / 60_000;
    if minute_no == 0 || minute_no <= s.r.l || day < s.d.k {
        return false; // replay, duplicate or clock went back
    }
    if day > s.d.k {
        close_day(s);
        s.d = DayState {
            k: day,
            ..DayState::default()
        };
    }

    // Run (Marathon, Gone Dark)
    if s.r.l == 0 || minute_no - s.r.l > MARATHON_GAP_MINUTES {
        s.r.s = minute_no;
    }
    s.r.l = minute_no;
    if m.net_bytes == 0 {
        s.r.i += 1;
    } else {
        s.r.i = 0;
    }

    // Counters
    let before = counter(s, "min");
    s.bump("min", 1);
    if (before + 1) / 60 > before / 60 {
        s.add_flat(2); // a finished active hour
    }
    if m.cpu >= 80.0 {
        s.bump("cpu_min", 1);
    }
    if m.gpu.map_or(false, |g| g >= 50.0) {
        s.bump("gpu_min", 1);
    }
    if m.fullscreen {
        s.bump("game_min", 1);
    }
    if m.widget_taskbars >= 2 {
        s.bump("dual_min", 1);
    }
    if matches!(m.local.hour, 2 | 3) {
        s.bump("owl_min", 1);
    }
    s.bump("net_b", m.net_bytes);
    s.bump("down_b", m.down_bytes);
    s.bump("up_b", m.up_bytes);
    s.bump("disk_b", m.disk_bytes);

    let mk = m.local.month_key();
    if s.tm.0 != mk {
        s.tm = (mk, 0);
    }
    s.tm.1 = s.tm.1.saturating_add(m.net_bytes);

    // Day of real use
    s.d.m += 1;
    if s.d.m == DAY_MINUTES {
        record_day(s, day, &m.local);
        if !s.d.a {
            s.add_flat(5); // clean day
        }
    }

    // Instant accidents and secrets
    if m.gpu_temp.map_or(false, |t| t >= 90.0) {
        hits.push("running_hot");
    }
    if m.ram_pct >= 98.0 {
        hits.push("red_line");
    }
    if let Some(p) = m.disk_used_pct {
        if p >= 98.0 {
            hits.push("full_house");
        }
        let tenths = (p.max(0.0) * 10.0).round() as u32;
        s.d.dm = s.d.dm.max(tenths);
        if s.d.dm - tenths >= 100 {
            hits.push("spring_clean");
        }
    }
    if let Some((pct, plugged)) = m.battery {
        if pct <= 5 && !plugged {
            hits.push("fumes");
        }
    }
    if m.uptime_s >= 30 * 86_400 {
        hits.push("five_nines");
    }
    if s.first != 0 && day.saturating_sub(s.first) >= 365 {
        hits.push("one_year");
    }
    true
}

/// A new day began: finish the books of the previous one.
fn close_day(s: &mut State) {
    if s.d.k != 0 && s.d.m >= DAY_MINUTES {
        if s.d.a {
            s.cw = 0;
        } else {
            s.cw += 1;
        }
    }
}

fn record_day(s: &mut State, day: u32, local: &LocalTime) {
    s.days.entry(local.month_key()).or_insert([0, 0])[0] += 1;
    if s.first == 0 || day < s.first {
        s.first = day;
    }
    let wd = local.weekday.min(6);
    let monday = day.saturating_sub(wd as u32);
    if s.w.0 != monday {
        s.w = (monday, 0);
    }
    let old = s.w.1.count_ones();
    s.w.1 |= 1 << wd;
    if old == 2 && s.w.1.count_ones() == 3 {
        s.days.entry(local.month_key()).or_insert([0, 0])[1] += 1;
    }
}

/// Singles whose condition is a function of the state (idempotent).
fn derive_hits(s: &State, hits: &mut Vec<&'static str>) {
    if counter(s, "min") >= 60 {
        hits.push("power_on");
    }
    if counter(s, "dual_min") >= 60 {
        hits.push("second_screen");
    }
    if counter(s, "net_b") >= GB {
        hits.push("first_gb");
    }
    if s.tm.1 >= TB {
        hits.push("tb_month");
    }
    if s.cw >= COOL_DAYS {
        hits.push("cool_week");
    }
    if s.r.l != 0 && s.r.l.saturating_sub(s.r.s) >= MARATHON_MINUTES {
        hits.push("marathon");
    }
    if s.r.i >= GONE_DARK_MINUTES {
        hits.push("gone_dark");
    }
    if counter(s, "owl_min") >= NIGHT_OWL_MINUTES {
        hits.push("night_owl");
    }
}

fn grant_single(s: &mut State, id: &'static str, now: u64, ctx: &Ctx, out: &mut Vec<Unlock>) {
    let Some(b) = Catalog::get(id) else { return };
    if b.is_ladder() || !ctx.available(id) || s.u.contains_key(id) {
        return;
    }
    s.u.insert(id.to_string(), now.max(1));
    out.push(Unlock::Badge {
        id: b.id,
        from: 0,
        to: 0,
        xp: b.xp,
        quiet: false,
    });
}

/// Move every ladder to the tier its count reaches (never down).
fn sync_ladders(s: &mut State, now: u64, ctx: &Ctx, quiet: bool, out: &mut Vec<Unlock>) {
    for b in Catalog::ladders() {
        if !ctx.available(b.id) {
            continue;
        }
        let have = s.tier(b.id);
        let want = tier_for(b, count_of(s, b), s.g);
        if want <= have {
            continue;
        }
        s.k.insert(b.id.to_string(), want);
        let kd = s.kd.entry(b.id.to_string()).or_default();
        if kd.len() < want as usize {
            kd.resize(want as usize, 0);
        }
        for t in have..want {
            kd[t as usize] = now;
        }
        s.u.entry(b.id.to_string()).or_insert(now.max(1));
        let xp = LADDER_XP[have as usize..want as usize].iter().sum();
        out.push(Unlock::Badge {
            id: b.id,
            from: have,
            to: want,
            xp,
            quiet,
        });
    }
}

/// Records: a set's tier is its weakest piece. Never higher than the badges hold.
fn sync_records(s: &mut State, ctx: &Ctx, out: &mut Vec<Unlock>) {
    for set in SETS {
        if set.members.iter().any(|m| !ctx.available(m)) {
            continue; // a Coming-soon piece: the set waits
        }
        let want = set.members.iter().map(|m| s.tier(m)).min().unwrap_or(0);
        let have = s.ps.get(set.id).copied().unwrap_or(0);
        if want > have {
            s.ps.insert(set.id.to_string(), want);
            let xp = RECORD_XP[have as usize..want as usize].iter().sum();
            out.push(Unlock::Record {
                set: set.id,
                from: have,
                to: want,
                xp,
            });
        }
    }
}

/// Completionist: every earnable badge except itself.
fn sync_completionist(s: &mut State, now: u64, ctx: &Ctx, out: &mut Vec<Unlock>) {
    let all = Catalog::all()
        .iter()
        .filter(|b| b.id != "completionist" && ctx.available(b.id))
        .all(|b| s.u.contains_key(b.id));
    if all {
        grant_single(s, "completionist", now, ctx, out);
    }
}

// ---------------------------------------------------------------------------
// merge / normalize
// ---------------------------------------------------------------------------

fn min_nz(a: u64, b: u64) -> u64 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (a, b) => a.min(b),
    }
}

fn merge_max<K: Ord + Clone, V: Ord + Copy>(
    a: &BTreeMap<K, V>,
    b: &BTreeMap<K, V>,
) -> BTreeMap<K, V> {
    let mut m = a.clone();
    for (k, &v) in b {
        m.entry(k.clone())
            .and_modify(|x| *x = (*x).max(v))
            .or_insert(v);
    }
    m
}

/// Union / max of two copies: earliest dates, max tiers, counters and XP,
/// earliest date per tier. Order independent and idempotent.
pub fn merge(a: &State, b: &State) -> State {
    let mut m = State::default();
    m.v = a.v.max(b.v);
    for src in [a, b] {
        for (id, &t) in &src.u {
            let e = m.u.entry(id.clone()).or_insert(t);
            *e = min_nz(*e, t);
        }
    }
    m.k = merge_max(&a.k, &b.k);
    for src in [a, b] {
        for (id, v) in &src.kd {
            let e = m.kd.entry(id.clone()).or_default();
            if e.len() < v.len() {
                e.resize(v.len(), 0);
            }
            for (i, &t) in v.iter().enumerate() {
                e[i] = min_nz(e[i], t);
            }
        }
    }
    m.ps = merge_max(&a.ps, &b.ps);
    m.lx = a.lx.max(b.lx);
    m.c = merge_max(&a.c, &b.c);
    m.f = a.f.max(b.f);
    m.d = match a.d.k.cmp(&b.d.k) {
        std::cmp::Ordering::Greater => a.d.clone(),
        std::cmp::Ordering::Less => b.d.clone(),
        std::cmp::Ordering::Equal => DayState {
            k: a.d.k,
            flat: a.d.flat.max(b.d.flat),
            m: a.d.m.max(b.d.m),
            a: a.d.a || b.d.a,
            dm: a.d.dm.max(b.d.dm),
        },
    };
    m.days = a.days.clone();
    for (k, v) in &b.days {
        let e = m.days.entry(*k).or_insert([0, 0]);
        e[0] = e[0].max(v[0]);
        e[1] = e[1].max(v[1]);
    }
    m.first = min_nz(a.first as u64, b.first as u64) as u32;
    m.w = if a.w.0 == b.w.0 {
        (a.w.0, a.w.1 | b.w.1)
    } else {
        a.w.max(b.w)
    };
    m.r = a.r.max(b.r);
    m.cw = a.cw.max(b.cw);
    m.tm = a.tm.max(b.tm);
    m.al = merge_max(&a.al, &b.al);
    m.g = a.g || b.g;
    m.ag = a.ag.clone(); // aging: the newest start wins, then the least dust
    for (k, v) in &b.ag {
        let better = |x: &Age| {
            v.start > x.start
                || (v.start == x.start && (v.left < x.left || (v.left == x.left && v.web > x.web)))
        };
        if m.ag.get(k).map_or(true, better) {
            m.ag.insert(k.clone(), *v);
        }
    }
    m.sh = a.sh.max(b.sh);
    normalize(m)
}

/// Repair a state: tiers only on ladders and within 1..=7, tier dates and first-earned
/// dates present, records never above the weakest piece, caps applied.
pub fn normalize(mut s: State) -> State {
    s.v = s.v.max(STATE_VERSION);
    s.k.retain(|id, t| *t > 0 && Catalog::get(id).map_or(true, |b| b.is_ladder()));
    for t in s.k.values_mut() {
        *t = (*t).min(7);
    }
    let keys = &s.k;
    s.kd.retain(|id, _| keys.contains_key(id));
    for (id, v) in s.kd.iter_mut() {
        v.resize(keys[id] as usize, 0);
    }
    for id in s.k.keys() {
        let first =
            s.kd.get(id)
                .and_then(|v| v.iter().copied().filter(|&t| t > 0).min())
                .unwrap_or(1);
        s.u.entry(id.clone()).or_insert(first);
    }
    for v in s.u.values_mut() {
        *v = (*v).max(1);
    }
    let k = &s.k;
    s.ps.retain(|_, t| *t > 0);
    for (id, t) in s.ps.iter_mut() {
        *t = (*t).min(7);
        if let Some(set) = Catalog::set(id) {
            let weakest = set
                .members
                .iter()
                .map(|m| k.get(*m).copied().unwrap_or(0))
                .min()
                .unwrap_or(0);
            *t = (*t).min(weakest);
        }
    }
    s.ps.retain(|_, t| *t > 0);
    let u = &s.u;
    let k = &s.k; // keep aging only for the tier on show of an earned medal (passed tiers drop)
    s.ag.retain(|key, a| {
        let (id, tier) = key.rsplit_once(':').unwrap_or((key.as_str(), ""));
        u.contains_key(id)
            && tier == k.get(id).copied().unwrap_or(0).to_string()
            && a.left.is_finite()
    });
    for a in s.ag.values_mut() {
        a.left = a.left.clamp(0.0, 1.0);
    }
    s.d.flat = s.d.flat.min(FLAT_CAP);
    s.w.1 &= 0x7f;
    while s.al.len() > 16 {
        let oldest = s.al.iter().min_by_key(|(_, &t)| t).map(|(&k, _)| k);
        match oldest {
            Some(k) => s.al.remove(&k),
            None => break,
        };
    }
    s
}

// ---------------------------------------------------------------------------
// JSON (hand written, compact, key-sorted = canonical)
// ---------------------------------------------------------------------------

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn num<T: Into<u64>>(n: T) -> Json {
    Json::Num(n.into() as f64)
}

fn map_json<K: ToString, V>(m: &BTreeMap<K, V>, f: impl Fn(&V) -> Json) -> Json {
    Json::Obj(m.iter().map(|(k, v)| (k.to_string(), f(v))).collect())
}

fn read_map<K: Ord>(j: Option<&Json>, key: impl Fn(&str) -> Option<K>) -> BTreeMap<K, u64> {
    let mut m = BTreeMap::new();
    if let Some(o) = j.and_then(|j| j.as_obj()) {
        for (k, v) in o {
            if let (Some(k), Some(v)) = (key(k), v.as_u64()) {
                m.insert(k, v);
            }
        }
    }
    m
}

fn nth(j: Option<&Json>, i: usize) -> u64 {
    j.and_then(|j| j.as_arr())
        .and_then(|a| a.get(i))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

impl State {
    /// Canonical compact JSON (keys sorted). Two states with equal content give equal text.
    pub fn to_json(&self) -> String {
        let d = obj(vec![
            ("k", num(self.d.k)),
            ("flat", num(self.d.flat)),
            ("m", num(self.d.m)),
            ("a", num(self.d.a as u8)),
            ("dm", num(self.d.dm)),
        ]);
        let j = obj(vec![
            ("v", num(self.v)),
            ("u", map_json(&self.u, |&t| num(t))),
            ("k", map_json(&self.k, |&t| num(t))),
            (
                "kd",
                map_json(&self.kd, |v| Json::Arr(v.iter().map(|&t| num(t)).collect())),
            ),
            ("ps", map_json(&self.ps, |&t| num(t))),
            ("lx", num(self.lx)),
            ("c", map_json(&self.c, |&n| num(n))),
            ("f", num(self.f)),
            ("d", d),
            (
                "days",
                map_json(&self.days, |v| Json::Arr(vec![num(v[0]), num(v[1])])),
            ),
            ("first", num(self.first)),
            ("w", Json::Arr(vec![num(self.w.0), num(self.w.1)])),
            (
                "r",
                Json::Arr(vec![num(self.r.l), num(self.r.s), num(self.r.i)]),
            ),
            ("cw", num(self.cw)),
            ("tm", Json::Arr(vec![num(self.tm.0), num(self.tm.1)])),
            ("al", map_json(&self.al, |&t| num(t))),
            ("g", num(self.g as u8)),
            (
                "ag",
                map_json(&self.ag, |a| {
                    Json::Arr(vec![
                        num(a.start),
                        Json::Num((a.left as f64 * 1000.0).round() / 1000.0),
                        num(a.web),
                    ])
                }),
            ),
            ("sh", num(self.sh)),
        ]);
        j.to_string()
    }

    /// Lenient parse: unknown fields are dropped, missing ones default. `None` only
    /// when the text is not a JSON object.
    pub fn from_json(text: &str) -> Option<State> {
        let j = json::parse(text)?;
        j.as_obj()?;
        let n = |name: &str| j.get(name).and_then(|v| v.as_u64()).unwrap_or(0);
        let d = j.get("d");
        let dn = |name: &str| {
            d.and_then(|d| d.get(name))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let mut s = State {
            v: n("v") as u32,
            u: read_map(j.get("u"), |k| Some(k.to_string())),
            k: read_map(j.get("k"), |k| Some(k.to_string()))
                .into_iter()
                .map(|(k, v)| (k, v.min(7) as u8))
                .collect(),
            kd: BTreeMap::new(),
            ps: read_map(j.get("ps"), |k| Some(k.to_string()))
                .into_iter()
                .map(|(k, v)| (k, v.min(7) as u8))
                .collect(),
            lx: n("lx"),
            c: read_map(j.get("c"), |k| Some(k.to_string())),
            f: n("f"),
            d: DayState {
                k: dn("k") as u32,
                flat: dn("flat") as u32,
                m: dn("m") as u32,
                a: dn("a") != 0,
                dm: dn("dm") as u32,
            },
            days: BTreeMap::new(),
            first: n("first") as u32,
            w: (nth(j.get("w"), 0) as u32, nth(j.get("w"), 1) as u8),
            r: Run {
                l: nth(j.get("r"), 0),
                s: nth(j.get("r"), 1),
                i: nth(j.get("r"), 2) as u32,
            },
            cw: n("cw") as u32,
            tm: (nth(j.get("tm"), 0) as u32, nth(j.get("tm"), 1)),
            al: read_map(j.get("al"), |k| k.parse::<u8>().ok()),
            g: n("g") != 0,
            ag: BTreeMap::new(),
            sh: n("sh"),
        };
        if let Some(o) = j.get("ag").and_then(|v| v.as_obj()) {
            for (key, v) in o {
                let arr = v.as_arr().unwrap_or(&[]);
                let left = match arr.get(1) {
                    Some(Json::Num(n)) if n.is_finite() => n.clamp(0.0, 1.0) as f32,
                    _ => 1.0,
                };
                s.ag.insert(
                    key.clone(),
                    Age {
                        start: nth(Some(v), 0),
                        left,
                        web: nth(Some(v), 2),
                    },
                );
            }
        }
        if let Some(o) = j.get("kd").and_then(|v| v.as_obj()) {
            for (id, v) in o {
                let list = v
                    .as_arr()
                    .unwrap_or(&[])
                    .iter()
                    .map(|t| t.as_u64().unwrap_or(0))
                    .collect();
                s.kd.insert(id.clone(), list);
            }
        }
        if let Some(o) = j.get("days").and_then(|v| v.as_obj()) {
            for (k, v) in o {
                if let Ok(k) = k.parse::<u32>() {
                    s.days
                        .insert(k, [nth(Some(v), 0) as u32, nth(Some(v), 1) as u32]);
                }
            }
        }
        Some(normalize(s))
    }
}

// ---------------------------------------------------------------------------
// Goals (the Server Room page, the Overview card, the nav chip, the tray tip)
// ---------------------------------------------------------------------------

/// The next tier of one ladder: how far along it is and what it pays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Goal {
    pub id: &'static str,
    pub name: &'static str,
    /// Tier this goal reaches (1 = Bronze .. 7 = Emulsion).
    pub tier: u8,
    /// Count now, the previous threshold and the goal's threshold (in the badge's unit, fractional).
    pub have: f64,
    pub from: u64,
    pub to: u64,
    pub unit: &'static str,
    /// XP the tier pays.
    pub xp: u32,
}

impl Goal {
    /// 0..1 along the way from the previous tier.
    pub fn frac(&self) -> f32 {
        let span = (self.to - self.from).max(1) as f64;
        ((self.have - self.from as f64) / span).clamp(0.0, 1.0) as f32
    }
    /// How much is still missing, in the badge's unit.
    pub fn left(&self) -> f64 {
        (self.to as f64 - self.have).max(0.0)
    }
    pub fn tier_name(&self) -> &'static str {
        TIER_NAMES[(self.tier.clamp(1, 7) - 1) as usize]
    }
}

/// An amount in a badge's unit, short: "12 GB", "19 h", "1 alert", "3 tasks".
pub fn fmt_amount(n: f64, unit: &str) -> String {
    let whole = |n: f64| {
        if n > 0.0 && n < 10.0 && n.fract() > 0.05 {
            format!("{:.1}", n)
        } else {
            format!("{}", n.ceil() as u64)
        }
    };
    match unit {
        "hours" => format!("{} h", whole(n)),
        "GB" if n >= 1000.0 => format!("{:.1} TB", n / 1000.0),
        "GB" => format!("{} GB", whole(n)),
        _ => {
            let v = n.ceil() as u64;
            let one = unit.strip_suffix('s').unwrap_or(unit);
            format!("{} {}", v, if v == 1 { one } else { unit })
        }
    }
}

/// A ladder's count in its own unit, with the fraction kept (38.6 GB). 0 for singles.
pub fn amount_of(s: &State, b: &Badge) -> f64 {
    if !b.is_ladder() {
        return 0.0;
    }
    counter(s, b.counter) as f64 / b.per.max(1) as f64
}

/// The next tier of ladder `id`, or None at the top (Ruby is the top until the Emulsion gate opens).
pub fn goal_of(s: &State, id: &str) -> Option<Goal> {
    let b = Catalog::get(id).filter(|b| b.is_ladder())?;
    let k = s.tier(b.id).min(7);
    if k >= 7 || (k >= 6 && !s.g) {
        return None;
    }
    let to = next_threshold(b, k)?;
    let from = if k == 0 { 0 } else { b.tiers[(k - 1) as usize] };
    let have = amount_of(s, b);
    Some(Goal {
        id: b.id,
        name: b.name,
        tier: k + 1,
        have,
        from,
        to,
        unit: b.unit,
        xp: LADDER_XP[k as usize],
    })
}

/// The goal closest to done (largest share of its step), ties to the cheaper tier. None when every ladder is at its top.
pub fn next_goal(s: &State) -> Option<Goal> {
    Catalog::ladders()
        .filter_map(|b| goal_of(s, b.id))
        .max_by(|a, b| {
            a.frac()
                .partial_cmp(&b.frac())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.tier.cmp(&a.tier))
        })
}

/// A rack (set): its certified tier and the members that hold back the next certificate.
#[derive(Clone, Debug, PartialEq)]
pub struct RackStatus {
    pub cert: u8,
    /// Tier the next certificate needs (None at the top).
    pub next: Option<u8>,
    /// Members still below `next`.
    pub blockers: Vec<&'static str>,
}

pub fn rack_status(s: &State, set: &Set) -> RackStatus {
    let cert = set
        .members
        .iter()
        .map(|m| s.tier(m))
        .min()
        .unwrap_or(0)
        .min(7);
    let top = if s.g { 7 } else { 6 };
    if cert >= top {
        return RackStatus {
            cert,
            next: None,
            blockers: Vec::new(),
        };
    }
    let next = cert + 1;
    let blockers = set
        .members
        .iter()
        .copied()
        .filter(|m| s.tier(m) < next)
        .collect();
    RackStatus {
        cert,
        next: Some(next),
        blockers,
    }
}

/// One line for the tray tip: "Level 3 · 37 / 218 XP · Next: Data Mover, 12 GB to Bronze".
pub fn tip_line(s: &State) -> String {
    let (lv, into, need) = level_progress(total_xp(s));
    let mut t = format!("Level {lv} \u{00B7} {into} / {need} XP");
    if let Some(g) = next_goal(s) {
        t.push_str(&format!(
            " \u{00B7} Next: {}, {} to {}",
            g.name,
            fmt_amount(g.left(), g.unit),
            g.tier_name()
        ));
    }
    t
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: u64 = 86_400_000;
    /// 2026-01-05 (a Monday), 00:00 "local" = UTC in tests.
    fn base_day() -> i64 {
        days_from_civil(2026, 1, 5)
    }

    /// Minute `i` (0-based) counted from midnight of day `base_day() + day_off`.
    fn minute(day_off: i64, i: u64) -> Minute {
        let day = base_day() + day_off;
        let ms = day as u64 * DAY_MS + i * 60_000;
        Minute {
            now_ms: ms,
            local: LocalTime::from_unix_ms(ms, 0),
            ..Minute::default()
        }
    }

    /// Feed `n` consecutive minutes starting at minute `from` of day `day_off`.
    fn feed(
        mut s: State,
        day_off: i64,
        from: u64,
        n: u64,
        tweak: impl Fn(&mut Minute),
    ) -> (State, Vec<Unlock>) {
        let mut all = Vec::new();
        for i in from..from + n {
            let mut m = minute(day_off, i);
            tweak(&mut m);
            let (ns, mut un) = apply(&s, &Event::ActiveMinute(m), &Ctx::default());
            s = ns;
            all.append(&mut un);
        }
        (s, all)
    }

    fn minutes(s: State, day_off: i64, from: u64, n: u64) -> (State, Vec<Unlock>) {
        feed(s, day_off, from, n, |_| {})
    }

    fn has_badge(un: &[Unlock], id: &str) -> bool {
        un.iter()
            .any(|u| matches!(u, Unlock::Badge { id: i, .. } if *i == id))
    }

    fn xp_for_levels(upto: u32) -> u64 {
        (1..upto).map(xp_to_next).sum()
    }

    #[test]
    fn catalog_is_consistent() {
        let ids: std::collections::BTreeSet<_> = BADGES.iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), BADGES.len(), "unique ids");
        assert_eq!(Catalog::ladders().count(), 13);
        for b in BADGES {
            if b.is_ladder() {
                assert!(
                    b.tiers.windows(2).all(|w| w[0] < w[1]) && b.tiers[0] > 0,
                    "{}",
                    b.id
                );
                assert_eq!(b.xp, 0);
            } else {
                assert_eq!(b.tiers, [0; 7]);
                assert!(b.xp > 0 && b.set.is_none(), "{}", b.id);
            }
            assert!(!b.line.is_empty() && !b.name.is_empty());
        }
        let mut in_sets = 0;
        for set in SETS {
            for m in set.members {
                let b = Catalog::get(m).expect("member exists");
                assert!(b.is_ladder());
                assert_eq!(b.set, Some(set.id));
                in_sets += 1;
            }
        }
        assert_eq!(in_sets, 12);
        assert_eq!(Catalog::ladders().filter(|b| b.set.is_none()).count(), 1); // stillhere
    }

    #[test]
    fn curve_titles_ranks() {
        assert_eq!(xp_to_next(1), 100);
        assert_eq!(xp_to_next(2), 155);
        assert_eq!(xp_to_next(10), 771);
        assert_eq!(level_of(0), 1);
        assert_eq!(level_of(99), 1);
        assert_eq!(level_of(100), 2);
        assert_eq!(level_of(254), 2);
        assert_eq!(level_of(255), 3);
        assert_eq!(level_of(xp_for_levels(30)), 30);
        assert_eq!(level_of(xp_for_levels(30) - 1), 29);
        let (l, into, need) = level_progress(130);
        assert_eq!((l, into, need), (2, 30, 155));

        let titles = [
            (1, "Rookie"),
            (2, "Rookie"),
            (3, "Tinkerer"),
            (5, "Tinkerer"),
            (6, "Builder"),
            (7, "Builder"),
            (8, "Tuner"),
            (11, "Tuner"),
            (12, "Overclocker"),
            (15, "Overclocker"),
            (16, "Engineer"),
            (19, "Engineer"),
            (20, "Architect"),
            (80, "Architect"),
        ];
        for (l, t) in titles {
            assert_eq!(title_of(l), t, "level {l}");
        }
        let ranks = [
            (1, Rank::Bronze),
            (4, Rank::Bronze),
            (5, Rank::Silver),
            (9, Rank::Silver),
            (10, Rank::Gold),
            (14, Rank::Gold),
            (15, Rank::Platinum),
            (19, Rank::Platinum),
            (20, Rank::Diamond),
            (29, Rank::Diamond),
            (30, Rank::Ruby),
            (39, Rank::Ruby),
        ];
        for (l, r) in ranks {
            assert_eq!(rank_of(l, false), r);
            assert_eq!(rank_of(l, true), r);
        }
        assert_eq!(rank_of(40, false), Rank::Ruby);
        assert_eq!(rank_of(99, false), Rank::Ruby);
        assert_eq!(rank_of(40, true), Rank::Emulsion);
        assert_eq!(rank_of(39, true), Rank::Ruby);
        assert_eq!(Rank::Emulsion.name(), "Emulsion");
    }

    #[test]
    fn every_ladder_threshold() {
        for b in Catalog::ladders() {
            for (i, &t) in b.tiers.iter().enumerate() {
                let want = i as u8 + 1;
                assert_eq!(
                    tier_for(b, t - 1, true),
                    want - 1,
                    "{} below tier {}",
                    b.id,
                    want
                );
                assert_eq!(tier_for(b, t, true), want, "{} at tier {}", b.id, want);
                // Without the gate everything stops at Ruby.
                assert_eq!(
                    tier_for(b, t, false),
                    want.min(6),
                    "{} ungated tier {}",
                    b.id,
                    want
                );
            }
            assert_eq!(tier_for(b, u64::MAX, false), 6);
            assert_eq!(tier_for(b, u64::MAX, true), 7);
            assert_eq!(tier_for(b, 0, true), 0);
        }
        // Spec spot checks.
        let watch = Catalog::get("watch").unwrap();
        assert_eq!(watch.tiers, [5, 50, 250, 1000, 2500, 6000, 12000]);
        assert_eq!(
            Catalog::get("stillhere").unwrap().tiers,
            [2, 6, 12, 24, 36, 60, 120]
        );
        assert_eq!(
            Catalog::get("ritual").unwrap().tiers,
            [2, 8, 26, 52, 104, 208, 416]
        );
        // Singles never get tiers.
        assert_eq!(tier_for(Catalog::get("marathon").unwrap(), 99999, true), 0);
    }

    #[test]
    fn ladder_apply_reaches_each_tier_via_counters() {
        for b in Catalog::ladders() {
            if matches!(b.id, "regular" | "ritual" | "stillhere") {
                continue; // derived from the days map, tested in loyalty
            }
            for (i, &th) in b.tiers.iter().enumerate() {
                let mut s = State::default();
                s.c.insert(b.counter.to_string(), th * b.per - 1);
                let (s, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &Ctx::default());
                assert_eq!(s.tier(b.id), i as u8, "{} just below tier {}", b.id, i + 1);
                // A fresh state may climb to tier i here, never past it.
                assert!(!un.iter().any(
                    |u| matches!(u, Unlock::Badge { id, to, .. } if *id == b.id && *to > i as u8)
                ));
                let mut s = s;
                s.g = true; // open gate so Emulsion is reachable in this unit test
                s.c.insert(b.counter.to_string(), th * b.per);
                let (s, un) = apply(&s, &Event::UpdateInstalled { now_ms: 6 }, &Ctx::default());
                assert_eq!(s.tier(b.id), i as u8 + 1, "{} at tier {}", b.id, i + 1);
                assert!(un
                    .iter()
                    .any(|u| matches!(u, Unlock::Badge { id, from, to, .. }
                    if *id == b.id && *from == i as u8 && *to == i as u8 + 1)));
            }
        }
    }

    #[test]
    fn tier_skip_pays_every_tier_passed() {
        let mut s = State::default();
        s.c.insert("min".into(), 1000 * 60 - 1);
        let (s, un) = apply(&s, &Event::ActiveMinute(minute(0, 0)), &Ctx::default());
        let up = un.iter().find_map(|u| match u {
            Unlock::Badge {
                id: "watch",
                from,
                to,
                xp,
                quiet,
            } => Some((*from, *to, *xp, *quiet)),
            _ => None,
        });
        // 5, 50, 250, 1000 hours -> Platinum (4): 25+50+100+250.
        assert_eq!(up, Some((0, 4, 425, false)));
        assert_eq!(s.tier("watch"), 4);
        assert_eq!(s.kd["watch"].len(), 4);
        assert!(s.kd["watch"].iter().all(|&t| t > 0));
        // Not tracked twice.
        let (_, un2) = apply(&s, &Event::ActiveMinute(minute(0, 1)), &Ctx::default());
        assert!(!un2
            .iter()
            .any(|u| matches!(u, Unlock::Badge { id: "watch", .. })));
    }

    #[test]
    fn total_xp_adds_everything() {
        let mut s = State::default();
        s.lx = 10;
        s.f = 7;
        s.k.insert("watch".into(), 3);
        s.ps.insert("rack_watch".into(), 2);
        s.u.insert("power_on".into(), 1);
        s.u.insert("watch".into(), 1);
        assert_eq!(total_xp(&s), 10 + 7 + (25 + 50 + 100) + (100 + 200) + 25);
    }

    #[test]
    fn records_follow_the_weakest_piece() {
        let mut s = State::default();
        for id in ["watch", "heavy", "render"] {
            s.k.insert(id.into(), 3);
        }
        s.k.insert("mover".into(), 2);
        let (s, un) = apply(&s, &Event::UpdateInstalled { now_ms: 9 }, &Ctx::default());
        assert!(un.contains(&Unlock::Record {
            set: "rack_watch",
            from: 0,
            to: 2,
            xp: 300
        }));
        assert_eq!(s.ps["rack_watch"], 2);
        // Raising the last piece to 5 lifts the record to 3: pays only tier 3.
        let mut s2 = s.clone();
        s2.k.insert("mover".into(), 5);
        let (s3, un) = apply(&s2, &Event::UpdateInstalled { now_ms: 10 }, &Ctx::default());
        assert!(un.contains(&Unlock::Record {
            set: "rack_watch",
            from: 2,
            to: 3,
            xp: 400
        }));
        assert_eq!(s3.ps["rack_watch"], 3);
        // A record never exceeds the badges: normalize clamps.
        let mut bad = s3.clone();
        bad.ps.insert("rack_watch".into(), 6);
        assert_eq!(normalize(bad).ps["rack_watch"], 3);
        // Skipping straight from nothing to Gold pays 100+200+400.
        let mut s4 = State::default();
        for id in ["bandwidth", "down", "up", "gameon"] {
            s4.k.insert(id.into(), 3);
        }
        let (_, un) = apply(&s4, &Event::UpdateInstalled { now_ms: 1 }, &Ctx::default());
        assert!(un.contains(&Unlock::Record {
            set: "rack_flow",
            from: 0,
            to: 3,
            xp: 700
        }));
    }

    #[test]
    fn unavailable_badges_wait() {
        let ctx = Ctx {
            unavailable: &["up_to_date", "mover"],
        };
        let mut s = State::default();
        s.c.insert("disk_b".into(), 100 * GB);
        for id in ["watch", "heavy", "render"] {
            s.k.insert(id.into(), 1);
        }
        let (s, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &ctx);
        assert!(un.is_empty());
        assert!(
            !s.earned("up_to_date") && s.tier("mover") == 0 && !s.ps.contains_key("rack_watch")
        );
    }

    #[test]
    fn flat_xp_cap_per_day() {
        // A 24 hour day of watching: 24 hours * 2 + clean day 5 = 53 -> capped at 40.
        let (s, _) = minutes(State::default(), 0, 0, 24 * 60);
        assert_eq!(s.f, 40);
        assert_eq!(s.d.flat, 40);
        // Next day starts fresh.
        let (s, _) = minutes(s, 1, 0, 60);
        assert_eq!(s.d.flat, 7); // finished hour + clean day
        assert_eq!(s.f, 47);
        // Handled alerts and tasks share the same cap.
        let mut s = State::default();
        let (s1, _) = minutes(s.clone(), 0, 0, 1);
        s = s1;
        for i in 0..60u64 {
            let t = 100_000_000 + i;
            s = apply(
                &s,
                &Event::TaskEnded {
                    ok: true,
                    now_ms: t,
                },
                &Ctx::default(),
            )
            .0;
        }
        assert_eq!(s.f, 40);
        assert_eq!(s.c["tasks"], 60); // counting is not capped
    }

    #[test]
    fn flat_sources() {
        // 60 minutes: +2 for the finished hour, +5 clean day at minute 30.
        let (s, _) = minutes(State::default(), 0, 0, 60);
        assert_eq!(s.f, 7);
        // An alert before minute 30 spoils the clean day.
        let (s, _) = minutes(State::default(), 0, 0, 1);
        let ms = minute(0, 1).now_ms;
        let (s, _) = apply(
            &s,
            &Event::AlertFired {
                kind: 1,
                now_ms: ms,
            },
            &Ctx::default(),
        );
        let (s, _) = minutes(s, 0, 1, 59);
        assert_eq!(s.f, 2);
        // No day known yet: nothing to cap against, no flat XP.
        let (s, _) = apply(
            &State::default(),
            &Event::TaskEnded {
                ok: true,
                now_ms: 9,
            },
            &Ctx::default(),
        );
        assert_eq!(s.f, 0);
        assert_eq!(s.c["tasks"], 1);
    }

    #[test]
    fn only_real_minutes_count() {
        let mut m = minute(0, 0);
        m.widget_taskbars = 0;
        let (s, un) = apply(&State::default(), &Event::ActiveMinute(m), &Ctx::default());
        assert_eq!(s, State::default());
        assert!(un.is_empty());

        // The same minute twice (replay) counts once; older minutes count nothing.
        let (s, _) = minutes(State::default(), 0, 10, 1);
        let (s2, un) = apply(&s, &Event::ActiveMinute(minute(0, 10)), &Ctx::default());
        assert_eq!(s2, s);
        assert!(un.is_empty());
        let (s3, _) = apply(&s, &Event::ActiveMinute(minute(0, 3)), &Ctx::default());
        assert_eq!(s3, s);
        assert_eq!(s.c["min"], 1);
        // A day in the past (clock moved back) counts nothing.
        let (s, _) = minutes(State::default(), 1, 0, 1);
        let mut old = minute(0, 0);
        old.now_ms += 10 * DAY_MS; // later timestamp, earlier local day
        let (s2, _) = apply(&s, &Event::ActiveMinute(old), &Ctx::default());
        assert_eq!(s2, s);
    }

    #[test]
    fn day_of_real_use_needs_30_minutes() {
        let (s, _) = minutes(State::default(), 0, 0, 29);
        assert_eq!(counter(&s, "days"), 0);
        let (s, _) = minutes(s, 0, 29, 1);
        assert_eq!(counter(&s, "days"), 1);
        let (s, _) = minutes(s, 0, 30, 100);
        assert_eq!(counter(&s, "days"), 1, "once a day");
        assert_eq!(s.first as i64, base_day());
    }

    #[test]
    fn loyalty_days_weeks_months() {
        let mut s = State::default();
        // Week 1 (Mon Jan 5): Mon, Tue, Wed, Thu -> one 3+ week, counted once.
        for d in 0..4 {
            s = minutes(s, d, 0, 30).0;
        }
        // Week 2 (Mon Jan 12): only Mon + Tue -> no.
        for d in 7..9 {
            s = minutes(s, d, 0, 30).0;
        }
        // Week of Mon Feb 2: Mon, Tue, Wed -> 3+ (Feb).
        let feb2 = days_from_civil(2026, 2, 2) - base_day();
        for d in feb2..feb2 + 3 {
            s = minutes(s, d, 0, 30).0;
        }
        assert_eq!(counter(&s, "days"), 9);
        assert_eq!(counter(&s, "weeks3"), 2);
        assert_eq!(counter(&s, "months"), 2);
        assert_eq!(s.days[&202601], [6, 1]);
        assert_eq!(s.days[&202602], [3, 1]);
        assert_eq!(count_of(&s, Catalog::get("regular").unwrap()), 9);
        assert_eq!(s.tier("regular"), 1); // 3 days
        assert_eq!(s.tier("stillhere"), 1); // 2 months
        assert_eq!(s.tier("ritual"), 1); // 2 weeks
    }

    #[test]
    fn week_spanning_month_boundary_counts_in_the_month_of_the_third_day() {
        // Mon Jan 26 .. Wed Jan 28 is all January; use Mon Dec 29 2025: Mon, Tue, Thu(Jan 1).
        let dec29 = days_from_civil(2025, 12, 29) - base_day();
        let mut s = State::default();
        for d in [dec29, dec29 + 1, dec29 + 3] {
            s = minutes(s, d, 0, 30).0;
        }
        assert_eq!(s.days[&202512], [2, 0]);
        assert_eq!(s.days[&202601], [1, 1]);
    }

    #[test]
    fn happy_accidents_and_start_badges() {
        let one = |tweak: &dyn Fn(&mut Minute), id: &str| {
            let mut m = minute(0, 0);
            tweak(&mut m);
            let (s, un) = apply(&State::default(), &Event::ActiveMinute(m), &Ctx::default());
            assert!(has_badge(&un, id), "{id}");
            assert!(s.earned(id));
        };
        one(&|m| m.gpu_temp = Some(90.0), "running_hot");
        one(&|m| m.ram_pct = 98.0, "red_line");
        one(&|m| m.disk_used_pct = Some(98.0), "full_house");
        one(&|m| m.battery = Some((5, false)), "fumes");
        one(&|m| m.uptime_s = 30 * 86_400, "five_nines");
        // Negative cases.
        let mut m = minute(0, 0);
        m.gpu_temp = Some(89.9);
        m.ram_pct = 97.0;
        m.battery = Some((5, true));
        m.disk_used_pct = Some(60.0);
        let (_, un) = apply(&State::default(), &Event::ActiveMinute(m), &Ctx::default());
        assert!(un.is_empty());

        let (s, un) = apply(
            &State::default(),
            &Event::TaskEnded {
                ok: false,
                now_ms: 5,
            },
            &Ctx::default(),
        );
        assert!(has_badge(&un, "access_denied"));
        assert!(!s.c.contains_key("tasks"));
        let (_, un) = apply(
            &State::default(),
            &Event::UpdateInstalled { now_ms: 5 },
            &Ctx::default(),
        );
        assert!(has_badge(&un, "up_to_date"));
        let (_, un) = apply(
            &State::default(),
            &Event::CycleClosed {
                under_cap: false,
                now_ms: 5,
            },
            &Ctx::default(),
        );
        assert!(un.is_empty());
        let (_, un) = apply(
            &State::default(),
            &Event::CycleClosed {
                under_cap: true,
                now_ms: 5,
            },
            &Ctx::default(),
        );
        assert!(has_badge(&un, "under_cap"));
    }

    #[test]
    fn power_on_second_screen_first_gb() {
        let (s, un) = minutes(State::default(), 0, 0, 59);
        assert!(!has_badge(&un, "power_on"));
        let (_, un) = minutes(s, 0, 59, 1);
        assert!(has_badge(&un, "power_on"));

        let (s, un) = feed(State::default(), 0, 0, 59, |m| m.widget_taskbars = 2);
        assert!(!has_badge(&un, "second_screen"));
        let (_, un) = feed(s, 0, 59, 1, |m| m.widget_taskbars = 2);
        assert!(has_badge(&un, "second_screen"));
        // Single taskbar does not count.
        let (_, un) = minutes(State::default(), 0, 0, 120);
        assert!(!has_badge(&un, "second_screen"));

        let (_, un) = feed(State::default(), 0, 0, 1, |m| m.net_bytes = GB);
        assert!(has_badge(&un, "first_gb"));
    }

    #[test]
    fn gone_dark_needs_ten_quiet_active_minutes_in_a_row() {
        let (s, un) = feed(State::default(), 0, 0, 9, |m| m.net_bytes = 0);
        assert!(!has_badge(&un, "gone_dark"));
        let (_, un) = feed(s, 0, 9, 1, |m| m.net_bytes = 0);
        assert!(has_badge(&un, "gone_dark"));
        // Traffic in between resets.
        let (s, _) = feed(State::default(), 0, 0, 9, |m| m.net_bytes = 0);
        let (s, _) = minutes(s, 0, 9, 1);
        let (_, un) = feed(s, 0, 10, 9, |m| m.net_bytes = 0);
        assert!(!has_badge(&un, "gone_dark"));
    }

    #[test]
    fn marathon_needs_a_continuous_day() {
        let (s, un) = minutes(State::default(), 0, 0, 24 * 60);
        assert!(
            !has_badge(&un, "marathon"),
            "1440 minutes is 1439 minutes of span"
        );
        let (_, un) = minutes(s, 1, 0, 1);
        assert!(has_badge(&un, "marathon"));
        // A gap longer than 5 minutes restarts the run.
        let (s, _) = minutes(State::default(), 0, 0, 600);
        let (_, un) = minutes(s, 0, 610, 1000);
        assert!(
            !has_badge(&un, "marathon"),
            "span since the restart is 999 minutes"
        );
    }

    #[test]
    fn cool_customer_counts_only_days_of_real_use_without_alert() {
        // Days 0..7 of real use, no alert. The 7th clean day closes on the next day's first minute.
        let mut s = State::default();
        let mut got = false;
        for d in 0..8 {
            // Skip a couple of calendar days to show gaps do not matter.
            let off = d + if d >= 3 { 2 } else { 0 };
            let (n, un) = minutes(s, off, 0, 30);
            s = n;
            got |= has_badge(&un, "cool_week");
        }
        assert!(got);
        assert!(s.earned("cool_week"));

        // An alert on day 4 resets the run.
        let mut s = State::default();
        let mut got = false;
        for d in 0..9 {
            let (n, _) = minutes(s, d, 0, 30);
            s = n;
            if d == 3 {
                let ms = minute(d, 31).now_ms;
                s = apply(
                    &s,
                    &Event::AlertFired {
                        kind: 0,
                        now_ms: ms,
                    },
                    &Ctx::default(),
                )
                .0;
            }
            got |= s.earned("cool_week");
        }
        assert!(!got);
        assert_eq!(s.cw, 4); // days 5..=8 closed so far: day 9 (index 8) is still open
    }

    #[test]
    fn alerts_are_handled_within_thirty_minutes() {
        let (s, _) = minutes(State::default(), 0, 0, 1);
        let t0 = 5_000_000_000u64;
        let (s, _) = apply(
            &s,
            &Event::AlertFired {
                kind: 2,
                now_ms: t0,
            },
            &Ctx::default(),
        );
        let (s, un) = apply(
            &s,
            &Event::AlertCleared {
                kind: 2,
                now_ms: t0 + 29 * 60_000,
            },
            &Ctx::default(),
        );
        assert_eq!(s.c["alerts"], 1);
        assert!(has_badge(&un, "responder"));
        // Clearing again counts nothing (duplicate).
        let (s2, _) = apply(
            &s,
            &Event::AlertCleared {
                kind: 2,
                now_ms: t0 + 30 * 60_000,
            },
            &Ctx::default(),
        );
        assert_eq!(s2.c["alerts"], 1);
        // Too slow.
        let (s, _) = apply(
            &s,
            &Event::AlertFired {
                kind: 2,
                now_ms: t0 + 100 * 60_000,
            },
            &Ctx::default(),
        );
        let (s, _) = apply(
            &s,
            &Event::AlertCleared {
                kind: 2,
                now_ms: t0 + 140 * 60_000,
            },
            &Ctx::default(),
        );
        assert_eq!(s.c["alerts"], 1);
        assert!(s.al.is_empty());
        // A clear with no fire counts nothing.
        let (s, _) = apply(
            &s,
            &Event::AlertCleared {
                kind: 7,
                now_ms: t0,
            },
            &Ctx::default(),
        );
        assert_eq!(s.c["alerts"], 1);
        // A stale pending alert is replaced by a new episode.
        let (s, _) = apply(
            &s,
            &Event::AlertFired {
                kind: 3,
                now_ms: t0,
            },
            &Ctx::default(),
        );
        let (s, _) = apply(
            &s,
            &Event::AlertFired {
                kind: 3,
                now_ms: t0 + 3_600_000,
            },
            &Ctx::default(),
        );
        let (s, _) = apply(
            &s,
            &Event::AlertCleared {
                kind: 3,
                now_ms: t0 + 3_600_000 + 60_000,
            },
            &Ctx::default(),
        );
        assert_eq!(s.c["alerts"], 2);
    }

    #[test]
    fn terabyte_club_resets_each_month() {
        let big = TB / 2;
        let (s, _) = feed(State::default(), 0, 0, 1, |m| m.net_bytes = big);
        let (_, un) = feed(s, 0, 1, 1, |m| m.net_bytes = big);
        assert!(has_badge(&un, "tb_month"));
        // Half a TB in January and half in February: not a club month.
        let feb = days_from_civil(2026, 2, 3) - base_day();
        let (s, _) = feed(State::default(), 0, 0, 1, |m| m.net_bytes = big);
        let (s, un) = feed(s, feb, 0, 1, |m| m.net_bytes = big);
        assert!(!has_badge(&un, "tb_month"));
        assert_eq!(s.tm, (202602, big));
    }

    #[test]
    fn secrets() {
        // Night Owl: 15 active minutes between 02:00 and 03:59.
        let (s, un) = minutes(State::default(), 0, 2 * 60, 14);
        assert!(!has_badge(&un, "night_owl"));
        let (_, un) = minutes(s, 0, 2 * 60 + 14, 1);
        assert!(has_badge(&un, "night_owl"));
        let (_, un) = minutes(State::default(), 0, 4 * 60, 60);
        assert!(!has_badge(&un, "night_owl"));
        // One Year: 365 days after the first day of real use.
        let (s, _) = minutes(State::default(), 0, 0, 30);
        let (s2, un) = minutes(s.clone(), 364, 0, 1);
        assert!(!has_badge(&un, "one_year"));
        let (_, un) = minutes(s2, 365, 0, 1);
        assert!(has_badge(&un, "one_year"));
        // Spring Cleaning: drive use drops 10 points within a day.
        let (s, _) = feed(State::default(), 0, 0, 1, |m| m.disk_used_pct = Some(80.0));
        let (s, un) = feed(s, 0, 1, 1, |m| m.disk_used_pct = Some(70.5));
        assert!(!has_badge(&un, "spring_clean"));
        let (s, un) = feed(s, 0, 2, 1, |m| m.disk_used_pct = Some(70.0));
        assert!(has_badge(&un, "spring_clean"));
        // Across days it does not count.
        let (s2, _) = feed(State::default(), 0, 0, 1, |m| m.disk_used_pct = Some(80.0));
        let (_, un) = feed(s2, 1, 0, 1, |m| m.disk_used_pct = Some(60.0));
        assert!(!has_badge(&un, "spring_clean"));
        drop(s);
    }

    #[test]
    fn completionist_is_every_earnable_badge_except_itself() {
        let ctx_all = Ctx::default();
        let mut s = State::default();
        for b in BADGES.iter().filter(|b| b.id != "completionist") {
            s.u.insert(b.id.to_string(), 1);
        }
        s.u.remove("fumes");
        let (s1, un) = apply(&s, &Event::UpdateInstalled { now_ms: 7 }, &ctx_all);
        assert!(!has_badge(&un, "completionist"));
        let mut s2 = s1;
        s2.u.insert("fumes".into(), 1);
        let (s3, un) = apply(&s2, &Event::UpdateInstalled { now_ms: 8 }, &ctx_all);
        assert!(has_badge(&un, "completionist"));
        assert_eq!(s3.u["completionist"], 8);
        // Coming-soon badges are excluded.
        let ctx = Ctx {
            unavailable: &["fumes"],
        };
        s.u.remove("fumes");
        let (_, un) = apply(&s, &Event::UpdateInstalled { now_ms: 7 }, &ctx);
        assert!(has_badge(&un, "completionist"));
    }

    #[test]
    fn level_up_unlock_carries_rank_and_title() {
        // Level 4 -> 5 with one single worth 250 XP (Marathon via a state tweak is awkward: use lx).
        let mut s = State::default();
        s.lx = xp_for_levels(5) - 1; // level 4
        let (s, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &Ctx::default());
        // Up to Date pays 25: level 5 = Silver, title Tinkerer unchanged.
        assert!(un.contains(&Unlock::Level {
            from: 4,
            to: 5,
            rank: Some(Rank::Silver),
            title: None
        }));
        assert_eq!(level(&s), 5);
        let mut s = State::default();
        s.lx = xp_for_levels(6) - 1;
        let (_, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &Ctx::default());
        assert!(un.contains(&Unlock::Level {
            from: 5,
            to: 6,
            rank: None,
            title: Some("Builder")
        }));
    }

    /// A state that satisfies every gate condition.
    fn gate_state() -> State {
        let mut s = State::default();
        // Level exactly 30 counting the three Ruby ladders below.
        let ladders: u64 = 3 * LADDER_XP[..6].iter().sum::<u32>() as u64;
        s.lx = xp_for_levels(30) - ladders;
        s.c.insert("min".into(), 2000 * 60);
        s.c.insert("net_b".into(), 5 * TB);
        for (i, id) in ["watch", "heavy", "render"].iter().enumerate() {
            s.k.insert(id.to_string(), 6);
            s.u.insert(id.to_string(), 1 + i as u64);
        }
        for m in 0..6u32 {
            s.days.insert(202601 + m, [30, 3]);
        }
        s
    }

    #[test]
    fn gate_needs_every_condition() {
        let s = gate_state();
        let p = gate_progress(&s);
        assert!(p.all_met(), "{p:?}");
        assert_eq!(p.level.1, 30);

        let cases: Vec<(&str, Box<dyn Fn(&mut State)>)> = vec![
            ("level", Box::new(|s| s.lx -= 5000)), // more than the XP apply itself adds
            (
                "days",
                Box::new(|s| {
                    s.days.insert(202601, [29, 3]); // 179 days
                }),
            ),
            (
                "months",
                Box::new(|s| {
                    let v = s.days.remove(&202606).unwrap();
                    s.days.insert(202605, [30 + v[0], 3]);
                }),
            ),
            (
                "ruby",
                Box::new(|s| {
                    s.k.insert("render".into(), 5);
                }),
            ),
            (
                "hours",
                Box::new(|s| {
                    s.c.insert("min".into(), 2000 * 60 - 1);
                }),
            ),
            (
                "traffic",
                Box::new(|s| {
                    s.c.insert("net_b".into(), 5 * TB - 1);
                }),
            ),
        ];
        for (name, f) in cases {
            let mut s = gate_state();
            f(&mut s);
            assert!(
                !gate_progress(&s).all_met(),
                "{name} missing must keep the gate shut"
            );
            // And apply does not open it.
            let (s2, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &Ctx::default());
            assert!(!s2.g, "{name}");
            assert!(!un.contains(&Unlock::Gate), "{name}");
            // Rank stays capped at Ruby however high the level.
            let mut s3 = s2.clone();
            s3.lx += 10_000_000;
            assert_eq!(rank(&s3), Rank::Ruby);
        }
    }

    #[test]
    fn gate_opening_is_one_moment_then_quiet_climb() {
        let mut s = gate_state();
        // Watch already far past its Emulsion threshold.
        s.c.insert("min".into(), 12_000 * 60);
        // Heavy at Ruby, count under Emulsion.
        s.c.insert("cpu_min".into(), 1600 * 60);
        let (s2, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &Ctx::default());
        assert!(s2.g);
        assert_eq!(un.iter().filter(|u| **u == Unlock::Gate).count(), 1);
        assert_eq!(s2.tier("watch"), 7);
        assert_eq!(s2.tier("heavy"), 6);
        let climb = un
            .iter()
            .find(|u| matches!(u, Unlock::Badge { id: "watch", .. }))
            .unwrap();
        assert!(matches!(
            climb,
            Unlock::Badge {
                from: 6,
                to: 7,
                xp: 2000,
                quiet: true,
                ..
            }
        ));
        // The gate unlock comes before the climb.
        let gi = un.iter().position(|u| *u == Unlock::Gate).unwrap();
        let ci = un.iter().position(|u| u == climb).unwrap();
        assert!(gi < ci);
        // Opening again emits nothing.
        let (_, un) = apply(&s2, &Event::UpdateInstalled { now_ms: 6 }, &Ctx::default());
        assert!(!un.contains(&Unlock::Gate));
        // Emulsion rank needs level 40 on top.
        assert!(level(&s2) >= 30 && rank(&s2) >= Rank::Ruby);
    }

    #[test]
    fn emulsion_record_needs_all_four_pieces() {
        let mut s = gate_state();
        s.g = true;
        for id in ["watch", "heavy", "render", "mover"] {
            s.c.insert(
                Catalog::get(id).unwrap().counter.to_string(),
                Catalog::get(id).unwrap().tiers[6] * Catalog::get(id).unwrap().per,
            );
        }
        s.c.insert("min".into(), 12_000 * 60);
        let (s, un) = apply(&s, &Event::UpdateInstalled { now_ms: 5 }, &Ctx::default());
        assert_eq!(s.ps["rack_watch"], 7);
        assert!(un.contains(&Unlock::Record {
            set: "rack_watch",
            from: 0,
            to: 7,
            xp: RECORD_XP.iter().sum()
        }));
    }

    fn sample_state() -> State {
        let (mut s, _) = minutes(State::default(), 0, 0, 90);
        s = apply(
            &s,
            &Event::AlertFired {
                kind: 1,
                now_ms: 99_999_999,
            },
            &Ctx::default(),
        )
        .0;
        s = apply(&s, &Event::UpdateInstalled { now_ms: 123 }, &Ctx::default()).0;
        s
    }

    #[test]
    fn json_round_trip() {
        let s = sample_state();
        let text = s.to_json();
        let back = State::from_json(&text).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.to_json(), text);
        // Gate state with everything.
        let g = apply(
            &gate_state(),
            &Event::UpdateInstalled { now_ms: 5 },
            &Ctx::default(),
        )
        .0;
        assert_eq!(State::from_json(&g.to_json()).unwrap(), g);
        // Lenient parse.
        assert_eq!(State::from_json("{}").unwrap(), State::default());
        assert!(State::from_json("[1]").is_none());
        assert!(State::from_json("not json").is_none());
        // Unknown fields are dropped, known fields still read.
        let t = State::from_json("{\"v\":1,\"zzz\":5,\"lx\":42}").unwrap();
        assert_eq!(t.lx, 42);
    }

    #[test]
    fn heavy_realistic_state_stays_under_4kb() {
        // Three years of daily use, every ladder at Ruby, every badge earned.
        let mut s = State::default();
        let t0 = 1_700_000_000_000u64;
        for b in BADGES {
            s.u.insert(b.id.to_string(), t0 + 123_456_789);
            if b.is_ladder() {
                s.k.insert(b.id.to_string(), 6);
                s.kd.insert(
                    b.id.to_string(),
                    (0..6).map(|i| t0 + i * 86_400_000 * 90).collect(),
                );
            }
        }
        for set in SETS {
            s.ps.insert(set.id.to_string(), 6);
        }
        for name in [
            "min", "cpu_min", "gpu_min", "game_min", "dual_min", "owl_min", "disk_b", "net_b",
            "down_b", "up_b", "alerts", "tasks",
        ] {
            s.c.insert(name.to_string(), 123_456_789_012);
        }
        for m in 0..36u32 {
            s.days.insert(202301 + (m / 12) * 100 + m % 12, [27, 4]);
        }
        s.lx = 12345;
        s.f = 54321;
        s.d = DayState {
            k: 20_000,
            flat: 40,
            m: 600,
            a: true,
            dm: 987,
        };
        s.first = 19_000;
        s.w = (20_000, 0x7f);
        s.r = Run {
            l: 29_000_000,
            s: 28_000_000,
            i: 4,
        };
        s.cw = 5;
        s.tm = (202611, 987_654_321_000);
        s.al.insert(1, t0);
        s.al.insert(2, t0);
        s.g = true;
        let s = normalize(s);
        let len = s.to_json().len();
        assert!(len < 4096, "state is {len} bytes");
    }

    #[test]
    fn merge_is_union_max_and_order_independent() {
        let a = sample_state();
        let mut b = State::default();
        b.lx = 500;
        b.f = 3;
        b.u.insert("power_on".into(), 50); // earlier than a's
        b.u.insert("first_gb".into(), 77);
        b.k.insert("watch".into(), 3);
        b.kd.insert("watch".into(), vec![10, 20, 30]);
        b.c.insert("min".into(), 99_999);
        b.days.insert(202601, [50, 0]);
        b.days.insert(209901, [1, 1]);
        b.first = 5;
        b.al.insert(9, 1);
        let mut a2 = a.clone();
        a2.k.insert("watch".into(), 1);
        a2.kd.insert("watch".into(), vec![15]);
        a2.u.insert("power_on".into(), 900);

        let ab = merge(&a2, &b);
        let ba = merge(&b, &a2);
        assert_eq!(ab, ba);
        assert_eq!(merge(&ab, &a2), ab, "idempotent");
        assert_eq!(merge(&ab, &b), ab);

        assert_eq!(ab.u["power_on"], 50);
        assert_eq!(ab.u["first_gb"], 77);
        assert_eq!(ab.tier("watch"), 3);
        assert_eq!(ab.kd["watch"], vec![10, 20, 30]); // earliest per tier
        assert_eq!(ab.c["min"], a2.c["min"].max(99_999));
        assert_eq!(ab.lx, 500);
        assert_eq!(ab.f, a2.f.max(3));
        assert_eq!(ab.days[&202601][0], a2.days[&202601][0].max(50));
        assert!(ab.days.contains_key(&209901));
        assert_eq!(ab.first, 5);
        assert!(ab.al.contains_key(&9) && ab.al.contains_key(&1));
        assert_eq!(ab.d, a2.d); // same day: fieldwise max (b has none)
        assert!(total_xp(&ab) >= total_xp(&a2).max(total_xp(&b)));
    }

    #[test]
    fn merge_day_and_week_pick_the_later_one() {
        let mut a = State::default();
        a.d = DayState {
            k: 10,
            flat: 40,
            m: 99,
            a: true,
            dm: 5,
        };
        a.w = (7, 0b11);
        a.r = Run {
            l: 100,
            s: 10,
            i: 3,
        };
        a.tm = (202601, 5);
        let mut b = State::default();
        b.d = DayState {
            k: 11,
            flat: 2,
            m: 1,
            a: false,
            dm: 0,
        };
        b.w = (7, 0b100);
        b.r = Run { l: 90, s: 80, i: 9 };
        b.tm = (202602, 1);
        let ab = merge(&a, &b);
        assert_eq!(ab, merge(&b, &a));
        assert_eq!(ab.d.k, 11);
        assert_eq!(ab.d.flat, 2);
        assert_eq!(ab.w, (7, 0b111));
        assert_eq!(ab.r.l, 100);
        assert_eq!(ab.tm, (202602, 1));
    }

    #[test]
    fn normalize_repairs() {
        let mut s = State::default();
        s.k.insert("watch".into(), 9);
        s.k.insert("marathon".into(), 3); // singles never get tiers
        s.k.insert("heavy".into(), 0);
        s.kd.insert("watch".into(), vec![5]);
        s.kd.insert("ghost".into(), vec![1]);
        s.d.flat = 99;
        let n = normalize(s);
        assert_eq!(n.k.len(), 1);
        assert_eq!(n.tier("watch"), 7);
        assert_eq!(n.kd["watch"].len(), 7);
        assert_eq!(n.kd["watch"][0], 5);
        assert!(!n.kd.contains_key("ghost"));
        assert_eq!(
            n.u["watch"], 5,
            "first earned falls back to the earliest tier date"
        );
        assert_eq!(n.d.flat, FLAT_CAP);
    }

    #[test]
    fn local_time_helpers() {
        let l = LocalTime::from_unix_ms(0, 0);
        assert_eq!(
            (l.year, l.month, l.day, l.hour, l.weekday),
            (1970, 1, 1, 0, 3)
        ); // Thursday
        let l = LocalTime::from_unix_ms(base_day() as u64 * DAY_MS + 5 * 3_600_000, 60);
        assert_eq!(
            (l.year, l.month, l.day, l.hour, l.weekday),
            (2026, 1, 5, 6, 0)
        );
        assert_eq!(l.day_number(), base_day());
        assert_eq!(l.month_key(), 202601);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
    }

    #[test]
    fn full_flow_is_deterministic() {
        let a = minutes(State::default(), 0, 0, 200);
        let b = minutes(State::default(), 0, 0, 200);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
    }

    // ---- medal care (aging and polishing)

    fn earned_gold(at: u64) -> State {
        let mut s = State::default();
        s.u.insert("watch".into(), at);
        s.k.insert("watch".into(), 3);
        s.kd.insert("watch".into(), vec![at, at, at]);
        normalize(s)
    }

    #[test]
    fn medals_age_with_real_hours() {
        let at = 1_800_000_000_000;
        let s = earned_gold(at);
        let w0 = s.wear("watch", at);
        assert!(w0.clean && w0.condition == 100);
        // the dust curve: min(1, 2(1 - e^(-days/527)))
        for (d, want) in [
            (10.0, 0.0377),
            (30.0, 0.1107),
            (90.0, 0.3141),
            (180.0, 0.5781),
            (270.0, 0.8019),
            (365.0, 0.9994),
        ] {
            assert!(
                (dust_curve(d) - want).abs() < 0.002,
                "day {d}: {}",
                dust_curve(d)
            );
            let w = s.wear("watch", at + (d as u64) * WEAR_DAY_MS);
            assert!((w.dust - want).abs() < 0.002 && w.scratch == 0.0, "{w:?}");
        }
        // the cobweb: none before day 20, full at day 350
        assert_eq!(s.wear("watch", at + 19 * WEAR_DAY_MS).web, 0.0);
        assert!(s.wear("watch", at + 30 * WEAR_DAY_MS).web > 0.0);
        assert!(s.wear("watch", at + 349 * WEAR_DAY_MS).web < 1.0);
        assert_eq!(s.wear("watch", at + 350 * WEAR_DAY_MS).web, 1.0);
        assert!(
            s.wear("watch", at + 30 * WEAR_DAY_MS).condition
                < s.wear("watch", at + 5 * WEAR_DAY_MS).condition
        );
        assert!(s.wear("nope", at + 99 * WEAR_DAY_MS).clean);
        // the bubble never ages
        let mut e = s.clone();
        e.k.insert("watch".into(), 7);
        e.kd.get_mut("watch").unwrap().extend([at, at, at, at]);
        assert!(e.wear("watch", at + 300 * WEAR_DAY_MS).clean);
    }

    #[test]
    fn polishing_takes_rubs_and_pays_shine() {
        let at = 1_800_000_000_000;
        let mut s = earned_gold(at);
        let now = at + 60 * WEAR_DAY_MS;
        let before = s.wear("watch", now);
        let (w1, g1) = s.polish("watch", now);
        assert_eq!(g1, SHINE_RUB);
        assert_eq!(w1.web, 0.0, "the first rub clears the cobweb");
        assert!(
            (w1.dust - before.dust * 0.7).abs() < 1e-4,
            "each rub takes 30% of the dust"
        );
        // the dust clock keeps running from the tier's start; the web regrows from the rub
        let later = s.wear("watch", now + 25 * WEAR_DAY_MS);
        assert!(
            (later.dust - 0.7 * dust_curve(85.0)).abs() < 1e-3,
            "{later:?}"
        );
        assert!(
            (later.web - web_curve(25.0)).abs() < 1e-4 && later.web > 0.0,
            "{later:?}"
        );
        let mut rubs = 1;
        let mut last = w1;
        let mut s2 = s.clone();
        while !last.clean && rubs < 40 {
            last = s2.polish("watch", now).0;
            rubs += 1;
        }
        assert!(last.clean && (4..=20).contains(&rubs), "{rubs} rubs");
        assert_eq!(
            s2.polish("watch", now).1,
            0,
            "no shine for an already spotless medal"
        );
        assert_eq!(s2.sh, rubs as u64 * SHINE_RUB + SHINE_SPOTLESS);
        // spotless starts the clocks again
        assert!((s2.wear("watch", now + 60 * WEAR_DAY_MS).dust - dust_curve(60.0)).abs() < 1e-4);
        // a new tier is a new, clean medal; the old tier's entry drops
        let mut up = s2.clone();
        up.k.insert("watch".into(), 4);
        up.kd.get_mut("watch").unwrap().push(now + 30 * WEAR_DAY_MS);
        let up = normalize(up);
        assert!(up.wear("watch", now + 30 * WEAR_DAY_MS).clean);
        assert!(up.ag.is_empty(), "{:?}", up.ag);
    }

    #[test]
    fn aging_round_trips_and_merges() {
        let at = 1_800_000_000_000;
        let mut a = earned_gold(at);
        a.polish("watch", at + 40 * WEAR_DAY_MS);
        let back = State::from_json(&a.to_json()).unwrap();
        assert_eq!(back.ag.keys().collect::<Vec<_>>(), vec!["watch:3"]);
        assert!(
            (back.ag["watch:3"].left - 0.7).abs() < 1e-3
                && back.ag["watch:3"].web == at + 40 * WEAR_DAY_MS
        );
        assert_eq!(back.sh, a.sh);
        // same start: the least dust wins, in either order
        let mut b = a.clone();
        b.polish("watch", at + 50 * WEAR_DAY_MS);
        let m = merge(&a, &b);
        assert!((m.ag["watch:3"].left - 0.49).abs() < 1e-3);
        assert_eq!(merge(&b, &a), m);
        // a newer start wins over less dust
        let mut c = earned_gold(at);
        c.ag.insert(
            "watch:3".into(),
            Age {
                start: at + 70 * WEAR_DAY_MS,
                left: 1.0,
                web: at + 70 * WEAR_DAY_MS,
            },
        );
        assert_eq!(merge(&b, &c).ag["watch:3"].start, at + 70 * WEAR_DAY_MS);
        assert_eq!(merge(&c, &b), merge(&b, &c));
        let mut stray = State::default();
        stray.ag.insert(
            "ghost:1".into(),
            Age {
                start: 1,
                left: 5.0,
                web: 1,
            },
        );
        assert!(
            normalize(stray).ag.is_empty(),
            "aging only for earned badges"
        );
    }

    #[test]
    fn next_goal_is_the_closest_and_racks_name_their_blockers() {
        let mut s = State::default();
        s.c.insert("disk_b".into(), 38 * GB); // Data Mover: 38 of 50 GB (76%)
        s.c.insert("net_b".into(), 64 * GB); // Bandwidth Bronze, 64 of 100 GB (60% of the step)
        s.k.insert("bandwidth".into(), 1);
        s.c.insert("min".into(), 31 * 60); // Watchkeeper Bronze, 31 of 50 h
        s.k.insert("watch".into(), 1);
        let g = next_goal(&s).unwrap();
        assert_eq!((g.id, g.tier, g.from, g.to), ("mover", 1, 0, 50));
        assert!((g.frac() - 0.76).abs() < 1e-3);
        assert_eq!(fmt_amount(g.left(), g.unit), "12 GB");
        assert_eq!(g.xp, LADDER_XP[0]);
        let bw = goal_of(&s, "bandwidth").unwrap();
        assert_eq!((bw.tier, bw.from, bw.to), (2, 10, 100));
        assert!(goal_of(&s, "power_on").is_none());
        // Flow rack: three at Bronze, Game On not started.
        for m in ["bandwidth", "down", "up"] {
            s.k.insert(m.into(), 1);
        }
        let r = rack_status(&s, Catalog::set("rack_flow").unwrap());
        assert_eq!(
            (r.cert, r.next, r.blockers.clone()),
            (0, Some(1), vec!["gameon"])
        );
        s.k.insert("gameon".into(), 2);
        let r = rack_status(&s, Catalog::set("rack_flow").unwrap());
        assert_eq!((r.cert, r.next, r.blockers.len()), (1, Some(2), 3));
        // Ruby is the top until the gate opens.
        s.k.insert("up".into(), 6);
        assert!(goal_of(&s, "up").is_none());
        s.g = true;
        assert_eq!(goal_of(&s, "up").unwrap().tier, 7);
        assert!(tip_line(&s).starts_with("Level "));
        assert_eq!(fmt_amount(1.0, "alerts"), "1 alert");
        assert_eq!(fmt_amount(0.4, "hours"), "0.4 h");
    }
}
