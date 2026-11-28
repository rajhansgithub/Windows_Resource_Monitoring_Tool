//! Paired Bluetooth devices (Classic + LE): name, kind, connected state, battery, last seen.
//!
//! Sources, all user-mode and admin-free:
//! * Device Query (`DevGetObjects`, association endpoints) filtered to paired devices: name,
//!   connected flag, protocol (Classic / LE), container id, Class of Device, LE appearance and
//!   the last-seen time. A Classic and an LE endpoint of one dual-mode device share a
//!   container id and are merged into one [`BtDevice`].
//! * Battery: the `{104EA319-...} 2` device property Windows Settings shows. Classic
//!   headsets keep it on the "Hands-Free AG" child node (`BTHENUM`), BLE devices on the
//!   `BTHLE\Dev_...` node. Found through the configuration manager and joined by container.
//!
//! `read()` is a one-shot read (a few ms, no polling inside). It still waits on Device
//! Query, so it never runs on the sensor beat: [`BtReader`] runs it on a thread of its own
//! every ~30 s. It never panics: every failure just leaves a field empty.
#![allow(dead_code)]

use crate::util::{from_wide, wide};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Condvar, Mutex};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW,
    CM_Locate_DevNodeW, CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT,
    CM_LOCATE_DEVNODE_NORMAL, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::DeviceQuery::{
    DevFreeObjects, DevGetObjects, DevObjectTypeAEP, DevQueryFlagNone, DEVPROP_FILTER_EXPRESSION,
    DEVPROP_OPERATOR_EQUALS, DEVPROP_OPERATOR_OR_CLOSE, DEVPROP_OPERATOR_OR_OPEN, DEV_OBJECT,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPROPCOMPKEY, DEVPROPERTY, DEVPROPKEY, DEVPROP_STORE_SYSTEM,
    DEVPROP_TRUE, DEVPROP_TYPE_BOOLEAN, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_FILETIME,
    DEVPROP_TYPE_GUID, DEVPROP_TYPE_INT16, DEVPROP_TYPE_INT32, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_UINT16, DEVPROP_TYPE_UINT32,
};

/// What a device is, for choosing its icon. Declaration order is the list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    Earbuds,
    Headphones,
    Headset,
    Speaker,
    Mouse,
    Keyboard,
    Gamepad,
    Pen,
    Phone,
    Watch,
    Computer,
    #[default]
    Other,
}

/// One paired Bluetooth device (one container; Classic and LE endpoints merged).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BtDevice {
    /// Container id as a lowercase "{guid}" string: stable key across reads.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub connected: bool,
    /// Battery %, 0..=100, as Windows stores it (stale but kept while disconnected).
    pub battery: Option<u8>,
    /// Per-bud and case levels and charging: filled later by an optional helper module; always None here.
    pub left: Option<u8>,
    pub right: Option<u8>,
    pub case: Option<u8>,
    pub charging: Option<bool>,
    /// Has a Bluetooth LE / Classic endpoint.
    pub le: bool,
    pub classic: bool,
    /// Last time Windows saw it, unix seconds.
    pub last_seen: Option<u64>,
}

// ---- property keys (from the Windows SDK headers: devpkey.h, devpropdef.h, BluetoothAEP) ----

const fn key(fmtid: u128, pid: u32) -> DEVPROPKEY {
    DEVPROPKEY { fmtid: GUID::from_u128(fmtid), pid }
}

/// PKEY_ItemNameDisplay (string).
const K_NAME: DEVPROPKEY = key(0xB725F130_47EF_101A_A5F1_02608C9EEBAC, 10);
/// System.Devices.Aep.IsPaired (bool).
const K_PAIRED: DEVPROPKEY = key(0xA35996AB_11CF_4935_8B61_A6761081ECDF, 16);
/// System.Devices.Aep.IsConnected (bool).
const K_CONNECTED: DEVPROPKEY = key(0xA35996AB_11CF_4935_8B61_A6761081ECDF, 7);
/// System.Devices.Aep.ProtocolId (GUID).
const K_PROTOCOL: DEVPROPKEY = key(0x3B2CE006_5E61_4FDE_BAB8_9B8AAC9B26DF, 5);
/// System.Devices.Aep.ContainerId (GUID).
const K_CONTAINER: DEVPROPKEY = key(0xE7C3FB29_CAA7_4F47_8C8B_BE59B330D4C5, 2);
/// System.Devices.Aep.Bluetooth.Cod.Major / .Minor (integers).
const K_COD_MAJOR: DEVPROPKEY = key(0x5FBD34CD_561A_412E_BA98_478A6B0FEF1D, 2);
const K_COD_MINOR: DEVPROPKEY = key(0x5FBD34CD_561A_412E_BA98_478A6B0FEF1D, 3);
/// System.Devices.Aep.Bluetooth.Le.Appearance (UINT16).
const K_APPEARANCE: DEVPROPKEY = key(0x995EF0B0_7EB3_4A8B_B9CE_068BB3F4AF69, 1);
/// System.Devices.Aep.Bluetooth.LastSeenTime (FILETIME).
const K_LAST_SEEN: DEVPROPKEY = key(0x2BD67D8B_8BEB_48D5_87E0_6CDA3428040A, 12);
/// Battery percent on a device node (BYTE).
const K_BATTERY: DEVPROPKEY = key(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5, 2);

