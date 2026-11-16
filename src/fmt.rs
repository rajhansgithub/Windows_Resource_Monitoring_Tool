//! Number formatting. Fixed-width friendly (3 significant digits).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedUnit {
    AutoBytes,
    AutoBits,
    KBs,
    MBs,
    Kbps,
    Mbps,
}

impl SpeedUnit {
    pub const ALL: [SpeedUnit; 6] = [
        SpeedUnit::AutoBytes,
        SpeedUnit::AutoBits,
        SpeedUnit::KBs,
        SpeedUnit::MBs,
        SpeedUnit::Kbps,
        SpeedUnit::Mbps,
    ];
    pub fn key(self) -> &'static str {
        match self {
            SpeedUnit::AutoBytes => "auto",
            SpeedUnit::AutoBits => "autobits",
            SpeedUnit::KBs => "KB/s",
            SpeedUnit::MBs => "MB/s",
            SpeedUnit::Kbps => "Kbps",
            SpeedUnit::Mbps => "Mbps",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SpeedUnit::AutoBytes => "Auto (B/s, KB/s, MB/s)",
            SpeedUnit::AutoBits => "Auto (bps, Kbps, Mbps)",
            SpeedUnit::KBs => "KB/s",
            SpeedUnit::MBs => "MB/s",
            SpeedUnit::Kbps => "Kbps",
            SpeedUnit::Mbps => "Mbps",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|u| u.key().eq_ignore_ascii_case(s))
    }
}

/// 3 significant digits: 1.23 / 12.3 / 123
fn sig3(v: f64) -> String {
    if v < 9.995 {
        format!("{:.2}", v)
    } else if v < 99.95 {
        format!("{:.1}", v)
    } else {
        format!("{:.0}", v)
    }
}

pub fn speed(bytes_per_s: f64, unit: SpeedUnit) -> String {
    let b = bytes_per_s.max(0.0);
    match unit {
        SpeedUnit::KBs => format!("{} KB/s", sig3(b / 1024.0)),
        SpeedUnit::MBs => format!("{} MB/s", sig3(b / 1048576.0)),
        SpeedUnit::Kbps => format!("{} Kbps", sig3(b * 8.0 / 1e3)),
        SpeedUnit::Mbps => format!("{} Mbps", sig3(b * 8.0 / 1e6)),
        SpeedUnit::AutoBytes => {
            if b < 1000.0 {
                format!("{:.0} B/s", b)
            } else if b < 1000.0 * 1024.0 {
                format!("{} KB/s", sig3(b / 1024.0))
            } else if b < 1000.0 * 1048576.0 {
                format!("{} MB/s", sig3(b / 1048576.0))
            } else {
                format!("{} GB/s", sig3(b / 1073741824.0))
            }
        }
        SpeedUnit::AutoBits => {
            let bits = b * 8.0;
            if bits < 1e3 {
                format!("{:.0} bps", bits)
            } else if bits < 1e6 {
                format!("{} Kbps", sig3(bits / 1e3))
            } else if bits < 1e9 {
                format!("{} Mbps", sig3(bits / 1e6))
            } else {
                format!("{} Gbps", sig3(bits / 1e9))
            }
        }
    }
}

/// Disk rates always in bytes.
pub fn rate(bytes_per_s: f64) -> String {
    speed(bytes_per_s, SpeedUnit::AutoBytes)
}

pub fn bytes(n: u64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", n)
    } else {
        format!("{} {}", sig3(v), U[i])
    }
}

pub fn duration(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    if d > 0 {
        format!("{}d {}h", d, h)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        assert_eq!(speed(0.0, SpeedUnit::AutoBytes), "0 B/s");
        assert_eq!(speed(1536.0, SpeedUnit::AutoBytes), "1.50 KB/s");
        assert_eq!(speed(125_000.0, SpeedUnit::AutoBits), "1.00 Mbps");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1023), "1.00 KB");
        assert_eq!(bytes(1024 * 1024 * 150), "150 MB");
        assert_eq!(bytes(1024 * 1024 * 1000), "0.98 GB");
        assert_eq!(duration(90061), "1d 1h");
    }
}
