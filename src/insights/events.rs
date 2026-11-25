//! Push-based Event Log listener (EvtSubscribe) and a small one-shot query helper.
//! OWNER: Incident Cards worker.

/// One Event Log record, already rendered to the fields we need.
#[derive(Clone, Debug, Default)]
pub struct SysEvent {
    /// "System" or "Application".
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
        self.data.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

/// Live subscription; dropping it closes the handles.
pub struct Listener {}

impl Listener {
    /// Subscribe to the System and Application events Insights cares about; each record is
    /// posted to `hwnd` as `WM_APP_INSIGHT_EVENT` with a boxed `SysEvent` in lParam.
    pub fn start(_hwnd: isize) -> Option<Listener> {
        None
    }
}

/// One-shot query, newest first, at most `max` records. `xpath` is a standard Event Log
/// XPath query (e.g. `*[System[Provider[@Name='Microsoft-Windows-WindowsUpdateClient'] and EventID=19]]`).
/// Blocking: call from a background thread or rarely (once a day / on wake).
pub fn query(_channel: &str, _xpath: &str, _max: usize) -> Vec<SysEvent> {
    Vec::new()
}
