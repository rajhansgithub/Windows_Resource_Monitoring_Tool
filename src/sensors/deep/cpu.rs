//! CPU facts for the CPU detail page: core map (cores, hybrid P/E classes, L3 groups, AMD
//! V-Cache), cache sizes, parked cores and the Windows power mode. No admin.
//!
//! `topology()` reads `GetLogicalProcessorInformationEx(RelationAll)` once (static facts, call
//! it on a worker thread). `CpuSets::parked` and `power_mode` are cheap live reads. The record
//! buffer is parsed by hand from little-endian bytes at the documented x64 offsets, so the
//! walker is alignment-safe, never reads past the buffer and is unit-testable without Windows.
//! Per-core clock and `% Performance Limit` come from PDH elsewhere.
#![allow(dead_code)]

use crate::util::from_wide;
use std::sync::OnceLock;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Power::{PowerGetActiveScheme, PowerReadFriendlyName};
use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetSystemCpuSetInformation, RelationAll, CACHE_RELATIONSHIP,
    PROCESSOR_RELATIONSHIP, SYSTEM_CPU_SET_INFORMATION, SYSTEM_CPU_SET_INFORMATION_0_0,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsProcessorFeaturePresent, PF_VIRT_FIRMWARE_ENABLED};

// Relationship values (LOGICAL_PROCESSOR_RELATIONSHIP).
const REL_CORE: u32 = 0;
const REL_CACHE: u32 = 2;
const REL_PACKAGE: u32 = 3;

// Record offsets (x64), checked against the windows-sys layouts at compile time below.
/// Header: Relationship u32 @0, Size u32 @4, union @8.
const HDR: usize = 8;
const PROC_EFF: usize = HDR + 1;
const PROC_GROUP_COUNT: usize = HDR + 22;
const PROC_MASKS: usize = HDR + 24;
const CACHE_LEVEL: usize = HDR;
const CACHE_SIZE: usize = HDR + 4;
const CACHE_TYPE: usize = HDR + 8;
const CACHE_GROUP_COUNT: usize = HDR + 30;
const CACHE_MASKS: usize = HDR + 32;
/// GROUP_AFFINITY: Mask u64 @0, Group u16 @8, 3 reserved u16.
const GROUP_AFFINITY_LEN: usize = 16;
// SYSTEM_CPU_SET_INFORMATION: Size @0, Type @4, CpuSet @8.
const CS_GROUP: usize = 12;
const CS_LP_INDEX: usize = 14;
const CS_FLAGS: usize = 19;
const CS_PARKED: u8 = 1;

const _: () = {
    use std::mem::offset_of as off;
    assert!(off!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous) == HDR);
    assert!(off!(PROCESSOR_RELATIONSHIP, EfficiencyClass) == PROC_EFF - HDR);
    assert!(off!(PROCESSOR_RELATIONSHIP, GroupCount) == PROC_GROUP_COUNT - HDR);
    assert!(off!(PROCESSOR_RELATIONSHIP, GroupMask) == PROC_MASKS - HDR);
    assert!(off!(CACHE_RELATIONSHIP, Level) == CACHE_LEVEL - HDR);
    assert!(off!(CACHE_RELATIONSHIP, CacheSize) == CACHE_SIZE - HDR);
    assert!(off!(CACHE_RELATIONSHIP, Type) == CACHE_TYPE - HDR);
    assert!(off!(CACHE_RELATIONSHIP, GroupCount) == CACHE_GROUP_COUNT - HDR);
    assert!(off!(CACHE_RELATIONSHIP, Anonymous) == CACHE_MASKS - HDR);
    assert!(off!(SYSTEM_CPU_SET_INFORMATION, Anonymous) == 8);
    assert!(off!(SYSTEM_CPU_SET_INFORMATION_0_0, Group) + 8 == CS_GROUP);
    assert!(off!(SYSTEM_CPU_SET_INFORMATION_0_0, LogicalProcessorIndex) + 8 == CS_LP_INDEX);
    assert!(off!(SYSTEM_CPU_SET_INFORMATION_0_0, Anonymous1) + 8 == CS_FLAGS);
};

