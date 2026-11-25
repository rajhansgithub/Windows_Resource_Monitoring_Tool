//! Incident Cards. OWNER: Incident Cards worker.

use super::{Ctx, Panel, Tone};
use crate::achievements::json::Json;
use super::Notice;
use super::events::SysEvent;

pub struct Incidents {}

impl Incidents {
    pub fn new() -> Incidents {
        Incidents {}
    }
    pub fn on_sample(&mut self, _c: &Ctx, _ring: &super::ring::Ring) -> Vec<Notice> {
        Vec::new()
    }
    pub fn on_event(&mut self, _e: &SysEvent, _ring: &super::ring::Ring, _now_ms: u64) -> Vec<Notice> {
        Vec::new()
    }
    pub fn wants_procs(&self) -> bool {
        false
    }
    /// Persistent state changed since the last save.
    pub fn dirty(&self) -> bool {
        false
    }
    pub fn save(&self) -> Json {
        Json::Null
    }
    pub fn load(&mut self, _j: &Json) {}
    pub fn panel(&self, _now_ms: u64) -> Panel {
        Panel { key: "incidents", title: "Incident Cards", tone: Tone::Learning, headline: "Not built yet.".into(), lines: vec![], chart: None, items: vec![] }
    }
}