/// Every property requested from the association endpoints.
const REQUESTED: [DEVPROPKEY; 9] = [
    K_NAME,
    K_PAIRED,
    K_CONNECTED,
    K_PROTOCOL,
    K_CONTAINER,
    K_COD_MAJOR,
    K_COD_MINOR,
    K_APPEARANCE,
    K_LAST_SEEN,
];

/// Protocol ids of the two Bluetooth endpoint kinds.
const PROTO_CLASSIC: u128 = 0xE0CBF06C_CD8B_4647_BB8A_263B43F0F974;
const PROTO_LE: u128 = 0xBB7BB05E_5972_42B5_94FC_76EAA7084D49;

/// FILETIME ticks (100 ns) per second, and the FILETIME of the unix epoch in seconds.
const TICKS_PER_SEC: u64 = 10_000_000;
const UNIX_EPOCH_SECS: u64 = 11_644_473_600;
/// Battery values above this are "unknown" markers, not percentages.
const MAX_PCT: u8 = 100;
/// Upper bound on a device id list buffer, in UTF-16 units (a sanity guard, not a real limit).
const MAX_ID_LIST: u32 = 1 << 20;

/// Every paired Bluetooth device, connected first, then by kind, then by name.
/// Empty when Windows has no Bluetooth stack or the query fails.
pub fn read() -> Vec<BtDevice> {
    let aeps = endpoints();
    if aeps.is_empty() {
        return Vec::new();
    }
    let bat = batteries();
    let mut out = merge(aeps, &bat);
    out.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then((a.kind as u8).cmp(&(b.kind as u8)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Example devices for CI screenshots and the frame-time benchmark (`RESMON_BT_DEMO`):
/// the runners have no Bluetooth radio. Covers every layout case: per-bud earbuds, a low
/// device, one with no level, disconnected ones.
pub fn demo() -> Vec<BtDevice> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let d = |name: &str, kind: Kind, connected: bool, battery: Option<u8>, ago_h: u64| BtDevice {
        id: format!("{{demo-{}}}", name.to_lowercase().replace(' ', "-")),
        name: name.into(),
        kind,
        connected,
        battery,
        le: true,
        last_seen: (!connected).then(|| now.saturating_sub(ago_h * 3600)),
        ..Default::default()
    };
    let mut buds = d("AirPods Pro", Kind::Earbuds, true, Some(80), 0);
    (buds.left, buds.right, buds.case, buds.charging) = (Some(90), Some(80), Some(60), Some(false));
    let mut pad = d("DualSense", Kind::Gamepad, true, Some(75), 0);
    pad.charging = Some(true);
    vec![
        buds,
        d("MX Master 3S", Kind::Mouse, true, Some(55), 0),
        d("Keychron K3", Kind::Keyboard, true, Some(12), 0),
        pad,
        d("Surface Pen", Kind::Pen, true, Some(40), 0),
        d("Pixel 9", Kind::Phone, true, None, 0),
        d("WF-1000XM5", Kind::Earbuds, false, Some(70), 3),
        d("Xbox Wireless Controller", Kind::Gamepad, false, Some(50), 48),
        d("WH-1000XM4", Kind::Headphones, false, Some(40), 144),
        d("JBL Flip 6", Kind::Speaker, false, None, 216),
    ]
}

// ---------------------------------------------------------------------------------------------
// Association endpoints
// ---------------------------------------------------------------------------------------------

/// One Bluetooth association endpoint as Device Query returned it.
#[derive(Default)]
struct Aep {
    name: String,
    paired: Option<bool>,
    connected: bool,
    protocol: u128,
    container: Option<u128>,
    major: Option<u32>,
    minor: Option<u32>,
    appearance: Option<u16>,
    last_seen: Option<u64>,
}

/// Paired Classic and LE endpoints.
///
/// The query always names the two Bluetooth protocol ids. Without them Device Query asks
/// every association-endpoint provider, network ones included (WSD, UPnP, DIAL, Wi-Fi
/// Direct, ...), and the synchronous call waits for their discovery: seconds on a
/// typical PC, which held the sensor thread in v2.22.0. Asks Windows to filter on
/// `IsPaired` too; if it rejects that, filters on protocol only and checks pairing here.
fn endpoints() -> Vec<Aep> {
    query_aeps(Filter::PairedBluetooth)
        .or_else(|| query_aeps(Filter::Bluetooth))
        .or_else(|| query_aeps(Filter::None))
        .unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    /// Bluetooth protocols and `IsPaired == true`.
    PairedBluetooth,
    /// Bluetooth protocols only.
    Bluetooth,
    /// Last resort: every endpoint (slow; only ever runs on the Bluetooth reader thread).
    None,
}

/// `DevGetObjects` over the association endpoints. `None` when the call failed.
fn query_aeps(filter: Filter) -> Option<Vec<Aep>> {
    let req: Vec<DEVPROPCOMPKEY> = REQUESTED
        .iter()
        .map(|k| DEVPROPCOMPKEY { Key: *k, Store: DEVPROP_STORE_SYSTEM, LocaleName: null() })
        .collect();
    let mut yes: u8 = DEVPROP_TRUE;
    let mut classic = GUID::from_u128(PROTO_CLASSIC);
    let mut le = GUID::from_u128(PROTO_LE);
    let prop = |op, key: DEVPROPKEY, ty, size: usize, buf: *mut c_void| DEVPROP_FILTER_EXPRESSION {
        Operator: op,
        Property: DEVPROPERTY {
            CompKey: DEVPROPCOMPKEY { Key: key, Store: DEVPROP_STORE_SYSTEM, LocaleName: null() },
            Type: ty,
            BufferSize: size as u32,
            Buffer: buf,
        },
    };
    let marker = |op| prop(op, K_PROTOCOL, 0, 0, null_mut());
    let gsize = std::mem::size_of::<GUID>();
    // Top-level terms are ANDed: (protocol == Classic OR protocol == LE) AND paired.
    let mut filters: Vec<DEVPROP_FILTER_EXPRESSION> = Vec::new();
    if filter != Filter::None {
        filters.push(marker(DEVPROP_OPERATOR_OR_OPEN));
        filters.push(prop(DEVPROP_OPERATOR_EQUALS, K_PROTOCOL, DEVPROP_TYPE_GUID, gsize, &mut classic as *mut GUID as *mut c_void));
        filters.push(prop(DEVPROP_OPERATOR_EQUALS, K_PROTOCOL, DEVPROP_TYPE_GUID, gsize, &mut le as *mut GUID as *mut c_void));
        filters.push(marker(DEVPROP_OPERATOR_OR_CLOSE));
    }
    if filter == Filter::PairedBluetooth {
        filters.push(prop(DEVPROP_OPERATOR_EQUALS, K_PAIRED, DEVPROP_TYPE_BOOLEAN, 1, &mut yes as *mut u8 as *mut c_void));
    }
    let filtered = filter == Filter::PairedBluetooth;
    let (nfilter, pfilter) = if filters.is_empty() { (0, null()) } else { (filters.len() as u32, filters.as_ptr()) };
    let mut count: u32 = 0;
    let mut objs: *mut DEV_OBJECT = null_mut();
    // SAFETY: `req`, `filters` and the values they point at (`yes`, `classic`, `le`) outlive the call; the out pointers are valid locals.
    let hr = unsafe {
        DevGetObjects(
            DevObjectTypeAEP,
            DevQueryFlagNone as u32,
            req.len() as u32,
            req.as_ptr(),
            nfilter,
            pfilter,
            &mut count,
            &mut objs,
        )
    };
    if hr < 0 {
        return None;
    }
    if objs.is_null() || count == 0 {
        return Some(Vec::new());
    }
    // SAFETY: on success `objs` points at `count` DEV_OBJECTs owned by Device Query until
    // DevFreeObjects below; we only read them in between.
    let list = unsafe { std::slice::from_raw_parts(objs as *const DEV_OBJECT, count as usize) };
    let mut out = Vec::new();
    for o in list {
        let props: &[DEVPROPERTY] = if o.pProperties.is_null() || o.cPropertyCount == 0 {
            &[]
        } else {
            // SAFETY: pProperties holds cPropertyCount entries, valid until DevFreeObjects.
            unsafe { std::slice::from_raw_parts(o.pProperties, o.cPropertyCount as usize) }
        };
        let a = parse_aep(props);
        let bluetooth = a.protocol == PROTO_CLASSIC || a.protocol == PROTO_LE;
        let paired = match a.paired {
            Some(p) => p,
            None => filtered,
        };
        if bluetooth && paired {
            out.push(a);
        }
    }
    // SAFETY: frees exactly what DevGetObjects returned; `list` and `props` are not used after.
    unsafe { DevFreeObjects(count, objs) };
    Some(out)
}

fn parse_aep(props: &[DEVPROPERTY]) -> Aep {
    let mut a = Aep::default();
    for p in props {
        let Some(b) = value_bytes(p) else { continue };
        let k = &p.CompKey.Key;
        let t = p.Type;
        if same_key(k, &K_NAME) {
            if t == DEVPROP_TYPE_STRING {
                a.name = utf16_string(b);
            }
        } else if same_key(k, &K_PAIRED) {
            a.paired = as_bool(t, b);
        } else if same_key(k, &K_CONNECTED) {
            a.connected = as_bool(t, b).unwrap_or(false);
        } else if same_key(k, &K_PROTOCOL) {
            a.protocol = as_guid(t, b).unwrap_or(0);
        } else if same_key(k, &K_CONTAINER) {
            a.container = as_guid(t, b);
        } else if same_key(k, &K_COD_MAJOR) {
            a.major = as_uint(t, b);
        } else if same_key(k, &K_COD_MINOR) {
            a.minor = as_uint(t, b);
        } else if same_key(k, &K_APPEARANCE) {
            a.appearance = as_uint(t, b).and_then(|v| u16::try_from(v).ok());
        } else if same_key(k, &K_LAST_SEEN) && t == DEVPROP_TYPE_FILETIME && b.len() >= 8 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&b[..8]);
            a.last_seen = filetime_to_unix(u64::from_le_bytes(raw));
        }
    }
    a
}

