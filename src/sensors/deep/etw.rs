//! Per-app network bytes from ETW, the way Resource Monitor's Network tab does it: a
//! real-time trace session on `Microsoft-Windows-Kernel-Network`, whose TCP and UDP
//! send/receive events carry the owning PID and the payload size. ADMIN ONLY by design:
//! starting a kernel-provider session needs an elevated process (or membership of
//! "Performance Log Users"), so `NetEtw::start` reports `StartError::NeedsAdmin` and the
//! page shows no per-app table.
//!
//! The session exists only while the Network page is open (the sensor thread creates a
//! `NetEtw` on page open and drops it on close). Cost: the callback runs on its own thread
//! ("etw-net"), does one mutex lock and one hash update per event and never allocates once
//! each PID has been seen, so 100 Mbit/s (about 8k events/s of full-size segments) stays
//! far below 1% of a core. The session also stops itself on `Drop`; a session left behind
//! by a crashed run is stopped and restarted on the next `start`.
//!
//! Provider facts (Windows 7 SDK manifest of Microsoft-Windows-Kernel-Network):
//! event ids 10/11 TCPv4 sent/received, 26/27 TCPv6, 42/43 UDPv4 sent/received, 58/59
//! UDPv6; keywords 0x10 IPv4 and 0x20 IPv6; every one of those events starts its user data
//! with `PID: u32` then `size: u32` (little endian). `EventHeader.ProcessId` is the logging
//! process, so the PID is read from the user data.

#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS};
use windows_sys::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
    EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
    EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD,
    PROCESS_TRACE_MODE_REAL_TIME, TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};

/// Session name; unique to this app so a leftover one can be recognised and stopped.
const SESSION_NAME: &str = "ResourceMonitor-Net";
/// Microsoft-Windows-Kernel-Network {7DD42A49-5329-4832-8DFD-43D979153A88}.
const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
/// Keyword mask: 0x10 (IPv4) | 0x20 (IPv6). These are the only keywords the provider
/// defines for data events, so this is "all data events" without the connect/accept noise
/// of keyword 0.
const KEYWORDS: u64 = 0x30;
/// How long `start` waits for the processing thread to open the trace.
const OPEN_WAIT: Duration = Duration::from_secs(3);
/// How long `Drop` waits for the processing thread before leaving it to finish alone.
const JOIN_WAIT_MS: u32 = 1500;

/// Why `NetEtw::start` failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// `ERROR_ACCESS_DENIED`: the app is not elevated.
    NeedsAdmin,
    /// Any other Win32 error code (0 when the processing thread died before reporting).
    Failed(u32),
}

/// Network bytes of one process since the session started and over the last interval.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AppBytes {
    pub pid: u32,
    /// Received rate over the last `take` interval, bytes per second.
    pub down_bps: f64,
    /// Sent rate over the last `take` interval, bytes per second.
    pub up_bps: f64,
    /// Bytes received since the session started.
    pub total_down: u64,
    /// Bytes sent since the session started.
    pub total_up: u64,
}

/// Direction of a counted event, from the process's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Send,
    Recv,
}

/// Transport of a counted event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// (received, sent) byte totals per PID, written by the ETW thread.
type Totals = HashMap<u32, (u64, u64)>;

/// State shared between the ETW callback thread and `NetEtw::take`.
struct Shared {
    totals: Mutex<Totals>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// A running per-app network trace. Owned by the sensor thread; dropping it stops the session.
pub struct NetEtw {
    shared: Arc<Shared>,
    /// Session handle for `ControlTraceW`.
    session: CONTROLTRACE_HANDLE,
    thread: Option<JoinHandle<()>>,
    /// Totals at the previous `take`, for rates.
    prev: Totals,
}

// SAFETY: the session handle is a plain integer id, and the shared state is behind a mutex.
unsafe impl Send for NetEtw {}

impl NetEtw {
    /// Starts the session and the "etw-net" processing thread. `NeedsAdmin` when not elevated.
    pub fn start() -> Result<NetEtw, StartError> {
        let name = crate::util::wide(SESSION_NAME);
        let session = start_session(&name)?;
        if let Err(e) = enable_provider(session) {
            stop_session(session, &name);
            return Err(e);
        }
        let shared = Arc::new(Shared { totals: Mutex::new(HashMap::with_capacity(64)) });
        let (tx, rx) = mpsc::channel::<Result<(), u32>>();
        let ctx = Arc::clone(&shared);
        let tname = name.clone();
        let spawned = std::thread::Builder::new().name("etw-net".into()).spawn(move || process(tname, ctx, tx));
        let thread = match spawned {
            Ok(t) => t,
            Err(_) => {
                stop_session(session, &name);
                return Err(StartError::Failed(0));
            }
        };
        match rx.recv_timeout(OPEN_WAIT) {
            Ok(Ok(())) => Ok(NetEtw { shared, session, thread: Some(thread), prev: HashMap::new() }),
            other => {
                // Stopping the session also ends a ProcessTrace that has already begun.
                stop_session(session, &name);
                let _ = thread.join();
                Err(match other {
                    Ok(Err(e)) => StartError::Failed(e),
                    _ => StartError::Failed(0),
                })
            }
        }
    }

