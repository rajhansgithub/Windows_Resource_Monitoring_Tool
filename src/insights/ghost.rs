//! Ghost Memory. OWNER: Held Back + Ghost Memory worker.

use super::{Ctx, Panel, Tone};
use crate::achievements::json::Json;

pub struct Ghost {}

impl Ghost {
    pub fn new() -> Ghost {
        Ghost {}
    }
    pub fn on_sample(&mut self, _c: &Ctx) {}
    /// Persistent state changed since the last save.
    pub fn dirty(&self) -> bool {
        false
    }
    pub fn save(&self) -> Json {
        Json::Null
    }
    pub fn load(&mut self, _j: &Json) {}
    pub fn panel(&self, _now_ms: u64) -> Panel {
        Panel { key: "ghost", title: "Ghost Memory", tone: Tone::Learning, headline: "Not built yet.".into(), lines: vec![], chart: None, items: vec![] }
    }
}
