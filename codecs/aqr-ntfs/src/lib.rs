//! NTFS volumes read into their files: a file system of aqr-disk ([`FS`]), which takes the image
//! apart into the contents of the files and the rest (the boot sector, the MFT, the indexes, the
//! log, free clusters).
//!
//! What it reads (from the Linux-NTFS project's documentation of the format; no implementation's
//! code read): the boot sector; the MFT, through the runs of its own `$DATA` (those of its
//! extension records too, as they are come to); each record's fixups, its `$FILE_NAME` (the
//! parent and the name: a path made by the parents up to the root) and its non-resident `$DATA`
//! attributes, unnamed and named (alternate data streams, `path:name`), their runs put in order
//! of their VCNs across the base record and its extension records. A stream's contents are its
//! clusters up to its initialized size, holes of a sparse stream left out.
//!
//! Left in the rest: resident data (in its record), compressed and encrypted streams (their
//! clusters as they are; LZNT1 is for later), the system files (records 0 to 15, and those below
//! `$Extend`), directories and their indexes, and what does not read: a record whose fixups do not
//! match, runs that do not decode.

use std::collections::HashMap;
use std::ops::Range;

pub use aqr_disk::File;

/// NTFS as a file system of aqr-disk.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "ntfs", starts, files };

/// Deepest path made, from the root.
const DEPTH: usize = 256;

/// The records of the system files, and the root directory's and `$Extend`'s.
const SYSTEM: u64 = 16;
const ROOT: u64 = 5;
const EXTEND: u64 = 11;

/// The stride of the fixups: they end every 512 bytes of a record, whatever the sector.
const STRIDE: usize = 512;

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

fn le64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

/// What the boot sector tells of a volume.
struct Volume {
    /// Bytes of a cluster and of an MFT record.
    cluster: u64,
    record: usize,
    /// The MFT's first cluster.
    mft: u64,
}

/// The boot sector `d` starts with, where it is NTFS's: its OEM name, the signature, and sizes of
/// the values it allows.
fn volume(d: &[u8]) -> Option<Volume> {
    let b = d.get(..512)?;
    if &b[3..11] != b"NTFS    " || b[510..512] != [0x55, 0xaa] {
        return None;
    }
    let bps = le16(b, 11) as u64;
    let spc = match b[13] {
        n @ 1..=0x80 => n as u64,
        n @ 0xf4..=0xff => 1 << (256 - n as u32),
        _ => return None,
    };
    if !matches!(bps, 512 | 1024 | 2048 | 4096) || !spc.is_power_of_two() {
        return None;
    }
    let cluster = bps * spc;
    let record = match b[64] as i8 {
        n @ 1.. => cluster.checked_mul(n as u64)?,
        n @ -31..=-1 => 1 << -(n as i32),
        _ => return None,
    };
    if !record.is_power_of_two() || !(512..=65536).contains(&record) || cluster > 2 << 20 {
        return None;
    }
    Some(Volume { cluster, record: record as usize, mft: le64(b, 48) })
}

/// Whether `d` starts like an NTFS volume ([`volume`]).
pub fn starts(d: &[u8]) -> bool {
    volume(d).is_some()
}

/// A run of clusters: its length in clusters and its first cluster, None of a hole.
type Run = (u64, Option<u64>);

/// The runs of mapping pairs `m`, as far as they decode.
fn runs(m: &[u8]) -> Vec<Run> {
    let mut out = Vec::new();
    let (mut p, mut lcn) = (0usize, 0i64);
    while let Some(&h) = m.get(p) {
        let (ls, os) = ((h & 15) as usize, (h >> 4) as usize);
        if h == 0 || !(1..=8).contains(&ls) || os > 8 {
            break;
        }
        let Some(f) = m.get(p + 1..p + 1 + ls + os) else { break };
        let len = f[..ls].iter().rev().fold(0u64, |v, &b| v << 8 | b as u64);
        if os == 0 {
            out.push((len, None));
        } else {
            // (signed, of the run before)
            let raw = f[ls..].iter().rev().fold(0u64, |v, &b| v << 8 | b as u64);
            let shift = 64 - 8 * os as u32;
            let delta = ((raw << shift) as i64) >> shift;
            lcn = lcn.wrapping_add(delta);
            out.push((len, u64::try_from(lcn).ok()));
        }
        p += 1 + ls + os;
    }
    out
}

