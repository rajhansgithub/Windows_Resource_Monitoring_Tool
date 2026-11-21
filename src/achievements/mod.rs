//! Achievements: pure core (`core`), persistence (`store`), tiny JSON helper (`json`).
//! Not wired into the app yet, so unused items are allowed for now.
#![allow(dead_code, unused_imports)]

pub mod core;
mod json;
pub mod store;

pub use self::core::*;
pub use self::store::Moment;
