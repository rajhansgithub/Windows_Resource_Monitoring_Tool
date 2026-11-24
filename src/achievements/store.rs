//! Achievements storage.
//!
//! * `%APPDATA%\ResourceMonitor\achievements.json` (roaming) mirrored to
//!   `%LOCALAPPDATA%\ResourceMonitor\achievements.json`. Both are merged on load
//!   (union / max), so a device never loses what another one earned.
//! * `achievements-new.json` (local only): queue of moments waiting to be shown.
//!
//! The `*_at`/`*_from`/`*_to` variants take explicit paths (used by tests).

use std::io;
use std::path::{Path, PathBuf};

use super::core::{merge, normalize, State, Unlock};
use super::json::{self, Json};
use crate::util;

const FILE: &str = "achievements.json";
const PENDING_FILE: &str = "achievements-new.json";
const PENDING_MAX: usize = 50;

pub fn roaming_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|b| PathBuf::from(b).join("ResourceMonitor").join(FILE))
}

pub fn local_path() -> PathBuf {
    util::data_dir().join(FILE)
}

pub fn pending_path() -> PathBuf {
    util::data_dir().join(PENDING_FILE)
}

/// Read one copy. A file that exists but cannot be parsed is kept as `*.bad.json`
/// (once) and treated as missing.
fn read_state(path: &Path) -> Option<State> {
    let text = std::fs::read_to_string(path).ok()?;
    match State::from_json(&text) {
        Some(s) => Some(s),
        None => {
            let _ = std::fs::copy(path, path.with_extension("bad.json"));
            None
        }
    }
}

/// Load and merge the roaming and local copies. Default state when neither exists.
pub fn load() -> State {
    load_from(roaming_path().as_deref(), &local_path())
}

pub fn load_from(roaming: Option<&Path>, local: &Path) -> State {
    let a = roaming.and_then(read_state);
    let b = read_state(local);
    match (a, b) {
        (Some(a), Some(b)) => merge(&a, &b),
        (Some(s), None) | (None, Some(s)) => s,
        (None, None) => State::default(),
    }
}

/// Merge with what is on disk (another device may have saved), then write both
/// copies atomically, skipping any file that already has the same content.
/// Returns the merged state; adopt it as the new in-memory state.
pub fn save(state: &State) -> io::Result<State> {
    save_to(state, roaming_path().as_deref(), &local_path())
}

pub fn save_to(state: &State, roaming: Option<&Path>, local: &Path) -> io::Result<State> {
    let on_disk = load_from(roaming, local);
    let merged = merge(state, &on_disk);
    let text = merged.to_json();
    let mut result = Ok(());
    for path in roaming.into_iter().chain(std::iter::once(local)) {
        if let Err(e) = write_if_changed(path, &text) {
            result = Err(e);
        }
    }
    result.map(|_| merged)
}

