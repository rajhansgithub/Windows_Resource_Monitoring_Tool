//! "What is filling my drive?": the biggest folders of a volume, like WizTree.
//!
//! Two engines feed one compact [`Tree`] (folders only, files are summed into their folder):
//!
//! * Fast (needs admin, NTFS only): the raw `$MFT` is read through the volume handle
//!   (`\\.\C:`, `FSCTL_GET_NTFS_VOLUME_DATA`, then the `$MFT` data runs parsed from record 0)
//!   in 4 MiB chunks. Every FILE record is fixed up and parsed for its best `$FILE_NAME`
//!   (parent reference and name) and its unnamed `$DATA` size, so one sequential read gives
//!   names, parents and sizes; `FSCTL_ENUM_USN_DATA` is not needed. Extension records add
//!   their `$DATA` size to their base record. Real (logical) size is used, like Explorer.
//! * Fallback (no admin, any filesystem): a `FindFirstFileExW` walk (`FindExInfoBasic`,
//!   `FIND_FIRST_EX_LARGE_FETCH`) that never follows reparse points.
//!
//! The scan runs on a thread named `space-scan`; the UI polls [`Scan::progress`] and takes
//! [`Scan::result`] once done. If the fast engine fails for any reason (no admin, not NTFS,
//! unexpected layout) the scan silently continues with the fallback.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Index of a folder in a [`Tree`]. The root folder is always `0`.
pub type NodeId = u32;

const NONE: u32 = u32::MAX;
/// Low 48 bits of an NTFS file reference are the MFT record number.
const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// Largest MFT record count we are willing to index (memory guard).
const MAX_RECORDS: u64 = 1 << 28;

// ---------------------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------------------

struct Node {
    parent: u32,
    name_off: u32,
    name_len: u16,
    own_files: u32,
    total_files: u32,
    own_bytes: u64,
    total_bytes: u64,
}

/// Compact folder tree of a scanned volume (about 48 bytes per folder plus its name).
pub struct Tree {
    nodes: Vec<Node>,
    names: String,
    /// CSR child index: children of `i` are `children[child_start[i]..child_start[i + 1]]`.
    child_start: Vec<u32>,
    children: Vec<u32>,
}

/// One row of [`Tree::top`]: a child folder or the "files here" pseudo entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The folder (for the pseudo entry: the folder whose own files it stands for).
    pub id: NodeId,
    pub name: String,
    /// Size in bytes (folder: everything below it).
    pub bytes: u64,
    /// File count (folder: everything below it).
    pub files: u64,
    /// This row is the files that sit directly in the parent, not a sub-folder.
    pub is_files_here: bool,
}

impl Tree {
    /// Total bytes of all files under the root.
    pub fn total(&self) -> u64 {
        self.nodes.first().map_or(0, |n| n.total_bytes)
    }

    /// Total number of files under the root.
    pub fn total_files(&self) -> u64 {
        self.nodes.first().map_or(0, |n| n.total_files as u64)
    }

    /// Number of folders (including the root).
    pub fn folders(&self) -> usize {
        self.nodes.len()
    }

    /// Bytes below a folder.
    pub fn bytes(&self, id: NodeId) -> u64 {
        self.nodes.get(id as usize).map_or(0, |n| n.total_bytes)
    }

    /// Parent folder, `None` for the root or an unknown id.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        if id == 0 {
            return None;
        }
        self.nodes.get(id as usize).map(|n| n.parent)
    }

    /// Folder name (the root's name is the scanned root, e.g. `C:\`).
    pub fn name(&self, id: NodeId) -> &str {
        match self.nodes.get(id as usize) {
            Some(n) => {
                let a = n.name_off as usize;
                self.names.get(a..a + n.name_len as usize).unwrap_or("")
            }
            None => "",
        }
    }

    /// Full path of a folder, e.g. `C:\Users\bob`.
    pub fn path(&self, id: NodeId) -> String {
        if id as usize >= self.nodes.len() {
            return String::new();
        }
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = id;
        // The depth guard stops a corrupt parent chain from looping forever.
        for _ in 0..self.nodes.len() {
            parts.push(self.name(cur));
            if cur == 0 {
                break;
            }
            cur = self.nodes[cur as usize].parent;
        }
        parts.reverse();
        let mut out = String::new();
        for (i, p) in parts.iter().enumerate() {
            if i > 0 && !out.is_empty() && !out.ends_with('\\') {
                out.push('\\');
            }
            out.push_str(p);
        }
        out
    }

    fn kids(&self, id: u32) -> &[u32] {
        let i = id as usize;
        match (self.child_start.get(i), self.child_start.get(i + 1)) {
            (Some(&a), Some(&b)) => self.children.get(a as usize..b as usize).unwrap_or(&[]),
            _ => &[],
        }
    }

    /// The `n` biggest children of `dir` (`None` = root), folders plus one "files here" pseudo
    /// entry for the files directly inside it, by size descending.
    pub fn top(&self, dir: Option<NodeId>, n: usize) -> Vec<Entry> {
        let id = dir.unwrap_or(0);
        let Some(node) = self.nodes.get(id as usize) else { return Vec::new() };
        if n == 0 {
            return Vec::new();
        }
        let mut cand: Vec<(u64, u32)> =
            self.kids(id).iter().map(|&k| (self.nodes[k as usize].total_bytes, k)).collect();
        if node.own_bytes > 0 || node.own_files > 0 {
            cand.push((node.own_bytes, NONE));
        }
        let cmp = |a: &(u64, u32), b: &(u64, u32)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
        if cand.len() > n {
            cand.select_nth_unstable_by(n - 1, cmp);
            cand.truncate(n);
        }
        cand.sort_by(cmp);
        cand.into_iter()
            .map(|(bytes, k)| {
                if k == NONE {
                    Entry {
                        id,
                        name: "(files here)".to_string(),
                        bytes,
                        files: node.own_files as u64,
                        is_files_here: true,
                    }
                } else {
                    Entry {
                        id: k,
                        name: self.name(k).to_string(),
                        bytes,
                        files: self.nodes[k as usize].total_files as u64,
                        is_files_here: false,
                    }
                }
            })
            .collect()
    }

    /// Build a tree from synthetic or enumerated records in any order. `root_ref` is the
    /// file reference of the root folder (5 on NTFS); records whose parent chain does not
    /// reach it are dropped.
    pub fn from_records(root_name: &str, root_ref: u64, records: &[Record]) -> Tree {
        let root = (root_ref & REF_MASK) as u32;
        let max = records
            .iter()
            .flat_map(|r| [r.file_ref & REF_MASK, r.parent_ref & REF_MASK])
            .max()
            .unwrap_or(0)
            .max(root as u64);
        let mut t = MftTable::new((max + 1).min(MAX_RECORDS) as usize);
        for r in records {
            let (rn, parent) = ((r.file_ref & REF_MASK) as u32, (r.parent_ref & REF_MASK) as u32);
            if r.is_dir {
                t.add_dir(rn, parent, &r.name);
            } else {
                t.add_file(rn, parent, r.size);
            }
        }
        t.build(root, root_name)
    }
}