/// The value bytes of a property; `None` when absent or empty (Device Query returns requested
/// but missing properties with an empty type and no buffer).
fn value_bytes(p: &DEVPROPERTY) -> Option<&[u8]> {
    if p.Buffer.is_null() || p.BufferSize == 0 {
        return None;
    }
    // SAFETY: Device Query owns BufferSize bytes at Buffer until DevFreeObjects.
    Some(unsafe { std::slice::from_raw_parts(p.Buffer as *const u8, p.BufferSize as usize) })
}

fn same_key(a: &DEVPROPKEY, b: &DEVPROPKEY) -> bool {
    a.pid == b.pid && guid_u128(&a.fmtid) == guid_u128(&b.fmtid)
}

fn guid_u128(g: &GUID) -> u128 {
    (g.data1 as u128) << 96
        | (g.data2 as u128) << 80
        | (g.data3 as u128) << 64
        | u64::from_be_bytes(g.data4) as u128
}

/// A GUID in its in-memory (little-endian `data1..3`) layout.
fn as_guid(t: u32, b: &[u8]) -> Option<u128> {
    if t != DEVPROP_TYPE_GUID || b.len() < 16 {
        return None;
    }
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u128;
    let d2 = u16::from_le_bytes([b[4], b[5]]) as u128;
    let d3 = u16::from_le_bytes([b[6], b[7]]) as u128;
    let mut d4 = [0u8; 8];
    d4.copy_from_slice(&b[8..16]);
    Some(d1 << 96 | d2 << 80 | d3 << 64 | u64::from_be_bytes(d4) as u128)
}

