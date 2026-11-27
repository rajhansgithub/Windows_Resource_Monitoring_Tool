//! Network detail page: "Wi-Fi or ISP?" verdict, outage timeline, adapter facts, live
//! connections by app and Wi-Fi details.
//!
//! * `Pinger` is the single background thread of the detail pages: once a second it sends one
//!   ICMP echo to the default gateway and one to the internet (1.1.1.1, fallback 8.8.8.8) with
//!   `IcmpSendEcho` (no admin, no raw sockets) and derives a verdict and an outage list.
//! * `adapters()` reads adapter facts with `GetAdaptersAddresses` (static, call once).
//! * `Conns` reads the TCP connection table with owning PIDs (live, well under 2 ms).
//! * `Wlan` / `wifi()` read signal, channel, PHY and link rates through the Native Wifi API.
//!   Since Windows 11 24H2 the SSID and signal need location consent, see
//!   <https://learn.microsoft.com/en-us/windows/win32/nativewifi/wi-fi-access-location-changes>.
#![allow(dead_code)]

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, SYSTEMTIME};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GetBestRoute2, GetExtendedTcpTable, IcmpCloseHandle, IcmpCreateFile,
    IcmpSendEcho, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST,
    ICMP_ECHO_REPLY, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IF_TYPE_PPP,
    IF_TYPE_PROP_VIRTUAL, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, IF_TYPE_WWANPP,
    IF_TYPE_WWANPP2, IP_ADAPTER_ADDRESSES_LH, IP_SUCCESS, MIB_IPFORWARD_ROW2,
    MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::NetworkManagement::WiFi::{
    dot11_phy_type_dsss, dot11_phy_type_eht, dot11_phy_type_erp, dot11_phy_type_he,
    dot11_phy_type_hrdsss, dot11_phy_type_ht, dot11_phy_type_ofdm, dot11_phy_type_vht,
    wlan_interface_state_connected, wlan_intf_opcode_channel_number,
    wlan_intf_opcode_current_connection, wlan_intf_opcode_rssi, WlanCloseHandle,
    WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
};
use windows_sys::Win32::Networking::WinSock::{
    IpDadStatePreferred, AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
    SOCKADDR_INET,
};
use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

// ------------------------------------------------------------------ ping maths (pure)

/// Samples considered by `verdict`.
const VERDICT_WINDOW: usize = 20;
/// Fewest samples needed before `verdict` says anything but `Unknown`.
const VERDICT_MIN: usize = 3;
/// Loss in percent at or above which a path counts as "losing".
const LOSS_LIMIT_PCT: f32 = 10.0;
/// RTT samples kept per series.
const HISTORY: usize = 120;
/// Outages kept (newest last).
const MAX_OUTAGES: usize = 50;
/// Shortest unreachable run (seconds) that is reported as an outage.
const MIN_OUTAGE_SECS: u64 = 3;
/// A gap between two samples longer than this (sleep, resume) ends an open outage run.
const MAX_GAP_SECS: u64 = 10;

/// What the last ~20 pings say about where a problem is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verdict {
    /// Not enough samples yet.
    #[default]
    Unknown,
    /// Gateway and internet both answer.
    Good,
    /// The gateway (router / Wi-Fi link) is losing packets.
    LocalLoss,
    /// The gateway answers but the internet is losing packets: the ISP side.
    IspLoss,
    /// Neither the gateway nor the internet answers.
    Offline,
    /// No default IPv4 gateway exists (cable out, Wi-Fi off).
    NoGateway,
}

impl Verdict {
    /// Short human text for the verdict headline.
    pub fn text(self) -> &'static str {
        match self {
            Verdict::Unknown => "Measuring...",
            Verdict::Good => "Connection is healthy",
            Verdict::LocalLoss => "Problem is between this PC and your router (Wi-Fi or cable)",
            Verdict::IspLoss => "Your router is fine, the problem is your internet provider",
            Verdict::Offline => "Nothing answers: this PC is offline",
            Verdict::NoGateway => "No network route: not connected to a network",
        }
    }
}

/// One internet outage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outage {
    /// Start, Unix seconds (UTC).
    pub start: u64,
    /// Duration in seconds.
    pub secs: u32,
    /// Where the problem was (`IspLoss`, `LocalLoss` or `Offline`).
    pub cause: Verdict,
    /// Gateway packet loss during the outage, percent 0..100.
    pub loss_gw: f32,
    /// Internet packet loss during the outage, percent 0..100.
    pub loss_net: f32,
}

/// Everything the page shows about the pings.
#[derive(Clone, Debug, Default)]
pub struct PingState {
    /// Gateway address being pinged ("" when there is none).
    pub gateway: String,
    /// Internet address being pinged.
    pub target: String,
    /// Last 120 gateway round trips in ms, `None` = lost, oldest first.
    pub gw: Vec<Option<f32>>,
    /// Last 120 internet round trips in ms, `None` = lost, oldest first.
    pub net: Vec<Option<f32>>,
    /// Verdict from the last 20 samples.
    pub verdict: Verdict,
    /// Outages, newest last, at most 50. A running outage (>= 3 s) is the last entry.
    pub outages: Vec<Outage>,
}

/// Packet loss of a series in percent (0..100); 0 for an empty series.
pub fn loss(s: &[Option<f32>]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let lost = s.iter().filter(|r| r.is_none()).count();
    lost as f32 * 100.0 / s.len() as f32
}