/// A synthetic or parsed directory entry for [`Tree::from_records`].
#[derive(Clone, Debug)]
pub struct Record {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Builds a [`Tree`] by adding folders parent-first (a parent always has a smaller id).
struct TreeBuilder {
    nodes: Vec<Node>,
    names: String,
}

impl TreeBuilder {
    fn new(root_name: &str) -> Self {
        let mut b = TreeBuilder { nodes: Vec::new(), names: String::new() };
        b.push(0, root_name);
        b
    }

    fn push(&mut self, parent: u32, name: &str) -> u32 {
        let name = if name.len() > u16::MAX as usize { &name[..0] } else { name };
        let id = self.nodes.len() as u32;
        self.nodes.push(Node {
            parent,
            name_off: self.names.len() as u32,
            name_len: name.len() as u16,
            own_files: 0,
            total_files: 0,
            own_bytes: 0,
            total_bytes: 0,
        });
        self.names.push_str(name);
        id
    }

    fn add_dir(&mut self, parent: u32, name: &str) -> u32 {
        self.push(parent, name)
    }

    fn add_files(&mut self, id: u32, bytes: u64, files: u32) {
        if let Some(n) = self.nodes.get_mut(id as usize) {
            n.own_bytes = n.own_bytes.saturating_add(bytes);
            n.own_files = n.own_files.saturating_add(files);
        }
    }

    fn finish(mut self) -> Tree {
        let n = self.nodes.len();
        for nd in &mut self.nodes {
            nd.total_bytes = nd.own_bytes;
            nd.total_files = nd.own_files;
        }
        // Parents precede children, so one reverse pass rolls the totals up.
        for i in (1..n).rev() {
            let (b, f, p) = (self.nodes[i].total_bytes, self.nodes[i].total_files, self.nodes[i].parent as usize);
            if p < i {
                self.nodes[p].total_bytes = self.nodes[p].total_bytes.saturating_add(b);
                self.nodes[p].total_files = self.nodes[p].total_files.saturating_add(f);
            }
        }
        let mut child_start = vec![0u32; n + 1];
        for nd in self.nodes.iter().skip(1) {
            child_start[nd.parent as usize + 1] += 1;
        }
        for i in 0..n {
            child_start[i + 1] += child_start[i];
        }
        let mut fill = child_start.clone();
        let mut children = vec![0u32; n.saturating_sub(1)];
        for (i, nd) in self.nodes.iter().enumerate().skip(1) {
            let slot = &mut fill[nd.parent as usize];
            children[*slot as usize] = i as u32;
            *slot += 1;
        }
        self.names.shrink_to_fit();
        self.nodes.shrink_to_fit();
        Tree { nodes: self.nodes, names: self.names, child_start, children }
    }
}

// ---------------------------------------------------------------------------------------
// MFT record table: order independent, then folded into a Tree
// ---------------------------------------------------------------------------------------

struct DirRec {
    rn: u32,
    name_off: u32,
    name_len: u16,
}

/// Per MFT record data (16 bytes per record) collected while scanning.
struct MftTable {
    /// Parent record number per record (`NONE` = unused or nameless).
    parent: Vec<u32>,
    /// Summed unnamed `$DATA` size per record.
    size: Vec<u64>,
    /// Index into `dirs` for directory records, else `NONE`.
    slot: Vec<u32>,
    dirs: Vec<DirRec>,
    names: String,
}

impl MftTable {
    fn new(records: usize) -> Self {
        MftTable {
            parent: vec![NONE; records],
            size: vec![0; records],
            slot: vec![NONE; records],
            dirs: Vec::new(),
            names: String::new(),
        }
    }

    fn add_dir(&mut self, rn: u32, parent: u32, name: &str) {
        let i = rn as usize;
        if i >= self.parent.len() || name.len() > u16::MAX as usize {
            return;
        }
        self.parent[i] = parent;
        self.slot[i] = self.dirs.len() as u32;
        self.dirs.push(DirRec { rn, name_off: self.names.len() as u32, name_len: name.len() as u16 });
        self.names.push_str(name);
    }

    fn add_file(&mut self, rn: u32, parent: u32, size: u64) {
        let i = rn as usize;
        if i < self.parent.len() {
            self.parent[i] = parent;
            self.size[i] = self.size[i].saturating_add(size);
        }
    }

    /// `$DATA` size found in an extension record of `base`.
    fn add_ext_size(&mut self, base: u32, size: u64) {
        if let Some(s) = self.size.get_mut(base as usize) {
            *s = s.saturating_add(size);
        }
    }

