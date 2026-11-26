//! Network throughput from the IP Helper interface table (64-bit octet counters).

use super::{AdapterInfo, Snapshot};
use std::collections::HashMap;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, MIB_IF_ROW2,
    MIB_IF_TABLE2,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

const FLAG_HARDWARE: u8 = 1 << 0;
const FLAG_FILTER: u8 = 1 << 1;

/// RAII wrapper around the system-allocated `MIB_IF_TABLE2`.
struct IfTable(*mut MIB_IF_TABLE2);

impl IfTable {
    fn open() -> Option<IfTable> {
        let mut t: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: valid out pointer; on success the table must be released with FreeMibTable.
        let r = unsafe { GetIfTable2(&mut t) };
        (r == 0 && !t.is_null()).then_some(IfTable(t))
    }

    fn rows(&self) -> &[MIB_IF_ROW2] {
        // SAFETY: the table holds `NumEntries` contiguous rows starting at `Table`.
        unsafe {
            let first = std::ptr::addr_of!((*self.0).Table).cast::<MIB_IF_ROW2>();
            std::slice::from_raw_parts(first, (*self.0).NumEntries as usize)
        }
    }
}

impl Drop for IfTable {
    fn drop(&mut self) {
        // SAFETY: pointer came from GetIfTable2 and is freed exactly once.
        unsafe { FreeMibTable(self.0.cast()) };
    }
}

fn flags(r: &MIB_IF_ROW2) -> u8 {
    r.InterfaceAndOperStatusFlags._bitfield
}

fn is_up(r: &MIB_IF_ROW2) -> bool {
    r.OperStatus == IfOperStatusUp
}

/// Alias without the trailing NUL padding.
fn alias(r: &MIB_IF_ROW2) -> &[u16] {
    let len = r.Alias.iter().position(|&c| c == 0).unwrap_or(r.Alias.len());
    &r.Alias[..len]
}

fn description(r: &MIB_IF_ROW2) -> String {
    let len = r.Description.iter().position(|&c| c == 0).unwrap_or(r.Description.len());
    String::from_utf16_lossy(&r.Description[..len])
}