/// Jitter in ms: mean absolute difference of consecutive round trips. Only pairs where both
/// samples were answered count; 0 when there is no such pair.
pub fn jitter(s: &[Option<f32>]) -> f32 {
    let (mut sum, mut n) = (0.0f32, 0u32);
    for w in s.windows(2) {
        if let (Some(a), Some(b)) = (w[0], w[1]) {
            sum += (a - b).abs();
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// Mean round trip in ms of the answered samples, `None` when all were lost.
pub fn mean_rtt(s: &[Option<f32>]) -> Option<f32> {
    let (sum, n) = s.iter().flatten().fold((0.0f32, 0u32), |(a, n), v| (a + v, n + 1));
    (n > 0).then(|| sum / n as f32)
}

fn tail(s: &[Option<f32>]) -> &[Option<f32>] {
    &s[s.len().saturating_sub(VERDICT_WINDOW)..]
}

/// Verdict from the gateway and internet series (only the last 20 samples count).
///
/// Loss of 10% or more is "losing". Both paths dead is `Offline`; the gateway losing while the
/// internet is too is `LocalLoss`; the gateway fine and the internet losing is `IspLoss`. A
/// gateway that drops ICMP while the internet answers is not a problem, so that is `Good`.
/// `NoGateway` is decided by the pinger (it knows whether a route exists), not here.
pub fn verdict(gw: &[Option<f32>], net: &[Option<f32>]) -> Verdict {
    let (g, n) = (tail(gw), tail(net));
    if g.len() < VERDICT_MIN || n.len() < VERDICT_MIN {
        return Verdict::Unknown;
    }
    let (lg, ln) = (loss(g), loss(n));
    if lg >= 100.0 && ln >= 100.0 {
        Verdict::Offline
    } else if ln >= LOSS_LIMIT_PCT && lg >= LOSS_LIMIT_PCT {
        Verdict::LocalLoss
    } else if ln >= LOSS_LIMIT_PCT {
        Verdict::IspLoss
    } else {
        Verdict::Good
    }
}

/// Cause of an outage from the gateway loss seen during it.
fn cause_of(loss_gw: f32) -> Verdict {
    if loss_gw >= 100.0 {
        Verdict::Offline
    } else if loss_gw >= LOSS_LIMIT_PCT {
        Verdict::LocalLoss
    } else {
        Verdict::IspLoss
    }
}

/// Which internet target to ping next: 0 = 1.1.1.1, 1 = 8.8.8.8. Switches after 3 failures in
/// a row while the gateway answers. Returns `(target index, failure streak)`.
pub fn next_target(cur: usize, streak: u32, gw_ok: bool, net_ok: bool) -> (usize, u32) {
    if net_ok || !gw_ok {
        return (cur, 0);
    }
    if streak + 1 >= 3 {
        (1 - cur.min(1), 0)
    } else {
        (cur, streak + 1)
    }
}

struct Run {
    start: u64,
    last: u64,
    n: u32,
    gw_lost: u32,
}

/// Turns a stream of samples into outages: a run of samples with the internet unreachable for
/// at least 3 s.
#[derive(Default)]
pub struct OutageTracker {
    run: Option<Run>,
    done: Vec<Outage>,
}

impl OutageTracker {
    fn close(&mut self, r: &Run, end: u64) {
        let secs = end.saturating_sub(r.start);
        if secs < MIN_OUTAGE_SECS {
            return;
        }
        let loss_gw = r.gw_lost as f32 * 100.0 / r.n.max(1) as f32;
        self.done.push(Outage {
            start: r.start,
            secs: secs.min(u32::MAX as u64) as u32,
            cause: cause_of(loss_gw),
            loss_gw,
            loss_net: 100.0,
        });
        if self.done.len() > MAX_OUTAGES {
            self.done.remove(0);
        }
    }

    /// Feeds one sample taken at Unix second `t`. A finished outage is duration
    /// `recovery time - first failed sample`; an outage cut off by a long gap between samples
    /// (sleep) ends one second after its last failed sample.
    pub fn push(&mut self, t: u64, gw_ok: bool, net_ok: bool) {
        if let Some(r) = &self.run {
            if t > r.last + MAX_GAP_SECS {
                if let Some(r) = self.run.take() {
                    self.close(&r, r.last + 1);
                }
            }
        }
        if net_ok {
            if let Some(r) = self.run.take() {
                self.close(&r, t);
            }
        } else if let Some(r) = &mut self.run {
            r.last = t;
            r.n += 1;
            r.gw_lost += u32::from(!gw_ok);
        } else {
            self.run = Some(Run { start: t, last: t, n: 1, gw_lost: u32::from(!gw_ok) });
        }
    }

    /// The outage in progress, once it has lasted 3 s.
    pub fn current(&self) -> Option<Outage> {
        let r = self.run.as_ref()?;
        let secs = (r.last + 1).saturating_sub(r.start);
        if secs < MIN_OUTAGE_SECS {
            return None;
        }
        let loss_gw = r.gw_lost as f32 * 100.0 / r.n.max(1) as f32;
        Some(Outage {
            start: r.start,
            secs: secs.min(u32::MAX as u64) as u32,
            cause: cause_of(loss_gw),
            loss_gw,
            loss_net: 100.0,
        })
    }

    /// Finished outages plus the running one, newest last, at most 50.
    pub fn all(&self) -> Vec<Outage> {
        let mut v = self.done.clone();
        v.extend(self.current());
        if v.len() > MAX_OUTAGES {
            v.drain(..v.len() - MAX_OUTAGES);
        }
        v
    }
}

/// Outages from a recorded sample list `(unix secs, gateway ok, internet ok)`, oldest first.
pub fn outages_from(samples: &[(u64, bool, bool)]) -> Vec<Outage> {
    let mut t = OutageTracker::default();
    for &(s, g, n) in samples {
        t.push(s, g, n);
    }
    t.all()
}

// ------------------------------------------------------------------ outage text (pure + local time)

/// Civil date and time (UTC) of a Unix time: year, month, day, hour, minute, second.
fn civil(secs: u64) -> [u32; 6] {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as u32;
    [y, m, d, rem / 3600, rem / 60 % 60, rem % 60]
}

/// `YYYY-MM-DD HH:MM:SS` of a civil date.
fn fmt_civil(c: [u32; 6]) -> String {
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", c[0], c[1], c[2], c[3], c[4], c[5])
}

/// UTC time of a Unix time as `YYYY-MM-DD HH:MM:SS`.
pub fn utc_text(secs: u64) -> String {
    fmt_civil(civil(secs))
}

/// Local time (current Windows time zone) of a Unix time, `None` if the conversion fails.
fn local_text(secs: u64) -> Option<String> {
    let c = civil(secs);
    let utc = SYSTEMTIME {
        wYear: c[0] as u16,
        wMonth: c[1] as u16,
        wDayOfWeek: 0,
        wDay: c[2] as u16,
        wHour: c[3] as u16,
        wMinute: c[4] as u16,
        wSecond: c[5] as u16,
        wMilliseconds: 0,
    };
    let mut local = utc;
    // SAFETY: valid in/out pointers; a null zone means the current time zone.
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    (ok != 0).then(|| {
        fmt_civil([
            u32::from(local.wYear),
            u32::from(local.wMonth),
            u32::from(local.wDay),
            u32::from(local.wHour),
            u32::from(local.wMinute),
            u32::from(local.wSecond),
        ])
    })
}

fn dur_text(secs: u32) -> String {
    if secs >= 3600 {
        format!("{} h {} min {} s", secs / 3600, secs / 60 % 60, secs % 60)
    } else if secs >= 60 {
        format!("{} min {} s", secs / 60, secs % 60)
    } else {
        format!("{secs} s")
    }
}

fn cause_text(c: Verdict) -> &'static str {
    match c {
        Verdict::IspLoss => "router reachable, internet unreachable (provider side)",
        Verdict::LocalLoss => "router also dropping packets (Wi-Fi or local network)",
        Verdict::Offline => "router and internet both unreachable (local link down)",
        Verdict::NoGateway => "no network route",
        _ => "unknown",
    }
}

/// Plain-text outage report with `time(unix secs)` as the time formatter and `zone` naming it.
pub fn outage_text_with(o: &[Outage], zone: &str, time: impl Fn(u64) -> String) -> String {
    if o.is_empty() {
        return "No internet outages recorded by Resource Monitor.".to_string();
    }
    let mut s = format!(
        "Internet outages recorded by Resource Monitor ({zone}).\n\
         Method: one ping per second to my router and to 1.1.1.1 / 8.8.8.8; an outage is 3 s or more of no answer from the internet.\n\n"
    );
    for x in o {
        s.push_str(&format!(
            "{}  lasted {}  {} (router loss {:.0}%, internet loss {:.0}%)\n",
            time(x.start),
            dur_text(x.secs),
            cause_text(x.cause),
            x.loss_gw,
            x.loss_net
        ));
    }
    s
}

/// Text for the "Copy for my ISP" button: one line per outage in local time (the Windows time
/// zone), or in UTC if the local conversion is unavailable.
pub fn outage_text(o: &[Outage]) -> String {
    if o.iter().all(|x| local_text(x.start).is_some()) {
        outage_text_with(o, "local time", |t| local_text(t).unwrap_or_else(|| utc_text(t)))
    } else {
        outage_text_with(o, "UTC", utc_text)
    }
}

// ------------------------------------------------------------------ pinger

/// RAII ICMP handle from `IcmpCreateFile`.
struct Icmp(HANDLE);

impl Icmp {
    fn open() -> Option<Icmp> {
        // SAFETY: no arguments; returns INVALID_HANDLE_VALUE on failure.
        let h = unsafe { IcmpCreateFile() };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Icmp(h))
    }

    /// One echo (32-byte payload, 1000 ms timeout). Round trip in ms, `None` = lost.
    fn ping(&self, dst: Ipv4Addr) -> Option<f32> {
        let payload = [b'a'; 32];
        let mut reply = [0u64; 64];
        let t0 = Instant::now();
        // SAFETY: payload and 512-byte, 8-aligned reply buffer outlive the synchronous call.
        let n = unsafe {
            IcmpSendEcho(
                self.0,
                u32::from_ne_bytes(dst.octets()),
                payload.as_ptr().cast(),
                payload.len() as u16,
                std::ptr::null(),
                reply.as_mut_ptr().cast(),
                std::mem::size_of_val(&reply) as u32,
                1000,
            )
        };
        if n == 0 {
            return None;
        }
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        // SAFETY: on success the buffer starts with an ICMP_ECHO_REPLY.
        let r = unsafe { std::ptr::read_unaligned(reply.as_ptr().cast::<ICMP_ECHO_REPLY>()) };
        (r.Status == IP_SUCCESS).then(|| ms.max(r.RoundTripTime as f32))
    }
}

impl Drop for Icmp {
    fn drop(&mut self) {
        // SAFETY: handle came from IcmpCreateFile and is closed once.
        unsafe { IcmpCloseHandle(self.0) };
    }
}

/// Next hop of the best IPv4 route to the internet; falls back to the first gateway of an up,
/// non-VPN adapter when the best route is on-link (typical for a full-tunnel VPN).
fn default_gateway() -> Option<Ipv4Addr> {
    // SAFETY: zeroed plain-old-data unions/structs; the API fills `row`.
    let mut row: MIB_IPFORWARD_ROW2 = unsafe { std::mem::zeroed() };
    let mut dest: SOCKADDR_INET = unsafe { std::mem::zeroed() };
    let mut src: SOCKADDR_INET = unsafe { std::mem::zeroed() };
    dest.Ipv4.sin_family = AF_INET;
    dest.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes([1, 1, 1, 1]);
    // SAFETY: valid pointers for the duration of the call.
    let rc = unsafe {
        GetBestRoute2(std::ptr::null(), 0, std::ptr::null(), &dest, 0, &mut row, &mut src)
    };
    if rc == 0 {
        // SAFETY: the union holds an IPv4 sockaddr when the family says so.
        let (fam, raw) = unsafe { (row.NextHop.si_family, row.NextHop.Ipv4.sin_addr.S_un.S_addr) };
        if fam == AF_INET && raw != 0 {
            return Some(Ipv4Addr::from(raw.to_ne_bytes()));
        }
    }
    adapters()
        .into_iter()
        .filter(|a| a.up && a.kind != "VPN")
        .flat_map(|a| a.gateway)
        .find_map(|g| g.parse::<Ipv4Addr>().ok())
}

const TARGETS: [Ipv4Addr; 2] = [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];

struct Shared {
    stop: AtomicBool,
    state: Mutex<PingState>,
}

/// Background pinger: one thread named "pinger", started when a network page opens and stopped
/// when it closes. Sequential echoes, so a total outage makes a sample take up to 2 s.
pub struct Pinger {
    shared: Arc<Shared>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Pinger {
    /// Starts the thread. If it cannot be spawned the state stays `Unknown` and empty.
    pub fn start() -> Pinger {
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), state: Mutex::new(PingState::default()) });
        let sh = shared.clone();
        let handle = std::thread::Builder::new().name("pinger".into()).spawn(move || run_pinger(&sh)).ok();
        Pinger { shared, handle }
    }

    /// Asks the thread to stop. It is detached, not joined: it exits within one ping timeout
    /// (at most ~2 s) and never blocks the caller. Safe to call twice.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.handle.take();
    }

    /// Cheap copy of the current state (a few KB under a mutex).
    pub fn snapshot(&self) -> PingState {
        self.shared.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for Pinger {
    fn drop(&mut self) {
        self.stop();
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn push_capped(v: &mut Vec<Option<f32>>, x: Option<f32>) {
    v.push(x);
    if v.len() > HISTORY {
        v.remove(0);
    }
}

/// Sleeps `d` in small steps so a stop request is noticed quickly.
fn sleep_unless_stopped(sh: &Shared, d: Duration) {
    let end = Instant::now() + d;
    while !sh.stop.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

fn run_pinger(sh: &Shared) {
    let mut icmp: Option<Icmp> = None;
    let mut tracker = OutageTracker::default();
    let (mut gw_hist, mut net_hist): (Vec<Option<f32>>, Vec<Option<f32>>) = (Vec::new(), Vec::new());
    let mut gateway: Option<Ipv4Addr> = None;
    let mut resolved: Option<Instant> = None;
    let (mut target, mut streak, mut gw_fail) = (0usize, 0u32, 0u32);

    while !sh.stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        if icmp.is_none() {
            icmp = Icmp::open();
        }
        let stale = resolved.is_none_or(|r| r.elapsed() >= Duration::from_secs(10));
        if gateway.is_none() || stale || gw_fail >= 3 {
            gateway = default_gateway();
            resolved = Some(Instant::now());
            gw_fail = 0;
        }
        let (gw_rtt, net_rtt) = match &icmp {
            Some(i) => {
                let g = gateway.and_then(|g| i.ping(g));
                let n = if sh.stop.load(Ordering::Relaxed) { None } else { i.ping(TARGETS[target]) };
                (g, n)
            }
            None => (None, None),
        };
        let (gw_ok, net_ok) = (gw_rtt.is_some(), net_rtt.is_some());
        gw_fail = if gw_ok || gateway.is_none() { 0 } else { gw_fail + 1 };
        (target, streak) = next_target(target, streak, gw_ok, net_ok);
        if sh.stop.load(Ordering::Relaxed) {
            break;
        }

        push_capped(&mut gw_hist, gw_rtt);
        push_capped(&mut net_hist, net_rtt);
        tracker.push(unix_now(), gw_ok, net_ok);
        if let Ok(mut st) = sh.state.lock() {
            st.gateway = gateway.map(|g| g.to_string()).unwrap_or_default();
            st.target = TARGETS[target].to_string();
            st.gw.clone_from(&gw_hist);
            st.net.clone_from(&net_hist);
            st.verdict =
                if gateway.is_none() { Verdict::NoGateway } else { verdict(&gw_hist, &net_hist) };
            st.outages = tracker.all();
        }
        sleep_unless_stopped(sh, Duration::from_secs(1).saturating_sub(t0.elapsed()));
    }
}

// ------------------------------------------------------------------ adapter facts

/// Facts about one network adapter.
#[derive(Clone, Debug, Default)]
pub struct AdapterFacts {
    /// Connection name ("Wi-Fi", "Ethernet 2").
    pub alias: String,
    /// Device description ("Intel(R) Wi-Fi 6E AX211 160MHz").
    pub description: String,
    /// MAC address as `AA-BB-CC-DD-EE-FF`, empty if none.
    pub mac: String,
    /// IPv4 addresses (preferred state only).
    pub ipv4: Vec<String>,
    /// Global unicast IPv6 addresses (2000::/3, preferred state only).
    pub ipv6: Vec<String>,
    /// Gateway addresses, IPv4 and IPv6.
    pub gateway: Vec<String>,
    /// DNS server addresses.
    pub dns: Vec<String>,
    /// DHCP is used for IPv4.
    pub dhcp: bool,
    /// Transmit link speed in bit/s (0 = unknown).
    pub link_up_bps: u64,
    /// Receive link speed in bit/s (0 = unknown).
    pub link_down_bps: u64,
    /// "Wi-Fi", "Ethernet", "VPN", "Cellular" or "Other".
    pub kind: &'static str,
    /// Operational status is up.
    pub up: bool,
}

/// Adapter kind from the interface type and description.
pub fn classify(if_type: u32, desc: &str) -> &'static str {
    let l = desc.to_ascii_lowercase();
    if l.contains("vpn")
        || l.contains("wireguard")
        || l.contains("tunnel")
        || l.contains("wintun")
        || l.contains("tap-windows")
        || desc.contains("TAP-")
    {
        return "VPN";
    }
    match if_type {
        IF_TYPE_PROP_VIRTUAL | IF_TYPE_PPP | IF_TYPE_TUNNEL => "VPN",
        IF_TYPE_IEEE80211 => "Wi-Fi",
        IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => "Cellular",
        IF_TYPE_ETHERNET_CSMACD => {
            let virt = ["virtual", "hyper-v", "vmware", "virtualbox", "vethernet"];
            if virt.iter().any(|v| l.contains(v)) {
                "Other"
            } else {
                "Ethernet"
            }
        }
        _ => "Other",
    }
}

/// Windows' default site-local DNS placeholders (fec0:0:0:ffff::1..3) are noise.
fn is_placeholder_dns(a: &Ipv6Addr) -> bool {
    let s = a.segments();
    s[0] == 0xfec0 && s[3] == 0xffff
}

fn is_global_v6(a: &Ipv6Addr) -> bool {
    a.octets()[0] & 0xE0 == 0x20
}

fn mac_text(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join("-")
}

/// Reads a NUL-terminated UTF-16 string (bounded at 1024 units).
fn pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: Windows hands out NUL-terminated strings; the bound guards a broken one.
    while len < 1024 && unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units were just read successfully.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// IPv4 or IPv6 text of a sockaddr, `None` for other families.
fn sockaddr_ip(sa: *const SOCKADDR, len: i32) -> Option<std::net::IpAddr> {
    if sa.is_null() {
        return None;
    }
    // SAFETY: a valid sockaddr starts with its family.
    let fam = unsafe { (*sa).sa_family };
    if fam == AF_INET && len as usize >= std::mem::size_of::<SOCKADDR_IN>() {
        // SAFETY: family and length checked.
        let raw = unsafe { (*sa.cast::<SOCKADDR_IN>()).sin_addr.S_un.S_addr };
        Some(Ipv4Addr::from(raw.to_ne_bytes()).into())
    } else if fam == AF_INET6 && len as usize >= std::mem::size_of::<SOCKADDR_IN6>() {
        // SAFETY: family and length checked.
        let b = unsafe { (*sa.cast::<SOCKADDR_IN6>()).sin6_addr.u.Byte };
        Some(Ipv6Addr::from(b).into())
    } else {
        None
    }
}

/// All non-loopback adapters (up ones first, then Wi-Fi, Ethernet, others by name). Call once
/// on a background thread; it allocates and takes a few ms.
pub fn adapters() -> Vec<AdapterFacts> {
    let mut out = Vec::new();
    let mut len = 16 * 1024u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buf.resize((len as usize).div_ceil(8), 0);
        // SAFETY: buffer of `len` bytes, 8-byte aligned.
        let r = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST,
                std::ptr::null(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r == 111 {
            // ERROR_BUFFER_OVERFLOW: `len` now holds the size needed.
            continue;
        }
        if r != 0 {
            return out;
        }
        let mut p = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !p.is_null() {
            // SAFETY: linked list inside `buf`, written by GetAdaptersAddresses.
            let a = unsafe { &*p };
            if a.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                out.push(read_adapter(a));
            }
            p = a.Next;
        }
        break;
    }
    let rank = |a: &AdapterFacts| match a.kind {
        "Wi-Fi" => 0,
        "Ethernet" => 1,
        "Cellular" => 2,
        "VPN" => 3,
        _ => 4,
    };
    out.sort_by(|a, b| {
        b.up.cmp(&a.up).then(rank(a).cmp(&rank(b))).then_with(|| a.alias.cmp(&b.alias))
    });
    out
}

fn read_adapter(a: &IP_ADAPTER_ADDRESSES_LH) -> AdapterFacts {
    let description = pwstr(a.Description);
    let mut f = AdapterFacts {
        alias: pwstr(a.FriendlyName),
        kind: classify(a.IfType, &description),
        description,
        mac: mac_text(&a.PhysicalAddress[..(a.PhysicalAddressLength as usize).min(8)]),
        // SAFETY: both union members are plain u32 bit sets.
        dhcp: unsafe { a.Anonymous2.Flags } & 0x4 != 0,
        link_up_bps: if a.TransmitLinkSpeed == u64::MAX { 0 } else { a.TransmitLinkSpeed },
        link_down_bps: if a.ReceiveLinkSpeed == u64::MAX { 0 } else { a.ReceiveLinkSpeed },
        up: a.OperStatus == IfOperStatusUp,
        ..AdapterFacts::default()
    };
    let mut u = a.FirstUnicastAddress;
    while !u.is_null() {
        // SAFETY: linked list owned by the caller's buffer.
        let ua = unsafe { &*u };
        if ua.DadState == IpDadStatePreferred {
            match sockaddr_ip(ua.Address.lpSockaddr, ua.Address.iSockaddrLength) {
                Some(std::net::IpAddr::V4(v4)) => f.ipv4.push(v4.to_string()),
                Some(std::net::IpAddr::V6(v6)) if is_global_v6(&v6) => f.ipv6.push(v6.to_string()),
                _ => {}
            }
        }
        u = ua.Next;
    }
    let mut g = a.FirstGatewayAddress;
    while !g.is_null() {
        // SAFETY: as above.
        let ga = unsafe { &*g };
        if let Some(ip) = sockaddr_ip(ga.Address.lpSockaddr, ga.Address.iSockaddrLength) {
            f.gateway.push(ip.to_string());
        }
        g = ga.Next;
    }
    let mut d = a.FirstDnsServerAddress;
    while !d.is_null() {
        // SAFETY: as above.
        let da = unsafe { &*d };
        match sockaddr_ip(da.Address.lpSockaddr, da.Address.iSockaddrLength) {
            Some(std::net::IpAddr::V6(v6)) if is_placeholder_dns(&v6) => {}
            Some(ip) => f.dns.push(ip.to_string()),
            None => {}
        }
        d = da.Next;
    }
    f
}

// ------------------------------------------------------------------ connections

/// One TCP connection with its owning process.
#[derive(Clone, Debug)]
pub struct Conn {
    /// Owning process id.
    pub pid: u32,
    /// "TCP" (UDP listeners are not read).
    pub proto: &'static str,
    /// Local `ip:port` (`[ip]:port` for IPv6).
    pub local: String,
    /// Remote `ip:port` (`[ip]:port` for IPv6).
    pub remote: String,
    /// TCP state name ("Established", "Listen", ...).
    pub state: &'static str,
}

/// Reader for the TCP connection tables; keeps its buffer between calls.
#[derive(Default)]
pub struct Conns {
    buf: Vec<u8>,
}

/// TCP state name from a `MIB_TCP_STATE` value.
pub fn tcp_state_name(s: u32) -> &'static str {
    match s {
        1 => "Closed",
        2 => "Listen",
        3 => "SynSent",
        4 => "SynRcvd",
        5 => "Established",
        6 => "FinWait1",
        7 => "FinWait2",
        8 => "CloseWait",
        9 => "Closing",
        10 => "LastAck",
        11 => "TimeWait",
        12 => "Deleted",
        _ => "Unknown",
    }
}