    fn build(mut self, root: u32, root_name: &str) -> Tree {
        let n = self.parent.len();
        if (root as usize) >= n {
            return TreeBuilder::new(root_name).finish();
        }
        if self.slot[root as usize] == NONE {
            self.add_dir(root, root, "");
        }
        let d = self.dirs.len();
        // CSR of directory children, keyed by parent directory index.
        let mut start = vec![0u32; d + 1];
        let parent_dir = |t: &MftTable, di: usize| -> Option<usize> {
            let rec = &t.dirs[di];
            if rec.rn == root {
                return None;
            }
            let p = *t.parent.get(rec.rn as usize)? as usize;
            let s = *t.slot.get(p)?;
            (s != NONE).then_some(s as usize)
        };
        for di in 0..d {
            if let Some(p) = parent_dir(&self, di) {
                start[p + 1] += 1;
            }
        }
        for i in 0..d {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut kids = vec![0u32; start[d] as usize];
        for di in 0..d {
            if let Some(p) = parent_dir(&self, di) {
                kids[fill[p] as usize] = di as u32;
                fill[p] += 1;
            }
        }
        drop(fill);
        // Breadth-first from the root assigns tree ids parent-first.
        let mut b = TreeBuilder::new(root_name);
        let mut node_of = vec![NONE; d];
        let root_di = self.slot[root as usize] as usize;
        node_of[root_di] = 0;
        let mut queue: Vec<u32> = vec![root_di as u32];
        let mut qi = 0;
        while qi < queue.len() {
            let di = queue[qi] as usize;
            qi += 1;
            let pid = node_of[di];
            for &k in &kids[start[di] as usize..start[di + 1] as usize] {
                let rec = &self.dirs[k as usize];
                let a = rec.name_off as usize;
                let name = self.names.get(a..a + rec.name_len as usize).unwrap_or("");
                node_of[k as usize] = b.add_dir(pid, name);
                queue.push(k);
            }
        }
        // Fold files into their folder.
        for rn in 0..n {
            let p = self.parent[rn];
            if p == NONE || self.slot[rn] != NONE {
                continue;
            }
            let Some(&s) = self.slot.get(p as usize) else { continue };
            if s == NONE || node_of[s as usize] == NONE {
                continue;
            }
            b.add_files(node_of[s as usize], self.size[rn], 1);
        }
        b.finish()
    }
}

// ---------------------------------------------------------------------------------------
// NTFS MFT record parsing (pure)
// ---------------------------------------------------------------------------------------

fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| {
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        u64::from_le_bytes(a)
    })
}

/// Undo the update-sequence protection of a FILE record in place. False if the record is
/// torn or malformed.
fn apply_fixups(rec: &mut [u8]) -> bool {
    if rec.len() < 48 || &rec[..4] != b"FILE" {
        return false;
    }
    let (Some(off), Some(cnt)) = (rd_u16(rec, 4), rd_u16(rec, 6)) else { return false };
    let (off, cnt) = (off as usize, cnt as usize);
    if cnt < 2 || (cnt - 1) * 512 != rec.len() || off + cnt * 2 > rec.len() {
        return false;
    }
    let usn = [rec[off], rec[off + 1]];
    for i in 0..cnt - 1 {
        let end = (i + 1) * 512;
        if rec[end - 2..end] != usn {
            return false;
        }
        rec[end - 2] = rec[off + 2 + 2 * i];
        rec[end - 1] = rec[off + 3 + 2 * i];
    }
    true
}

/// What the scanner needs from one FILE record.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parsed {
    is_dir: bool,
    /// Record number of the base record when this is an extension record.
    base: Option<u32>,
    /// Parent record number from the best `$FILE_NAME`.
    parent: Option<u32>,
    /// Best name as (byte offset in the record, UTF-16 units).
    name: Option<(usize, usize)>,
    /// Unnamed `$DATA` logical size.
    data_size: Option<u64>,
}

/// Fix up and parse an in-use FILE record. `None` if it is unused, torn or malformed.
fn parse_record(rec: &mut [u8]) -> Option<Parsed> {
    if !apply_fixups(rec) {
        return None;
    }
    let flags = rd_u16(rec, 22)?;
    if flags & 1 == 0 {
        return None;
    }
    let used = (rd_u32(rec, 24)? as usize).min(rec.len());
    let base_ref = rd_u64(rec, 32)? & REF_MASK;
    let mut out = Parsed { is_dir: flags & 2 != 0, ..Parsed::default() };
    if base_ref != 0 {
        out.base = Some(base_ref as u32);
    }
    let mut best_rank = 0u8;
    let mut p = rd_u16(rec, 20)? as usize;
    while p + 16 <= used {
        let ty = rd_u32(rec, p)?;
        if ty == 0xFFFF_FFFF {
            break;
        }
        let alen = rd_u32(rec, p + 4)? as usize;
        if alen < 16 || !alen.is_multiple_of(8) || p + alen > used {
            break;
        }
        let a = &rec[p..p + alen];
        let non_res = a[8] != 0;
        match ty {
            0x30 if !non_res => {
                let vlen = rd_u32(a, 16)? as usize;
                let voff = rd_u16(a, 20)? as usize;
                if let Some(v) = a.get(voff..voff + vlen) {
                    if v.len() >= 66 {
                        let units = v[64] as usize;
                        let rank = match v[65] {
                            1 | 3 => 3,
                            0 => 2,
                            2 => 1,
                            _ => 0,
                        };
                        if rank > best_rank && 66 + units * 2 <= v.len() {
                            best_rank = rank;
                            out.parent = Some((rd_u64(v, 0)? & REF_MASK) as u32);
                            out.name = Some((p + voff + 66, units));
                        }
                    }
                }
            }
            0x80 if a[9] == 0 && out.data_size.is_none() => {
                if non_res {
                    // Only the first extent (starting VCN 0) carries the real size.
                    if rd_u64(a, 16)? == 0 {
                        out.data_size = Some(rd_u64(a, 48)?);
                    }
                } else {
                    out.data_size = Some(rd_u32(a, 16)? as u64);
                }
            }
            _ => {}
        }
        p += alen;
    }
    Some(out)
}

/// Decode a record name located by [`parse_record`].
fn record_name(rec: &[u8], at: (usize, usize)) -> String {
    let Some(b) = rec.get(at.0..at.0 + at.1 * 2) else { return String::new() };
    char::decode_utf16(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])))
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// One extent of the `$MFT` data stream.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Run {
    /// Start cluster, `None` for a sparse hole.
    lcn: Option<u64>,
    clusters: u64,
}

/// Decode an NTFS runlist (mapping pairs).
fn parse_runs(b: &[u8]) -> Option<Vec<Run>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut lcn: i64 = 0;
    while i < b.len() && b[i] != 0 {
        let h = b[i];
        let (ls, os) = ((h & 0x0F) as usize, (h >> 4) as usize);
        i += 1;
        if ls == 0 || ls > 8 || os > 8 || i + ls + os > b.len() {
            return None;
        }
        let mut len = 0u64;
        for (k, &x) in b[i..i + ls].iter().enumerate() {
            len |= (x as u64) << (8 * k);
        }
        i += ls;
        if os == 0 {
            out.push(Run { lcn: None, clusters: len });
        } else {
            let mut v = 0u64;
            for (k, &x) in b[i..i + os].iter().enumerate() {
                v |= (x as u64) << (8 * k);
            }
            if os < 8 && b[i + os - 1] & 0x80 != 0 {
                v |= !0u64 << (8 * os);
            }
            i += os;
            lcn = lcn.checked_add(v as i64)?;
            if lcn < 0 {
                return None;
            }
            out.push(Run { lcn: Some(lcn as u64), clusters: len });
        }
    }
    Some(out)
}

