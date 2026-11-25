//! Last hour of samples, compact. Fed every tick by `Insights::on_sample`.
//! OWNER: Incident Cards worker.

use super::Ctx;

pub struct Ring {}

impl Ring {
    pub fn new() -> Ring {
        Ring {}
    }
    pub fn push(&mut self, _c: &Ctx) {}
}