/// Port from a table DWORD: the low 16 bits hold it in network byte order.
pub fn port_of(dw: u32) -> u16 {
    u16::from_be((dw & 0xFFFF) as u16)
}

impl Conns {
    /// Fills the buffer with the table of `family`; returns the number of rows.
    fn fetch(&mut self, family: u16) -> usize {
        for _ in 0..4 {
            let mut size = self.buf.len() as u32;
            // SAFETY: `buf` has `size` bytes (a null pointer with size 0 only queries the size).
            let rc = unsafe {
                GetExtendedTcpTable(
                    if size == 0 { std::ptr::null_mut() } else { self.buf.as_mut_ptr().cast() },
                    &mut size,
                    0,
                    u32::from(family),
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            };
            match rc {
                0 => {
                    if self.buf.len() < 4 {
                        return 0;
                    }
                    let n = u32::from_ne_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
                    return n;
                }
                // ERROR_INSUFFICIENT_BUFFER: `size` is the need; add slack for new connections.
                122 => self.buf.resize(size as usize + 64 * 32, 0),
                _ => return 0,
            }
        }
        0
    }

    /// Reads all IPv4 and IPv6 TCP connections. Typically 0.3 to 1 ms; the table buffer is reused.
    pub fn read(&mut self) -> Vec<Conn> {
        let mut out = Vec::new();
        let n4 = self.fetch(AF_INET);
        let row = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
        out.reserve(n4);
        for i in 0..n4 {
            let off = 4 + i * row;
            if off + row > self.buf.len() {
                break;
            }
            // SAFETY: bounds checked; the rows are plain u32 fields, read unaligned.
            let r = unsafe { std::ptr::read_unaligned(self.buf.as_ptr().add(off).cast::<MIB_TCPROW_OWNER_PID>()) };
            out.push(Conn {
                pid: r.dwOwningPid,
                proto: "TCP",
                local: format!("{}:{}", Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()), port_of(r.dwLocalPort)),
                remote: format!("{}:{}", Ipv4Addr::from(r.dwRemoteAddr.to_ne_bytes()), port_of(r.dwRemotePort)),
                state: tcp_state_name(r.dwState),
            });
        }
        let n6 = self.fetch(AF_INET6);
        let row = std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>();
        for i in 0..n6 {
            let off = 4 + i * row;
            if off + row > self.buf.len() {
                break;
            }
            // SAFETY: as above.
            let r = unsafe { std::ptr::read_unaligned(self.buf.as_ptr().add(off).cast::<MIB_TCP6ROW_OWNER_PID>()) };
            out.push(Conn {
                pid: r.dwOwningPid,
                proto: "TCP",
                local: format!("[{}]:{}", Ipv6Addr::from(r.ucLocalAddr), port_of(r.dwLocalPort)),
                remote: format!("[{}]:{}", Ipv6Addr::from(r.ucRemoteAddr), port_of(r.dwRemotePort)),
                state: tcp_state_name(r.dwState),
            });
        }
        out
    }
}