/// Data runs and real size of the `$MFT` itself, from its fixed-up record 0.
fn mft_runs(rec0: &[u8]) -> Option<(Vec<Run>, u64)> {
    let used = (rd_u32(rec0, 24)? as usize).min(rec0.len());
    let mut p = rd_u16(rec0, 20)? as usize;
    while p + 16 <= used {
        let ty = rd_u32(rec0, p)?;
        if ty == 0xFFFF_FFFF {
            break;
        }
        let alen = rd_u32(rec0, p + 4)? as usize;
        if alen < 16 || p + alen > used {
            break;
        }
        let a = &rec0[p..p + alen];
        if ty == 0x80 && a[9] == 0 && a[8] != 0 && rd_u64(a, 16)? == 0 {
            let roff = rd_u16(a, 32)? as usize;
            return Some((parse_runs(a.get(roff..)?)?, rd_u64(a, 48)?));
        }
        p += alen;
    }
    None
}

/// Geometry from `NTFS_VOLUME_DATA_BUFFER`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct VolInfo {
    bytes_per_sector: u32,
    bytes_per_cluster: u32,
    record_size: u32,
    mft_valid_len: u64,
    mft_start_lcn: u64,
}

/// Parse the raw `NTFS_VOLUME_DATA_BUFFER` bytes (96 bytes minimum).
fn parse_vol(b: &[u8]) -> Option<VolInfo> {
    let v = VolInfo {
        bytes_per_sector: rd_u32(b, 40)?,
        bytes_per_cluster: rd_u32(b, 44)?,
        record_size: rd_u32(b, 48)?,
        mft_valid_len: rd_u64(b, 56)?,
        mft_start_lcn: rd_u64(b, 64)?,
    };
    let ok = v.bytes_per_sector.is_power_of_two()
        && v.bytes_per_cluster.is_power_of_two()
        && v.bytes_per_cluster >= v.bytes_per_sector
        && v.record_size.is_power_of_two()
        && (512..=65536).contains(&v.record_size)
        && v.record_size >= v.bytes_per_sector
        && v.bytes_per_cluster <= 1 << 21;
    ok.then_some(v)
}

/// `"C:"`, `"C:\"` or `"c:/"` give the drive letter; any other path (a sub-folder, UNC)
/// is not a volume root.
fn drive_letter(root: &str) -> Option<char> {
    let b = root.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    root[2..].chars().all(|c| c == '\\' || c == '/').then(|| (b[0] as char).to_ascii_uppercase())
}

/// Fold one MFT record into the table. Returns (files, bytes) it added to the progress.
fn ingest(t: &mut MftTable, rn: u32, rec: &mut [u8]) -> (u64, u64) {
    if rec.len() < 48 || &rec[..4] != b"FILE" || rd_u16(rec, 22).is_none_or(|f| f & 1 == 0) {
        return (0, 0);
    }
    let Some(p) = parse_record(rec) else { return (0, 0) };
    if let Some(base) = p.base {
        return match p.data_size {
            Some(sz) if !p.is_dir => {
                t.add_ext_size(base, sz);
                (0, sz)
            }
            _ => (0, 0),
        };
    }
    let Some(parent) = p.parent else { return (0, 0) };
    if p.is_dir {
        let name = p.name.map(|at| record_name(rec, at)).unwrap_or_default();
        t.add_dir(rn, parent, &name);
        (0, 0)
    } else {
        let sz = p.data_size.unwrap_or(0);
        t.add_file(rn, parent, sz);
        (1, sz)
    }
}

// ---------------------------------------------------------------------------------------
// Scan thread
// ---------------------------------------------------------------------------------------

/// Snapshot of a running or finished scan.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Files seen so far.
    pub files: u64,
    /// Bytes seen so far.
    pub bytes: u64,
    pub done: bool,
    /// The fast MFT engine is (or was, if done) in use; false for the folder walk.
    pub fast: bool,
    /// Set when the scan produced no result.
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    error: Option<String>,
    tree: Option<Arc<Tree>>,
}

#[derive(Default)]
struct Shared {
    files: AtomicU64,
    bytes: AtomicU64,
    done: AtomicBool,
    cancel: AtomicBool,
    fast: AtomicBool,
    state: Mutex<State>,
}

impl Shared {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Why an engine stopped without a tree.
enum ScanErr {
    Cancelled,
    Failed(String),
}

/// A volume scan running on its own thread (`space-scan`). Dropping it cancels the scan.
pub struct Scan {
    sh: Arc<Shared>,
}

/// Start scanning `root` (a drive root such as `C:\` or any folder). With `prefer_fast`, an
/// NTFS drive root is read through the MFT (admin); otherwise, or if that fails, folders are
/// walked.
pub fn start(root: &str, prefer_fast: bool) -> Scan {
    let sh = Arc::new(Shared::default());
    let (t_sh, t_root) = (sh.clone(), root.to_string());
    let spawned = std::thread::Builder::new()
        .name("space-scan".to_string())
        .spawn(move || run(t_sh, t_root, prefer_fast));
    if spawned.is_err() {
        sh.state().error = Some("could not start the scan thread".to_string());
        sh.done.store(true, Ordering::Release);
    }
    Scan { sh }
}

impl Scan {
    /// Current counters and state; cheap, call it every UI tick.
    pub fn progress(&self) -> Progress {
        let error = self.sh.state().error.clone();
        Progress {
            files: self.sh.files.load(Ordering::Relaxed),
            bytes: self.sh.bytes.load(Ordering::Relaxed),
            done: self.sh.done.load(Ordering::Acquire),
            fast: self.sh.fast.load(Ordering::Relaxed),
            error,
        }
    }