    /// Per-pid rates since the previous call (`elapsed_secs` apart) plus totals since start,
    /// busiest first (down + up); pids that never moved a byte are left out.
    pub fn take(&mut self, elapsed_secs: f64) -> Vec<AppBytes> {
        let g = lock(&self.shared.totals);
        build_rows(g.iter().map(|(&p, &(r, s))| (p, r, s)), &mut self.prev, elapsed_secs)
    }
}

impl Drop for NetEtw {
    fn drop(&mut self) {
        let name = crate::util::wide(SESSION_NAME);
        stop_session(self.session, &name);
        if let Some(t) = self.thread.take() {
            // ProcessTrace returns once the session is gone; if it somehow does not, leave
            // the thread detached rather than hanging the sensor thread (it owns an `Arc` of
            // everything the callback touches, so detaching is safe).
            let mut waited = 0;
            while !t.is_finished() && waited < JOIN_WAIT_MS {
                std::thread::sleep(Duration::from_millis(10));
                waited += 10;
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

// ---- session control ---------------------------------------------------------------

/// `EVENT_TRACE_PROPERTIES` followed by room for the session name, 8-byte aligned.
struct PropsBuf {
    buf: Vec<u64>,
}

impl PropsBuf {
    fn new(name_wide: &[u16]) -> PropsBuf {
        let bytes = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name_wide.len() * 2;
        PropsBuf { buf: vec![0u64; bytes.div_ceil(8)] }
    }

    fn as_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        self.buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES
    }

    /// Fills the header for StartTrace / ControlTrace.
    fn init(&mut self) {
        let total = (self.buf.len() * 8) as u32;
        // SAFETY: the buffer is zeroed, 8-byte aligned and at least one struct long.
        let p = unsafe { &mut *self.as_ptr() };
        p.Wnode.BufferSize = total;
        p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // ClientContext 1 = QueryPerformanceCounter timestamps.
        p.Wnode.ClientContext = 1;
        p.BufferSize = 64; // KB per buffer
        p.MinimumBuffers = 2;
        p.MaximumBuffers = 16;
        p.FlushTimer = 1; // seconds: deliver partially filled buffers promptly
        p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        p.LogFileNameOffset = 0;
        p.LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
    }
}

/// Stops a session by name (and handle when known). Errors are ignored.
fn stop_session(session: CONTROLTRACE_HANDLE, name: &[u16]) {
    let mut pb = PropsBuf::new(name);
    pb.init();
    // SAFETY: `name` is NUL-terminated; `pb` is a valid properties buffer that outlives the call.
    unsafe { ControlTraceW(session, name.as_ptr(), pb.as_ptr(), EVENT_TRACE_CONTROL_STOP) };
}

/// StartTraceW, stopping a stale session of the same name once.
fn start_session(name: &[u16]) -> Result<CONTROLTRACE_HANDLE, StartError> {
    for attempt in 0..2 {
        let mut pb = PropsBuf::new(name);
        pb.init();
        let mut h = CONTROLTRACE_HANDLE { Value: 0 };
        // SAFETY: `name` is NUL-terminated; `pb` holds the properties plus the name space.
        let r = unsafe { StartTraceW(&mut h, name.as_ptr(), pb.as_ptr()) };
        match r {
            ERROR_SUCCESS => return Ok(h),
            ERROR_ACCESS_DENIED => return Err(StartError::NeedsAdmin),
            ERROR_ALREADY_EXISTS if attempt == 0 => stop_session(CONTROLTRACE_HANDLE { Value: 0 }, name),
            e => return Err(StartError::Failed(e)),
        }
    }
    Err(StartError::Failed(ERROR_ALREADY_EXISTS))
}

fn enable_provider(session: CONTROLTRACE_HANDLE) -> Result<(), StartError> {
    // SAFETY: valid session handle and GUID pointer; no enable parameters.
    let r = unsafe {
        EnableTraceEx2(
            session,
            &KERNEL_NETWORK,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_INFORMATION as u8,
            KEYWORDS,
            0,
            0,
            std::ptr::null(),
        )
    };
    match r {
        ERROR_SUCCESS => Ok(()),
        ERROR_ACCESS_DENIED => Err(StartError::NeedsAdmin),
        e => Err(StartError::Failed(e)),
    }
}

/// Body of the "etw-net" thread: OpenTrace, report, then block in ProcessTrace until the
/// session is stopped.
fn process(mut name: Vec<u16>, shared: Arc<Shared>, tx: mpsc::Sender<Result<(), u32>>) {
    // SAFETY: all-zero is a valid EVENT_TRACE_LOGFILEW (null pointers, no callbacks).
    let mut lf: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
    lf.LoggerName = name.as_mut_ptr();
    lf.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    lf.Anonymous2.EventRecordCallback = Some(on_event);
    // `shared` outlives ProcessTrace: this function holds the Arc until it returns.
    lf.Context = Arc::as_ptr(&shared) as *mut c_void;
    // SAFETY: `lf` and the name it points to stay alive for the whole trace.
    let h = unsafe { OpenTraceW(&mut lf) };
    if is_invalid_handle(h.Value) {
        let _ = tx.send(Err(last_error()));
        return;
    }
    let _ = tx.send(Ok(()));
    // SAFETY: `h` was just opened; blocks until the session ends.
    unsafe {
        ProcessTrace(&h, 1, std::ptr::null(), std::ptr::null());
        CloseTrace(PROCESSTRACE_HANDLE { Value: h.Value });
    }
    drop(shared);
}

fn is_kernel_network(g: &GUID) -> bool {
    let k = &KERNEL_NETWORK;
    g.data1 == k.data1 && g.data2 == k.data2 && g.data3 == k.data3 && g.data4 == k.data4
}

fn is_invalid_handle(v: u64) -> bool {
    // INVALID_PROCESSTRACE_HANDLE is INVALID_HANDLE_VALUE: all ones at pointer width.
    v == u64::MAX || v == u32::MAX as u64
}

fn last_error() -> u32 {
    // SAFETY: trivially safe thread-local read.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// Event record callback: runs on the "etw-net" thread for every event.
unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
    // SAFETY: ProcessTrace passes a valid record for the duration of the call.
    let Some(r) = (unsafe { rec.as_ref() }) else { return };
    if !is_kernel_network(&r.EventHeader.ProviderId) {
        return;
    }
    let Some((_, dir)) = classify(r.EventHeader.EventDescriptor.Id) else { return };
    if r.UserData.is_null() {
        return;
    }
    // SAFETY: UserData points at UserDataLength readable bytes.
    let data = unsafe { std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize) };
    let Some((pid, size)) = parse_pid_size(data) else { return };
    // SAFETY: Context is the `Arc<Shared>` pointer set in `process`, alive during ProcessTrace.
    let Some(shared) = (unsafe { (r.UserContext as *const Shared).as_ref() }) else { return };
    add_bytes(&mut lock(&shared.totals), pid, dir, size);
}

// ---- pure helpers ---------------------------------------------------------------------

/// Kernel-Network event id to (protocol, direction); `None` for every id that is not a TCP or
/// UDP data send/receive (connects, retransmits, disconnects and so on).
pub fn classify(id: u16) -> Option<(Proto, Dir)> {
    Some(match id {
        10 | 26 => (Proto::Tcp, Dir::Send),
        11 | 27 => (Proto::Tcp, Dir::Recv),
        42 | 58 => (Proto::Udp, Dir::Send),
        43 | 59 => (Proto::Udp, Dir::Recv),
        _ => return None,
    })
}

/// (PID, payload bytes) from the start of a Kernel-Network data event's user data.
pub fn parse_pid_size(data: &[u8]) -> Option<(u32, u32)> {
    let pid = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?);
    let size = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
    Some((pid, size))
}

/// Adds `size` bytes to `pid`'s received or sent total (saturating).
fn add_bytes(t: &mut Totals, pid: u32, dir: Dir, size: u32) {
    let e = t.entry(pid).or_insert((0, 0));
    match dir {
        Dir::Recv => e.0 = e.0.saturating_add(size as u64),
        Dir::Send => e.1 = e.1.saturating_add(size as u64),
    }
}

/// Bytes per second from two running totals; 0 for a non-positive interval or a counter that
/// went backwards.
pub fn rate_bps(prev: u64, now: u64, elapsed_secs: f64) -> f64 {
    if elapsed_secs <= 0.0 || !elapsed_secs.is_finite() {
        return 0.0;
    }
    now.saturating_sub(prev) as f64 / elapsed_secs
}

/// Turns (pid, received, sent) totals into rows against the previous totals, which are then
/// replaced. Sorted by down + up rate (ties: larger total first, then pid), zero-total pids dropped.
fn build_rows(now: impl Iterator<Item = (u32, u64, u64)>, prev: &mut Totals, elapsed_secs: f64) -> Vec<AppBytes> {
    let mut rows = Vec::with_capacity(prev.len().max(16));
    for (pid, rx, tx) in now {
        if rx == 0 && tx == 0 {
            continue;
        }
        let (prx, ptx) = prev.get(&pid).copied().unwrap_or((0, 0));
        rows.push(AppBytes {
            pid,
            down_bps: rate_bps(prx, rx, elapsed_secs),
            up_bps: rate_bps(ptx, tx, elapsed_secs),
            total_down: rx,
            total_up: tx,
        });
        prev.insert(pid, (rx, tx));
    }
    rows.sort_by(|a, b| {
        let ra = a.down_bps + a.up_bps;
        let rb = b.down_bps + b.up_bps;
        rb.total_cmp(&ra)
            .then((b.total_down + b.total_up).cmp(&(a.total_down + a.total_up)))
            .then(a.pid.cmp(&b.pid))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_ids() {
        assert_eq!(classify(10), Some((Proto::Tcp, Dir::Send)));
        assert_eq!(classify(11), Some((Proto::Tcp, Dir::Recv)));
        assert_eq!(classify(26), Some((Proto::Tcp, Dir::Send)));
        assert_eq!(classify(27), Some((Proto::Tcp, Dir::Recv)));
        assert_eq!(classify(42), Some((Proto::Udp, Dir::Send)));
        assert_eq!(classify(43), Some((Proto::Udp, Dir::Recv)));
        assert_eq!(classify(58), Some((Proto::Udp, Dir::Send)));
        assert_eq!(classify(59), Some((Proto::Udp, Dir::Recv)));
        for id in [0, 12, 13, 14, 15, 16, 17, 18, 28, 29, 30, 49, 60, 1000] {
            assert_eq!(classify(id), None, "id {id}");
        }
    }

    #[test]
    fn parse_user_data() {
        let mut d = Vec::new();
        d.extend_from_slice(&1234u32.to_le_bytes());
        d.extend_from_slice(&1460u32.to_le_bytes());
        d.extend_from_slice(&[0u8; 20]);
        assert_eq!(parse_pid_size(&d), Some((1234, 1460)));
        assert_eq!(parse_pid_size(&d[..8]), Some((1234, 1460)));
        assert_eq!(parse_pid_size(&d[..7]), None);
        assert_eq!(parse_pid_size(&[]), None);
    }

    #[test]
    fn accumulate() {
        let mut t = Totals::new();
        add_bytes(&mut t, 7, Dir::Recv, 100);
        add_bytes(&mut t, 7, Dir::Send, 40);
        add_bytes(&mut t, 7, Dir::Recv, 5);
        assert_eq!(t[&7], (105, 40));
        add_bytes(&mut t, 7, Dir::Send, u32::MAX);
        t.get_mut(&7).unwrap().1 = u64::MAX - 1;
        add_bytes(&mut t, 7, Dir::Send, 10);
        assert_eq!(t[&7].1, u64::MAX);
    }

    #[test]
    fn rates() {
        assert_eq!(rate_bps(100, 300, 2.0), 100.0);
        assert_eq!(rate_bps(300, 100, 2.0), 0.0);
        assert_eq!(rate_bps(0, 100, 0.0), 0.0);
        assert_eq!(rate_bps(0, 100, -1.0), 0.0);
        assert_eq!(rate_bps(0, 100, f64::NAN), 0.0);
    }

    #[test]
    fn rows_sorted_and_filtered() {
        let mut prev = Totals::new();
        let now = vec![(1, 0, 0), (2, 1000, 0), (3, 100, 4000), (4, 10, 10)];
        let rows = build_rows(now.into_iter(), &mut prev, 1.0);
        let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![3, 2, 4]);
        assert_eq!(rows[0].down_bps, 100.0);
        assert_eq!(rows[0].up_bps, 4000.0);
        // Second interval: only pid 2 moved.
        let now = vec![(2, 3000, 0), (3, 100, 4000), (4, 10, 10)];
        let rows = build_rows(now.into_iter(), &mut prev, 2.0);
        assert_eq!(rows[0].pid, 2);
        assert_eq!(rows[0].down_bps, 1000.0);
        assert_eq!(rows[0].total_down, 3000);
        assert_eq!(rows[1].down_bps + rows[1].up_bps, 0.0);
    }
}
