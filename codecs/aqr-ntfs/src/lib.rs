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
//! Compressed streams are told as such, [`aqr_disk::Compressed`]: an LZNT1 attribute's units, and
//! a file of WOF's by its `WofCompressedData`. Left in the rest: resident data (in its record),
//! encrypted streams (their clusters as they are), the system files (records 0 to 15, and those below
//! `$Extend`), directories and their indexes, and what does not read: a record whose fixups do not
//! match, runs that do not decode.

use std::collections::HashMap;
use std::ops::Range;

pub use aqr_disk::File;
use aqr_disk::Image;
use aqr_disk::bytes::{le16, le32, le64};
use aqr_disk::{MAX_DEPTH, utf16_to_utf8};

/// NTFS as a file system of aqr-disk.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "ntfs", starts, files };

/// The records of the system files, and the root directory's and `$Extend`'s.
const SYSTEM: u64 = 16;
const ROOT: u64 = 5;
const EXTEND: u64 = 11;

/// The stride of the fixups: they end every 512 bytes of a record, whatever the sector.
const STRIDE: usize = 512;

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
fn volume(d: &dyn Image) -> Option<Volume> {
    let b = d.get(0..512)?;
    let b = &b[..];
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

/// Whether `d` starts like an NTFS volume (`volume`).
pub fn starts(d: &dyn Image) -> bool {
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
    Runs { vcn: u64, runs: Vec<Run>, data: u64, initialized: u64, unit: u16 },
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
                unit: le16(a, 34),
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
    /// Of a compressed one (not encrypted), its compression unit's clusters (as a power of two).
    lznt1: Option<u16>,
}

/// What the records of a file tell: its name and parent, whether a directory, its streams by name.
#[derive(Default)]
struct Entry {
    name: Option<(u8, Vec<u16>, u64)>,
    directory: bool,
    streams: HashMap<Vec<u16>, Stream>,
    /// The algorithm of WOF's file provider, of a reparse point of WOF's.
    wof: Option<u32>,
    /// The size of the unnamed stream where it is resident.
    resident: Option<u64>,
}

/// The runs of stream `s` in order of their VCNs, up to where one is missing.
fn in_vcn_order(s: &Stream) -> Vec<Run> {
    let mut pieces: Vec<&(u64, Vec<Run>)> = s.pieces.iter().collect();
    pieces.sort_by_key(|p| p.0);
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
    all
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
        aqr_disk::push_extent(&mut out, start..end);
    }
    out
}

/// The files of NTFS volume `d`.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(v) = volume(d) else { return Vec::new() };
    // the MFT's runs, as known: its first record's, then those its extension records add
    let first = |d: &dyn Image, at: u64| -> Option<Vec<u8>> {
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
                // (WOF's reparse point: its tag, WOF_EXTERNAL_INFO of the file provider, then
                // FILE_PROVIDER_EXTERNAL_INFO_V1, its algorithm)
                (0xc0, Body::Resident(p)) if p.len() >= 24 && le32(p, 0) == 0x8000_0017 && le32(p, 12) == 2 => {
                    e.wof = Some(le32(p, 20));
                }
                (0x80, Body::Resident(v)) if a.name.is_empty() => e.resident = Some(v.len() as u64),
                (0x80, Body::Runs { vcn, runs, data, initialized, unit }) => {
                    if base == 0 && a.name.is_empty() && vcn > 0 {
                        // (the MFT's own runs, of an extension record of it)
                        map.extend(vcn, &runs, v.cluster);
                    }
                    let s = e.streams.entry(a.name).or_default();
                    s.packed |= a.flags & 0x4001 != 0;
                    if vcn == 0 {
                        s.sizes = Some((data, initialized));
                        s.lznt1 = (a.flags & 0x4001 == 1 && (1..=8).contains(&unit)).then_some(unit);
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
            Some((_, name, parent)) if n != EXTEND && depth < MAX_DEPTH && *parent != n => {
                path(*parent, entries, paths, depth + 1).map(|mut p| {
                    if !p.is_empty() {
                        p.push(b'/');
                    }
                    p.extend(utf16_to_utf8(name.iter().copied()));
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
    let wof: Vec<u16> = "WofCompressedData".encode_utf16().collect();
    for n in numbers {
        let e = &entries[&n];
        if e.directory {
            continue;
        }
        let Some(p) = path(n, &entries, &mut paths, 0) else { continue };
        // (a compressed stream: its units' clusters allocated, in order, and the bytes of each)
        let units = |s: &Stream| -> Option<(Vec<Range<usize>>, aqr_disk::Compressed)> {
            let ((len, _), unit) = (s.sizes?, s.lznt1?);
            let cu = 1u64 << unit;
            // (units at most: of 64 KiB, a terabyte)
            let count = len.div_ceil(cu.checked_mul(v.cluster)?);
            if count > 1 << 24 {
                return None;
            }
            let mut allocated = vec![0u32; count as usize];
            let mut out: Vec<Range<usize>> = Vec::new();
            let mut vcn = 0u64;
            for (n, lcn) in in_vcn_order(s) {
                // (each unit the run overlaps, its clusters of it)
                let (mut k, end) = (0u64, n.min((count * cu).saturating_sub(vcn)));
                while k < end {
                    let u = (vcn + k) / cu;
                    let m = ((u + 1) * cu - (vcn + k)).min(end - k);
                    if let Some(at) = lcn.and_then(|l| l.checked_add(k)?.checked_mul(v.cluster))
                        && let Ok(at) = usize::try_from(at)
                    {
                        let bytes = (m * v.cluster) as usize;
                        allocated[u as usize] += bytes as u32;
                        aqr_disk::push_extent(&mut out, at..at + bytes);
                    }
                    k += m;
                }
                vcn = vcn.saturating_add(n);
                if vcn >= count * cu {
                    break;
                }
            }
            let unit = u32::try_from(cu * v.cluster).ok()?;
            Some((out, aqr_disk::Compressed::Lznt1 { len, unit, allocated }))
        };
        // (the extents of stream `s`, up to its initialized size)
        let taken = |s: &Stream| -> Option<Vec<Range<usize>>> {
            let (data, initialized) = s.sizes.filter(|_| !s.packed)?;
            Some(extents(&in_vcn_order(s), v.cluster, initialized.min(data)))
        };
        // a file of WOF's: its contents the stream WofCompressedData holds compressed, of the
        // unnamed stream's size
        let len = e.streams.get(&[][..]).and_then(|s| s.sizes).map(|s| s.0).or(e.resident);
        if let (Some(algorithm), Some(s), Some(len)) = (e.wof, e.streams.get(&wof), len)
            && aqr_disk::Compressed::wof_chunk(algorithm).is_some()
            && let Some(extents) = taken(s)
        {
            out.push(File {
                name: p,
                extents,
                rank: 0,
                compressed: Some(aqr_disk::Compressed::Wof { algorithm, len }),
            });
            continue;
        }
        let mut names: Vec<&Vec<u16>> = e.streams.keys().collect();
        names.sort();
        for name in names {
            let s = &e.streams[name];
            let (extents, compressed) = match units(s) {
                Some((extents, how)) => (extents, Some(how)),
                None => match taken(s) {
                    Some(extents) => (extents, None),
                    None => continue,
                },
            };
            let mut full = p.clone();
            if !name.is_empty() {
                full.push(b':');
                full.extend(utf16_to_utf8(name.iter().copied()));
            }
            out.push(File { name: full, extents, rank: 0, compressed });
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
    fn record(&self, d: &dyn Image, i: usize, len: usize) -> Option<Vec<u8>> {
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
                out.extend_from_slice(&d.get(a..b)?);
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
