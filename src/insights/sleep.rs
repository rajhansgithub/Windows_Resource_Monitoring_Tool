//! Sleep Verdict. OWNER: Sleep Verdict worker.

use super::{Ctx, Panel, Tone};
use crate::achievements::json::Json;
use super::Notice;
use super::Power;
use super::events::SysEvent;

pub struct Sleep {}

impl Sleep {
    pub fn new() -> Sleep {
        Sleep {}
    }
    pub fn on_sample(&mut self, _c: &Ctx) -> Vec<Notice> {
        Vec::new()
    }
    pub fn on_event(&mut self, _e: &SysEvent) {}
    pub fn on_power(&mut self, _p: Power, _now_ms: u64) -> Vec<Notice> {
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
        Panel { key: "sleep", title: "Sleep Verdict", tone: Tone::Learning, headline: "Not built yet.".into(), lines: vec![], chart: None, items: vec![] }
    }
}