/// One physical core.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreInfo {
    /// Logical processor indices of this core's threads (`group * 64 + bit`).
    pub logical: Vec<u32>,
    /// Windows EfficiencyClass: higher is faster; all equal on a non-hybrid CPU.
    pub efficiency_class: u8,
    /// Index into `CpuTopo::l3` of the L3 this core uses; `u32::MAX` when no L3 was reported.
    pub l3_group: u32,
}

/// One cache level summary (all identical caches folded into one row).
#[derive(Clone, Debug, PartialEq)]
pub struct CacheInfo {
    /// 1, 2 or 3.
    pub level: u8,
    /// Size of one cache instance, bytes.
    pub size: u64,
    /// "Data", "Instruction", "Unified" or "Trace".
    pub kind: &'static str,
    /// Logical processors sharing one instance.
    pub shared_by: u32,
    /// How many such instances the machine has (total = `size * instances`).
    pub instances: u32,
}

/// Everything static about the CPU layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuTopo {
    /// Physical cores in OS order.
    pub cores: Vec<CoreInfo>,
    /// L1d, L1i, L2, L3 summaries, sorted by level then kind.
    pub caches: Vec<CacheInfo>,
    /// Every L3 instance: (bytes, logical processors sharing it), ordered by first processor.
    pub l3: Vec<(u64, Vec<u32>)>,
    /// Physical packages.
    pub sockets: u32,
    /// More than one efficiency class (Intel P+E, ARM big.LITTLE).
    pub hybrid: bool,
    /// Index into `l3` of the clearly larger L3 when groups differ (AMD X3D V-Cache CCD).
    pub vcache_group: Option<usize>,
    /// Firmware virtualization (VT-x / AMD-V) enabled; None when unknown.
    pub virtualization: Option<bool>,
}

/// Performance class of a core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Highest efficiency class on a hybrid CPU.
    Performance,
    /// Any lower class on a hybrid CPU.
    Efficiency,
    /// Non-hybrid CPU: every core alike.
    Uniform,
}

/// Active power mode ("Windows power mode" slider) and power plan.
#[derive(Clone, Debug, PartialEq)]
pub struct PowerMode {
    /// "Best power efficiency", "Better performance", "Balanced", "Best performance" or
    /// "Custom"; empty when this Windows has no overlay API (before 1709).
    pub overlay: &'static str,
    /// Friendly name of the active power plan ("Balanced", "High performance", ...).
    pub plan: String,
}

// ---------------------------------------------------------------------------------------
// Pure parsing
// ---------------------------------------------------------------------------------------

fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off.checked_add(2)?)?.try_into().ok()?))
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off.checked_add(4)?)?.try_into().ok()?))
}

fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off.checked_add(8)?)?.try_into().ok()?))
}

/// Splits a variable-length buffer into `(relationship, record bytes)`. Stops at the first
/// record whose size is implausible or runs past the buffer.
fn records(buf: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut off = 0usize;
    std::iter::from_fn(move || {
        let rel = rd_u32(buf, off)?;
        let size = rd_u32(buf, off + 4)? as usize;
        if size < HDR {
            return None;
        }
        let rec = buf.get(off..off.checked_add(size)?)?;
        off += size;
        Some((rel, rec))
    })
}

/// Logical processor indices set in `count` GROUP_AFFINITY entries starting at `first`.
fn group_masks(rec: &[u8], first: usize, count: usize) -> Vec<u32> {
    let mut out = Vec::new();
    for i in 0..count {
        let at = first + i * GROUP_AFFINITY_LEN;
        let (Some(mask), Some(group)) = (rd_u64(rec, at), rd_u16(rec, at + 8)) else { break };
        let base = group as u32 * 64;
        out.extend((0..64u32).filter(|bit| mask >> bit & 1 != 0).map(|bit| base + bit));
    }
    out
}