/// "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}", lowercase.
fn guid_string(g: u128) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:04x}-{:012x}}}",
        (g >> 96) as u32,
        (g >> 80) as u16,
        (g >> 64) as u16,
        (g >> 48) as u16,
        (g & 0xFFFF_FFFF_FFFF) as u64
    )
}

fn as_bool(t: u32, b: &[u8]) -> Option<bool> {
    (t == DEVPROP_TYPE_BOOLEAN).then(|| b[0] != 0)
}

/// Any small integer type (Windows stores CoD parts and appearance with varying widths).
fn as_uint(t: u32, b: &[u8]) -> Option<u32> {
    match t {
        DEVPROP_TYPE_BYTE => b.first().map(|&v| v as u32),
        DEVPROP_TYPE_UINT16 | DEVPROP_TYPE_INT16 if b.len() >= 2 => {
            Some(u16::from_le_bytes([b[0], b[1]]) as u32)
        }
        DEVPROP_TYPE_UINT32 | DEVPROP_TYPE_INT32 if b.len() >= 4 => {
            Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }
        _ => None,
    }
}

/// NUL-terminated UTF-16 bytes to a trimmed string.
fn utf16_string(b: &[u8]) -> String {
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    from_wide(&w).trim().to_string()
}

/// FILETIME (100 ns ticks since 1601) to unix seconds; `None` for 0 / before 1970.
fn filetime_to_unix(ft: u64) -> Option<u64> {
    if ft == 0 {
        return None;
    }
    (ft / TICKS_PER_SEC).checked_sub(UNIX_EPOCH_SECS)
}

// ---------------------------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------------------------

/// A device being built from its endpoints.
struct Acc {
    dev: BtDevice,
    major: Option<u32>,
    minor: Option<u32>,
    appearance: Option<u16>,
    /// `dev.name` came from the Classic endpoint.
    classic_name: bool,
}

