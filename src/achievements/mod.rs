//! Achievements: pure core (`core`), persistence (`store`), tiny JSON helper (`json`).
//! The observer is wired into the app; unused items are still allowed for the UI to pick up.
#![allow(dead_code, unused_imports)]

pub mod core;
pub mod gpu;
mod json;
pub mod observer;
pub mod store;

pub use self::core::*;
pub use self::store::Moment;