fn cache_kind(t: u32) -> &'static str {
    match t {
        0 => "Unified",
        1 => "Instruction",
        2 => "Data",
        _ => "Trace",
    }
}

/// Sort rank so a level lists Data, Instruction, Unified.
fn kind_rank(k: &str) -> u8 {
    match k {
        "Data" => 0,
        "Instruction" => 1,
        "Unified" => 2,
        _ => 3,
    }
}

/// The L3 group that is clearly larger than the rest (>= 25 % bigger than the smallest, and
/// the only one of that size), as on AMD X3D parts where one CCD stacks extra cache.
fn vcache_index(l3: &[(u64, Vec<u32>)]) -> Option<usize> {
    if l3.len() < 2 {
        return None;
    }
    let max = l3.iter().map(|g| g.0).max()?;
    let min = l3.iter().map(|g| g.0).min()?;
    if min == 0 || max * 4 < min * 5 || l3.iter().filter(|g| g.0 == max).count() != 1 {
        return None;
    }
    l3.iter().position(|g| g.0 == max)
}

/// Builds a `CpuTopo` from a `GetLogicalProcessorInformationEx(RelationAll)` buffer.
/// `virtualization` is left `None`.
fn parse_topology(buf: &[u8]) -> CpuTopo {
    let mut cores: Vec<CoreInfo> = Vec::new();
    let mut raw: Vec<(u8, &'static str, u64, Vec<u32>)> = Vec::new();
    let mut sockets = 0u32;
    for (rel, rec) in records(buf) {
        match rel {
            REL_CORE => {
                let n = rd_u16(rec, PROC_GROUP_COUNT).unwrap_or(0) as usize;
                let logical = group_masks(rec, PROC_MASKS, n);
                let class = rec.get(PROC_EFF).copied().unwrap_or(0);
                cores.push(CoreInfo { logical, efficiency_class: class, l3_group: u32::MAX });
            }
            REL_CACHE => {
                let level = rec.get(CACHE_LEVEL).copied().unwrap_or(0);
                let size = rd_u32(rec, CACHE_SIZE).unwrap_or(0) as u64;
                let kind = cache_kind(rd_u32(rec, CACHE_TYPE).unwrap_or(0));
                // Older Windows leaves GroupCount 0 with a single GroupMask.
                let n = (rd_u16(rec, CACHE_GROUP_COUNT).unwrap_or(1) as usize).max(1);
                raw.push((level, kind, size, group_masks(rec, CACHE_MASKS, n)));
            }
            REL_PACKAGE => sockets += 1,
            _ => {}
        }
    }

    let mut l3: Vec<(u64, Vec<u32>)> =
        raw.iter().filter(|c| c.0 == 3 && c.2 > 0).map(|c| (c.2, c.3.clone())).collect();
    l3.sort_by_key(|g| g.1.first().copied().unwrap_or(u32::MAX));

    for c in &mut cores {
        let first = c.logical.first().copied();
        if let Some(i) = l3.iter().position(|g| first.is_some_and(|f| g.1.contains(&f))) {
            c.l3_group = i as u32;
        }
    }

    let mut caches: Vec<CacheInfo> = Vec::new();
    for (level, kind, size, logical) in &raw {
        if *level == 0 || *size == 0 {
            continue;
        }
        match caches.iter_mut().find(|c| c.level == *level && c.kind == *kind && c.size == *size) {
            Some(c) => c.instances += 1,
            None => caches.push(CacheInfo {
                level: *level,
                size: *size,
                kind,
                shared_by: logical.len() as u32,
                instances: 1,
            }),
        }
    }
    caches.sort_by_key(|c| (c.level, kind_rank(c.kind), c.size));

    let hybrid = cores.iter().any(|c| c.efficiency_class != cores[0].efficiency_class);
    CpuTopo {
        sockets: if sockets == 0 && !cores.is_empty() { 1 } else { sockets },
        hybrid,
        vcache_group: vcache_index(&l3),
        cores,
        caches,
        l3,
        virtualization: None,
    }
}

/// Performance or efficiency class of the core owning `logical`. `Uniform` on a non-hybrid
/// CPU or for an unknown index.
pub fn core_kind(t: &CpuTopo, logical: u32) -> Kind {
    if !t.hybrid {
        return Kind::Uniform;
    }
    let Some(core) = t.cores.iter().find(|c| c.logical.contains(&logical)) else {
        return Kind::Uniform;
    };
    let top = t.cores.iter().map(|c| c.efficiency_class).max().unwrap_or(0);
    if core.efficiency_class == top {
        Kind::Performance
    } else {
        Kind::Efficiency
    }
}

/// Parked logical processors (`group * 64 + index`) from a `GetSystemCpuSetInformation` buffer.
fn parse_parked(buf: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    for (ty, rec) in records_cpuset(buf) {
        if ty != 0 {
            continue; // CpuSetInformation is the only defined type
        }
        let (Some(group), Some(&idx), Some(&flags)) =
            (rd_u16(rec, CS_GROUP), rec.get(CS_LP_INDEX), rec.get(CS_FLAGS))
        else {
            continue;
        };
        if flags & CS_PARKED != 0 {
            out.push(group as u32 * 64 + idx as u32);
        }
    }
    out
}

/// Like `records`, but the CPU set header is `Size u32 @0, Type u32 @4` (same shape, so reuse).
fn records_cpuset(buf: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    let mut off = 0usize;
    std::iter::from_fn(move || {
        let size = rd_u32(buf, off)? as usize;
        let ty = rd_u32(buf, off + 4)?;
        if size < HDR {
            return None;
        }
        let rec = buf.get(off..off.checked_add(size)?)?;
        off += size;
        Some((ty, rec))
    })
}

/// Friendly name for a power-mode overlay GUID.
fn overlay_name(g: &GUID) -> &'static str {
    const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> GUID {
        GUID { data1: d1, data2: d2, data3: d3, data4: d4 }
    }
    let efficiency = guid(0x961c_c777, 0x2547, 0x4f9d, [0x81, 0x74, 0x7d, 0x86, 0x18, 0x1b, 0x8a, 0x7a]);
    let better = guid(0x3af9_b8d9, 0x7c97, 0x431d, [0xad, 0x78, 0x34, 0xa8, 0xbf, 0xea, 0x43, 0x9f]);
    let best = guid(0xded5_74b5, 0x45a0, 0x4f42, [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38]);
    let same = |a: &GUID, b: &GUID| (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4);
    if same(g, &efficiency) {
        "Best power efficiency"
    } else if same(g, &better) {
        "Better performance"
    } else if same(g, &best) {
        "Best performance"
    } else if same(g, &guid(0, 0, 0, [0; 8])) {
        "Balanced"
    } else {
        "Custom"
    }
}