fn eq_ignore_case(a: &[u16], want: &str) -> bool {
    char::decode_utf16(a.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .flat_map(char::to_lowercase)
        .eq(want.chars().flat_map(char::to_lowercase))
}

/// Physical (hardware, non-filter) interface that is up and not loopback/tunnel.
fn is_auto_candidate(r: &MIB_IF_ROW2) -> bool {
    let f = flags(r);
    f & FLAG_HARDWARE != 0
        && f & FLAG_FILTER == 0
        && is_up(r)
        && r.Type != IF_TYPE_SOFTWARE_LOOPBACK
        && r.Type != IF_TYPE_TUNNEL
}

struct Prev {
    rx: u64,
    tx: u64,
    generation: u32,
}

#[derive(Default)]
pub struct Net {
    prev: HashMap<u64, Prev>,
    generation: u32,
}

impl Net {
    /// Fills `net_up/down` and `net_sent/recv_delta` of `snap`.
    pub fn sample(&mut self, adapter: Option<&str>, elapsed: f64, snap: &mut Snapshot) {
        snap.net_up = 0.0;
        snap.net_down = 0.0;
        snap.net_sent_delta = 0;
        snap.net_recv_delta = 0;

        let Some(table) = IfTable::open() else {
            self.prev.clear();
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let wanted = adapter.filter(|a| !a.is_empty());

        for row in table.rows() {
            let selected = match wanted {
                None => is_auto_candidate(row),
                Some(name) => flags(row) & FLAG_FILTER == 0 && eq_ignore_case(alias(row), name),
            };
            if !selected {
                continue;
            }
            // SAFETY: the LUID union is always valid as its 64-bit value.
            let luid = unsafe { row.InterfaceLuid.Value };
            match self.prev.get_mut(&luid) {
                Some(p) => {
                    // A counter that went backwards was reset: count 0 for this sample.
                    snap.net_recv_delta += row.InOctets.saturating_sub(p.rx);
                    snap.net_sent_delta += row.OutOctets.saturating_sub(p.tx);
                    p.rx = row.InOctets;
                    p.tx = row.OutOctets;
                    p.generation = generation;
                }
                None => {
                    self.prev.insert(
                        luid,
                        Prev { rx: row.InOctets, tx: row.OutOctets, generation },
                    );
                }
            }
        }
        self.prev.retain(|_, p| p.generation == generation);

        if elapsed > 0.0 {
            snap.net_up = snap.net_sent_delta as f64 / elapsed;
            snap.net_down = snap.net_recv_delta as f64 / elapsed;
        }
    }
}

/// Physical interfaces for the tray menu: connected first, then by alias.
pub fn adapters() -> Vec<AdapterInfo> {
    let Some(table) = IfTable::open() else {
        return Vec::new();
    };
    let mut out: Vec<AdapterInfo> = Vec::new();
    for row in table.rows() {
        let f = flags(row);
        if f & FLAG_HARDWARE == 0
            || f & FLAG_FILTER != 0
            || row.Type == IF_TYPE_SOFTWARE_LOOPBACK
            || row.Type == IF_TYPE_TUNNEL
        {
            continue;
        }
        let name = String::from_utf16_lossy(alias(row));
        if name.is_empty() {
            continue;
        }
        let connected = is_up(row);
        match out.iter_mut().find(|a| a.alias == name) {
            Some(existing) => existing.connected |= connected,
            None => out.push(AdapterInfo { alias: name, description: description(row), connected }),
        }
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.alias.cmp(&b.alias)));
    out
}

// ------------------------------------------------------------------ Monitor page detail

/// Per-adapter rates, link speed and IPv4 address (Monitor page only).
#[derive(Default)]
pub struct NetDetail {
    prev: HashMap<u64, (u64, u64)>,
    /// LUID -> first IPv4 address, refreshed with the volumes.
    ips: HashMap<u64, String>,
}

const IF_TYPE_IEEE80211: u32 = 71;

impl NetDetail {
    pub fn reset(&mut self) {
        self.prev.clear();
        self.ips.clear();
    }

    pub fn sample(&mut self, elapsed: f64, refresh_ips: bool, out: &mut Vec<super::detail::NetStat>) {
        out.clear();
        if refresh_ips || self.ips.is_empty() {
            self.ips = ipv4_by_luid();
        }
        let Some(table) = IfTable::open() else {
            self.prev.clear();
            return;
        };
        let mut seen: Vec<u64> = Vec::new();
        for row in table.rows() {
            let f = flags(row);
            if f & FLAG_HARDWARE == 0 || f & FLAG_FILTER != 0 || row.Type == IF_TYPE_SOFTWARE_LOOPBACK || row.Type == IF_TYPE_TUNNEL {
                continue;
            }
            let alias = String::from_utf16_lossy(alias(row));
            if alias.is_empty() || out.iter().any(|a: &super::detail::NetStat| a.alias == alias) {
                continue;
            }
            // SAFETY: the LUID union is always valid as its 64-bit value.
            let luid = unsafe { row.InterfaceLuid.Value };
            seen.push(luid);
            let (mut down, mut up) = (0.0, 0.0);
            if let Some(&(rx, tx)) = self.prev.get(&luid) {
                if elapsed > 0.0 {
                    down = row.InOctets.saturating_sub(rx) as f64 / elapsed;
                    up = row.OutOctets.saturating_sub(tx) as f64 / elapsed;
                }
            }
            self.prev.insert(luid, (row.InOctets, row.OutOctets));
            let connected = is_up(row);
            out.push(super::detail::NetStat {
                alias,
                description: description(row),
                connected,
                wifi: row.Type == IF_TYPE_IEEE80211,
                down,
                up,
                link_bps: if connected { row.ReceiveLinkSpeed.min(u64::MAX / 2) } else { 0 },
                ipv4: self.ips.get(&luid).cloned().unwrap_or_default(),
            });
        }
        self.prev.retain(|k, _| seen.contains(k));
        out.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.alias.cmp(&b.alias)));
    }
}

/// First IPv4 unicast address of every adapter, keyed by LUID.
fn ipv4_by_luid() -> HashMap<u64, String> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    let mut map = HashMap::new();
    let mut len = 16 * 1024u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..3 {
        buf.resize((len as usize).div_ceil(8), 0);
        // SAFETY: buffer of `len` bytes, 8-byte aligned.
        let r = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_INET),
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                std::ptr::null(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if r == 111 {
            // ERROR_BUFFER_OVERFLOW: `len` now holds the size needed.
            continue;
        }
        if r != 0 {
            return map;
        }
        let mut p = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !p.is_null() {
            // SAFETY: linked list inside `buf`, written by GetAdaptersAddresses.
            let a = unsafe { &*p };
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                // SAFETY: as above.
                let ua = unsafe { &*u };
                let sa = ua.Address.lpSockaddr;
                // SAFETY: a valid sockaddr of the stated family.
                if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
                    let sin = unsafe { &*sa.cast::<SOCKADDR_IN>() };
                    let b = unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes();
                    // SAFETY: the LUID union is always valid as its 64-bit value.
                    map.insert(unsafe { a.Luid.Value }, format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]));
                    break;
                }
                u = ua.Next;
            }
            p = a.Next;
        }
        return map;
    }
    map
}