    /// The finished tree, `None` while scanning, after cancel or on failure.
    pub fn result(&self) -> Option<Arc<Tree>> {
        self.sh.state().tree.clone()
    }

    /// Ask the thread to stop; it does within about 100 ms.
    pub fn cancel(&self) {
        self.sh.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Marks the scan done however the thread exits.
struct DoneGuard(Arc<Shared>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::Release);
    }
}

fn run(sh: Arc<Shared>, root: String, prefer_fast: bool) {
    let _guard = DoneGuard(sh.clone());
    let mut result: Option<Tree> = None;
    if prefer_fast {
        if let Some(letter) = drive_letter(&root) {
            sh.fast.store(true, Ordering::Relaxed);
            match sys::scan_mft(letter, &root, &sh) {
                Ok(t) => result = Some(t),
                Err(ScanErr::Cancelled) => return,
                Err(ScanErr::Failed(_)) => {
                    sh.fast.store(false, Ordering::Relaxed);
                    sh.files.store(0, Ordering::Relaxed);
                    sh.bytes.store(0, Ordering::Relaxed);
                }
            }
        }
    }
    if result.is_none() {
        match sys::walk(&root, &sh) {
            Ok(t) => result = Some(t),
            Err(ScanErr::Cancelled) => return,
            Err(ScanErr::Failed(e)) => {
                sh.state().error = Some(e);
                return;
            }
        }
    }
    if let Some(t) = result {
        sh.state().tree = Some(Arc::new(t));
    }
}

// ---------------------------------------------------------------------------------------
// Win32 engines
// ---------------------------------------------------------------------------------------

#[cfg(not(windows))]
mod sys {
    use super::*;

    pub fn scan_mft(_: char, _: &str, _: &Shared) -> Result<Tree, ScanErr> {
        Err(ScanErr::Failed("not windows".to_string()))
    }

    pub fn walk(_: &str, _: &Shared) -> Result<Tree, ScanErr> {
        Err(ScanErr::Failed("not windows".to_string()))
    }
}

#[cfg(windows)]
mod sys {
    use super::*;
    use crate::util::wide;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW,
        FindNextFileW, ReadFile, SetFilePointerEx, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BEGIN, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FIND_FIRST_EX_LARGE_FETCH, OPEN_EXISTING, WIN32_FIND_DATAW,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// `FSCTL_GET_NTFS_VOLUME_DATA` (the Win32_System_Ioctl feature is not enabled).
    const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;
    /// Read size for the `$MFT` stream.
    const CHUNK: usize = 4 << 20;
    const ERROR_NO_MORE_FILES: u32 = 18;

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by CreateFileW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open_volume(letter: char) -> Option<Handle> {
        let path = wide(&format!("\\\\.\\{letter}:"));
        // SAFETY: path is NUL-terminated; no security attributes or template.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Handle(h))
    }

    fn volume_info(h: &Handle) -> Option<VolInfo> {
        let mut out = [0u8; 128];
        let mut ret = 0u32;
        // SAFETY: out is writable for 128 bytes; no input buffer.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                out.as_mut_ptr() as *mut c_void,
                out.len() as u32,
                &mut ret,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || ret < 96 {
            return None;
        }
        parse_vol(&out[..ret as usize])
    }

    /// Read `buf.len()` bytes at `off` (sector aligned). Returns bytes read.
    fn read_at(h: &Handle, off: u64, buf: &mut [u8]) -> Option<usize> {
        // SAFETY: plain seek on a handle we own.
        if unsafe { SetFilePointerEx(h.0, off as i64, std::ptr::null_mut(), FILE_BEGIN) } == 0 {
            return None;
        }
        let mut done = 0usize;
        while done < buf.len() {
            let mut n = 0u32;
            let want = (buf.len() - done).min(1 << 30) as u32;
            // SAFETY: buf[done..] is writable for `want` bytes.
            let ok = unsafe {
                ReadFile(
                    h.0,
                    buf[done..].as_mut_ptr(),
                    want,
                    &mut n,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return if done > 0 { Some(done) } else { None };
            }
            if n == 0 {
                break;
            }
            done += n as usize;
        }
        Some(done)
    }

    fn fail<T>(msg: &str) -> Result<T, ScanErr> {
        Err(ScanErr::Failed(msg.to_string()))
    }

    /// Fast engine: read and parse the raw `$MFT`.
    pub fn scan_mft(letter: char, root: &str, sh: &Shared) -> Result<Tree, ScanErr> {
        let Some(h) = open_volume(letter) else { return fail("cannot open the volume (admin needed)") };
        let Some(vol) = volume_info(&h) else { return fail("not an NTFS volume") };
        let rec_size = vol.record_size as usize;
        let cluster = vol.bytes_per_cluster as u64;

        // Record 0 ($MFT) holds the data runs of the MFT itself.
        let first = (rec_size.max(vol.bytes_per_sector as usize)).div_ceil(vol.bytes_per_sector as usize)
            * vol.bytes_per_sector as usize;
        let mut buf0 = vec![0u8; first];
        match read_at(&h, vol.mft_start_lcn.saturating_mul(cluster), &mut buf0) {
            Some(n) if n >= rec_size => {}
            _ => return fail("cannot read the MFT"),
        }
        if !apply_fixups(&mut buf0[..rec_size]) {
            return fail("bad $MFT record");
        }
        let Some((runs, real)) = mft_runs(&buf0[..rec_size]) else { return fail("no $MFT data runs") };
        let total_len = if vol.mft_valid_len > 0 { vol.mft_valid_len.min(real) } else { real };
        let n_records = total_len / rec_size as u64;
        if n_records == 0 || n_records > MAX_RECORDS {
            return fail("unexpected MFT size");
        }
        let mapped: u64 = runs.iter().map(|r| r.clusters).sum::<u64>().saturating_mul(cluster);
        if mapped < total_len {
            return fail("fragmented $MFT attribute list not supported");
        }

        let align = rec_size.max(cluster as usize);
        let chunk = (CHUNK / align).max(1) * align;
        let mut buf = vec![0u8; chunk];
        let mut table = MftTable::new(n_records as usize);
        let (mut files, mut bytes) = (0u64, 0u64);
        let mut rn: u64 = 0;
        'runs: for run in &runs {
            let run_bytes = run.clusters.saturating_mul(cluster);
            let mut done = 0u64;
            while done < run_bytes {
                if rn >= n_records {
                    break 'runs;
                }
                if sh.cancelled() {
                    return Err(ScanErr::Cancelled);
                }
                let want = (run_bytes - done).min(chunk as u64) as usize;
                let Some(lcn) = run.lcn else {
                    // Sparse hole: no records stored.
                    rn += want as u64 / rec_size as u64;
                    done += want as u64;
                    continue;
                };
                let got = match read_at(&h, lcn.saturating_mul(cluster) + done, &mut buf[..want]) {
                    Some(g) if g >= rec_size => g,
                    _ => return fail("MFT read failed"),
                };
                for rec in buf[..got].chunks_exact_mut(rec_size) {
                    if rn >= n_records {
                        break;
                    }
                    let (f, b) = ingest(&mut table, rn as u32, rec);
                    files += f;
                    bytes += b;
                    rn += 1;
                }
                sh.files.store(files, Ordering::Relaxed);
                sh.bytes.store(bytes, Ordering::Relaxed);
                done += want as u64;
            }
        }
        drop(buf);
        Ok(table.build(5, root))
    }