// ---------------------------------------------------------------------------------------
// Win32 calls
// ---------------------------------------------------------------------------------------

/// 8-byte aligned scratch buffer viewed as bytes.
fn aligned(len: usize) -> Vec<u64> {
    vec![0u64; len.div_ceil(8)]
}

fn bytes(v: &[u64], len: usize) -> &[u8] {
    let all = v.len() * 8;
    // SAFETY: u64 -> u8 view of an initialised allocation, length clamped to it.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), len.min(all)) }
}

/// Reads the CPU layout once. Empty topology (no cores) when the call fails.
pub fn topology() -> CpuTopo {
    let mut len = 0u32;
    // SAFETY: a null buffer with length 0 only asks for the required size.
    unsafe { GetLogicalProcessorInformationEx(RelationAll, std::ptr::null_mut(), &mut len) };
    let mut topo = CpuTopo::default();
    for _ in 0..3 {
        if len == 0 {
            break;
        }
        let mut buf = aligned(len as usize);
        let cap = (buf.len() * 8) as u32;
        let mut got = cap;
        // SAFETY: buf is 8-byte aligned and `got` bytes are writable.
        let ok = unsafe { GetLogicalProcessorInformationEx(RelationAll, buf.as_mut_ptr().cast(), &mut got) };
        if ok != 0 {
            topo = parse_topology(bytes(&buf, got as usize));
            break;
        }
        // SAFETY: last error of the call above.
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            break;
        }
        len = got; // grew between calls: retry with the new size
    }
    // SAFETY: plain feature query.
    topo.virtualization = Some(unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) } != 0);
    topo
}

