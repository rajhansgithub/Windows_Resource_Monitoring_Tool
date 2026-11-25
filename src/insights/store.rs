//! `%LOCALAPPDATA%\ResourceMonitor\insights.json`: incidents, nights, daily rows.
//! Small (kept well under 64 KB by each module's caps), written atomically.

use crate::achievements::json::{self, Json};
use std::path::PathBuf;

pub fn path() -> PathBuf {
    crate::util::data_dir().join("insights.json")
}

pub fn load() -> Option<Json> {
    let t = std::fs::read_to_string(path()).ok()?;
    json::parse(&t)
}

pub fn save(j: &Json) -> std::io::Result<()> {
    crate::util::write_atomic(&path(), j.to_string().as_bytes())
}

/// Helpers for the modules' save/load code.
pub fn num(j: &Json, k: &str) -> Option<f64> {
    match j.get(k)? {
        Json::Num(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}

pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