    /// Fallback engine: recursive `FindFirstFileExW` walk that does not follow reparse points.
    pub fn walk(root: &str, sh: &Shared) -> Result<Tree, ScanErr> {
        let root_dir = root.trim_end_matches(['\\', '/']).to_string();
        let mut b = TreeBuilder::new(root);
        let mut stack: Vec<(u32, String)> = vec![(0, root_dir)];
        let (mut files, mut bytes) = (0u64, 0u64);
        let mut first = true;
        while let Some((id, dir)) = stack.pop() {
            if sh.cancelled() {
                return Err(ScanErr::Cancelled);
            }
            let ok = list_dir(&dir, sh, |name, attrs, size| {
                if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                        let child = b.add_dir(id, name);
                        stack.push((child, format!("{dir}\\{name}")));
                    }
                } else {
                    b.add_files(id, size, 1);
                    files += 1;
                    bytes += size;
                    if files.is_multiple_of(256) {
                        sh.files.store(files, Ordering::Relaxed);
                        sh.bytes.store(bytes, Ordering::Relaxed);
                    }
                }
            });
            match ok {
                Err(ScanErr::Cancelled) => return Err(ScanErr::Cancelled),
                Err(e) if first => return Err(e),
                _ => {}
            }
            first = false;
            sh.files.store(files, Ordering::Relaxed);
            sh.bytes.store(bytes, Ordering::Relaxed);
        }
        Ok(b.finish())
    }

    /// Enumerate one directory, calling `f(name, attributes, size)`. Cancellation is checked
    /// every 256 entries.
    fn list_dir(
        dir: &str,
        sh: &Shared,
        mut f: impl FnMut(&str, u32, u64),
    ) -> Result<(), ScanErr> {
        let prefix = if dir.starts_with("\\\\") { "" } else { "\\\\?\\" };
        let pat = wide(&format!("{prefix}{dir}\\*"));
        // SAFETY: WIN32_FIND_DATAW is plain data; zero is valid.
        let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        // SAFETY: pat is NUL-terminated, fd is writable.
        let h = unsafe {
            FindFirstFileExW(
                pat.as_ptr(),
                FindExInfoBasic,
                &mut fd as *mut _ as *mut c_void,
                FindExSearchNameMatch,
                std::ptr::null(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            // SAFETY: trivial thread-local read.
            let e = unsafe { GetLastError() };
            return fail(&format!("cannot read {dir} (error {e})"));
        }
        let mut count = 0u32;
        let result = loop {
            let n = fd.cFileName.iter().position(|&c| c == 0).unwrap_or(fd.cFileName.len());
            let name = &fd.cFileName[..n];
            let dots = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
            if !dots {
                let size = ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64;
                f(&String::from_utf16_lossy(name), fd.dwFileAttributes, size);
            }
            count += 1;
            if count.is_multiple_of(256) && sh.cancelled() {
                break Err(ScanErr::Cancelled);
            }
            // SAFETY: h is a live find handle, fd is writable.
            if unsafe { FindNextFileW(h, &mut fd) } == 0 {
                // SAFETY: trivial thread-local read.
                let e = unsafe { GetLastError() };
                break if e == ERROR_NO_MORE_FILES { Ok(()) } else { fail("directory read failed") };
            }
        };
        // SAFETY: h came from FindFirstFileExW and is closed once.
        unsafe { FindClose(h) };
        result
    }
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(r: u64, p: u64, name: &str) -> Record {
        Record { file_ref: r, parent_ref: p, name: name.into(), size: 0, is_dir: true }
    }

    fn file(r: u64, p: u64, size: u64) -> Record {
        Record { file_ref: r, parent_ref: p, name: String::new(), size, is_dir: false }
    }

    fn sample() -> Tree {
        // Children listed before their parents on purpose; refs carry a sequence number.
        let seq = 7u64 << 48;
        Tree::from_records(
            "C:\\",
            5,
            &[
                file(20, 11, 1000),
                file(21 | seq, 11, 500),
                dir(11, 10, "bob"),
                file(22, 12, 5000),
                dir(10 | seq, 5, "Users"),
                dir(12, 5 | seq, "Windows"),
                file(23, 5, 100),
                dir(30, 99, "orphan"),
                file(31, 30, 9999),
            ],
        )
    }

    #[test]
    fn totals_and_order() {
        let t = sample();
        assert_eq!(t.total(), 6600);
        assert_eq!(t.total_files(), 4);
        assert_eq!(t.folders(), 4);
        let top = t.top(None, 10);
        let names: Vec<_> = top.iter().map(|e| (e.name.as_str(), e.bytes, e.is_files_here)).collect();
        assert_eq!(
            names,
            vec![("Windows", 5000, false), ("Users", 1500, false), ("(files here)", 100, true)]
        );
        assert_eq!(top[1].files, 2);
        assert_eq!(top[2].files, 1);
    }

    #[test]
    fn top_truncates_and_descends() {
        let t = sample();
        assert_eq!(t.top(None, 1).len(), 1);
        assert_eq!(t.top(None, 1)[0].name, "Windows");
        assert!(t.top(None, 0).is_empty());
        let users = t.top(None, 10)[1].id;
        let kids = t.top(Some(users), 10);
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "bob");
        assert_eq!(kids[0].bytes, 1500);
        let bob = kids[0].id;
        let here = t.top(Some(bob), 10);
        assert_eq!(here.len(), 1);
        assert!(here[0].is_files_here);
        assert_eq!((here[0].bytes, here[0].files), (1500, 2));
        assert!(t.top(Some(9999), 5).is_empty());
    }

    #[test]
    fn paths() {
        let t = sample();
        assert_eq!(t.path(0), "C:\\");
        let users = t.top(None, 10)[1].id;
        assert_eq!(t.path(users), "C:\\Users");
        let bob = t.top(Some(users), 1)[0].id;
        assert_eq!(t.path(bob), "C:\\Users\\bob");
        assert_eq!(t.parent(bob), Some(users));
        assert_eq!(t.parent(0), None);
        assert_eq!(t.path(12345), "");
        // A root without trailing backslash still joins with one.
        let t2 = Tree::from_records("D:\\data", 5, &[dir(8, 5, "x")]);
        assert_eq!(t2.path(1), "D:\\data\\x");
    }

    #[test]
    fn extension_sizes_add_to_base() {
        let mut t = MftTable::new(40);
        t.add_dir(5, 5, ".");
        t.add_file(30, 5, 100);
        t.add_ext_size(30, 50);
        t.add_ext_size(31, 7); // extension record seen before its base
        t.add_file(31, 5, 1);
        let tree = t.build(5, "C:\\");
        assert_eq!(tree.total(), 158);
        assert_eq!(tree.total_files(), 2);
    }

    #[test]
    fn empty_and_missing_root() {
        let t = Tree::from_records("C:\\", 5, &[]);
        assert_eq!(t.total(), 0);
        assert!(t.top(None, 5).is_empty());
        assert_eq!(t.folders(), 1);
    }

    #[test]
    fn drive_letters() {
        assert_eq!(drive_letter("C:\\"), Some('C'));
        assert_eq!(drive_letter("d:"), Some('D'));
        assert_eq!(drive_letter("e:/"), Some('E'));
        assert_eq!(drive_letter("C:\\Users"), None);
        assert_eq!(drive_letter("\\\\srv\\share"), None);
    }

    // ---- hand-built MFT records ----

    fn put16(b: &mut [u8], o: usize, v: u16) {
        b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut [u8], o: usize, v: u64) {
        b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn name_attr(parent: u64, name: &str, ns: u8) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let vlen = 66 + units.len() * 2;
        let alen = (24 + vlen).div_ceil(8) * 8;
        let mut a = vec![0u8; alen];
        put32(&mut a, 0, 0x30);
        put32(&mut a, 4, alen as u32);
        put32(&mut a, 16, vlen as u32);
        put16(&mut a, 20, 24);
        put64(&mut a, 24, parent);
        a[24 + 64] = units.len() as u8;
        a[24 + 65] = ns;
        for (i, u) in units.iter().enumerate() {
            put16(&mut a, 24 + 66 + 2 * i, *u);
        }
        a
    }

    fn data_nonres(vcn: u64, real: u64, runs: &[u8]) -> Vec<u8> {
        let alen = (64 + runs.len()).div_ceil(8) * 8;
        let mut a = vec![0u8; alen];
        put32(&mut a, 0, 0x80);
        put32(&mut a, 4, alen as u32);
        a[8] = 1;
        put16(&mut a, 10, 64);
        put64(&mut a, 16, vcn);
        put16(&mut a, 32, 64);
        put64(&mut a, 40, real.div_ceil(4096) * 4096);
        put64(&mut a, 48, real);
        put64(&mut a, 56, real);
        a[64..64 + runs.len()].copy_from_slice(runs);
        a
    }

    fn data_res(len: u32) -> Vec<u8> {
        let mut a = vec![0u8; 24 + len as usize];
        a.resize(a.len().div_ceil(8) * 8, 0);
        put32(&mut a, 0, 0x80);
        let total = a.len() as u32;
        put32(&mut a, 4, total);
        put32(&mut a, 16, len);
        put16(&mut a, 20, 24);
        a
    }

    /// 1024-byte FILE record with update sequence protection applied.
    fn build_record(flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[..4].copy_from_slice(b"FILE");
        put16(&mut r, 4, 0x30);
        put16(&mut r, 6, 3);
        put16(&mut r, 20, 0x38);
        put16(&mut r, 22, flags);
        put64(&mut r, 32, base);
        let mut p = 0x38;
        for a in attrs {
            r[p..p + a.len()].copy_from_slice(a);
            p += a.len();
        }
        put32(&mut r, p, 0xFFFF_FFFF);
        put32(&mut r, 24, (p + 8) as u32);
        put32(&mut r, 28, 1024);
        // Protect: save sector tails in the array, stamp the USN.
        let (t1, t2) = ([r[510], r[511]], [r[1022], r[1023]]);
        put16(&mut r, 0x30, 0xBEEF);
        r[0x32..0x34].copy_from_slice(&t1);
        r[0x34..0x36].copy_from_slice(&t2);
        put16(&mut r, 510, 0xBEEF);
        put16(&mut r, 1022, 0xBEEF);
        r
    }

    #[test]
    fn fixups_restore_sector_tails() {
        let mut r = build_record(1, 0, &[]);
        // Pretend the original sector tails held data.
        r[0x32] = 0x11;
        r[0x33] = 0x22;
        r[0x34] = 0x33;
        r[0x35] = 0x44;
        assert!(apply_fixups(&mut r));
        assert_eq!(&r[510..512], &[0x11, 0x22]);
        assert_eq!(&r[1022..1024], &[0x33, 0x44]);
    }

    #[test]
    fn fixups_reject_torn_records() {
        let mut r = build_record(1, 0, &[]);
        r[1022] = 0; // sector tail no longer matches the USN
        assert!(!apply_fixups(&mut r));
        let mut bad = build_record(1, 0, &[]);
        bad[0] = b'B';
        assert!(!apply_fixups(&mut bad));
        let mut short = vec![0u8; 20];
        assert!(!apply_fixups(&mut short));
    }

    #[test]
    fn parse_file_record_prefers_win32_name() {
        let mut r = build_record(
            1,
            0,
            &[
                name_attr(0x0005_0000_0000_0042, "PROGRA~1", 2),
                name_attr(0x0005_0000_0000_0042, "Program Files", 1),
                data_nonres(0, 123_456, &[0x11, 0x20, 0x05, 0]),
            ],
        );
        let p = parse_record(&mut r).expect("parsed");
        assert!(!p.is_dir);
        assert_eq!(p.base, None);
        assert_eq!(p.parent, Some(0x42));
        assert_eq!(p.data_size, Some(123_456));
        assert_eq!(record_name(&r, p.name.expect("name")), "Program Files");
    }

    #[test]
    fn parse_dir_resident_and_skip_unused() {
        let mut d = build_record(3, 0, &[name_attr(5, "Users", 3)]);
        let p = parse_record(&mut d).expect("dir");
        assert!(p.is_dir);
        assert_eq!(p.data_size, None);
        assert_eq!(p.parent, Some(5));
        let mut small = build_record(1, 0, &[name_attr(5, "a.txt", 1), data_res(77)]);
        assert_eq!(parse_record(&mut small).and_then(|p| p.data_size), Some(77));
        let mut unused = build_record(0, 0, &[name_attr(5, "gone", 1)]);
        assert!(parse_record(&mut unused).is_none());
    }

    #[test]
    fn extension_record_and_later_extents() {
        let mut ext = build_record(1, 0x0001_0000_0000_0030, &[data_nonres(0, 5000, &[0x11, 0x02, 0x10, 0])]);
        let p = parse_record(&mut ext).expect("ext");
        assert_eq!(p.base, Some(0x30));
        assert_eq!(p.data_size, Some(5000));
        // A continuation extent (starting VCN > 0) has no real size.
        let mut cont = build_record(1, 0x30, &[data_nonres(9, 0, &[0x11, 0x02, 0x10, 0])]);
        assert_eq!(parse_record(&mut cont).and_then(|p| p.data_size), None);
    }

    #[test]
    fn ingest_builds_tree() {
        let mut t = MftTable::new(64);
        let mut root = build_record(3, 0, &[name_attr(5, ".", 1)]);
        let mut docs = build_record(3, 0, &[name_attr(5, "Docs", 1)]);
        let mut f1 = build_record(1, 0, &[name_attr(40, "a.bin", 1), data_nonres(0, 2000, &[0x11, 1, 2, 0])]);
        let mut ext = build_record(1, 41, &[data_nonres(0, 300, &[0x11, 1, 2, 0])]);
        let mut f2 = build_record(1, 0, &[name_attr(5, "b.bin", 1), data_res(10)]);
        assert_eq!(ingest(&mut t, 5, &mut root), (0, 0));
        assert_eq!(ingest(&mut t, 40, &mut docs), (0, 0));
        assert_eq!(ingest(&mut t, 41, &mut f1), (1, 2000));
        assert_eq!(ingest(&mut t, 42, &mut ext), (0, 300));
        assert_eq!(ingest(&mut t, 43, &mut f2), (1, 10));
        // The extension record points at base 41.
        let mut e2 = build_record(1, 41, &[data_nonres(0, 300, &[0x11, 1, 2, 0])]);
        let _ = ingest(&mut t, 44, &mut e2);
        let tree = t.build(5, "C:\\");
        assert_eq!(tree.total(), 2000 + 300 + 300 + 10);
        let top = tree.top(None, 5);
        assert_eq!(top[0].name, "Docs");
        assert_eq!(top[0].bytes, 2600);
        assert_eq!(top[1].bytes, 10);
    }

    #[test]
    fn runlists() {
        // 0x11: len 1 byte, offset 1 byte. Run 1: 0x20 clusters at 0x30; run 2: 0x10 at 0x30-0x10.
        assert_eq!(
            parse_runs(&[0x11, 0x20, 0x30, 0x11, 0x10, 0xF0, 0]),
            Some(vec![
                Run { lcn: Some(0x30), clusters: 0x20 },
                Run { lcn: Some(0x20), clusters: 0x10 }
            ])
        );
        // Sparse run (no offset bytes), then a 3-byte offset.
        assert_eq!(
            parse_runs(&[0x01, 0x08, 0x31, 0x02, 0x00, 0x10, 0x00, 0]),
            Some(vec![Run { lcn: None, clusters: 8 }, Run { lcn: Some(0x1000), clusters: 2 }])
        );
        // Truncated run and negative start are rejected.
        assert_eq!(parse_runs(&[0x21, 0x01]), None);
        assert_eq!(parse_runs(&[0x11, 0x01, 0x80, 0]), None);
        assert_eq!(parse_runs(&[0]), Some(vec![]));
    }

    #[test]
    fn mft_record_zero() {
        let mut r = build_record(
            1,
            0,
            &[name_attr(5, "$MFT", 3), data_nonres(0, 3 << 20, &[0x22, 0x00, 0x03, 0x00, 0x0C, 0])],
        );
        assert!(apply_fixups(&mut r));
        let (runs, real) = mft_runs(&r).expect("runs");
        assert_eq!(real, 3 << 20);
        assert_eq!(runs, vec![Run { lcn: Some(0x0C00), clusters: 0x300 }]);
    }

    #[test]
    fn volume_buffer() {
        let mut b = vec![0u8; 128];
        put32(&mut b, 40, 512);
        put32(&mut b, 44, 4096);
        put32(&mut b, 48, 1024);
        put64(&mut b, 56, 600 << 20);
        put64(&mut b, 64, 786_432);
        let v = parse_vol(&b).expect("vol");
        assert_eq!(
            v,
            VolInfo {
                bytes_per_sector: 512,
                bytes_per_cluster: 4096,
                record_size: 1024,
                mft_valid_len: 600 << 20,
                mft_start_lcn: 786_432
            }
        );
        put32(&mut b, 48, 1000);
        assert!(parse_vol(&b).is_none());
        assert!(parse_vol(&b[..60]).is_none());
    }

    #[test]
    fn unix_stub_scan_reports_error() {
        // On non-Windows the engines fail; the scan must finish with an error, never hang.
        if cfg!(windows) {
            return;
        }
        let s = start("C:\\", false);
        for _ in 0..200 {
            if s.progress().done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let p = s.progress();
        assert!(p.done);
        assert!(p.error.is_some());
        assert!(s.result().is_none());
    }
}