/// One `BtDevice` per container; adds batteries and the kind.
fn merge(aeps: Vec<Aep>, bat: &HashMap<u128, u8>) -> Vec<BtDevice> {
    let mut map: HashMap<u128, Acc> = HashMap::new();
    for a in aeps {
        let Some(cid) = a.container else { continue };
        let acc = map.entry(cid).or_insert_with(|| Acc {
            dev: BtDevice { id: guid_string(cid), ..Default::default() },
            major: None,
            minor: None,
            appearance: None,
            classic_name: false,
        });
        let classic = a.protocol == PROTO_CLASSIC;
        acc.dev.classic |= classic;
        acc.dev.le |= !classic;
        acc.dev.connected |= a.connected;
        acc.dev.last_seen = acc.dev.last_seen.max(a.last_seen);
        acc.major = acc.major.or(a.major);
        acc.minor = acc.minor.or(a.minor);
        acc.appearance = acc.appearance.or(a.appearance);
        if !a.name.is_empty() && (acc.dev.name.is_empty() || (classic && !acc.classic_name)) {
            acc.dev.name = a.name;
            acc.classic_name = classic;
        }
    }
    let mut out = Vec::with_capacity(map.len());
    for (cid, mut acc) in map {
        acc.dev.battery = bat.get(&cid).copied();
        if acc.dev.name.is_empty() && acc.dev.battery.is_none() {
            continue;
        }
        acc.dev.kind = kind_from(acc.major, acc.minor, acc.appearance, &acc.dev.name);
        out.push(acc.dev);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Battery (configuration manager)
// ---------------------------------------------------------------------------------------------

/// Battery percent per container id, over the present `BTHENUM` and `BTHLE` device nodes.
/// A container can have the value on several nodes (copies); the highest wins.
fn batteries() -> HashMap<u128, u8> {
    let mut map: HashMap<u128, u8> = HashMap::new();
    for filter in ["BTHENUM", "BTHLE"] {
        let list = id_list(filter);
        let mut start = 0;
        while start < list.len() && list[start] != 0 {
            let Some(len) = list[start..].iter().position(|&c| c == 0) else { break };
            let end = start + len;
            // The id with its terminating NUL, as CM_Locate_DevNodeW wants it.
            let id = &list[start..=end];
            start = end + 1;
            let Some(node) = locate(id) else { continue };
            let Some(pct) = node_battery(node) else { continue };
            let Some(cid) = node_container(node) else { continue };
            let e = map.entry(cid).or_insert(pct);
            *e = (*e).max(pct);
        }
    }
    map
}

/// Multi-string of present device ids under one enumerator (empty on failure).
fn id_list(enumerator: &str) -> Vec<u16> {
    let f = wide(enumerator);
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    for _ in 0..2 {
        let mut len: u32 = 0;
        // SAFETY: `f` is NUL-terminated; `len` is a valid out pointer.
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, f.as_ptr(), flags) } != CR_SUCCESS
            || len == 0
            || len > MAX_ID_LIST
        {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds `len` UTF-16 units.
        let cr = unsafe { CM_Get_Device_ID_ListW(f.as_ptr(), buf.as_mut_ptr(), len, flags) };
        if cr == CR_SUCCESS {
            return buf;
        }
        if cr != CR_BUFFER_SMALL {
            break;
        }
    }
    Vec::new()
}

/// DevNode handle for a NUL-terminated device instance id.
fn locate(id: &[u16]) -> Option<u32> {
    let mut inst: u32 = 0;
    // SAFETY: `id` is NUL-terminated (it ends at the list's terminator).
    let cr = unsafe { CM_Locate_DevNodeW(&mut inst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) };
    (cr == CR_SUCCESS).then_some(inst)
}

/// A node property into a small stack buffer: its type and the valid bytes.
fn node_prop(node: u32, k: &DEVPROPKEY) -> Option<(u32, [u8; 16], usize)> {
    let mut ty: u32 = 0;
    let mut buf = [0u8; 16];
    let mut size = buf.len() as u32;
    // SAFETY: `buf`/`size` describe a valid 16-byte buffer; `k` points at a valid key.
    let cr = unsafe { CM_Get_DevNode_PropertyW(node, k, &mut ty, buf.as_mut_ptr(), &mut size, 0) };
    (cr == CR_SUCCESS && size as usize <= buf.len()).then_some((ty, buf, size as usize))
}

fn node_battery(node: u32) -> Option<u8> {
    let (ty, buf, size) = node_prop(node, &K_BATTERY)?;
    (ty == DEVPROP_TYPE_BYTE && size >= 1 && buf[0] <= MAX_PCT).then_some(buf[0])
}

fn node_container(node: u32) -> Option<u128> {
    let (ty, buf, size) = node_prop(node, &DEVPKEY_Device_ContainerId)?;
    as_guid(ty, &buf[..size])
}

// ---------------------------------------------------------------------------------------------
// Kind
// ---------------------------------------------------------------------------------------------

/// Name keywords, matched against the lowercased name in table order (first hit wins, so
/// specific rows come before general ones). `Sub` is a substring, `Word` a whole token (split
/// on non-alphanumerics) for short words that would otherwise hit inside others
/// ("pen" in "open", "phone" in "headphone", "band" in "bandit").
enum Pat {
    Sub(&'static [&'static str]),
    Word(&'static [&'static str]),
}