/// Host part of an `ip:port` / `[ip]:port` string.
fn host_of(addr: &str) -> &str {
    let h = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    h.trim_start_matches('[').trim_end_matches(']')
}

fn is_loopback_host(h: &str) -> bool {
    h.starts_with("127.") || h == "::1"
}

/// Established connections per process: `(pid, count, up to 5 distinct remote hosts)`, busiest
/// first. Loopback connections (local IPC) are ignored. Remote hosts are IP addresses; no
/// reverse DNS is done.
pub fn by_app(c: &[Conn]) -> Vec<(u32, usize, Vec<String>)> {
    let mut out: Vec<(u32, usize, Vec<String>)> = Vec::new();
    for x in c.iter().filter(|x| x.state == "Established") {
        let host = host_of(&x.remote);
        if is_loopback_host(host) {
            continue;
        }
        let i = match out.iter().position(|e| e.0 == x.pid) {
            Some(i) => i,
            None => {
                out.push((x.pid, 0, Vec::new()));
                out.len() - 1
            }
        };
        let e = &mut out[i];
        e.1 += 1;
        if e.2.len() < 5 && !e.2.iter().any(|h| h == host) {
            e.2.push(host.to_string());
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

// ------------------------------------------------------------------ Wi-Fi

/// Wi-Fi link details of the connected wireless interface.
#[derive(Clone, Debug, Default)]
pub struct WifiInfo {
    /// Network name; `None` when hidden or when location access is blocked.
    pub ssid: Option<String>,
    /// Signal quality 0..100.
    pub signal_pct: Option<u32>,
    /// Received signal strength in dBm.
    pub rssi_dbm: Option<i32>,
    /// Channel number.
    pub channel: Option<u32>,
    /// "2.4 GHz", "5 GHz" or "6 GHz" (derived from the channel number).
    pub band: Option<&'static str>,
    /// "Wi-Fi 4/5/6/6E/7" from the PHY type.
    pub phy: Option<&'static str>,
    /// Receive link rate in Mbit/s.
    pub rx_mbps: Option<u32>,
    /// Transmit link rate in Mbit/s.
    pub tx_mbps: Option<u32>,
    /// Windows refused the connection query (Win11 24H2 needs location consent for the Wi-Fi
    /// name and signal); only RSSI and channel could be read.
    pub location_blocked: bool,
}

/// Band from the channel number: 1..=14 is 2.4 GHz, 32..=177 is 5 GHz and 178+ is 6 GHz.
/// Channels 1..=233 also exist in 6 GHz, which this number alone cannot tell from 2.4/5 GHz.
pub fn band_from_channel(ch: u32) -> Option<&'static str> {
    match ch {
        1..=14 => Some("2.4 GHz"),
        32..=177 => Some("5 GHz"),
        178..=233 => Some("6 GHz"),
        _ => None,
    }
}

/// Wi-Fi generation name from a `DOT11_PHY_TYPE` and the band ("Wi-Fi 6E" is HE on 6 GHz).
pub fn phy_name(phy: i32, band: Option<&str>) -> Option<&'static str> {
    Some(match phy {
        p if p == dot11_phy_type_eht => "Wi-Fi 7",
        p if p == dot11_phy_type_he => {
            if band == Some("6 GHz") {
                "Wi-Fi 6E"
            } else {
                "Wi-Fi 6"
            }
        }
        p if p == dot11_phy_type_vht => "Wi-Fi 5",
        p if p == dot11_phy_type_ht => "Wi-Fi 4",
        p if p == dot11_phy_type_erp => "Wi-Fi 3",
        p if p == dot11_phy_type_ofdm => "Wi-Fi 2",
        p if p == dot11_phy_type_hrdsss || p == dot11_phy_type_dsss => "Wi-Fi 1",
        _ => return None,
    })
}

/// Rough signal percent from RSSI (-100 dBm = 0, -50 dBm = 100), the usual Windows mapping.
pub fn rssi_to_pct(rssi: i32) -> u32 {
    ((rssi + 100) * 2).clamp(0, 100) as u32
}

/// Owner of the Native Wifi client handle, so it is not reopened every second. Not `Send`:
/// keep it on the UI thread.
pub struct Wlan {
    h: HANDLE,
}

impl Default for Wlan {
    fn default() -> Self {
        Wlan::new()
    }
}

impl Wlan {
    /// A closed reader; the handle is opened on the first `read`.
    pub fn new() -> Wlan {
        Wlan { h: std::ptr::null_mut() }
    }

    fn close(&mut self) {
        if !self.h.is_null() {
            // SAFETY: handle came from WlanOpenHandle and is closed once.
            unsafe { WlanCloseHandle(self.h, std::ptr::null()) };
            self.h = std::ptr::null_mut();
        }
    }

    fn ensure_open(&mut self) -> bool {
        if self.h.is_null() {
            let (mut ver, mut h): (u32, HANDLE) = (0, std::ptr::null_mut());
            // SAFETY: valid out pointers.
            if unsafe { WlanOpenHandle(2, std::ptr::null(), &mut ver, &mut h) } == 0 {
                self.h = h;
            }
        }
        !self.h.is_null()
    }

    /// GUID of the first connected wireless interface.
    fn connected_interface(&mut self) -> Option<windows_sys::core::GUID> {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        // SAFETY: valid handle and out pointer; the list is freed below.
        let rc = unsafe { WlanEnumInterfaces(self.h, std::ptr::null(), &mut list) };
        if rc != 0 || list.is_null() {
            // The service restarted or the handle went stale: reopen next time.
            self.close();
            return None;
        }
        // SAFETY: the list holds `dwNumberOfItems` contiguous entries.
        let found = unsafe {
            let n = (*list).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list).InterfaceInfo).cast::<windows_sys::Win32::NetworkManagement::WiFi::WLAN_INTERFACE_INFO>();
            std::slice::from_raw_parts(first, n)
                .iter()
                .find(|i| i.isState == wlan_interface_state_connected)
                .map(|i| i.InterfaceGuid)
        };
        // SAFETY: allocated by WlanEnumInterfaces.
        unsafe { WlanFreeMemory(list.cast()) };
        found
    }

    /// Queries one opcode into a `T`. Returns the Win32 error code on failure.
    fn query<T: Copy>(&self, guid: &windows_sys::core::GUID, opcode: i32) -> Result<T, u32> {
        let (mut size, mut data, mut vt) = (0u32, std::ptr::null_mut(), 0i32);
        // SAFETY: valid handle, GUID and out pointers; the data is freed below.
        let rc = unsafe { WlanQueryInterface(self.h, guid, opcode, std::ptr::null(), &mut size, &mut data, &mut vt) };
        if rc != 0 {
            return Err(rc);
        }
        let res = if !data.is_null() && size as usize >= std::mem::size_of::<T>() {
            // SAFETY: the buffer holds at least size_of::<T>() bytes.
            Ok(unsafe { std::ptr::read_unaligned(data.cast::<T>()) })
        } else {
            Err(0)
        };
        // SAFETY: allocated by WlanQueryInterface.
        unsafe { WlanFreeMemory(data.cast()) };
        res
    }

    /// Reads the connected Wi-Fi interface, `None` when there is no wireless adapter or it is
    /// not connected. About 0.5 ms; safe to call every second.
    pub fn read(&mut self) -> Option<WifiInfo> {
        if !self.ensure_open() {
            return None;
        }
        let guid = self.connected_interface()?;
        let mut w = WifiInfo::default();
        match self.query::<WLAN_CONNECTION_ATTRIBUTES>(&guid, wlan_intf_opcode_current_connection) {
            Ok(c) => {
                let a = &c.wlanAssociationAttributes;
                let n = (a.dot11Ssid.uSSIDLength as usize).min(32);
                if n > 0 {
                    w.ssid = Some(String::from_utf8_lossy(&a.dot11Ssid.ucSSID[..n]).into_owned());
                }
                w.signal_pct = Some(a.wlanSignalQuality.min(100));
                w.rx_mbps = Some(a.ulRxRate / 1000);
                w.tx_mbps = Some(a.ulTxRate / 1000);
                w.phy = phy_name(a.dot11PhyType, None);
                if let Ok(ch) = self.query::<u32>(&guid, wlan_intf_opcode_channel_number) {
                    w.channel = Some(ch);
                    w.band = band_from_channel(ch);
                    w.phy = phy_name(a.dot11PhyType, w.band);
                }
            }
            Err(5) => {
                // ERROR_ACCESS_DENIED: no location consent (Windows 11 24H2 and later).
                w.location_blocked = true;
                if let Ok(ch) = self.query::<u32>(&guid, wlan_intf_opcode_channel_number) {
                    w.channel = Some(ch);
                    w.band = band_from_channel(ch);
                }
            }
            Err(_) => {}
        }
        if let Ok(r) = self.query::<i32>(&guid, wlan_intf_opcode_rssi) {
            w.rssi_dbm = Some(r);
            if w.signal_pct.is_none() {
                w.signal_pct = Some(rssi_to_pct(r));
            }
        }
        if w.channel.is_none() && !w.location_blocked {
            if let Ok(ch) = self.query::<u32>(&guid, wlan_intf_opcode_channel_number) {
                w.channel = Some(ch);
                w.band = band_from_channel(ch);
            }
        }
        Some(w)
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        self.close();
    }
}

