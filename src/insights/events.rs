//! Push-based Event Log listener (EvtSubscribe) and a small one-shot query helper.
//! OWNER: Incident Cards worker.
//!
//! The listener asks Windows to call us back only for the handful of System / Application
//! events Insights uses (see `SYSTEM_QUERY` / `APPLICATION_QUERY`). There is no polling: the
//! callback runs on a Windows thread-pool thread, renders the record to XML, picks out the
//! few fields we need (`parse_event_xml`, pure and unit-tested) and posts a boxed `SysEvent`
//! to the main window. Standard users can read both logs; no admin needed.

use crate::util::wide;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_INSUFFICIENT_BUFFER};
use windows_sys::Win32::System::EventLog::*;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

/// One Event Log record, already rendered to the fields we need.
#[derive(Clone, Debug, Default)]
pub struct SysEvent {
    /// "System" or "Application".
    #[allow(dead_code)] // kept for diagnostics
    pub channel: String,
    /// Provider name, e.g. "Display", "Microsoft-Windows-WHEA-Logger".
    pub provider: String,
    pub id: u32,
    /// Event time, unix ms.
    pub t_ms: u64,
    /// EventData / UserData name=value pairs (unnamed Data gets "0", "1", ...).
    pub data: Vec<(String, String)>,
}