const NAME_KINDS: &[(Kind, Pat)] = &[
    (Kind::Headphones, Pat::Sub(&["airpods max"])),
    (
        Kind::Earbuds,
        Pat::Sub(&["buds", "pods", "earphone", "wf-", "tws", "beats fit", "powerbeats"]),
    ),
    (
        Kind::Headphones,
        Pat::Sub(&["wh-", "headphone", "quietcomfort", "qc"]),
    ),
    (Kind::Keyboard, Pat::Sub(&["keyboard", "keychron", "mx keys"])),
    (Kind::Keyboard, Pat::Word(&["keys"])),
    (Kind::Mouse, Pat::Sub(&["mouse", "mx master", "mx anywhere"])),
    (
        Kind::Gamepad,
        Pat::Sub(&["controller", "gamepad", "dualsense", "dualshock", "xbox", "8bitdo"]),
    ),
    (Kind::Pen, Pat::Sub(&["stylus", "pencil"])),
    (Kind::Pen, Pat::Word(&["pen"])),
    (Kind::Watch, Pat::Sub(&["watch"])),
    (Kind::Watch, Pat::Word(&["band"])),
    (Kind::Speaker, Pat::Sub(&["speaker", "soundlink"])),
    (Kind::Speaker, Pat::Word(&["flip", "charge", "boom"])),
    (Kind::Phone, Pat::Sub(&["iphone", "galaxy s"])),
    (Kind::Phone, Pat::Word(&["phone", "pixel"])),
];

/// Kind from the device name alone; `Other` when no keyword matches.
fn kind_from_name(name: &str) -> Kind {
    let lower = name.to_lowercase();
    for (kind, pat) in NAME_KINDS {
        let hit = match pat {
            Pat::Sub(list) => list.iter().any(|k| lower.contains(k)),
            Pat::Word(list) => lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|t| list.contains(&t)),
        };
        if hit {
            return *kind;
        }
    }
    Kind::Other
}

/// Kind from the Class of Device parts. Assumes what Windows stores in
/// `Aep.Bluetooth.Cod.Major/Minor`: the major class (CoD bits 12..8) and the 6-bit minor
/// class (CoD bits 7..2, already shifted down). A minor above 0x3F can only be the raw low
/// CoD byte, so it is shifted down by 2 to be safe.
fn kind_from_cod(major: u32, minor: u32) -> Kind {
    let m = if minor > 0x3F { minor >> 2 } else { minor };
    match major {
        1 => Kind::Computer,
        2 => Kind::Phone,
        // Audio/Video: 1 wearable headset, 2 hands-free, 5 loudspeaker, 6 headphones,
        // 7 portable audio, 8 car audio, 10 hi-fi audio.
        4 => match m {
            1 | 2 => Kind::Headset,
            5 | 7 | 8 | 10 => Kind::Speaker,
            6 => Kind::Headphones,
            _ => Kind::Other,
        },
        // Peripheral: minor bits 5..4 = device kind (1 keyboard, 2 pointing, 3 combo),
        // bits 3..0 = subtype (1 joystick, 2 gamepad, 5 digitizer tablet).
        5 => match (m >> 4 & 3, m & 0xF) {
            (_, 1 | 2) => Kind::Gamepad,
            (_, 5) => Kind::Pen,
            (1 | 3, _) => Kind::Keyboard,
            (2, _) => Kind::Mouse,
            _ => Kind::Other,
        },
        7 => Kind::Watch,
        _ => Kind::Other,
    }
}

/// Kind from a Bluetooth LE GAP appearance value (category = value >> 6).
fn kind_from_appearance(a: u16) -> Kind {
    match a {
        0x03C1 => Kind::Keyboard,
        0x03C2 => Kind::Mouse,
        0x03C3 | 0x03C4 => Kind::Gamepad,
        0x03C5 | 0x03C7 => Kind::Pen,
        0x0841 => Kind::Speaker,
        0x0941 => Kind::Earbuds,
        0x0942 => Kind::Headset,
        0x0943 | 0x0944 => Kind::Headphones,
        _ => match a >> 6 {
            1 => Kind::Phone,
            2 => Kind::Computer,
            3 => Kind::Watch,
            _ => Kind::Other,
        },
    }
}