/// True when the stored file already holds exactly this canonical content.
fn write_if_changed(path: &Path, text: &str) -> io::Result<()> {
    if let Ok(old) = std::fs::read_to_string(path) {
        if old == text {
            return Ok(());
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    util::write_atomic(path, text.as_bytes())
}

/// Same content, whatever the key order or whitespace of the stored text.
#[allow(dead_code)]
pub fn same_content(a: &State, b: &State) -> bool {
    normalize(a.clone()).to_json() == normalize(b.clone()).to_json()
}

/// Canonical form of stored text (`None` when it is not a state).
#[allow(dead_code)]
pub fn canonical(text: &str) -> Option<String> {
    State::from_json(text).map(|s| s.to_json())
}

// ---------------------------------------------------------------------------
// Pending moments
// ---------------------------------------------------------------------------

/// Something to show the user: one entry per `Unlock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moment {
    /// "badge", "record", "level" or "gate".
    pub kind: String,
    /// Badge id, set id, or (level) the new rank name when the rank changed; "" otherwise.
    pub id: String,
    pub from: u32,
    pub to: u32,
    pub xp: u32,
    /// Unix ms the unlock happened.
    pub at: u64,
    /// Emulsion climb right after the gate opened: show nothing of its own.
    pub quiet: bool,
}

impl Moment {
    pub fn from_unlock(u: &Unlock, at: u64) -> Moment {
        let m = |kind: &str, id: &str, from: u32, to: u32, xp: u32, quiet: bool| Moment {
            kind: kind.to_string(),
            id: id.to_string(),
            from,
            to,
            xp,
            at,
            quiet,
        };
        match u {
            Unlock::Badge { id, from, to, xp, quiet } => m("badge", id, *from as u32, *to as u32, *xp, *quiet),
            Unlock::Record { set, from, to, xp } => m("record", set, *from as u32, *to as u32, *xp, false),
            Unlock::Level { from, to, rank, .. } => m("level", rank.map_or("", |r| r.name()), *from, *to, 0, false),
            Unlock::Gate => m("gate", "", 0, 0, 0, false),
        }
    }

    /// Identity used to remove a shown moment: `kind:id:to:at`.
    pub fn key(&self) -> String {
        format!("{}:{}:{}:{}", self.kind, self.id, self.to, self.at)
    }

    fn to_json(&self) -> Json {
        let n = |v: u64| Json::Num(v as f64);
        let mut o = std::collections::BTreeMap::new();
        o.insert("k".to_string(), Json::Str(self.kind.clone()));
        o.insert("id".to_string(), Json::Str(self.id.clone()));
        o.insert("f".to_string(), n(self.from as u64));
        o.insert("t".to_string(), n(self.to as u64));
        o.insert("x".to_string(), n(self.xp as u64));
        o.insert("at".to_string(), n(self.at));
        o.insert("q".to_string(), n(self.quiet as u64));
        Json::Obj(o)
    }

    fn from_json(j: &Json) -> Option<Moment> {
        let n = |k: &str| j.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        Some(Moment {
            kind: j.get("k")?.as_str()?.to_string(),
            id: j.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            from: n("f") as u32,
            to: n("t") as u32,
            xp: n("x") as u32,
            at: n("at"),
            quiet: n("q") != 0,
        })
    }
}

pub fn read_pending() -> Vec<Moment> {
    read_pending_at(&pending_path())
}

pub fn read_pending_at(path: &Path) -> Vec<Moment> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    json::parse(&text)
        .and_then(|j| j.as_arr().map(|a| a.iter().filter_map(Moment::from_json).collect()))
        .unwrap_or_default()
}

fn write_pending(path: &Path, items: &[Moment]) -> io::Result<()> {
    let text = Json::Arr(items.iter().map(|m| m.to_json()).collect()).to_string();
    util::write_atomic(path, text.as_bytes())
}

/// Append moments (same `key()` is not queued twice; keeps the newest 50).
pub fn push_pending(items: &[Moment]) -> io::Result<()> {
    push_pending_at(&pending_path(), items)
}

pub fn push_pending_at(path: &Path, items: &[Moment]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut all = read_pending_at(path);
    for it in items {
        if !all.iter().any(|m| m.key() == it.key()) {
            all.push(it.clone());
        }
    }
    if all.len() > PENDING_MAX {
        all.drain(..all.len() - PENDING_MAX);
    }
    write_pending(path, &all)
}

/// Remove a shown moment by its `key()`.
pub fn pop_pending(key: &str) -> io::Result<()> {
    pop_pending_at(&pending_path(), key)
}