impl SysEvent {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.data
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

/// System-log events Insights uses. Several small Selects instead of one long XPath:
/// the Event Log rejects XPath expressions with too many sub-expressions.
const SYSTEM_QUERY: &str = "<QueryList><Query Id='0' Path='System'>\
<Select Path='System'>*[System[Provider[@Name='Display'] and EventID=4101]]</Select>\
<Select Path='System'>*[System[Provider[@Name='Microsoft-Windows-WHEA-Logger'] and \
(EventID=17 or EventID=18 or EventID=19 or EventID=47)]]</Select>\
<Select Path='System'>*[System[Provider[@Name='Microsoft-Windows-Kernel-Power'] and \
(EventID=41 or EventID=42 or EventID=107 or EventID=506 or EventID=507)]]</Select>\
<Select Path='System'>*[System[Provider[@Name='disk'] and (EventID=7 or EventID=51 or EventID=153)]]</Select>\
<Select Path='System'>*[System[Provider[@Name='Microsoft-Windows-Kernel-Processor-Power'] and EventID=37]]</Select>\
<Select Path='System'>*[System[Provider[@Name='Microsoft-Windows-Power-Troubleshooter'] and EventID=1]]</Select>\
<Select Path='System'>*[System[Provider[@Name='Microsoft-Windows-WindowsUpdateClient'] and \
(EventID=19 or EventID=20)]]</Select>\
</Query></QueryList>";

/// Application-log events Insights uses (crashes and hangs).
const APPLICATION_QUERY: &str = "<QueryList><Query Id='0' Path='Application'>\
<Select Path='Application'>*[System[Provider[@Name='Application Error'] and EventID=1000]]</Select>\
<Select Path='Application'>*[System[Provider[@Name='Application Hang'] and EventID=1002]]</Select>\
</Query></QueryList>";

/// Events logged early in boot, before the app starts: an unclean restart and a fatal
/// hardware error. Re-read once at start so the card still appears (Incidents de-dupes).
const BOOT_QUERY: &str = "*[System[((Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=41) or \
(Provider[@Name='Microsoft-Windows-WHEA-Logger'] and EventID=18)) and TimeCreated[timediff(@SystemTime) <= {ms}]]]";

/// Callback context: lives in the `Listener` until after its subscription is closed.
struct Sub {
    hwnd: isize,
    channel: &'static str,
}

/// Live subscription; dropping it closes the handles.
pub struct Listener {
    system: Option<(EVT_HANDLE, Box<Sub>)>,
    application: Option<(EVT_HANDLE, Box<Sub>)>,
}

impl Listener {
    /// Subscribe to the System and Application events Insights cares about; each record is
    /// posted to `hwnd` as `WM_APP_INSIGHT_EVENT` with a boxed `SysEvent` in lParam.
    /// A channel that cannot be opened is logged once and skipped; `None` if neither works.
    pub fn start(hwnd: isize) -> Option<Listener> {
        let l = Listener {
            system: subscribe(hwnd, "System", SYSTEM_QUERY),
            application: subscribe(hwnd, "Application", APPLICATION_QUERY),
        };
        replay_boot_events(hwnd);
        if l.system.is_none() && l.application.is_none() {
            return None;
        }
        Some(l)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        for (h, _ctx) in [self.system.take(), self.application.take()]
            .into_iter()
            .flatten()
        {
            // SAFETY: handle from EvtSubscribe, closed once. EvtClose on a subscription waits
            // for a running callback to return, so `_ctx` is dropped only after that.
            unsafe { EvtClose(h) };
        }
    }
}

fn subscribe(hwnd: isize, channel: &'static str, query: &str) -> Option<(EVT_HANDLE, Box<Sub>)> {
    let ctx = Box::new(Sub { hwnd, channel });
    let q = wide(query);
    // SAFETY: NUL-terminated query; the context pointer stays valid while the subscription
    // lives (the Box moves into the Listener, its heap address does not change).
    let h = unsafe {
        EvtSubscribe(
            0,
            std::ptr::null_mut(),
            std::ptr::null(),
            q.as_ptr(),
            0,
            &*ctx as *const Sub as *const core::ffi::c_void,
            Some(on_event),
            EvtSubscribeToFutureEvents,
        )
    };
    if h == 0 {
        // SAFETY: plain thread-local error read.
        let err = unsafe { GetLastError() };
        crate::util::diag(&format!(
            "insights: cannot watch the {} event log (error {})",
            channel, err
        ));
        return None;
    }
    Some((h, ctx))
}

/// Re-read today's boot-time events on a short-lived thread and post them like live ones.
fn replay_boot_events(hwnd: isize) {
    // SAFETY: no arguments.
    let up_ms = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    let ms = up_ms.saturating_add(10 * 60_000).min(86_400_000);
    let xpath = BOOT_QUERY.replace("{ms}", &ms.to_string());
    let _ = std::thread::Builder::new()
        .name("insights-boot-events".into())
        .spawn(move || {
            for e in query("System", &xpath, 8) {
                post(hwnd, e);
            }
        });
}

/// Hand an event to the UI thread; frees it if the window is gone.
fn post(hwnd: isize, e: SysEvent) {
    let p = Box::into_raw(Box::new(e));
    // SAFETY: the receiver (`app.rs`, WM_APP_INSIGHT_EVENT) takes ownership with
    // Box::from_raw. When posting fails nobody will, so we free it here.
    unsafe {
        if PostMessageW(hwnd as _, super::WM_APP_INSIGHT_EVENT, 0, p as isize) == 0 {
            drop(Box::from_raw(p));
        }
    }
}

/// Subscription callback (Windows thread pool). Never panics: errors just drop the record.
unsafe extern "system" fn on_event(
    action: EVT_SUBSCRIBE_NOTIFY_ACTION,
    ctx: *const core::ffi::c_void,
    ev: EVT_HANDLE,
) -> u32 {
    if action != EvtSubscribeActionDeliver || ctx.is_null() {
        return 0;
    }
    // SAFETY: `ctx` is the `Sub` registered with this subscription; it outlives it.
    let sub = &*(ctx as *const Sub);
    if let Some(e) = render(ev).and_then(|x| parse_event_xml(&x, sub.channel)) {
        post(sub.hwnd, e);
    }
    0
}

/// Render one event handle to its XML text.
fn render(ev: EVT_HANDLE) -> Option<String> {
    let mut buf: Vec<u16> = vec![0; 2048];
    for _ in 0..2 {
        let (mut used, mut props) = (0u32, 0u32);
        let bytes = (buf.len() * 2) as u32;
        // SAFETY: buffer is `bytes` long; out pointers are valid.
        let ok = unsafe {
            EvtRender(
                0,
                ev,
                EvtRenderEventXml,
                bytes,
                buf.as_mut_ptr() as *mut _,
                &mut used,
                &mut props,
            )
        };
        if ok != 0 {
            let n = (used as usize / 2).min(buf.len());
            return Some(crate::util::from_wide(&buf[..n]));
        }
        // SAFETY: plain thread-local error read.
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || used as usize > 1 << 20 {
            return None;
        }
        buf = vec![0; used as usize / 2 + 1];
    }
    None
}

/// One-shot query, newest first, at most `max` records. `xpath` is a standard Event Log
/// XPath query (e.g. `*[System[Provider[@Name='Microsoft-Windows-WindowsUpdateClient'] and EventID=19]]`).
/// Blocking: call from a background thread or rarely (once a day / on wake).
pub fn query(channel: &str, xpath: &str, max: usize) -> Vec<SysEvent> {
    let mut out = Vec::new();
    if max == 0 {
        return out;
    }
    let (ch, q) = (wide(channel), wide(xpath));
    // SAFETY: NUL-terminated strings; local session (0).
    let rs = unsafe {
        EvtQuery(
            0,
            ch.as_ptr(),
            q.as_ptr(),
            EvtQueryChannelPath | EvtQueryReverseDirection,
        )
    };
    if rs == 0 {
        return out;
    }
    let mut batch = [0isize; 16];
    'outer: while out.len() < max {
        let mut got = 0u32;
        // SAFETY: `batch` holds 16 handles; wait at most 5 s for a batch.
        if unsafe {
            EvtNext(
                rs,
                batch.len() as u32,
                batch.as_mut_ptr(),
                5000,
                0,
                &mut got,
            )
        } == 0
        {
            break; // ERROR_NO_MORE_ITEMS or a real error: either way we are done.
        }
        let got = (got as usize).min(batch.len());
        for (i, &h) in batch[..got].iter().enumerate() {
            if out.len() < max {
                if let Some(e) = render(h).and_then(|x| parse_event_xml(&x, channel)) {
                    out.push(e);
                }
            }
            // SAFETY: each returned handle is closed exactly once.
            unsafe { EvtClose(h) };
            if out.len() >= max {
                for &rest in &batch[i + 1..got] {
                    // SAFETY: as above, the remaining handles of this batch.
                    unsafe { EvtClose(rest) };
                }
                break 'outer;
            }
        }
    }
    // SAFETY: result-set handle from EvtQuery.
    unsafe { EvtClose(rs) };
    out
}