/// Reusable buffer for the live parked-core query.
#[derive(Default)]
pub struct CpuSets {
    buf: Vec<u64>,
}

impl CpuSets {
    /// Logical processors (`group * 64 + index`) the OS has parked right now. Empty on failure
    /// or when nothing is parked. One syscall, no allocation once the buffer has grown.
    pub fn parked(&mut self) -> Vec<u32> {
        // SAFETY: pseudo handle, never closed.
        let process = unsafe { GetCurrentProcess() };
        for _ in 0..3 {
            let cap = (self.buf.len() * 8) as u32;
            let mut got = 0u32;
            // SAFETY: buf holds `cap` writable bytes (or is empty with cap 0 to query the size).
            let ok = unsafe {
                GetSystemCpuSetInformation(self.buf.as_mut_ptr().cast(), cap, &mut got, process, 0)
            };
            if ok != 0 {
                return parse_parked(bytes(&self.buf, got as usize));
            }
            // SAFETY: last error of the call above.
            if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || got == 0 {
                return Vec::new();
            }
            self.buf = aligned(got as usize);
        }
        Vec::new()
    }
}

/// Parked logical processors right now (allocates a scratch buffer; prefer `CpuSets::parked`
/// for the once-a-second poll).
pub fn parked() -> Vec<u32> {
    CpuSets::default().parked()
}

type OverlayFn = unsafe extern "system" fn(*mut GUID) -> u32;

/// `PowerGetEffectiveOverlayScheme` is not in windows-sys; resolve it once.
fn overlay_fn() -> Option<OverlayFn> {
    static F: OnceLock<Option<usize>> = OnceLock::new();
    let addr = (*F.get_or_init(|| {
        let name: Vec<u16> = "powrprof.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: NUL-terminated name; powrprof is already loaded by the Power* imports.
        let m: HMODULE = unsafe { LoadLibraryW(name.as_ptr()) };
        if m.is_null() {
            return None;
        }
        // SAFETY: valid module and NUL-terminated ASCII name.
        unsafe { GetProcAddress(m, c"PowerGetEffectiveOverlayScheme".as_ptr().cast()) }.map(|p| p as usize)
    }))?;
    // SAFETY: the export has the signature DWORD (GUID *).
    Some(unsafe { std::mem::transmute::<usize, OverlayFn>(addr) })
}