pub fn pop_pending_at(path: &Path, key: &str) -> io::Result<()> {
    let mut all = read_pending_at(path);
    let n = all.len();
    all.retain(|m| m.key() != key);
    if all.len() == n {
        return Ok(());
    }
    write_pending(path, &all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::achievements::core::{apply, Ctx, Event};

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("resmon-ach-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn earned(id: &str, at: u64) -> State {
        let mut s = State::default();
        s.u.insert(id.to_string(), at);
        s
    }

    #[test]
    fn save_writes_both_copies_and_load_merges() {
        let d = temp_dir("save");
        let (r, l) = (d.join("roam").join(FILE), d.join("local").join(FILE));
        let s = earned("power_on", 100);
        let merged = save_to(&s, Some(&r), &l).unwrap();
        assert_eq!(merged, s);
        assert_eq!(std::fs::read_to_string(&r).unwrap(), std::fs::read_to_string(&l).unwrap());
        assert_eq!(load_from(Some(&r), &l), s);

        // Another device wrote something else to the roaming copy.
        let mut other = earned("first_gb", 50);
        other.u.insert("power_on".into(), 40);
        std::fs::write(&r, other.to_json()).unwrap();
        let loaded = load_from(Some(&r), &l);
        assert_eq!(loaded.u["power_on"], 40);
        assert!(loaded.earned("first_gb"));

        // Saving never loses what is on disk.
        let merged = save_to(&earned("up_to_date", 7), Some(&r), &l).unwrap();
        assert!(merged.earned("first_gb") && merged.earned("up_to_date") && merged.earned("power_on"));
        assert_eq!(load_from(Some(&r), &l), merged);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_or_corrupt_files() {
        let d = temp_dir("corrupt");
        let (r, l) = (d.join("roam").join(FILE), d.join(FILE));
        assert_eq!(load_from(Some(&r), &l), State::default());
        assert_eq!(load_from(None, &l), State::default());
        std::fs::write(&l, "{ not json").unwrap();
        assert_eq!(load_from(None, &l), State::default());
        assert!(l.with_extension("bad.json").exists());
        // The good copy wins over the corrupt one.
        std::fs::create_dir_all(r.parent().unwrap()).unwrap();
        std::fs::write(&r, earned("power_on", 5).to_json()).unwrap();
        assert!(load_from(Some(&r), &l).earned("power_on"));
        // No roaming dir (APPDATA unset): local only works.
        let merged = save_to(&earned("first_gb", 3), None, &l).unwrap();
        assert!(load_from(None, &l).earned("first_gb") && merged.earned("first_gb"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn same_content_ignores_key_order_and_spacing() {
        let s = earned("power_on", 100);
        let text = s.to_json();
        let shuffled = "{ \"u\" : { \"power_on\" : 100 } , \"v\":1 }";
        assert_eq!(canonical(shuffled).unwrap(), text);
        assert!(same_content(&s, &State::from_json(shuffled).unwrap()));
        assert!(!same_content(&s, &State::default()));
    }

    #[test]
    fn unchanged_content_is_not_rewritten() {
        let d = temp_dir("same");
        let p = d.join("x.json");
        write_if_changed(&p, "abc").unwrap();
        let t0 = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        write_if_changed(&p, "abc").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), t0);
        write_if_changed(&p, "abcd").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "abcd");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pending_queue_push_pop() {
        let d = temp_dir("pending");
        let p = d.join(PENDING_FILE);
        assert!(read_pending_at(&p).is_empty());
        let (_, un) = apply(&State::default(), &Event::UpdateInstalled { now_ms: 42 }, &Ctx::default());
        let moments: Vec<Moment> = un.iter().map(|u| Moment::from_unlock(u, 42)).collect();
        assert_eq!(moments.len(), 1);
        assert_eq!(moments[0].key(), "badge:up_to_date:0:42");
        push_pending_at(&p, &moments).unwrap();
        push_pending_at(&p, &moments).unwrap(); // no duplicate
        let extra = Moment { kind: "level".into(), id: "Silver".into(), from: 4, to: 5, xp: 0, at: 43, quiet: false };
        push_pending_at(&p, &[extra.clone()]).unwrap();
        let got = read_pending_at(&p);
        assert_eq!(got, vec![moments[0].clone(), extra.clone()]);
        pop_pending_at(&p, &moments[0].key()).unwrap();
        assert_eq!(read_pending_at(&p), vec![extra.clone()]);
        pop_pending_at(&p, "nope").unwrap();
        pop_pending_at(&p, &extra.key()).unwrap();
        assert!(read_pending_at(&p).is_empty());
        // Capped.
        let many: Vec<Moment> = (0..80)
            .map(|i| Moment { kind: "badge".into(), id: "x".into(), from: 0, to: 0, xp: 1, at: i, quiet: i % 2 == 0 })
            .collect();
        push_pending_at(&p, &many).unwrap();
        let got = read_pending_at(&p);
        assert_eq!(got.len(), 50);
        assert_eq!(got.last().unwrap().at, 79);
        assert!(got[0].quiet);
        let _ = std::fs::remove_dir_all(&d);
    }
}
