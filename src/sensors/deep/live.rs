//! Live values of the open device page, read on the sensor thread with the Monitor detail.
//! Only the open page's sources run: its PDH counters are attached when it opens and
//! removed when it closes, and nothing here runs while the Monitor page is hidden.
//!
//! The one exception is the network pinger: once the Network page has been opened it keeps
//! pinging (two tiny ICMP echoes a second, on its own thread) so the outage log covers the
//! whole session, not only the minutes the page was on screen.

use super::{battery, bluetooth, cpu, etw, gpu, net, reader::Reader, refresh::Meter, AppNet, DeepLive, Dev};
use std::sync::{Arc, Mutex};
use crate::sensors::pdh::{Counter, Query};

/// Re-read static facts this often while a page stays open.
const STATIC_SECS: f64 = 60.0;
/// Connections and Wi-Fi are re-read this often.
const NET_SECS: f64 = 2.0;
/// Paired Bluetooth devices are re-read this often on the overview (battery levels move
/// in 10% steps; Windows itself refreshes them rarely)...
const BT_SECS: f64 = 30.0;
/// ...and this often while the Bluetooth page is open.
const BT_PAGE_SECS: f64 = 10.0;

#[derive(Default)]
struct Counters {
    // CPU page
    freq: Option<Counter>,
    perf: Option<Counter>,
    intr: Option<Counter>,
    dpc: Option<Counter>,
    ctx: Option<Counter>,
    queue: Option<Counter>,
    // GPU page
    ded: Option<Counter>,
    shr: Option<Counter>,
    eng3d: Option<Counter>,
}

/// What a device kind needs attached.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Cpu,
    Gpu,
    Other,
}

fn group(d: Dev) -> Group {
    match d {
        Dev::Cpu => Group::Cpu,
        Dev::Gpu => Group::Gpu,
        _ => Group::Other,
    }
}

/// The ETW session for per-app bytes. Starting and stopping it can block for a second or
/// two, so both happen on a short-lived thread, never on the sensor beat.
#[derive(Default)]
enum Etw {
    #[default]
    Off,
    Starting(Arc<Mutex<Option<Result<etw::NetEtw, etw::StartError>>>>),
    On(etw::NetEtw),
    /// Refused for lack of admin rights: not retried until the app restarts elevated.
    Denied,
    /// Failed otherwise: tried again the next time the page opens.
    Failed,
}

#[derive(Default)]
pub struct LiveSensors {
    dev: Option<Dev>,
    c: Counters,
    buf: Vec<u64>,
    rows: Vec<(String, f64)>,
    sets: cpu::CpuSets,
    reader: Reader,
    /// Seconds since statics were asked for (None = not since the page opened).
    static_age: Option<f64>,
    /// Drive numbers the last static read covered.
    disks: Vec<u32>,
    meter: Meter,
    pinger: Option<net::Pinger>,
    conns: net::Conns,
    wlan: Option<net::Wlan>,
    net_age: f64,
    bats: Option<battery::Batteries>,
    bat: Vec<battery::Battery>,
    etw: Etw,
    /// The Bluetooth module is on (set before each sample).
    pub bt_on: bool,
    bt: std::sync::Arc<Vec<bluetooth::BtDevice>>,
    /// Seconds since the last Bluetooth read (None = never, or the module was off).
    bt_age: Option<f64>,
}