/// One-shot Wi-Fi read (opens and closes the WLAN handle). Prefer a kept `Wlan` for live use.
pub fn wifi() -> Option<WifiInfo> {
    Wlan::new().read()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[i32]) -> Vec<Option<f32>> {
        v.iter().map(|&x| (x >= 0).then_some(x as f32)).collect()
    }

    #[test]
    fn loss_percent() {
        assert_eq!(loss(&[]), 0.0);
        assert_eq!(loss(&s(&[5, 5, 5, 5])), 0.0);
        assert_eq!(loss(&s(&[5, -1, 5, -1])), 50.0);
        assert_eq!(loss(&s(&[-1, -1])), 100.0);
    }

    #[test]
    fn jitter_mean_abs_diff() {
        assert_eq!(jitter(&[]), 0.0);
        assert_eq!(jitter(&s(&[10])), 0.0);
        assert_eq!(jitter(&s(&[10, 20, 10])), 10.0);
        // Lost samples break the pair chain.
        assert_eq!(jitter(&s(&[10, -1, 30, 40])), 10.0);
        assert_eq!(jitter(&s(&[-1, -1])), 0.0);
    }

    #[test]
    fn mean_ignores_lost() {
        assert_eq!(mean_rtt(&s(&[10, -1, 30])), Some(20.0));
        assert_eq!(mean_rtt(&s(&[-1])), None);
    }

    #[test]
    fn verdict_cases() {
        let ok = vec![Some(5.0); 20];
        let dead = vec![None; 20];
        assert_eq!(verdict(&[], &[]), Verdict::Unknown);
        assert_eq!(verdict(&ok[..2], &ok[..2]), Verdict::Unknown);
        assert_eq!(verdict(&ok, &ok), Verdict::Good);
        assert_eq!(verdict(&ok, &dead), Verdict::IspLoss);
        assert_eq!(verdict(&dead, &dead), Verdict::Offline);
        // Router drops ICMP but the internet answers: fine.
        assert_eq!(verdict(&dead, &ok), Verdict::Good);
        // 10% loss on both: local problem.
        let mut lossy = ok.clone();
        lossy[3] = None;
        lossy[9] = None;
        assert_eq!(verdict(&lossy, &lossy), Verdict::LocalLoss);
        // 5% loss is below the limit.
        let mut one = ok.clone();
        one[3] = None;
        assert_eq!(verdict(&ok, &one), Verdict::Good);
        // Only the last 20 samples count.
        let mut long = vec![None; 50];
        long.extend(ok.clone());
        assert_eq!(verdict(&long, &long), Verdict::Good);
    }

    #[test]
    fn tracker_basic_outage() {
        let mut t = OutageTracker::default();
        for i in 0..5 {
            t.push(100 + i, true, true);
        }
        for i in 5..10 {
            t.push(100 + i, true, false);
        }
        // Running outage is visible after 3 s.
        let cur = t.current().expect("running");
        assert_eq!(cur.start, 105);
        assert_eq!(cur.secs, 5);
        assert_eq!(cur.cause, Verdict::IspLoss);
        t.push(110, true, true);
        let all = t.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].start, 105);
        assert_eq!(all[0].secs, 5);
        assert_eq!(all[0].loss_gw, 0.0);
        assert_eq!(all[0].loss_net, 100.0);
    }

    #[test]
    fn tracker_threshold_and_causes() {
        // 2 s of failure: dropped. 3 s: kept.
        let o = outages_from(&[(10, true, false), (11, true, false), (12, true, true)]);
        assert!(o.is_empty());
        let o = outages_from(&[(10, true, false), (11, true, false), (12, true, false), (13, true, true)]);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].secs, 3);
        // Gateway also dead: Offline; half dead: LocalLoss.
        let o = outages_from(&[(0, false, false), (1, false, false), (2, false, false), (3, false, true)]);
        assert_eq!(o[0].cause, Verdict::Offline);
        let o = outages_from(&[(0, false, false), (1, true, false), (2, false, false), (3, true, false), (4, true, true)]);
        assert_eq!(o[0].cause, Verdict::LocalLoss);
        assert_eq!(o[0].loss_gw, 50.0);
    }

    #[test]
    fn tracker_gap_closes_run() {
        // The PC slept for an hour with the run open: outage ends 1 s after the last failure.
        let mut t = OutageTracker::default();
        for i in 0..4 {
            t.push(100 + i, true, false);
        }
        t.push(3700, true, true);
        let all = t.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].secs, 4);
    }

    #[test]
    fn tracker_caps_at_fifty() {
        let mut t = OutageTracker::default();
        for k in 0..60u64 {
            let b = k * 100;
            for i in 0..4 {
                t.push(b + i, true, false);
            }
            t.push(b + 4, true, true);
        }
        let all = t.all();
        assert_eq!(all.len(), 50);
        assert_eq!(all[0].start, 1000);
        assert_eq!(all[49].start, 5900);
    }

    #[test]
    fn target_fallback() {
        assert_eq!(next_target(0, 0, true, false), (0, 1));
        assert_eq!(next_target(0, 1, true, false), (0, 2));
        assert_eq!(next_target(0, 2, true, false), (1, 0));
        // Gateway down: streak does not build.
        assert_eq!(next_target(0, 2, false, false), (0, 0));
        assert_eq!(next_target(1, 1, true, true), (1, 0));
    }

    #[test]
    fn civil_time() {
        assert_eq!(utc_text(0), "1970-01-01 00:00:00");
        assert_eq!(utc_text(1_700_000_000), "2023-11-14 22:13:20");
        assert_eq!(utc_text(951_782_400), "2000-02-29 00:00:00");
    }

    #[test]
    fn outage_text_lines() {
        assert!(outage_text_with(&[], "UTC", utc_text).contains("No internet outages"));
        let o = [Outage { start: 1_700_000_000, secs: 75, cause: Verdict::IspLoss, loss_gw: 0.0, loss_net: 100.0 }];
        let t = outage_text_with(&o, "UTC", utc_text);
        assert!(t.contains("(UTC)"));
        assert!(t.contains("2023-11-14 22:13:20  lasted 1 min 15 s"));
        assert!(t.contains("provider side"));
    }

    #[test]
    fn classify_kinds() {
        assert_eq!(classify(71, "Intel(R) Wi-Fi 6E AX211"), "Wi-Fi");
        assert_eq!(classify(6, "Realtek PCIe GbE Family Controller"), "Ethernet");
        assert_eq!(classify(6, "TAP-Windows Adapter V9"), "VPN");
        assert_eq!(classify(6, "WireGuard Tunnel"), "VPN");
        assert_eq!(classify(6, "Hyper-V Virtual Ethernet Adapter"), "Other");
        assert_eq!(classify(53, "Something"), "VPN");
        assert_eq!(classify(23, "WAN Miniport (PPTP)"), "VPN");
        assert_eq!(classify(131, "Teredo Tunneling Pseudo-Interface"), "VPN");
        assert_eq!(classify(243, "Fibocom WWAN"), "Cellular");
        assert_eq!(classify(15, "FDDI"), "Other");
    }

    #[test]
    fn address_filters() {
        assert!(is_global_v6(&"2001:db8::1".parse().unwrap()));
        assert!(is_global_v6(&"2a02:1::1".parse().unwrap()));
        assert!(!is_global_v6(&"fe80::1".parse().unwrap()));
        assert!(!is_global_v6(&"fd00::1".parse().unwrap()));
        assert!(is_placeholder_dns(&"fec0:0:0:ffff::1".parse().unwrap()));
        assert!(!is_placeholder_dns(&"2001:4860:4860::8888".parse().unwrap()));
        assert_eq!(mac_text(&[0xaa, 0x0b, 0xcc, 0, 1, 2]), "AA-0B-CC-00-01-02");
    }

    fn conn(pid: u32, remote: &str, state: &'static str) -> Conn {
        Conn { pid, proto: "TCP", local: "192.168.1.5:50000".into(), remote: remote.into(), state }
    }

    #[test]
    fn tcp_parsing() {
        assert_eq!(port_of(0x5000), 80);
        assert_eq!(port_of(0xBB01), 443);
        assert_eq!(port_of(0xFFFF_0000 | 0x5000), 80);
        assert_eq!(tcp_state_name(5), "Established");
        assert_eq!(tcp_state_name(2), "Listen");
        assert_eq!(tcp_state_name(99), "Unknown");
        assert_eq!(host_of("1.2.3.4:443"), "1.2.3.4");
        assert_eq!(host_of("[2001:db8::1]:443"), "2001:db8::1");
    }

    #[test]
    fn by_app_groups() {
        let c = vec![
            conn(10, "1.1.1.1:443", "Established"),
            conn(10, "1.1.1.1:80", "Established"),
            conn(10, "8.8.8.8:443", "Established"),
            conn(10, "9.9.9.9:443", "Listen"),
            conn(20, "[2001:db8::1]:443", "Established"),
            conn(20, "127.0.0.1:5000", "Established"),
            conn(20, "[::1]:5000", "Established"),
            conn(30, "0.0.0.0:0", "Listen"),
        ];
        let g = by_app(&c);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0], (10, 3, vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()]));
        assert_eq!(g[1], (20, 1, vec!["2001:db8::1".to_string()]));
    }

    #[test]
    fn by_app_limits_hosts() {
        let c: Vec<Conn> = (1..=8).map(|i| conn(1, &format!("5.5.5.{i}:443"), "Established")).collect();
        let g = by_app(&c);
        assert_eq!(g[0].1, 8);
        assert_eq!(g[0].2.len(), 5);
    }

    #[test]
    fn wifi_helpers() {
        assert_eq!(band_from_channel(6), Some("2.4 GHz"));
        assert_eq!(band_from_channel(36), Some("5 GHz"));
        assert_eq!(band_from_channel(149), Some("5 GHz"));
        assert_eq!(band_from_channel(197), Some("6 GHz"));
        assert_eq!(band_from_channel(0), None);
        assert_eq!(phy_name(dot11_phy_type_he, Some("5 GHz")), Some("Wi-Fi 6"));
        assert_eq!(phy_name(dot11_phy_type_he, Some("6 GHz")), Some("Wi-Fi 6E"));
        assert_eq!(phy_name(dot11_phy_type_eht, None), Some("Wi-Fi 7"));
        assert_eq!(phy_name(dot11_phy_type_vht, None), Some("Wi-Fi 5"));
        assert_eq!(phy_name(dot11_phy_type_ht, None), Some("Wi-Fi 4"));
        assert_eq!(phy_name(0, None), None);
        assert_eq!(rssi_to_pct(-50), 100);
        assert_eq!(rssi_to_pct(-70), 60);
        assert_eq!(rssi_to_pct(-100), 0);
        assert_eq!(rssi_to_pct(-120), 0);
        assert_eq!(rssi_to_pct(-30), 100);
    }
}