/// An attribute of a record: its type, name (UTF-16), flags, and its value (resident) or its
/// first VCN, runs and sizes (allocated, data, initialized: of the first piece only).
struct Attr<'a> {
    kind: u32,
    name: Vec<u16>,
    flags: u16,
    body: Body<'a>,
}

enum Body<'a> {
    Resident(&'a [u8]),
    Runs { vcn: u64, runs: Vec<Run>, data: u64, initialized: u64 },
}

/// The attributes of record `r` (its fixups applied), as far as they read.
fn attrs(r: &[u8]) -> Vec<Attr<'_>> {
    let mut out = Vec::new();
    let used = (le32(r, 24) as usize).min(r.len());
    let mut at = le16(r, 20) as usize;
    while at + 16 <= used {
        let a = &r[at..];
        let (kind, len) = (le32(a, 0), le32(a, 4) as usize);
        if kind == 0xffff_ffff || len < 16 || at + len > used {
            break;
        }
        let a = &a[..len];
        let (nlen, noff) = (a[9] as usize, le16(a, 10) as usize);
        let name = a.get(noff..noff + 2 * nlen).map(units);
        let body = match a[8] {
            0 if len >= 24 => {
                let (vlen, voff) = (le32(a, 16) as usize, le16(a, 20) as usize);
                a.get(voff..voff.saturating_add(vlen)).map(Body::Resident)
            }
            1 if len >= 64 => a.get(le16(a, 32) as usize..).map(|m| Body::Runs {
                vcn: le64(a, 16),
                runs: runs(m),
                data: le64(a, 48),
                initialized: le64(a, 56),
            }),
            _ => None,
        };
        if let (Some(name), Some(body)) = (name, body) {
            out.push(Attr { kind, name, flags: le16(a, 12), body });
        }
        at += len;
    }
    out
}

/// Record `r` with its fixups applied, where they match: the update sequence's number at the end
/// of every 512 bytes, put back from the array.
fn fixed(mut r: Vec<u8>) -> Option<Vec<u8>> {
    if &r[..4] != b"FILE" {
        return None;
    }
    let (at, count) = (le16(&r, 4) as usize, le16(&r, 6) as usize);
    if count != r.len() / STRIDE + 1 || at + 2 * count > r.len() {
        return None;
    }
    let usn = le16(&r, at);
    for i in 1..count {
        let p = i * STRIDE - 2;
        if le16(&r, p) != usn {
            return None;
        }
        r[p] = r[at + 2 * i];
        r[p + 1] = r[at + 2 * i + 1];
    }
    Some(r)
}

/// The pieces of a stream of a file, as its records give them: of each, the first VCN and runs,
/// and the sizes of the first; whether it is compressed or encrypted.
#[derive(Default)]
struct Stream {
    pieces: Vec<(u64, Vec<Run>)>,
    sizes: Option<(u64, u64)>,
    packed: bool,
}

/// What the records of a file tell: its name and parent, whether a directory, its streams by name.
#[derive(Default)]
struct Entry {
    name: Option<(u8, Vec<u16>, u64)>,
    directory: bool,
    streams: HashMap<Vec<u16>, Stream>,
}

/// The extents of `runs` (in order) up to `len` bytes of clusters of `cluster` bytes, holes left
/// out (their bytes counted).
fn extents(runs: &[Run], cluster: u64, len: u64) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut at = 0u64;
    for &(n, lcn) in runs {
        if at >= len {
            break;
        }
        let bytes = n.saturating_mul(cluster).min(len - at);
        at += bytes;
        let Some(start) = lcn.and_then(|l| l.checked_mul(cluster)).and_then(|s| usize::try_from(s).ok()) else {
            continue;
        };
        let end = start.saturating_add(usize::try_from(bytes).unwrap_or(usize::MAX));
        match out.last_mut() {
            Some(r) if r.end == start => r.end = end,
            _ => out.push(start..end),
        }
    }
    out
}