// ---------------------------------------------------------------------------------------------
// Minimal XML reading for rendered event records (pure, no allocations beyond the result).

/// Most name/value pairs kept per event; longer values are cut.
const MAX_DATA: usize = 32;
const MAX_VALUE: usize = 512;

/// Pick provider, id, time and EventData / UserData values out of `EvtRenderEventXml` text.
/// `channel` is used when the XML has no `<Channel>`. Returns None when it is not an event.
pub fn parse_event_xml(xml: &str, channel: &str) -> Option<SysEvent> {
    let sys = section(xml, "System")?;
    let provider = tag_open(sys, "Provider")
        .and_then(|t| attr(t, "Name"))
        .map(unescape)
        .unwrap_or_default();
    let id = text_of(sys, "EventID")?.trim().parse::<u32>().ok()?;
    let t_ms = tag_open(sys, "TimeCreated")
        .and_then(|t| attr(t, "SystemTime"))
        .and_then(iso_to_unix_ms)
        .unwrap_or(0);
    let channel = text_of(sys, "Channel")
        .map(unescape)
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| channel.to_string());
    let mut data = Vec::new();
    if let Some(ed) = section(xml, "EventData") {
        let mut unnamed = 0usize;
        leaves(ed, |tag, open, text| {
            if tag != "Data" {
                return;
            }
            let name = match attr(open, "Name") {
                Some(n) => unescape(n),
                None => {
                    unnamed += 1;
                    (unnamed - 1).to_string()
                }
            };
            push_pair(&mut data, name, text);
        });
    }
    if let Some(ud) = section(xml, "UserData") {
        leaves(ud, |tag, _open, text| {
            push_pair(&mut data, tag.to_string(), text)
        });
    }
    Some(SysEvent {
        channel,
        provider,
        id,
        t_ms,
        data,
    })
}

fn push_pair(data: &mut Vec<(String, String)>, name: String, text: &str) {
    if data.len() < MAX_DATA {
        let mut v = unescape(text.trim());
        if v.len() > MAX_VALUE {
            let mut cut = MAX_VALUE;
            while !v.is_char_boundary(cut) {
                cut -= 1;
            }
            v.truncate(cut);
        }
        data.push((name, v));
    }
}

/// Inner text of the first `<name ...>...</name>` element (None if absent or self-closing).
fn section<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let (start, open) = find_open(xml, name, 0)?;
    if open.ends_with("/>") {
        return Some("");
    }
    let body = start + open.len();
    let close = format!("</{}>", name);
    let end = xml[body..].find(&close)? + body;
    Some(&xml[body..end])
}

/// Inner text of a simple element such as `<EventID Qualifiers='0'>4101</EventID>`.
fn text_of<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let s = section(xml, name)?;
    Some(s.split('<').next().unwrap_or(""))
}