/// Best guess of what a device is: Class of Device first, then LE appearance, then the name.
/// The name overrides `Other` and refines a generic audio kind (many earbuds report the
/// headset / headphones class, and "AirPods Max" are headphones), but never turns an audio
/// device into a keyboard or the like.
pub fn kind_from(
    cod_major: Option<u32>,
    cod_minor: Option<u32>,
    appearance: Option<u16>,
    name: &str,
) -> Kind {
    let by_cod = cod_major.map_or(Kind::Other, |m| kind_from_cod(m, cod_minor.unwrap_or(0)));
    let base = match by_cod {
        Kind::Other => appearance.map_or(Kind::Other, kind_from_appearance),
        k => k,
    };
    let by_name = kind_from_name(name);
    match base {
        Kind::Other => by_name,
        Kind::Headset | Kind::Headphones
            if matches!(by_name, Kind::Earbuds | Kind::Headphones | Kind::Speaker) =>
        {
            by_name
        }
        k => k,
    }
}

// ---------------------------------------------------------------------------------------------
// Background reader
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct ReaderState {
    /// A read is wanted (true while one is queued, not while one runs).
    job: bool,
    done: Option<Arc<Vec<BtDevice>>>,
}

type Shared = Arc<(Mutex<ReaderState>, Condvar)>;

/// Runs [`read`] on a thread of its own, so a slow Device Query never holds the sensor
/// thread (and with it every number on screen). Cheap until the first `request`.
#[derive(Default)]
pub struct BtReader {
    s: Option<Shared>,
    latest: Arc<Vec<BtDevice>>,
}

impl BtReader {
    /// Ask for a fresh read. Never blocks; repeated asks while one runs collapse into one.
    pub fn request(&mut self) {
        let s = self.s.get_or_insert_with(spawn);
        let (m, cv) = &**s;
        m.lock().unwrap_or_else(|e| e.into_inner()).job = true;
        cv.notify_one();
    }

    /// The newest finished read (empty until the first one lands).
    pub fn latest(&mut self) -> Arc<Vec<BtDevice>> {
        if let Some(s) = &self.s {
            if let Some(d) = s.0.lock().unwrap_or_else(|e| e.into_inner()).done.take() {
                self.latest = d;
            }
        }
        self.latest.clone()
    }

    /// Forget the last read (the module was turned off).
    pub fn clear(&mut self) {
        if let Some(s) = &self.s {
            s.0.lock().unwrap_or_else(|e| e.into_inner()).done = None;
        }
        self.latest = Arc::default();
    }
}

fn spawn() -> Shared {
    let s: Shared = Arc::default();
    let t = s.clone();
    let _ = std::thread::Builder::new().name("bt-reader".into()).spawn(move || {
        let (m, cv) = &*t;
        loop {
            {
                let mut st = m.lock().unwrap_or_else(|e| e.into_inner());
                while !st.job {
                    st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
                }
                st.job = false;
            }
            let t0 = std::time::Instant::now();
            // CI has no Bluetooth radio: RESMON_BT_DEMO fills the page with example devices.
            let list = if std::env::var_os("RESMON_BT_DEMO").is_some() { demo() } else { read() };
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            crate::perf::record("bluetooth", ms);
            if ms >= SLOW_READ_MS {
                crate::util::diag(&format!("slow: bluetooth read {ms:.0} ms ({} devices)", list.len()));
            }
            m.lock().unwrap_or_else(|e| e.into_inner()).done = Some(Arc::new(list));
        }
    });
    s
}

