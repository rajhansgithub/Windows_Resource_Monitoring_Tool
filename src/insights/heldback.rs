//! Held Back. OWNER: Held Back + Ghost Memory worker.

use super::{Ctx, Panel, Tone};
use crate::achievements::json::Json;
use super::Power;
use super::events::SysEvent;

pub struct HeldBack {}

impl HeldBack {
    pub fn new() -> HeldBack {
        HeldBack {}
    }
    pub fn on_sample(&mut self, _c: &Ctx) {}
    pub fn on_event(&mut self, _e: &SysEvent) {}
    pub fn on_power(&mut self, _p: Power) {}
    pub fn save(&self) -> Json {
        Json::Null
    }
    pub fn load(&mut self, _j: &Json) {}
    pub fn panel(&self, _now_ms: u64) -> Panel {
        Panel { key: "heldback", title: "Held Back", tone: Tone::Learning, headline: "Not built yet.".into(), lines: vec![], chart: None, items: vec![] }
    }
}

/// Register for effective power mode changes (PowerRegisterForEffectivePowerModeNotifications);
/// the callback posts `WM_APP_POWER_MODE` to `hwnd`.
pub fn register_power_mode(_hwnd: isize) {}
