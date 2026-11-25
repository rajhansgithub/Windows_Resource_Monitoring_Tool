//! Before / After. OWNER: Before / After worker.

use super::{Ctx, Panel, Tone};
use crate::achievements::json::Json;
use super::events::SysEvent;

pub struct Baseline {}

impl Baseline {
    pub fn new() -> Baseline {
        Baseline {}
    }
    pub fn on_sample(&mut self, _c: &Ctx) {}
    pub fn on_event(&mut self, _e: &SysEvent) {}
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
        Panel { key: "baseline", title: "Before / After", tone: Tone::Learning, headline: "Not built yet.".into(), lines: vec![], chart: None, items: vec![] }
    }
}