/// Friendly name of the active power plan.
fn active_plan() -> Option<String> {
    let mut guid: *mut GUID = std::ptr::null_mut();
    // SAFETY: on success `guid` is a LocalAlloc'd GUID that we free below.
    if unsafe { PowerGetActiveScheme(std::ptr::null_mut(), &mut guid) } != 0 || guid.is_null() {
        return None;
    }
    let mut buf = [0u16; 256];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `guid` is valid; buf has `size` writable bytes.
    let rc = unsafe {
        PowerReadFriendlyName(
            std::ptr::null_mut(),
            guid,
            std::ptr::null(),
            std::ptr::null(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    // SAFETY: allocated by PowerGetActiveScheme, freed once.
    unsafe { LocalFree(guid.cast()) };
    if rc != 0 {
        return None;
    }
    let name = from_wide(&buf);
    (!name.is_empty()).then_some(name)
}

/// Current power-mode slider position and power plan. None when neither can be read.
pub fn power_mode() -> Option<PowerMode> {
    let overlay = overlay_fn().and_then(|f| {
        let mut g = GUID { data1: 0, data2: 0, data3: 0, data4: [0; 8] };
        // SAFETY: f has the documented signature; g is a valid out pointer.
        (unsafe { f(&mut g) } == 0).then(|| overlay_name(&g))
    });
    let plan = active_plan();
    if overlay.is_none() && plan.is_none() {
        return None;
    }
    Some(PowerMode { overlay: overlay.unwrap_or(""), plan: plan.unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn le32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn le64(v: &mut Vec<u8>, x: u64) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn affinity(v: &mut Vec<u8>, mask: u64, group: u16) {
        le64(v, mask);
        le16(v, group);
        v.extend_from_slice(&[0; 6]);
    }
    /// Finishes a record: patches Size into bytes 4..8, pads to 8.
    fn finish(mut rec: Vec<u8>) -> Vec<u8> {
        while rec.len() & 7 != 0 {
            rec.push(0);
        }
        let n = rec.len() as u32;
        rec[4..8].copy_from_slice(&n.to_le_bytes());
        rec
    }
    fn core(class: u8, mask: u64, group: u16) -> Vec<u8> {
        let mut r = Vec::new();
        le32(&mut r, REL_CORE);
        le32(&mut r, 0);
        r.push(1); // Flags: SMT
        r.push(class);
        r.extend_from_slice(&[0; 20]);
        le16(&mut r, 1);
        affinity(&mut r, mask, group);
        finish(r)
    }
    fn cache(level: u8, kind: u32, size: u32, mask: u64, group: u16) -> Vec<u8> {
        let mut r = Vec::new();
        le32(&mut r, REL_CACHE);
        le32(&mut r, 0);
        r.push(level);
        r.push(8); // associativity
        le16(&mut r, 64);
        le32(&mut r, size);
        le32(&mut r, kind);
        r.extend_from_slice(&[0; 18]);
        le16(&mut r, 1);
        affinity(&mut r, mask, group);
        finish(r)
    }
    fn package(mask: u64) -> Vec<u8> {
        let mut r = Vec::new();
        le32(&mut r, REL_PACKAGE);
        le32(&mut r, 0);
        r.push(0);
        r.push(0);
        r.extend_from_slice(&[0; 20]);
        le16(&mut r, 1);
        affinity(&mut r, mask, 0);
        finish(r)
    }
    const MB: u32 = 1024 * 1024;

    #[test]
    fn cores_and_caches() {
        let mut b = Vec::new();
        b.extend(package(0xF));
        b.extend(core(0, 0b0011, 0));
        b.extend(core(0, 0b1100, 0));
        b.extend(cache(1, 2, 32 * 1024, 0b0011, 0));
        b.extend(cache(1, 2, 32 * 1024, 0b1100, 0));
        b.extend(cache(1, 1, 32 * 1024, 0b0011, 0));
        b.extend(cache(2, 0, 512 * 1024, 0b0011, 0));
        b.extend(cache(3, 0, 16 * MB, 0xF, 0));
        let t = parse_topology(&b);
        assert_eq!(t.sockets, 1);
        assert!(!t.hybrid);
        assert_eq!(t.cores.len(), 2);
        assert_eq!(t.cores[0].logical, vec![0, 1]);
        assert_eq!(t.cores[1].logical, vec![2, 3]);
        assert_eq!(t.cores[1].l3_group, 0);
        assert_eq!(t.l3, vec![(16 * MB as u64, vec![0, 1, 2, 3])]);
        let l1d = &t.caches[0];
        assert_eq!((l1d.level, l1d.kind, l1d.size, l1d.shared_by, l1d.instances), (1, "Data", 32768, 2, 2));
        assert_eq!(t.caches[1].kind, "Instruction");
        assert_eq!(t.caches[2].kind, "Unified");
        assert_eq!(t.caches.last().map(|c| (c.level, c.size)), Some((3, 16 * MB as u64)));
        assert_eq!(t.vcache_group, None);
        assert_eq!(core_kind(&t, 0), Kind::Uniform);
    }

    #[test]
    fn hybrid_kinds() {
        let mut b = Vec::new();
        b.extend(core(1, 0b11, 0)); // P core, 2 threads
        b.extend(core(0, 0b100, 0)); // E core
        let t = parse_topology(&b);
        assert!(t.hybrid);
        assert_eq!(core_kind(&t, 1), Kind::Performance);
        assert_eq!(core_kind(&t, 2), Kind::Efficiency);
        assert_eq!(core_kind(&t, 40), Kind::Uniform);
        assert_eq!(t.sockets, 1);
    }

    #[test]
    fn second_processor_group() {
        let t = parse_topology(&core(0, 0b101, 1));
        assert_eq!(t.cores[0].logical, vec![64, 66]);
    }

    #[test]
    fn vcache_detection() {
        let mut b = Vec::new();
        b.extend(cache(3, 0, 32 * MB, 0x0000_FFFF, 0));
        b.extend(cache(3, 0, 96 * MB, 0xFFFF_0000, 0));
        b.extend(core(0, 0x1, 0));
        b.extend(core(0, 0x1_0000, 0));
        let t = parse_topology(&b);
        assert_eq!(t.l3.len(), 2);
        assert_eq!(t.vcache_group, Some(1));
        assert_eq!(t.cores[0].l3_group, 0);
        assert_eq!(t.cores[1].l3_group, 1);
        // Equal groups, or two tied big groups: no V-Cache call.
        let eq = vec![(32u64, vec![0]), (32, vec![1])];
        assert_eq!(vcache_index(&eq), None);
        let tie = vec![(96u64, vec![0]), (96, vec![1]), (32, vec![2])];
        assert_eq!(vcache_index(&tie), None);
    }

    #[test]
    fn truncated_and_garbage_buffers_are_safe() {
        let good = core(0, 0b11, 0);
        for n in 0..good.len() {
            let _ = parse_topology(&good[..n]);
        }
        let mut bad = good.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_topology(&bad).cores.is_empty());
        let mut zero = good;
        zero[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_topology(&zero).cores.is_empty());
        assert_eq!(parse_topology(&[]), CpuTopo::default());
    }

    #[test]
    fn parked_cpu_sets() {
        let mut b = Vec::new();
        for (group, idx, flags) in [(0u16, 0u8, 0u8), (0, 1, 1), (1, 3, 3), (1, 4, 2)] {
            let mut r = Vec::new();
            le32(&mut r, 32);
            le32(&mut r, 0);
            le32(&mut r, 7); // Id
            le16(&mut r, group);
            r.push(idx);
            r.push(0); // CoreIndex
            r.push(0); // LastLevelCacheIndex
            r.push(0); // NumaNodeIndex
            r.push(0); // EfficiencyClass
            r.push(flags);
            r.resize(32, 0);
            b.extend(r);
        }
        assert_eq!(parse_parked(&b), vec![1, 64 + 3]);
        assert!(parse_parked(&b[..20]).is_empty());
    }

    #[test]
    fn overlay_guids() {
        let g = |d1, d2, d3, d4| GUID { data1: d1, data2: d2, data3: d3, data4: d4 };
        let z = g(0, 0, 0, [0; 8]);
        assert_eq!(overlay_name(&z), "Balanced");
        let eff = g(0x961cc777, 0x2547, 0x4f9d, [0x81, 0x74, 0x7d, 0x86, 0x18, 0x1b, 0x8a, 0x7a]);
        assert_eq!(overlay_name(&eff), "Best power efficiency");
        let best = g(0xded574b5, 0x45a0, 0x4f42, [0x87, 0x37, 0x46, 0x34, 0x5c, 0x09, 0xc2, 0x38]);
        assert_eq!(overlay_name(&best), "Best performance");
        assert_eq!(overlay_name(&g(1, 2, 3, [4; 8])), "Custom");
    }
}
