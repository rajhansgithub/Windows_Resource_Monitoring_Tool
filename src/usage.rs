//! Persistent per-day network usage. Exact byte counts from interface counters.
//! Stored as CSV: `YYYY-MM-DD,sent,received`.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Date key: yyyymmdd.
pub type Day = u32;

pub fn day(y: u32, m: u32, d: u32) -> Day {
    y * 10000 + m * 100 + d
}
pub fn ymd(k: Day) -> (u32, u32, u32) {
    (k / 10000, (k / 100) % 100, k % 100)
}

pub fn today() -> Day {
    let st = crate::util::local_time();
    day(st.wYear as u32, st.wMonth as u32, st.wDay as u32)
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

pub fn prev_day(k: Day) -> Day {
    let (y, m, d) = ymd(k);
    if d > 1 {
        day(y, m, d - 1)
    } else if m > 1 {
        day(y, m - 1, days_in_month(y, m - 1))
    } else {
        day(y - 1, 12, 31)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub sent: u64,
    pub recv: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.sent + self.recv
    }
    fn add(&mut self, o: Usage) {
        self.sent += o.sent;
        self.recv += o.recv;
    }
}

pub struct UsageStore {
    pub days: BTreeMap<Day, Usage>,
    dirty: bool,
    path: PathBuf,
}

impl UsageStore {
    pub fn load() -> Self {
        let path = crate::util::data_dir().join("usage.csv");
        let mut s = UsageStore { days: BTreeMap::new(), dirty: false, path };
        match std::fs::read_to_string(&s.path) {
            Ok(txt) => s.parse_csv(txt.trim_start_matches('\u{FEFF}')),
            Err(_) => {
                s.import_legacy();
                if !s.days.is_empty() {
                    s.dirty = true;
                    s.save();
                }
            }
        }
        s
    }

    fn parse_csv(&mut self, txt: &str) {
        for line in txt.lines() {
            let mut it = line.split(',');
            let (Some(d), Some(a), Some(b)) = (it.next(), it.next(), it.next()) else { continue };
            let Some(k) = parse_date(d.trim()) else { continue };
            let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) else { continue };
            self.days.insert(k, Usage { sent: a, recv: b });
        }
    }

    /// Old .NET format: {"DailyStats":{"2026-02-21":{"BytesSent":1,"BytesReceived":2},...}}
    fn import_legacy(&mut self) {
        let Some(dir) = crate::util::legacy_dir() else { return };
        let Ok(txt) = std::fs::read_to_string(dir.join("network_usage.json")) else { return };
        let mut rest = txt.as_str();
        while let Some(i) = rest.find('"') {
            rest = &rest[i + 1..];
            let Some(j) = rest.find('"') else { break };
            let key = &rest[..j];
            rest = &rest[j + 1..];
            let Some(k) = parse_date(key) else { continue };
            let Some(end) = rest.find('}') else { break };
            let body = &rest[..end];
            let num = |name: &str| -> u64 {
                body.find(name)
                    .and_then(|p| {
                        let t = &body[p + name.len()..];
                        let t = t[t.find(':')? + 1..].trim_start();
                        let n = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
                        t[..n].parse().ok()
                    })
                    .unwrap_or(0)
            };
            self.days.insert(k, Usage { sent: num("BytesSent"), recv: num("BytesReceived") });
            rest = &rest[end..];
        }
    }

    pub fn add(&mut self, sent: u64, recv: u64) {
        if sent == 0 && recv == 0 {
            return;
        }
        self.days.entry(today()).or_default().add(Usage { sent, recv });
        self.dirty = true;
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let mut o = String::with_capacity(self.days.len() * 32);
        for (k, u) in &self.days {
            let (y, m, d) = ymd(*k);
            o.push_str(&format!("{:04}-{:02}-{:02},{},{}\n", y, m, d, u.sent, u.recv));
        }
        if crate::util::write_atomic(&self.path, o.as_bytes()).is_ok() {
            self.dirty = false;
        }
    }

    pub fn get(&self, k: Day) -> Usage {
        self.days.get(&k).copied().unwrap_or_default()
    }

    pub fn range(&self, from: Day, to_incl: Day) -> Usage {
        let mut u = Usage::default();
        for (_, v) in self.days.range(from..=to_incl) {
            u.add(*v);
        }
        u
    }

    pub fn month(&self, y: u32, m: u32) -> Usage {
        self.range(day(y, m, 1), day(y, m, 31))
    }

    pub fn year(&self, y: u32) -> Usage {
        self.range(day(y, 1, 1), day(y, 12, 31))
    }

    pub fn all(&self) -> Usage {
        self.range(0, u32::MAX)
    }

    /// Start day of the current billing cycle given a reset day-of-month.
    pub fn cycle_start(reset: u32, now: Day) -> Day {
        let (y, m, d) = ymd(now);
        if d >= reset {
            day(y, m, reset)
        } else if m > 1 {
            day(y, m - 1, reset)
        } else {
            day(y - 1, 12, reset)
        }
    }
}

fn parse_date(s: &str) -> Option<Day> {
    let mut it = s.split('-');
    let y: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    (it.next().is_none() && (1..=12).contains(&m) && (1..=31).contains(&d) && y > 1900).then(|| day(y, m, d))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates() {
        assert_eq!(prev_day(day(2024, 3, 1)), day(2024, 2, 29));
        assert_eq!(prev_day(day(2026, 1, 1)), day(2025, 12, 31));
        assert_eq!(UsageStore::cycle_start(15, day(2026, 1, 3)), day(2025, 12, 15));
        assert_eq!(parse_date("2026-02-21"), Some(20260221));
        assert_eq!(parse_date("DailyStats"), None);
    }
}
