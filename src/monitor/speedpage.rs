//! Full speed page: is each part running as fast as it can, and if not, why and how to
//! fix it. Short words: a number pair ("x8 / x16"), a meter, and a two-word fix per part;
//! one card below explains the selected part with "does it matter?" meters.
//!
//! Facts come from `sensors::deep::speed` (read on a worker thread when the page opens and
//! on Scan). The verdicts are pure (`checks`) and unit-tested. Drawn in the current look:
//! meters and numerals go through `Canvas::meter` / `Canvas::numeral`, colours from the
//! look tokens. A child module of `monitor`, laid out and drawn instead of the Monitor
//! overview while `Monitor::speed` is set (the Full speed nav page).

use super::*;
use crate::driver::{self, Missing};
use crate::sensors::deep::speed::{Facts, Probe, UsbKind};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Bad,
    Warn,
    Info,
    Good,
}

/// What the action button of the selected part does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    /// Set screen `gdi` to this refresh rate.
    SetHz(String, u32),
    /// Open a link or a Settings page.
    Open(&'static str),
}

/// One part and its verdict.
#[derive(Clone, Debug)]
pub struct Check {
    /// Three-letter tag ("GPU").
    pub tag: &'static str,
    pub name: String,
    /// Now / best, as shown ("x8", "x16").
    pub now: String,
    pub max: String,
    /// now / best, for the meter.
    pub frac: f32,
    pub tone: Tone,
    /// Two-word fix under the meter.
    pub fix: String,
    /// Detail card: headline and one line why.
    pub head: String,
    pub why: String,
    pub act: Act,
    pub act_label: String,
    /// "Does it matter?": (what, 0..1, short text).
    pub imp: Vec<(&'static str, f32, String)>,
}

impl Check {
    fn new(tag: &'static str, name: &str) -> Check {
        Check {
            tag,
            name: name.into(),
            now: String::new(),
            max: String::new(),
            frac: 1.0,
            tone: Tone::Good,
            fix: "Full speed".into(),
            head: "Full speed".into(),
            why: String::new(),
            act: Act::None,
            act_label: String::new(),
            imp: Vec::new(),
        }
    }
    pub fn held(&self) -> bool {
        matches!(self.tone, Tone::Bad | Tone::Warn)
    }
}

fn imp(v: &[(&'static str, f32, &str)]) -> Vec<(&'static str, f32, String)> {
    v.iter().map(|(a, b, c)| (*a, *b, c.to_string())).collect()
}

fn mbps(bps: u64) -> String {
    if bps >= 1_000_000_000 && bps % 1_000_000_000 != 0 {
        format!("{:.1}G", bps as f64 / 1e9)
    } else if bps >= 1_000_000_000 {
        format!("{}G", bps / 1_000_000_000)
    } else {
        format!("{}", bps / 1_000_000)
    }
}

/// Rated speed in a memory part number: "CMK32GX4M2D3600C18" -> 3600, "F5-6000J3038F16G" ->
/// 6000, Kingston "KF436C17BBK2/16" -> 3600. 0 when none is found.
pub fn part_speed(part: &str) -> u32 {
    let p = part.to_ascii_uppercase();
    // Kingston Fury: KF + DDR gen digit + two speed digits.
    if let Some(rest) = p.strip_prefix("KF") {
        let d: Vec<u32> = rest.chars().take(3).filter_map(|c| c.to_digit(10)).collect();
        if d.len() == 3 {
            let s = (d[1] * 10 + d[2]) * 100;
            if (2400..=8400).contains(&s) {
                return s;
            }
        }
    }
    let b = p.as_bytes();
    let mut best = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let j = (i..b.len()).find(|&k| !b[k].is_ascii_digit()).unwrap_or(b.len());
            if j - i == 4 {
                let v: u32 = p[i..j].parse().unwrap_or(0);
                let fits = (2400..=8400).contains(&v) && (v % 100 == 0 || v % 100 == 66 || v % 100 == 33);
                if fits {
                    best = best.max(v);
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    best
}

/// Every verdict for these facts, in page order.
pub fn checks(f: &Facts) -> Vec<Check> {
    let mut out = Vec::new();
    let laptop = f.battery;
    let missing_gpu = f.missing.iter().find(|m| m.gpu);

    // ---- graphics card
    let dgpu = f.gpus.iter().find(|g| !g.integrated);
    if let Some(m) = missing_gpu {
        let mut c = Check::new("GPU", "Graphics");
        c.now = "Basic".into();
        c.max = driver::vendor_name(m.vendor).to_string();
        c.frac = 0.1;
        c.tone = Tone::Bad;
        c.fix = "Get driver".into();
        c.head = "Driver missing".into();
        c.why = "Windows' basic driver. Low Hz, no GPU speed.".into();
        c.imp = imp(&[("Games", 1.0, "unplayable"), ("Screen", 0.7, "60 Hz cap"), ("AI", 1.0, "no GPU")]);
        out.push(c);
    } else if let Some(g) = dgpu {
        let mut c = Check::new("GPU", "Graphics");
        match g.pcie {
            Some(p) if p.max_width > 0 && p.width > 0 => {
                c.now = format!("x{}", p.width);
                c.max = format!("x{}", p.max_width);
                c.frac = p.width as f32 / p.max_width as f32;
                if p.width < p.max_width {
                    let quarter = c.frac <= 0.25;
                    c.tone = Tone::Warn;
                    c.fix = "Reseat card".into();
                    c.head = if quarter { "A quarter of the lanes".into() } else { "Half the lanes".into() };
                    c.why = format!("Card does x{}. Runs x{}.", p.max_width, p.width);
                    c.imp = if quarter {
                        imp(&[("Games", 0.4, "−5–15%"), ("AI load", 0.8, "much slower"), ("Video", 0.3, "slower")])
                    } else {
                        imp(&[("Games", 0.1, "−2%"), ("AI load", 0.5, "slower"), ("Video", 0.1, "same")])
                    };
                } else {
                    c.head = "All lanes in use".into();
                    c.why = if p.gen < p.max_gen {
                        format!("x{}. Gen {} now, idle power saving.", p.width, p.gen)
                    } else {
                        format!("x{} at PCIe Gen {}.", p.width, p.gen)
                    };
                }
            }
            _ => {
                c.now = "OK".into();
                c.why = "Driver in place.".into();
            }
        }
        out.push(c);
    }

    // ---- screens
    for (i, sc) in f.screens.iter().enumerate() {
        let s = &sc.s;
        let internal = s.connector == "Internal";
        let name = if f.screens.len() > 1 { format!("Monitor {}", i + 1) } else { "Monitor".into() };
        let on_igpu = f.gpus.iter().any(|g| g.luid == s.adapter_luid && g.integrated);
        if !internal && on_igpu && dgpu.is_some() && !laptop {
            let mut c = Check::new("CBL", &format!("{name} cable"));
            c.now = "Board".into();
            c.max = "GPU".into();
            c.frac = 0.3;
            c.tone = Tone::Bad;
            c.fix = "Move cable".into();
            c.head = "Plugged into motherboard".into();
            c.why = "Games run on built-in graphics.".into();
            c.imp = imp(&[("Games", 1.0, "2× or more"), ("Video", 0.3, "smoother"), ("Office", 0.0, "—")]);
            out.push(c);
        }
        let hz = s.hz.round() as u32;
        let edid_max = s.edid.as_ref().and_then(|e| e.range).map_or(0, |r| r.1);
        let mut c = Check::new("MON", &name);
        c.now = hz.to_string();
        if sc.offered > hz + 1 && !(internal && f.on_ac == Some(false)) {
            c.max = format!("{} Hz", sc.offered);
            c.frac = hz as f32 / sc.offered as f32;
            c.tone = Tone::Bad;
            c.fix = "1-tap fix".into();
            c.head = format!("Stuck at {hz} Hz");
            c.why = "Windows setting. Not the cable.".into();
            c.act = Act::SetHz(s.gdi.clone(), sc.offered);
            c.act_label = format!("Set {} Hz", sc.offered);
            let x = sc.offered as f32 / hz.max(1) as f32;
            c.imp = vec![("Games", (1.0 - c.frac).max(0.2), format!("{x:.1}× smoother")), ("Scroll", 0.6, "smoother".into()), ("AI", 0.0, "—".into())];
        } else if !internal && edid_max > sc.offered.max(hz) + 5 && sc.offered > 0 {
            c.max = format!("{edid_max} Hz");
            c.frac = hz as f32 / edid_max as f32;
            c.tone = Tone::Warn;
            c.fix = if s.connector == "HDMI" { "Try DisplayPort".into() } else { "Better cable".into() };
            c.head = "Cable or port limit".into();
            c.why = format!("Screen does {edid_max} Hz. Link carries {}.", sc.offered.max(hz));
            c.imp = imp(&[("Games", 0.6, "smoother"), ("Scroll", 0.4, "smoother"), ("AI", 0.0, "—")]);
        } else {
            c.max = "Hz".into();
            c.head = format!("Full {hz} Hz");
            c.why = format!("{} \u{00B7} best rate set.", if s.connector.is_empty() { "Screen" } else { s.connector });
        }
        out.push(c);
    }

    // ---- memory
    let sticks: Vec<_> = f.hw.dimms.iter().filter(|d| d.size > 0).collect();
    if !sticks.is_empty() {
        let speed = sticks.iter().map(|d| d.speed).filter(|v| *v > 0).min().unwrap_or(0);
        let rated = sticks.iter().map(|d| d.rated.max(part_speed(&d.part))).max().unwrap_or(0);
        let mut c = Check::new("RAM", "Memory");
        c.now = if speed > 0 { speed.to_string() } else { "OK".into() };
        if !laptop && speed > 0 && rated > speed + 200 {
            c.max = rated.to_string();
            c.frac = speed as f32 / rated as f32;
            c.tone = Tone::Warn;
            c.fix = "XMP off".into();
            c.head = "XMP / EXPO off".into();
            c.why = format!("Sold as {rated}. Runs {speed}. One BIOS switch.");
            c.imp = imp(&[("Games", 0.4, "+4–12%"), ("iGPU", 0.8, "+15–37%"), ("Office", 0.0, "—")]);
        } else {
            c.max = "MT/s".into();
            c.head = "Rated speed".into();
            c.why = format!("{} {}", f.hw.mem_type, if rated > 0 { "at its rating." } else { "running." }).trim().to_string();
        }
        out.push(c);
        let slots = f.hw.dimms.len();
        let mut c = Check::new("DIM", "RAM sticks");
        c.now = sticks.len().to_string();
        if sticks.len() == 1 && slots >= 2 {
            c.max = "2".into();
            c.frac = 0.5;
            c.tone = Tone::Warn;
            c.fix = "Add 2nd stick".into();
            c.head = "Single channel".into();
            c.why = "One stick. Two = double bandwidth.".into();
            c.imp = imp(&[("Games", 0.4, "+12%"), ("iGPU", 1.0, "≈2×"), ("Office", 0.1, "small")]);
        } else {
            c.max = slots.max(sticks.len()).to_string();
            c.frac = 1.0;
            c.head = "Dual channel".into();
            c.why = format!("{} sticks fitted.", sticks.len());
            if sticks.len() == 1 {
                c.head = "Built-in memory".into();
                c.why = "Soldered. Nothing to add.".into();
            }
        }
        out.push(c);
    }

    // ---- drives
    for d in &f.drives {
        let Some((gen, width, max_gen, max_width)) = d.pcie else { continue };
        if max_width == 0 || width == 0 {
            continue;
        }
        let mut c = Check::new("NVM", "SSD");
        if f.drives.iter().filter(|x| x.pcie.is_some()).count() > 1 {
            c.name = format!("SSD {}", d.d.num);
        }
        if width < max_width {
            c.now = format!("x{width}");
            c.max = format!("x{max_width}");
            c.frac = width as f32 / max_width as f32;
            c.tone = Tone::Warn;
            c.fix = "Other M.2 slot".into();
            c.head = "Fewer lanes".into();
            c.why = format!("Drive does x{max_width}. Slot gives x{width}.");
            c.imp = imp(&[("Copies", 0.5, "½ speed"), ("Games", 0.1, "≤2 s"), ("Office", 0.0, "—")]);
        } else if gen < max_gen {
            c.now = format!("Gen{gen}");
            c.max = format!("Gen{max_gen}");
            c.frac = gen as f32 / max_gen as f32;
            c.tone = Tone::Info;
            c.fix = "Fine".into();
            c.head = "Fine as is".into();
            c.why = "Slower link or idle. Games load ≤2 s slower.".into();
            c.imp = imp(&[("Games", 0.05, "≤2 s"), ("Copies", 0.5, "½ speed"), ("Office", 0.0, "—")]);
        } else {
            c.now = format!("Gen{gen}");
            c.max = format!("x{width}");
            c.head = "Full link".into();
            c.why = format!("PCIe Gen {gen} x{width}.");
        }
        out.push(c);
    }

    // ---- USB
    let cares = |k: UsbKind| matches!(k, UsbKind::Storage | UsbKind::Camera | UsbKind::Net);
    let mut usb_ok = None;
    for u in f.usb.iter().filter(|u| cares(u.kind)) {
        let label = match u.kind {
            UsbKind::Storage => "USB drive",
            UsbKind::Camera => "USB camera",
            _ => "USB network",
        };
        if u.can_ss && !u.runs_ss {
            let mut c = Check::new("USB", label);
            c.now = "USB2".into();
            c.max = "USB3".into();
            c.frac = 0.1;
            c.tone = Tone::Bad;
            c.fix = "Blue port".into();
            c.head = "Slow port".into();
            c.why = format!("{}: fast device, USB 2 port or cable.", short(&u.name));
            c.imp = if u.kind == UsbKind::Storage {
                imp(&[("Copies", 1.0, "40 vs 400 MB/s"), ("Backup", 0.7, "10× longer"), ("Games", 0.0, "—")])
            } else {
                imp(&[("Video", 0.7, "lower quality"), ("Speed", 0.6, "capped"), ("Games", 0.0, "—")])
            };
            out.push(c);
        } else if u.can_ssp && u.runs_ss && !u.runs_ssp {
            let mut c = Check::new("USB", label);
            c.now = "5G".into();
            c.max = "10G".into();
            c.frac = 0.5;
            c.tone = Tone::Info;
            c.fix = "10G port".into();
            c.head = "Half speed port".into();
            c.why = format!("{}: does 10 Gbps, port gives 5.", short(&u.name));
            c.imp = imp(&[("Copies", 0.5, "½ speed"), ("Games", 0.0, "—"), ("Office", 0.0, "—")]);
            out.push(c);
        } else if u.runs_ss && usb_ok.is_none() {
            usb_ok = Some(u.clone());
        }
    }
    if let Some(u) = usb_ok.filter(|_| !out.iter().any(|c| c.tag == "USB")) {
        let mut c = Check::new("USB", "USB drive");
        c.now = if u.runs_ssp { "10G".into() } else { "5G".into() };
        c.max = "Gbps".into();
        c.head = "Fast port".into();
        c.why = format!("{} on a USB 3 port.", short(&u.name));
        out.push(c);
    }

    // ---- network
    for n in f.nets.iter().filter(|n| n.a.kind == "Ethernet" && n.a.up) {
        let link = n.a.link_down_bps.max(n.a.link_up_bps);
        if link == 0 {
            continue;
        }
        let mut c = Check::new("LAN", "Ethernet");
        c.now = mbps(link);
        if n.max_bps >= 1_000_000_000 && link <= 100_000_000 {
            c.max = mbps(n.max_bps);
            c.frac = link as f32 / n.max_bps as f32;
            c.tone = Tone::Bad;
            c.fix = "Swap cable".into();
            c.head = format!("Link at {}", mbps(link));
            c.why = format!("PC does {}. Worn cable likely.", mbps(n.max_bps));
            c.imp = imp(&[("Downloads", 1.0, "12 MB/s cap"), ("Games", 0.1, "ping same"), ("Stream", 0.2, "ok")]);
        } else if n.max_bps > link {
            c.max = mbps(n.max_bps);
            c.frac = link as f32 / n.max_bps as f32;
            c.tone = Tone::Info;
            c.fix = "Fine".into();
            c.head = "Router sets the speed".into();
            c.why = format!("PC does {}. Router or switch gives {}.", mbps(n.max_bps), mbps(link));
            c.imp = imp(&[("Downloads", 0.2, "only above plan"), ("Games", 0.0, "—"), ("Stream", 0.0, "—")]);
        } else {
            c.max = "Mbps".into();
            c.head = "Full link".into();
            c.why = format!("{} Mbps.", link / 1_000_000);
        }
        out.push(c);
    }

    // ---- power mode
    if let Some(p) = &f.power {
        let mut c = Check::new("PWR", "Power mode");
        let saver = p.overlay == "Best power efficiency" || p.plan.to_ascii_lowercase().contains("saver");
        if saver && f.on_ac != Some(false) {
            c.now = "Eco".into();
            c.max = "Bal".into();
            c.frac = 0.6;
            c.tone = Tone::Warn;
            c.fix = "Change mode".into();
            c.head = "Power saver, plugged in".into();
            c.why = "Processor held back to save power.".into();
            c.act = Act::Open("ms-settings:powersleep");
            c.act_label = "Open power".into();
            c.imp = imp(&[("Games", 0.5, "+10–20%"), ("Apps", 0.3, "snappier"), ("Battery", 0.0, "n/a")]);
        } else {
            c.now = "Full".into();
            c.max = "".into();
            c.head = "Full power".into();
            c.why = if p.overlay.is_empty() { p.plan.clone() } else { p.overlay.to_string() };
        }
        out.push(c);
    }
    out
}

fn short(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "Device".into();
    }
    if s.chars().count() > 28 {
        format!("{}…", s.chars().take(27).collect::<String>())
    } else {
        s.to_string()
    }
}

// ------------------------------------------------------------------ driver flow

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drv {
    Idle,
    Searching,
    /// Download and install running as admin; the step from the helper.
    Working,
    Done,
    /// Windows Update has nothing: offer the maker's app.
    NoneFound,
    Cancelled,
    Failed,
}

/// Page state kept while the app runs (not cleared with the Monitor history).
pub struct SpeedState {
    probe: Probe,
    pub facts: Option<Arc<Facts>>,
    pub checks: Vec<Check>,
    pub sel: usize,
    drv: Arc<Mutex<Drv>>,
    pub step: String,
    pub when: Option<Instant>,
    /// Layout (content coordinates).
    pub hdr: Rect,
    pub banner: Option<Rect>,
    pub cards: Vec<Rect>,
    pub det: Option<Rect>,
    pub cant: Option<Rect>,
    pub scan_btn: Rect,
    pub act_btn: Option<Rect>,
    pub drv_btn: Option<Rect>,
    t0: Instant,
}

impl Default for SpeedState {
    fn default() -> Self {
        SpeedState {
            probe: Probe::default(),
            facts: None,
            checks: Vec::new(),
            sel: usize::MAX,
            drv: Arc::new(Mutex::new(Drv::Idle)),
            step: String::new(),
            when: None,
            hdr: Rect::default(),
            banner: None,
            cards: Vec::new(),
            det: None,
            cant: None,
            scan_btn: Rect::default(),
            act_btn: None,
            drv_btn: None,
            t0: Instant::now(),
        }
    }
}

impl SpeedState {
    pub fn drv(&self) -> Drv {
        self.drv.lock().map(|g| g.clone()).unwrap_or(Drv::Idle)
    }
    fn set_drv(&self, d: Drv) {
        if let Ok(mut g) = self.drv.lock() {
            *g = d;
        }
    }
    pub fn missing(&self) -> Vec<Missing> {
        self.facts.as_ref().map(|f| f.missing.clone()).unwrap_or_default()
    }
    pub fn scanning(&self) -> bool {
        self.probe.running()
    }
}

impl Monitor {
    /// The Full speed page is on screen: start the first read, pick up a finished one.
    /// True when the page must be laid out again.
    pub fn speed_tick(&mut self) -> bool {
        let sp = &mut self.sp;
        if !sp.probe.started {
            sp.probe.start();
        }
        let mut changed = false;
        if let Some(f) = sp.probe.latest() {
            if sp.facts.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &f)) {
                sp.checks = checks(&f);
                sp.facts = Some(f);
                sp.when = Some(Instant::now());
                if sp.sel >= sp.checks.len() || sp.sel == usize::MAX {
                    sp.sel = sp.checks.iter().position(Check::held).unwrap_or(0);
                }
                changed = true;
            }
        }
        if sp.drv() == Drv::Working {
            let s = driver::read_progress();
            if s != sp.step {
                sp.step = s;
                changed = true;
            }
        }
        changed || sp.probe.running()
    }

    pub fn speed_scan(&mut self) {
        self.sp.probe.started = false;
        self.sp.probe.start();
    }

    pub fn speed_pick(&mut self, i: usize) {
        self.sp.sel = i;
    }

    /// The action button of the selected part.
    pub fn speed_act(&mut self) {
        let Some(c) = self.sp.checks.get(self.sp.sel) else { return };
        match c.act.clone() {
            Act::SetHz(gdi, hz) => {
                crate::sensors::deep::speed::set_refresh(&gdi, hz);
                self.speed_scan();
            }
            Act::Open(url) => crate::util::shell_open(url, None),
            Act::None => {
                if c.tag == "GPU" && c.tone == Tone::Bad {
                    self.speed_driver();
                }
            }
        }
    }

    /// "Get driver": search Windows Update, install as admin, or open the maker's page.
    pub fn speed_driver(&mut self) {
        let missing = self.sp.missing();
        match self.sp.drv() {
            Drv::Searching | Drv::Working => {}
            Drv::NoneFound | Drv::Failed => {
                let v = missing.iter().find(|m| m.gpu).map_or(0, |m| m.vendor);
                crate::util::shell_open(driver::vendor_url(v).unwrap_or("ms-settings:windowsupdate-optionalupdates"), None);
            }
            Drv::Done => self.speed_scan(),
            Drv::Idle | Drv::Cancelled => {
                if missing.is_empty() {
                    return;
                }
                self.sp.set_drv(Drv::Searching);
                self.sp.step.clear();
                let st = self.sp.drv.clone();
                let _ = std::thread::Builder::new().name("driver-fix".into()).spawn(move || {
                    let set = |d: Drv| {
                        if let Ok(mut g) = st.lock() {
                            *g = d;
                        }
                    };
                    match driver::search(&missing) {
                        Ok(v) if v.is_empty() => set(Drv::NoneFound),
                        Err(_) => set(Drv::NoneFound),
                        Ok(_) => {
                            set(Drv::Working);
                            set(match driver::install_elevated(&missing) {
                                None => Drv::Cancelled,
                                Some(0) => Drv::Done,
                                Some(3) => Drv::NoneFound,
                                Some(_) => Drv::Failed,
                            });
                        }
                    }
                });
            }
        }
    }

    pub fn speed_animating(&self) -> bool {
        self.sp.probe.running() || matches!(self.sp.drv(), Drv::Searching | Drv::Working)
    }

    /// Lay the page out from `y0`; returns the bottom.
    pub(super) fn speed_layout(&mut self, l: i32, r: i32, y0: i32) -> i32 {
        let dpi = self.dpi as i32;
        let s = |v: i32| (v * dpi + 48) / 96;
        let gap = s(10);
        let w = r - l;
        let mut y = y0;
        self.hits.clear();
        let sp = &mut self.sp;
        sp.hdr = Rect { l, t: y, r, b: y + s(64) };
        sp.scan_btn = Rect { l: r - s(96), t: y + s(18), r, b: y + s(18) + s(32) };
        self.hits.push((sp.scan_btn, Hit::SpScan));
        y = sp.hdr.b + s(12);
        sp.banner = None;
        sp.drv_btn = None;
        if !sp.missing().is_empty() {
            let b = Rect { l, t: y, r, b: y + s(70) };
            let bw = s(150);
            let btn = Rect { l: r - s(16) - bw, t: b.t + s(19), r: r - s(16), b: b.t + s(19) + s(32) };
            sp.banner = Some(b);
            sp.drv_btn = Some(btn);
            self.hits.push((btn, Hit::SpDriver));
            y = b.b + gap;
        }
        let min_w = s(214);
        let cols = ((w + gap) / (min_w + gap)).clamp(1, 6);
        let cw = (w - gap * (cols - 1)) / cols;
        let ch = s(118);
        sp.cards.clear();
        for i in 0..sp.checks.len() {
            let (cx, cy) = ((i as i32) % cols, (i as i32) / cols);
            let rc = Rect { l: l + cx * (cw + gap), t: y + cy * (ch + gap), r: l + cx * (cw + gap) + cw, b: y + cy * (ch + gap) + ch };
            sp.cards.push(rc);
            self.hits.push((rc, Hit::SpPick(i as u8)));
        }
        if !sp.checks.is_empty() {
            let rows = (sp.checks.len() as i32 + cols - 1) / cols;
            y += rows * (ch + gap);
        }
        sp.det = None;
        sp.act_btn = None;
        if let Some(c) = sp.checks.get(sp.sel) {
            let wide = w >= s(620);
            let h = if wide { s(150) } else { s(150) + s(26) * c.imp.len() as i32 + s(16) };
            let d = Rect { l, t: y, r, b: y + h };
            sp.det = Some(d);
            let has_act = !c.act_label.is_empty() || (c.tag == "GPU" && c.tone == Tone::Bad);
            if has_act {
                let btn = Rect { l: l + s(18), t: d.t + s(102), r: l + s(18) + s(148), b: d.t + s(102) + s(32) };
                sp.act_btn = Some(btn);
                self.hits.push((btn, Hit::SpAct));
            }
            y = d.b + gap;
        }
        sp.cant = Some(Rect { l, t: y, r, b: y + s(26) });
        y + s(26) + s(8)
    }

    pub(super) fn speed_draw(&self, cv: &mut Canvas, f: &Fonts, col: &Colors, dy: i32, hot: Option<usize>) {
        let d = Draw { m: self, f, col: *col, dx: 0, dy, hot, track: col.line, sub: col.fg.with_a(if col.dark { 10 } else { 14 }) };
        d.speed(cv);
    }
}

impl Draw<'_> {
    fn tok(&self) -> crate::look::Tokens {
        crate::look::tokens(self.dark())
    }

    fn tone_col(&self, t: Tone) -> Rgba {
        let k = self.tok();
        match t {
            Tone::Bad => k.crit,
            Tone::Warn => k.warn,
            Tone::Info => k.dim,
            Tone::Good => k.good,
        }
    }

    /// A pill button in the look's shape; `primary` fills it with the accent.
    fn sp_button(&self, cv: &mut Canvas, r: Rect, text: &str, primary: bool, hit: Hit) {
        let k = self.tok();
        let r = self.sh(r);
        let rad = self.sf(k.r_ctl.min(r.h() / 2) as f32).max(2.0);
        let rad = if k.look == crate::look::Look::Bento { r.h() as f32 / 2.0 } else { rad };
        let hot = self.is_hot(hit);
        let (bg, fg) = if primary { (k.accent_fill, k.on_accent) } else { (k.raised, k.fg) };
        let bg = if hot { bg.mix(k.fg, 0.12) } else { bg };
        cv.fill_round(r, rad, bg);
        if !primary {
            cv.stroke_round(r, rad, k.border);
        }
        let t = crate::look::label(text);
        let (tw, th) = cv.measure(self.f.cap, &t);
        cv.text(self.f.cap, &t, (r.l + r.r - tw) / 2, (r.t + r.b - th) / 2, fg);
    }

    fn speed(&self, cv: &mut Canvas) {
        let m = self.m;
        let sp = &m.sp;
        let k = self.tok();
        let s = |v: i32| self.s(v);
        let tiny = |cv: &mut Canvas, t: &str, x: i32, y: i32, c: Rgba| cv.text(self.f.tiny, &crate::look::label(t), x, y, c);

        // ---- header: held-back count, scan
        let h = self.sh(sp.hdr);
        let held = sp.checks.iter().filter(|c| c.held()).count();
        let big = if sp.checks.is_empty() { "--".to_string() } else { held.to_string() };
        let bw = cv.numeral(m.num_font, &big, h.l, h.t + s(4), if held > 0 { k.fg } else { k.good });
        let sub = format!(" / {}", sp.checks.len());
        let sw = cv.text(m.num_sm_font, &sub, h.l + bw, h.t + s(12), k.dim);
        tiny(cv, "Held back", h.l + bw + sw + s(10), h.t + s(28), k.dim);
        let status = if sp.scanning() {
            "Checking…".to_string()
        } else if let Some(t) = sp.when {
            let m = t.elapsed().as_secs() / 60;
            if m == 0 { "Checked now".into() } else { format!("Checked {m} min ago") }
        } else {
            String::new()
        };
        let st = crate::look::label(&status);
        let stw = cv.measure(self.f.tiny, &st).0;
        let scan = self.sh(sp.scan_btn);
        cv.text(self.f.tiny, &st, scan.l - s(12) - stw, scan.t + s(10), k.dim);
        self.sp_button(cv, sp.scan_btn, "Scan", false, Hit::SpScan);
        if sp.scanning() {
            // A sweep under the header while the read runs.
            let bar = Rect { l: h.l, t: h.b + s(2), r: h.r, b: h.b + s(5) };
            cv.fill_round(bar, self.sf(1.5), k.track);
            let t = sp.t0.elapsed().as_secs_f32() % 1.4 / 1.4;
            let w = bar.w() * 3 / 10;
            let x = bar.l - w + ((bar.w() + w) as f32 * t) as i32;
            let seg = Rect { l: x.max(bar.l), r: (x + w).min(bar.r), ..bar };
            if seg.r > seg.l {
                cv.fill_round(seg, self.sf(1.5), k.accent_fill);
            }
        }

        // ---- missing driver banner
        if let (Some(b), Some(btn)) = (sp.banner, sp.drv_btn) {
            let b = self.sh(b);
            let bento = k.look == crate::look::Look::Bento;
            let rad = self.sf(k.r_card as f32);
            let (bg, fg, dim) = if bento { (k.alt, k.alt_fg, k.alt_dim) } else { (k.crit.with_a(if self.dark() { 38 } else { 30 }), k.fg, k.dim) };
            cv.fill_round(b, rad, if bento { bg } else { k.panel });
            if !bento {
                cv.fill_round(b, rad, bg);
                cv.stroke_round(b, rad, k.crit.with_a(110));
            }
            let ic = Rect { l: b.l + s(16), t: b.t + s(18), r: b.l + s(16) + s(34), b: b.t + s(18) + s(34) };
            cv.fill_round(ic, self.sf(k.r_ctl as f32).max(3.0), k.crit);
            let (iw, ih) = cv.measure(self.f.semi, "!");
            cv.text(self.f.semi, "!", (ic.l + ic.r - iw) / 2, (ic.t + ic.b - ih) / 2, Rgba(255, 255, 255, 255));
            let miss = sp.missing();
            let title = match miss.iter().find(|x| x.gpu) {
                Some(_) if miss.len() == 1 => "GPU driver missing".to_string(),
                Some(_) => format!("GPU + {} more drivers missing", miss.len() - 1),
                None if miss.len() == 1 => "1 driver missing".to_string(),
                None => format!("{} drivers missing", miss.len()),
            };
            let x = ic.r + s(14);
            cv.text(self.f.semi, &crate::look::label(&title), x, b.t + s(14), fg);
            let line = match sp.drv() {
                Drv::Idle => short(&miss.iter().map(|m| m.name.clone()).collect::<Vec<_>>().join(", ")),
                Drv::Searching => "Searching Windows Update…".into(),
                Drv::Working => match sp.step.as_str() {
                    "install" => "Installing (signed by Microsoft)…".into(),
                    _ => "Downloading…".into(),
                },
                Drv::Done => "Installed. Restart if Windows asks.".into(),
                Drv::NoneFound => "Not on Windows Update. Use the maker's app.".into(),
                Drv::Cancelled => "Admin prompt closed.".into(),
                Drv::Failed => "Install failed. Try the maker's app.".into(),
            };
            let maxw = self.sh(btn).l - s(12) - x;
            let line = cv.ellipsize(self.f.small, &line, maxw.max(s(40)));
            cv.text(self.f.small, &line, x, b.t + s(36), dim);
            // Steps: find, download, install.
            let steps = ["Find", "Download", "Install"];
            let at = match (sp.drv(), sp.step.as_str()) {
                (Drv::Searching, _) => 0,
                (Drv::Working, "install") => 2,
                (Drv::Working, _) => 1,
                (Drv::Done, _) => 3,
                _ => usize::MAX,
            };
            let mut sx = x;
            let sy = b.t + s(52);
            if b.h() >= s(66) && maxw > s(180) {
                for (i, t) in steps.iter().enumerate() {
                    let t = crate::look::label(t);
                    let (tw, th) = cv.measure(self.f.tiny, &t);
                    let pr = Rect { l: sx, t: sy, r: sx + tw + s(12), b: sy + th + s(4) };
                    let (pb, pf) = if at != usize::MAX && i < at {
                        (k.good, Rgba(255, 255, 255, 255))
                    } else if i == at {
                        (k.accent_fill, k.on_accent)
                    } else {
                        (dim.with_a(40), dim)
                    };
                    cv.fill_round(pr, self.sf(k.r_ctl.min(pr.h() / 2) as f32).max(2.0), pb);
                    cv.text(self.f.tiny, &t, pr.l + s(6), pr.t + s(2), pf);
                    sx = pr.r + s(5);
                }
            }
            let label = match sp.drv() {
                Drv::Idle | Drv::Cancelled => "Get driver".to_string(),
                Drv::Searching | Drv::Working => "Working…".into(),
                Drv::Done => "Scan again".into(),
                Drv::NoneFound | Drv::Failed => {
                    let v = miss.iter().find(|m| m.gpu).map_or(0, |m| m.vendor);
                    let n = driver::vendor_name(v);
                    if n.is_empty() { "Windows Update".into() } else { format!("Open {n} app") }
                }
            };
            self.sp_button(cv, btn, &label, true, Hit::SpDriver);
        }

        // ---- part cards
        let rad = self.sf(k.r_card as f32);
        let lcd = k.numerals == crate::look::Numerals::Lcd;
        for (i, (c, r)) in sp.checks.iter().zip(&sp.cards).enumerate() {
            let r = self.sh(*r);
            let tone = self.tone_col(c.tone);
            let sel = i == sp.sel;
            let hot = self.is_hot(Hit::SpPick(i as u8));
            cv.fill_round(r, rad, if hot && !sel { k.raised } else { k.panel });
            if sel {
                cv.stroke_round(r, rad, k.accent);
                cv.stroke_round(Rect { l: r.l + 1, t: r.t + 1, r: r.r - 1, b: r.b - 1 }, (rad - 1.0).max(0.0), k.accent);
            } else {
                cv.stroke_round(r, rad, k.border);
            }
            let x = r.l + s(12);
            let tw = tiny(cv, c.tag, x, r.t + s(12), k.dim);
            let name = cv.ellipsize(self.f.cap, &crate::look::label(&c.name), r.r - s(32) - (x + tw + s(8)));
            cv.text(self.f.cap, &name, x + tw + s(8), r.t + s(10), k.fg);
            let dot = Rect { l: r.r - s(20), t: r.t + s(13), r: r.r - s(11), b: r.t + s(22) };
            let dr = if k.look == crate::look::Look::Lime { self.sf(1.0) } else { self.sf(4.5) };
            cv.fill_round(dot, dr, tone);
            // Value: big now, small "/ max".
            let vy = r.t + s(34);
            let (ink, vx) = if lcd {
                let well = Rect { l: x, t: vy - s(2), r: r.r - s(12), b: vy + s(32) };
                cv.fill_round(well, self.sf(k.r_ctl as f32), k.lcd);
                (k.lcd_ink, x + s(8))
            } else {
                (k.fg, x)
            };
            let nw = cv.numeral(m.num_sm_font, &c.now, vx, vy, ink);
            if !c.max.is_empty() {
                let mx = format!(" / {}", c.max);
                cv.text(self.f.small, &mx, vx + nw, vy + s(12), if lcd { ink.with_a(150) } else { k.dim });
            }
            let mr = Rect { l: x, t: r.t + s(74), r: r.r - s(12), b: r.t + s(74) + s(11) };
            cv.meter(mr, c.frac, tone, k.track);
            let fix = crate::look::label(&c.fix);
            let fix = cv.ellipsize(self.f.cap, &fix, r.w() - s(24));
            cv.text(self.f.cap, &fix, x, r.t + s(92), k.fg);
        }

        // ---- detail of the selected part
        if let (Some(dr), Some(c)) = (sp.det, sp.checks.get(sp.sel)) {
            let dr = self.sh(dr);
            cv.fill_round(dr, rad, k.panel);
            cv.stroke_round(dr, rad, k.border);
            let x = dr.l + s(18);
            tiny(cv, &format!("{} \u{00B7} {}", c.tag, c.name), x, dr.t + s(16), k.dim);
            let head = cv.ellipsize(self.f.head, &crate::look::label(&c.head), dr.w() / if dr.w() >= s(620) { 2 } else { 1 } - s(30));
            cv.text(self.f.head, &head, x, dr.t + s(34), k.fg);
            if !c.why.is_empty() {
                let why = cv.ellipsize(self.f.small, &c.why, dr.w() / if dr.w() >= s(620) { 2 } else { 1 } - s(30));
                cv.text(self.f.small, &why, x, dr.t + s(68), k.dim);
            }
            if let Some(btn) = sp.act_btn {
                let label = if c.act_label.is_empty() { "Get driver".to_string() } else { c.act_label.clone() };
                self.sp_button(cv, btn, &label, c.held(), Hit::SpAct);
            }
            // "Does it matter?" meters: right half when wide, below when narrow.
            let wide = dr.w() >= s(620);
            let (il, it) = if wide { (dr.l + dr.w() / 2 + s(10), dr.t + s(26)) } else { (x, dr.t + s(150)) };
            let ir = dr.r - s(18);
            let tone = self.tone_col(c.tone);
            if c.imp.is_empty() {
                tiny(cv, "Nothing to fix", il, it + s(6), k.dim);
            }
            for (j, (what, frac, txt)) in c.imp.iter().enumerate() {
                let y = it + j as i32 * s(30);
                let lw = s(76);
                tiny(cv, what, il, y + s(2), k.dim);
                let tw = cv.measure(self.f.small, txt).0.min(s(130));
                let txt = cv.ellipsize(self.f.small, txt, s(130));
                cv.text(self.f.small, &txt, ir - tw, y, k.fg);
                let mr = Rect { l: il + lw, t: y + s(3), r: ir - tw - s(12), b: y + s(3) + s(10) };
                if mr.w() > s(30) {
                    cv.meter(mr, *frac, if *frac > 0.0 { tone } else { k.track }, k.track);
                }
            }
        }

        // ---- what Windows cannot tell us
        if let Some(cr) = sp.cant {
            let cr = self.sh(cr);
            let mut x = cr.l;
            let lab = crate::look::label("Can't see");
            for (i, t) in [lab.as_str(), "PSU", "Charger watts", "Cable type", "DSC"].iter().enumerate() {
                let t = if i == 0 { t.to_string() } else { crate::look::label(t) };
                let (tw, th) = cv.measure(self.f.tiny, &t);
                let pr = Rect { l: x, t: cr.t, r: x + tw + s(14), b: cr.t + th + s(8) };
                if pr.r > cr.r {
                    break;
                }
                if i > 0 {
                    cv.stroke_round(pr, self.sf(k.r_ctl.min(pr.h() / 2) as f32).max(2.0), k.border);
                }
                cv.text(self.f.tiny, &t, pr.l + s(7), pr.t + s(4), k.dim);
                x = pr.r + s(6);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::deep::display::Screen;
    use crate::sensors::deep::gpu::{Adapter, PcieLink};
    use crate::sensors::deep::speed::{DriveSpeed, NetSpeed, ScreenSpeed, UsbDev};
    use crate::sensors::hw::Dimm;

    fn screen(hz: f32, conn: &'static str, luid: u64) -> Screen {
        Screen {
            name: "Mon".into(),
            gdi: "\\\\.\\DISPLAY1".into(),
            primary: true,
            w: 2560,
            h: 1440,
            hz,
            connector: conn,
            adapter_luid: luid,
            bits: None,
            encoding: "",
            hdr_supported: false,
            hdr_on: false,
            sdr_white_nits: None,
            max_nits: None,
            max_full_frame_nits: None,
            min_nits: None,
            primaries: None,
            dpi_scale: None,
            edid: None,
        }
    }

    fn dgpu(pcie: Option<PcieLink>) -> Adapter {
        Adapter { luid: 2, name: "RTX".into(), vendor_id: 0x10DE, dedicated: 8 << 30, pcie, ..Default::default() }
    }

    fn find<'a>(v: &'a [Check], tag: &str) -> &'a Check {
        v.iter().find(|c| c.tag == tag).unwrap_or_else(|| panic!("no {tag}"))
    }

    #[test]
    fn part_numbers() {
        assert_eq!(part_speed("CMK32GX4M2D3600C18"), 3600);
        assert_eq!(part_speed("F4-3200C16-8GVKB"), 3200);
        assert_eq!(part_speed("F5-6000J3038F16G"), 6000);
        assert_eq!(part_speed("KF436C17BBK2/16"), 3600);
        assert_eq!(part_speed("M378A1K43CB2-CTD"), 0);
        assert_eq!(part_speed("HMA81GU6CJR8N-VK"), 0);
    }

    #[test]
    fn gpu_lanes() {
        let f = Facts { gpus: vec![dgpu(Some(PcieLink { gen: 4, width: 8, max_gen: 4, max_width: 16 }))], ..Default::default() };
        let c = checks(&f);
        let g = find(&c, "GPU");
        assert_eq!((g.now.as_str(), g.max.as_str(), g.tone), ("x8", "x16", Tone::Warn));
        // An x8 card at x8, Gen 1 at idle: fine.
        let f = Facts { gpus: vec![dgpu(Some(PcieLink { gen: 1, width: 8, max_gen: 4, max_width: 8 }))], ..Default::default() };
        assert_eq!(find(&checks(&f), "GPU").tone, Tone::Good);
    }

    #[test]
    fn refresh_setting_vs_cable() {
        let f = Facts { screens: vec![ScreenSpeed { s: screen(60.0, "DisplayPort", 2), offered: 144 }], ..Default::default() };
        let m = checks(&f);
        let c = find(&m, "MON");
        assert_eq!(c.tone, Tone::Bad);
        assert_eq!(c.act, Act::SetHz("\\\\.\\DISPLAY1".into(), 144));
        let mut s = screen(60.0, "HDMI", 2);
        s.edid = Some(crate::sensors::deep::display::Edid { range: Some((48, 144)), ..Default::default() });
        let f = Facts { screens: vec![ScreenSpeed { s, offered: 60 }], ..Default::default() };
        let m = checks(&f);
        let c = find(&m, "MON");
        assert_eq!((c.tone, c.fix.as_str()), (Tone::Warn, "Try DisplayPort"));
        let f = Facts { screens: vec![ScreenSpeed { s: screen(143.9, "DisplayPort", 2), offered: 144 }], ..Default::default() };
        assert_eq!(find(&checks(&f), "MON").tone, Tone::Good);
    }

    #[test]
    fn monitor_on_motherboard() {
        let igpu = Adapter { luid: 1, integrated: true, ..Default::default() };
        let f = Facts { gpus: vec![igpu, dgpu(None)], screens: vec![ScreenSpeed { s: screen(60.0, "HDMI", 1), offered: 60 }], ..Default::default() };
        assert_eq!(find(&checks(&f), "CBL").tone, Tone::Bad);
        // Laptops drive the screen from the built-in GPU on purpose.
        let f = Facts { battery: true, ..f };
        assert!(checks(&f).iter().all(|c| c.tag != "CBL"));
    }

    #[test]
    fn memory() {
        let st = |slot: &str, size: u64, speed: u32, rated: u32, part: &str| Dimm { slot: slot.into(), size, speed, rated, part: part.into(), maker: String::new() };
        let mut f = Facts::default();
        f.hw.dimms = vec![st("A1", 16 << 30, 2133, 2133, "CMK32GX4M2D3600C18"), st("A2", 0, 0, 0, ""), st("B1", 0, 0, 0, ""), st("B2", 0, 0, 0, "")];
        let c = checks(&f);
        assert_eq!(find(&c, "RAM").tone, Tone::Warn);
        assert_eq!(find(&c, "RAM").max, "3600");
        assert_eq!(find(&c, "DIM").tone, Tone::Warn);
        f.hw.dimms = vec![st("A2", 16 << 30, 3600, 3600, ""), st("B2", 16 << 30, 3600, 3600, "")];
        let c = checks(&f);
        assert_eq!((find(&c, "RAM").tone, find(&c, "DIM").tone), (Tone::Good, Tone::Good));
    }

    #[test]
    fn ssd_and_usb_and_lan() {
        let mut f = Facts::default();
        f.drives = vec![DriveSpeed { d: Default::default(), pcie: Some((3, 4, 4, 4)) }];
        f.usb = vec![UsbDev { name: "Samsung T7".into(), kind: UsbKind::Storage, can_ss: true, runs_ss: false, ..Default::default() }];
        let mut a = crate::sensors::deep::net::AdapterFacts::default();
        a.kind = "Ethernet";
        a.up = true;
        a.link_down_bps = 100_000_000;
        f.nets = vec![NetSpeed { a, max_bps: 1_000_000_000 }];
        let c = checks(&f);
        assert_eq!(find(&c, "NVM").tone, Tone::Info);
        assert_eq!(find(&c, "USB").tone, Tone::Bad);
        let lan = find(&c, "LAN");
        assert_eq!((lan.tone, lan.now.as_str(), lan.max.as_str()), (Tone::Bad, "100", "1G"));
    }

    #[test]
    fn power_and_driver() {
        let mut f = Facts { on_ac: Some(true), ..Default::default() };
        f.power = Some(crate::sensors::deep::cpu::PowerMode { overlay: "Best power efficiency", plan: "Balanced".into() });
        f.missing = vec![Missing { name: "NVIDIA graphics".into(), vendor: 0x10DE, gpu: true, hwids: vec![] }];
        let c = checks(&f);
        assert_eq!(find(&c, "PWR").tone, Tone::Warn);
        assert_eq!(find(&c, "GPU").fix, "Get driver");
        // On battery the saver is the point.
        f.on_ac = Some(false);
        assert_eq!(find(&checks(&f), "PWR").tone, Tone::Good);
    }
}