/// The files of NTFS volume `d`.
pub fn files(d: &[u8]) -> Vec<File> {
    let Some(v) = volume(d) else { return Vec::new() };
    // the MFT's runs, as known: its first record's, then those its extension records add
    let first = |d: &[u8], at: u64| -> Option<Vec<u8>> {
        let s = usize::try_from(at.checked_mul(v.cluster)?).ok()?;
        fixed(d.get(s..s.checked_add(v.record)?)?.to_vec())
    };
    let Some(zero) = first(d, v.mft) else { return Vec::new() };
    let mut mft: Vec<(u64, Vec<Run>)> = Vec::new();
    let mut size = 0;
    for a in attrs(&zero) {
        if let (0x80, true, Body::Runs { vcn, runs, data, .. }) = (a.kind, a.name.is_empty(), &a.body) {
            mft.push((*vcn, runs.clone()));
            size = size.max(*data);
        }
    }
    // (records, at most as the volume could hold)
    let count = (size / v.record as u64).min(d.len() as u64 / v.record as u64) as usize;
    let mut map = MftMap::of(&mft, v.cluster);
    drop(mft);
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    let mut i = 0;
    while i < count {
        let Some(r) = map.record(d, i, v.record).and_then(fixed) else {
            i += 1;
            continue;
        };
        let flags = le16(&r, 22);
        if flags & 1 == 0 {
            i += 1;
            continue;
        }
        // (the base record's reference, of its sequence number too: none of a base record, and
        // record 0, the MFT's, of an extension record of the MFT)
        let base = match le64(&r, 32) {
            0 => i as u64,
            b => b & 0xffff_ffff_ffff,
        };
        let e = entries.entry(base).or_default();
        e.directory |= base == i as u64 && flags & 2 != 0;
        for a in attrs(&r) {
            match (a.kind, a.body) {
                (0x30, Body::Resident(f)) if f.len() >= 66 => {
                    let (n, space) = (f[64] as usize, f[65]);
                    let Some(name) = f.get(66..66 + 2 * n) else { continue };
                    // (a DOS name only where there is no other)
                    let better = e.name.as_ref().is_none_or(|(s, ..)| *s == 2 && space != 2);
                    if better {
                        e.name = Some((space, units(name), le64(f, 0) & 0xffff_ffff_ffff));
                    }
                }
                (0x80, Body::Runs { vcn, runs, data, initialized }) => {
                    if base == 0 && a.name.is_empty() && vcn > 0 {
                        // (the MFT's own runs, of an extension record of it)
                        map.extend(vcn, &runs, v.cluster);
                    }
                    let s = e.streams.entry(a.name).or_default();
                    s.packed |= a.flags & 0x4001 != 0;
                    if vcn == 0 {
                        s.sizes = Some((data, initialized));
                    }
                    s.pieces.push((vcn, runs));
                }
                _ => {}
            }
        }
        i += 1;
    }
    // the paths, by the parents up to the root
    let mut paths: HashMap<u64, Option<Vec<u8>>> = HashMap::new();
    fn path(
        n: u64,
        entries: &HashMap<u64, Entry>,
        paths: &mut HashMap<u64, Option<Vec<u8>>>,
        depth: usize,
    ) -> Option<Vec<u8>> {
        if n == ROOT {
            return Some(Vec::new());
        }
        if let Some(p) = paths.get(&n) {
            return p.clone();
        }
        let out = match entries.get(&n).and_then(|e| e.name.as_ref()) {
            // (below $Extend, or of no way to the root: none)
            Some((_, name, parent)) if n != EXTEND && depth < DEPTH && *parent != n => {
                path(*parent, entries, paths, depth + 1).map(|mut p| {
                    if !p.is_empty() {
                        p.push(b'/');
                    }
                    p.extend(utf16(name));
                    p
                })
            }
            _ => None,
        };
        paths.insert(n, out.clone());
        out
    }
    let mut numbers: Vec<u64> = entries.keys().copied().filter(|&n| n >= SYSTEM).collect();
    numbers.sort_unstable();
    let mut out = Vec::new();
    for n in numbers {
        let e = &entries[&n];
        if e.directory {
            continue;
        }
        let Some(p) = path(n, &entries, &mut paths, 0) else { continue };
        let mut names: Vec<&Vec<u16>> = e.streams.keys().collect();
        names.sort();
        for name in names {
            let s = &e.streams[name];
            let Some((data, initialized)) = s.sizes.filter(|_| !s.packed) else { continue };
            let mut pieces: Vec<&(u64, Vec<Run>)> = s.pieces.iter().collect();
            pieces.sort_by_key(|p| p.0);
            // (the runs in order of their VCNs, up to where one is missing)
            let mut all: Vec<Run> = Vec::new();
            let mut vcn = 0u64;
            for (start, runs) in pieces {
                if *start != vcn {
                    break;
                }
                for &r in runs {
                    vcn = vcn.saturating_add(r.0);
                    all.push(r);
                }
            }
            let mut full = p.clone();
            if !name.is_empty() {
                full.push(b':');
                full.extend(utf16(name));
            }
            out.push(File { name: full, extents: extents(&all, v.cluster, initialized.min(data)), rank: 0 });
        }
    }
    out
}