/// The opening tag text (`<Provider Name='x'/>`) of the first `name` element.
fn tag_open<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    find_open(xml, name, 0).map(|(_, t)| t)
}

/// Find `<name` followed by a delimiter; returns (byte offset, whole opening tag).
fn find_open<'a>(xml: &'a str, name: &str, from: usize) -> Option<(usize, &'a str)> {
    let mut i = from;
    while let Some(p) = xml.get(i..)?.find('<') {
        let s = i + p;
        let rest = &xml[s + 1..];
        if rest.starts_with(name) {
            let after = rest.as_bytes().get(name.len()).copied();
            if matches!(after, Some(b' ' | b'>' | b'/' | b'\t' | b'\r' | b'\n')) {
                let end = xml[s..].find('>')? + s + 1;
                return Some((s, &xml[s..end]));
            }
        }
        i = s + 1;
    }
    None
}

/// Value of `name='v'` or `name="v"` inside an opening tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut i = 0;
    while let Some(p) = tag.get(i..)?.find(name) {
        let s = i + p;
        let before_ok = s > 0 && matches!(tag.as_bytes()[s - 1], b' ' | b'\t' | b'\r' | b'\n');
        let rest = tag[s + name.len()..].trim_start();
        if before_ok {
            if let Some(r) = rest.strip_prefix('=') {
                let r = r.trim_start();
                let q = r.chars().next()?;
                if q == '\'' || q == '"' {
                    let end = r[1..].find(q)?;
                    return Some(&r[1..1 + end]);
                }
            }
        }
        i = s + name.len();
    }
    None
}

/// Call `f(tag, opening_tag, text)` for every leaf element (no child elements) in `xml`.
fn leaves<'a>(xml: &'a str, mut f: impl FnMut(&'a str, &'a str, &'a str)) {
    // The innermost open element not yet closed: (name, opening tag, body start).
    let mut cur: Option<(&str, &str, usize)> = None;
    let mut i = 0;
    while let Some(p) = xml.get(i..).and_then(|s| s.find('<')) {
        let s = i + p;
        let Some(e) = xml[s..].find('>') else { return };
        let end = s + e + 1;
        let tag = &xml[s..end];
        if tag.starts_with("</") {
            let name = tag[2..tag.len() - 1].trim();
            if let Some((n, open, body)) = cur.take() {
                if n == name {
                    f(n, open, &xml[body..s]);
                }
            }
        } else if tag.starts_with("<?") || tag.starts_with("<!") {
            // declaration / comment: skip
        } else {
            let name = tag[1..]
                .split(|c: char| c == ' ' || c == '/' || c == '>' || c.is_whitespace())
                .next()
                .unwrap_or("");
            if tag.ends_with("/>") {
                cur = None;
                f(name, tag, "");
            } else {
                cur = Some((name, tag, end));
            }
        }
        i = end;
    }
}

/// Decode the five XML entities and numeric references.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(semi) = rest.find(';').filter(|&n| n <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16)
                .ok()
                .and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// "2024-05-01T12:34:56.1234567Z" (UTC) -> unix ms.