fn utf16(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

impl LiveSensors {
    /// Attach the counters the open page needs (None = Monitor hidden or overview).
    pub fn sync(&mut self, q: &Query, dev: Option<Dev>) {
        let was = self.dev.map(group);
        let now = dev.map(group);
        if dev.is_none() {
            self.static_age = None;
        }
        if dev != self.dev {
            self.leave(self.dev, dev);
            if dev == Some(Dev::Bluetooth) {
                self.bt_now();
            }
        }
        self.dev = dev;
        if was == now {
            return;
        }
        self.c = Counters::default();
        let c = &mut self.c;
        match now {
            Some(Group::Cpu) => {
                c.freq = q.add(r"\Processor Information(*)\Processor Frequency");
                c.perf = q.add(r"\Processor Information(*)\% Processor Performance");
                c.intr = q.add(r"\Processor Information(_Total)\% Interrupt Time");
                c.dpc = q.add(r"\Processor Information(_Total)\% DPC Time");
                c.ctx = q.add(r"\System\Context Switches/sec");
                c.queue = q.add(r"\System\Processor Queue Length");
            }
            Some(Group::Gpu) => {
                c.ded = q.add(r"\GPU Process Memory(*)\Dedicated Usage");
                c.shr = q.add(r"\GPU Process Memory(*)\Shared Usage");
                c.eng3d = q.add(r"\GPU Engine(*engtype_3D)\Utilization Percentage");
            }
            _ => {}
        }
    }

    /// Stop what only the page being left needed.
    fn leave(&mut self, old: Option<Dev>, new: Option<Dev>) {
        if matches!(old, Some(Dev::Display(_))) && !matches!(new, Some(Dev::Display(_))) {
            self.meter.set(None);
        }
        if old == Some(Dev::Battery) {
            self.bats = None;
            self.bat.clear();
        }
        if old == Some(Dev::Net) {
            self.wlan = None;
            // A start still running drops its session on its own thread when it lands.
            self.etw = match std::mem::take(&mut self.etw) {
                Etw::On(e) => {
                    let _ = std::thread::Builder::new().name("etw-stop".into()).spawn(move || drop(e));
                    Etw::Off
                }
                Etw::Denied => Etw::Denied,
                _ => Etw::Off,
            };
        }
    }

    /// Fill `out` after `Query::collect`. `disks` = PhysicalDrive numbers present now.
    pub fn sample(&mut self, elapsed: f64, disks: &[u32], out: &mut DeepLive) {
        self.bluetooth(elapsed);
        let Some(dev) = self.dev else {
            *out = DeepLive { st: self.reader.latest(), ping: self.pinger.as_ref().map(net::Pinger::snapshot), bt: self.bt.clone(), ..Default::default() };
            return;
        };

        // Static facts: on open, every minute, and when the drive list changes.
        let due = self.static_age.is_none_or(|a| a >= STATIC_SECS) || self.disks != disks;
        if due {
            self.static_age = Some(0.0);
            self.disks = disks.to_vec();
            self.reader.request(disks);
        } else if let Some(a) = &mut self.static_age {
            *a += elapsed;
        }
        // Only the open page's fields are filled; the rest clear so a stale page never shows.
        let old = std::mem::take(out);
        out.dev = Some(dev);
        out.st = self.reader.latest();
        out.ping = self.pinger.as_ref().map(net::Pinger::snapshot);
        out.bt = self.bt.clone();

        match dev {
            Dev::Cpu => self.cpu(out),
            Dev::Gpu => self.gpu(out),
            Dev::Display(i) => {
                let gdi = out.st.screens.get(i as usize).map(|s| s.gdi.clone());
                self.meter.set(gdi.as_deref());
                out.measured_hz = super::refresh::tidy(self.meter.hz());
            }
            Dev::Net => {
                if self.pinger.is_none() {
                    self.pinger = Some(net::Pinger::start());
                }
                self.net_age += elapsed;
                if self.net_age >= NET_SECS || old.dev != Some(Dev::Net) {
                    self.net_age = 0.0;
                    out.conns = self.conns.read();
                    out.wifi = self.wlan.get_or_insert_with(net::Wlan::new).read();
                } else {
                    out.conns = old.conns;
                    out.wifi = old.wifi;
                }
                self.app_net(elapsed, out);
            }
            Dev::Battery => {
                if self.bats.is_none() {
                    let mut b = battery::Batteries::open();
                    self.bat = b.statics();
                    self.bats = Some(b);
                } else if let Some(b) = &mut self.bats {
                    b.live(&mut self.bat);
                }
                out.batteries = self.bat.clone();
            }
            Dev::Mem | Dev::Disk(_) | Dev::Bluetooth => {}
        }
    }

    /// Re-read the paired Bluetooth devices when due (only while the Monitor page is on
    /// screen, since `sample` runs only then). One device-query call plus a short devnode
    /// walk: a few ms, every 10-30 s.
    fn bluetooth(&mut self, elapsed: f64) {
        if !self.bt_on {
            if self.bt_age.is_some() {
                self.bt_age = None;
                self.bt = std::sync::Arc::default();
            }
            return;
        }
        let every = if self.dev == Some(Dev::Bluetooth) { BT_PAGE_SECS } else { BT_SECS };
        match &mut self.bt_age {
            Some(a) if *a + elapsed < every => *a += elapsed,
            _ => {
                let _p = crate::perf::span("bluetooth");
                self.bt_age = Some(0.0);
                // CI has no Bluetooth radio: RESMON_BT_DEMO fills the page with example devices.
                let list = if std::env::var_os("RESMON_BT_DEMO").is_some() { bluetooth::demo() } else { bluetooth::read() };
                self.bt = std::sync::Arc::new(list);
            }
        }
    }

    /// Read the Bluetooth devices on the next sample (the page just opened).
    pub fn bt_now(&mut self) {
        if self.bt_age.is_some() {
            self.bt_age = Some(f64::MAX / 2.0);
        }
    }

    /// Per-app bytes: start the ETW session on first use, then read it every second.
    fn app_net(&mut self, elapsed: f64, out: &mut DeepLive) {
        match &mut self.etw {
            Etw::Off => {
                let slot: Arc<Mutex<Option<_>>> = Arc::default();
                let s = slot.clone();
                let ok = std::thread::Builder::new()
                    .name("etw-start".into())
                    .spawn(move || {
                        let r = etw::NetEtw::start();
                        *s.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    })
                    .is_ok();
                self.etw = if ok { Etw::Starting(slot) } else { Etw::Off };
                out.app_net_state = AppNet::Starting;
            }
            Etw::Starting(slot) => {
                let r = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                match r {
                    None => out.app_net_state = AppNet::Starting,
                    Some(Ok(e)) => {
                        self.etw = Etw::On(e);
                        out.app_net_state = AppNet::On;
                    }
                    Some(Err(etw::StartError::NeedsAdmin)) => {
                        self.etw = Etw::Denied;
                        out.app_net_state = AppNet::NeedsAdmin;
                    }
                    Some(Err(etw::StartError::Failed(_))) => {
                        self.etw = Etw::Failed;
                        out.app_net_state = AppNet::Failed;
                    }
                }
            }
            Etw::On(e) => {
                out.app_net = e.take(elapsed);
                out.app_net_state = AppNet::On;
            }
            Etw::Denied => out.app_net_state = AppNet::NeedsAdmin,
            Etw::Failed => out.app_net_state = AppNet::Failed,
        }
    }

    fn cpu(&mut self, out: &mut DeepLive) {
        let val = |c: &Option<Counter>| c.as_ref().and_then(Counter::value);
        // Effective clock per logical processor = nominal frequency x % performance.
        let mut freq: Vec<(u32, u32, f64)> = Vec::new();
        let mut perf: Vec<(u32, u32, f64)> = Vec::new();
        for (c, dst) in [(&self.c.freq, &mut freq), (&self.c.perf, &mut perf)] {
            if let Some(c) = c {
                c.for_each(&mut self.buf, |name, v| {
                    if let Some((g, i)) = utf16(name).split_once(',') {
                        if let (Ok(g), Ok(i)) = (g.parse::<u32>(), i.parse::<u32>()) {
                            dst.push((g, i, v));
                        }
                    }
                });
            }
        }
        freq.sort_by_key(|x| (x.0, x.1));
        perf.sort_by_key(|x| (x.0, x.1));
        out.core_mhz = freq
            .iter()
            .map(|f| {
                let p = perf.iter().find(|p| p.0 == f.0 && p.1 == f.1).map_or(100.0, |p| p.2);
                (f.2 * p / 100.0).max(0.0) as f32
            })
            .collect();
        out.parked = self.sets.parked();
        out.interrupt_pct = val(&self.c.intr).map(|v| v.clamp(0.0, 100.0) as f32);
        out.dpc_pct = val(&self.c.dpc).map(|v| v.clamp(0.0, 100.0) as f32);
        out.ctx_switches = val(&self.c.ctx).map(|v| v.max(0.0) as f32);
        out.queue = val(&self.c.queue).map(|v| v.max(0.0) as f32);
    }

    fn gpu(&mut self, out: &mut DeepLive) {
        let rows = |c: &Option<Counter>, buf: &mut Vec<u64>, rows: &mut Vec<(String, f64)>| {
            rows.clear();
            if let Some(c) = c {
                c.for_each(buf, |name, v| rows.push((utf16(name), v)));
            }
            gpu::sum_by_pid(rows.drain(..))
        };
        let ded = rows(&self.c.ded, &mut self.buf, &mut self.rows);
        let shr = rows(&self.c.shr, &mut self.buf, &mut self.rows);
        let e3d = rows(&self.c.eng3d, &mut self.buf, &mut self.rows);
        let mut mem: Vec<(u32, u64, u64, u64)> = ded.iter().map(|d| (d.0, d.1, d.2.max(0.0) as u64, 0)).collect();
        for s in &shr {
            match mem.iter_mut().find(|m| m.0 == s.0 && m.1 == s.1) {
                Some(m) => m.3 = s.2.max(0.0) as u64,
                None => mem.push((s.0, s.1, 0, s.2.max(0.0) as u64)),
            }
        }
        mem.retain(|m| m.0 != 0 && (m.2 > 0 || m.3 > 0));
        mem.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)));
        out.gpu_mem = mem;
        out.gpu_3d = e3d.into_iter().filter(|e| e.0 != 0 && e.2 > 0.0).map(|e| (e.0, e.1, e.2.clamp(0.0, 100.0) as f32)).collect();
        out.gpu_perf = out.st.gpus.iter().filter_map(|a| gpu::perf(a.luid).map(|p| (a.luid, p))).collect();
    }
}