/// Where the MFT's bytes are: its runs in order, the bytes of each and where they start in the
/// image (None of a hole).
struct MftMap {
    spans: Vec<(u64, Option<u64>)>,
    /// The VCN after the last run.
    end: u64,
}

impl MftMap {
    fn of(pieces: &[(u64, Vec<Run>)], cluster: u64) -> MftMap {
        let mut pieces: Vec<&(u64, Vec<Run>)> = pieces.iter().collect();
        pieces.sort_by_key(|p| p.0);
        let mut spans = Vec::new();
        let mut vcn = 0u64;
        for (start, runs) in pieces {
            if *start != vcn {
                break;
            }
            for &(n, lcn) in runs {
                vcn = vcn.saturating_add(n);
                spans.push((n.saturating_mul(cluster), lcn.and_then(|l| l.checked_mul(cluster))));
            }
        }
        MftMap { spans, end: vcn }
    }

    /// The runs of the piece from `vcn` added, where it starts where the runs end.
    fn extend(&mut self, vcn: u64, runs: &[Run], cluster: u64) {
        if vcn != self.end {
            return;
        }
        for &(n, lcn) in runs {
            self.end = self.end.saturating_add(n);
            self.spans.push((n.saturating_mul(cluster), lcn.and_then(|l| l.checked_mul(cluster))));
        }
    }

    /// The bytes of record `i` of `len` bytes, where they are within the runs and `d`.
    fn record(&self, d: &[u8], i: usize, len: usize) -> Option<Vec<u8>> {
        let (mut at, mut left) = ((i as u64).checked_mul(len as u64)?, len as u64);
        let mut out = Vec::with_capacity(len);
        let mut start = 0u64;
        for &(n, lcn) in &self.spans {
            if left == 0 {
                break;
            }
            if at < start.saturating_add(n) {
                let from = lcn?.checked_add(at - start)?;
                let take = left.min(start + n - at);
                let (a, b) = (usize::try_from(from).ok()?, usize::try_from(from.checked_add(take)?).ok()?);
                out.extend_from_slice(d.get(a..b)?);
                (at, left) = (at + take, left - take);
            }
            start = start.saturating_add(n);
        }
        (left == 0).then_some(out)
    }
}

/// The UTF-16 code units of `b`.
fn units(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect()
}

/// A name of UTF-16 code units as UTF-8, unpaired surrogates replaced.
fn utf16(units: &[u16]) -> Vec<u8> {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>()
        .into_bytes()
}