fn iso_to_unix_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut ms = 0i64;
    if b.get(19) == Some(&b'.') {
        let frac: String = s[20..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .take(3)
            .collect();
        if !frac.is_empty() {
            ms = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
        }
    }
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let t = ((days * 86_400 + h * 3600 + mi * 60 + se) * 1000) + ms;
    (t >= 0).then_some(t as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY_4101: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
<Provider Name='Display'/><EventID Qualifiers='0'>4101</EventID><Version>0</Version><Level>3</Level><Task>0</Task>\
<Opcode>0</Opcode><Keywords>0x80000000000000</Keywords><TimeCreated SystemTime='2025-11-25T13:02:07.5214416Z'/>\
<EventRecordID>48211</EventRecordID><Correlation/><Execution ProcessID='0' ThreadID='0'/><Channel>System</Channel>\
<Computer>DESKTOP-1</Computer><Security/></System><EventData><Data>nvlddmkm</Data><Data></Data></EventData></Event>";

    const WHEA_17: &str = "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
<Provider Name='Microsoft-Windows-WHEA-Logger' Guid='{c26c4f3c-3f66-4e99-8f8a-39405cfed220}'/><EventID>17</EventID>\
<Version>0</Version><Level>3</Level><Task>0</Task><Opcode>0</Opcode><Keywords>0x8000000000000000</Keywords>\
<TimeCreated SystemTime='2025-11-25T08:15:00.000000000Z'/><EventRecordID>9001</EventRecordID>\
<Correlation ActivityID='{6b5d0b6e-1111-2222-3333-444455556666}'/><Execution ProcessID='4' ThreadID='2196'/>\
<Channel>System</Channel><Computer>pc</Computer><Security UserID='S-1-5-19'/></System><EventData>\
<Data Name='ErrorSource'>4</Data><Data Name='ErrorType'>4</Data><Data Name='Length'>1004</Data>\
<Data Name='PrimaryDeviceName'>PCI\\VEN_8086&amp;DEV_A33C</Data><Data Name='RawData'/></EventData></Event>";

    const APP_1000: &str = "<Event xmlns=\"http://schemas.microsoft.com/win/2004/08/events/event\"><System>\
<Provider Name=\"Application Error\" Guid=\"{a0e9b465-b939-57d7-b27d-95d8e925ff57}\"/><EventID>1000</EventID>\
<Version>0</Version><Level>2</Level><Task>100</Task><Opcode>0</Opcode><Keywords>0x8000000000000000</Keywords>\
<TimeCreated SystemTime=\"2025-11-25T20:40:01.1Z\"/><EventRecordID>777</EventRecordID><Correlation/>\
<Execution ProcessID=\"9120\" ThreadID=\"1\"/><Channel>Application</Channel><Computer>pc</Computer><Security/>\
</System><EventData><Data Name=\"AppName\">Game.exe</Data><Data Name=\"AppVersion\">1.0.0.0</Data>\
<Data Name=\"ModuleName\">d3d11.dll</Data><Data Name=\"ExceptionCode\">c0000005</Data></EventData></Event>";

    const USERDATA: &str = "<Event><System><Provider Name='Microsoft-Windows-Kernel-Power'/><EventID>41</EventID>\
<TimeCreated SystemTime='1970-01-01T00:00:01Z'/></System><UserData><Ev xmlns='x'><BugcheckCode>0</BugcheckCode>\
<SleepInProgress>4</SleepInProgress><Empty/></Ev></UserData></Event>";

    #[test]
    fn display_reset() {
        let e = parse_event_xml(DISPLAY_4101, "x").expect("parsed");
        assert_eq!(e.provider, "Display");
        assert_eq!(e.id, 4101);
        assert_eq!(e.channel, "System");
        assert_eq!(e.t_ms, 1_764_075_727_521);
        assert_eq!(e.get("0"), Some("nvlddmkm"));
        assert_eq!(e.get("1"), Some(""));
    }

    #[test]
    fn whea_named_data_and_entities() {
        let e = parse_event_xml(WHEA_17, "System").expect("parsed");
        assert_eq!(e.provider, "Microsoft-Windows-WHEA-Logger");
        assert_eq!(e.id, 17);
        assert_eq!(e.t_ms, 1_764_058_500_000);
        assert_eq!(e.get("ErrorSource"), Some("4"));
        assert_eq!(e.get("PrimaryDeviceName"), Some("PCI\\VEN_8086&DEV_A33C"));
        assert_eq!(e.get("RawData"), Some(""));
    }

    #[test]
    fn app_crash_double_quotes() {
        let e = parse_event_xml(APP_1000, "Application").expect("parsed");
        assert_eq!(e.provider, "Application Error");
        assert_eq!(e.id, 1000);
        assert_eq!(e.channel, "Application");
        assert_eq!(e.t_ms, 1_764_103_201_100);
        assert_eq!(e.get("AppName"), Some("Game.exe"));
        assert_eq!(e.get("ModuleName"), Some("d3d11.dll"));
    }

    #[test]
    fn user_data_and_channel_fallback() {
        let e = parse_event_xml(USERDATA, "System").expect("parsed");
        assert_eq!(e.channel, "System");
        assert_eq!(e.t_ms, 1000);
        assert_eq!(e.get("BugcheckCode"), Some("0"));
        assert_eq!(e.get("SleepInProgress"), Some("4"));
        assert_eq!(e.get("Empty"), Some(""));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_event_xml("", "System").is_none());
        assert!(parse_event_xml(
            "<Event><System><EventID>abc</EventID></System></Event>",
            "S"
        )
        .is_none());
        assert!(parse_event_xml("<Event><System><EventID>5", "S").is_none());
        assert_eq!(iso_to_unix_ms("nope"), None);
        assert_eq!(unescape("a &amp; b &#65; &bogus; &"), "a & b A &bogus; &");
    }
}