/// A Bluetooth read slower than this is logged to diagnostics.log.
const SLOW_READ_MS: f32 = 500.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// Startup guard: the Bluetooth query is narrowed to the two Bluetooth protocols, so it
    /// must not wait on network discovery. Prints which filter Windows took (CI log).
    #[test]
    fn device_query_is_quick() {
        let t = std::time::Instant::now();
        let paired = query_aeps(Filter::PairedBluetooth);
        let ms = t.elapsed().as_millis();
        println!("bluetooth query (paired + protocol filter): {ms} ms, {:?} endpoints", paired.as_ref().map(Vec::len));
        if paired.is_none() {
            let t = std::time::Instant::now();
            let proto = query_aeps(Filter::Bluetooth);
            println!("bluetooth query (protocol filter): {} ms, {:?} endpoints", t.elapsed().as_millis(), proto.as_ref().map(Vec::len));
        }
        assert!(ms < 1500, "Bluetooth device query took {ms} ms");
    }

    /// Asking for a read never waits for it.
    #[test]
    fn reader_never_blocks() {
        let mut r = BtReader::default();
        let t = std::time::Instant::now();
        for _ in 0..5 {
            r.request();
            let _ = r.latest();
        }
        assert!(t.elapsed().as_millis() < 50, "BtReader blocked for {} ms", t.elapsed().as_millis());
    }

    #[test]
    fn cod_headphones() {
        assert_eq!(kind_from(Some(4), Some(6), None, "Some Cans"), Kind::Headphones);
        assert_eq!(kind_from(Some(4), Some(1), None, "Jabra Talk"), Kind::Headset);
        // Headset class but named like buds.
        assert_eq!(kind_from(Some(4), Some(1), None, "Galaxy Buds2 Pro"), Kind::Earbuds);
        assert_eq!(kind_from(Some(4), Some(5), None, "JBL Go"), Kind::Speaker);
    }

    #[test]
    fn cod_peripherals() {
        // Windows gives the 6-bit minor; the raw CoD low byte is accepted too.
        assert_eq!(kind_from(Some(5), Some(0x10), None, "x"), Kind::Keyboard);
        assert_eq!(kind_from(Some(5), Some(0x20), None, "x"), Kind::Mouse);
        assert_eq!(kind_from(Some(5), Some(0x40), None, "x"), Kind::Keyboard);
        assert_eq!(kind_from(Some(5), Some(0x02), None, "x"), Kind::Gamepad);
        assert_eq!(kind_from(Some(5), Some(0x05), None, "x"), Kind::Pen);
        assert_eq!(kind_from(Some(2), Some(3), None, "x"), Kind::Phone);
        assert_eq!(kind_from(Some(1), Some(3), None, "x"), Kind::Computer);
        assert_eq!(kind_from(Some(7), Some(1), None, "x"), Kind::Watch);
    }

    #[test]
    fn le_earbud_appearance() {
        assert_eq!(kind_from(None, None, Some(0x0941), "Whatever"), Kind::Earbuds);
        assert_eq!(kind_from(None, None, Some(0x0943), "Whatever"), Kind::Headphones);
        assert_eq!(kind_from(None, None, Some(0x00C0), "Whatever"), Kind::Watch);
    }

    #[test]
    fn name_only() {
        assert_eq!(kind_from(None, None, None, "AirPods Pro"), Kind::Earbuds);
        assert_eq!(kind_from(None, None, None, "AirPods Max"), Kind::Headphones);
        assert_eq!(kind_from(None, None, None, "WH-1000XM5"), Kind::Headphones);
        assert_eq!(kind_from(None, None, None, "Open Thing"), Kind::Other);
        assert_eq!(kind_from(None, None, None, "Surface Pen"), Kind::Pen);
        assert_eq!(kind_from(None, None, None, "Logitech MX Keys"), Kind::Keyboard);
        assert_eq!(kind_from(None, None, None, "Xbox Wireless Controller"), Kind::Gamepad);
    }

    #[test]
    fn mouse_via_appearance() {
        assert_eq!(kind_from(None, None, Some(0x03C2), "BT-123"), Kind::Mouse);
        assert_eq!(kind_from(None, Some(2), Some(0x03C2), "BT-123"), Kind::Mouse);
    }

    #[test]
    fn unknown_is_other() {
        assert_eq!(kind_from(None, None, None, "XYZ-9000"), Kind::Other);
        assert_eq!(kind_from(Some(31), Some(0), Some(0), ""), Kind::Other);
    }

    #[test]
    fn filetime() {
        // 1970-01-01 and 2001-09-09 01:46:40 UTC (unix 1_000_000_000).
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), Some(0));
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000_000_000_000), Some(1_000_000_000));
        assert_eq!(filetime_to_unix(0), None);
        assert_eq!(filetime_to_unix(1_000_000), None);
    }

    #[test]
    fn guid_text() {
        let g = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef_u128;
        assert_eq!(guid_string(g), "{01234567-89ab-cdef-0123-456789abcdef}");
    }

    #[test]
    fn merge_dual_mode() {
        let cid = 7u128;
        let classic = Aep {
            name: "Pods".into(),
            protocol: PROTO_CLASSIC,
            container: Some(cid),
            major: Some(4),
            minor: Some(6),
            last_seen: Some(10),
            ..Default::default()
        };
        let le = Aep {
            name: "Pods LE".into(),
            protocol: PROTO_LE,
            connected: true,
            container: Some(cid),
            last_seen: Some(20),
            ..Default::default()
        };
        let mut bat = HashMap::new();
        bat.insert(cid, 55u8);
        let v = merge(vec![le, classic], &bat);
        assert_eq!(v.len(), 1);
        let d = &v[0];
        assert_eq!(d.name, "Pods");
        assert!(d.le && d.classic && d.connected);
        assert_eq!(d.battery, Some(55));
        assert_eq!(d.last_seen, Some(20));
        assert_eq!(d.kind, Kind::Earbuds);
    }
}
