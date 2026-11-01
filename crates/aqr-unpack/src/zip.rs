//! ZIP archives read by their central directory (found from the end, past data before the
//! archive): the entries' names (UTF-8 where flagged or given by Info-ZIP's Unicode path field,
//! else of the system's code page where they are not UTF-8), kinds (directories, files, Unix's
//! symbolic links), times (NTFS's field, else Info-ZIP's extended timestamp, else the MS-DOS
//! time as local time), Unix modes and owners, and Windows' attributes; the contents decoded
//! by entry on the pool's threads (stored, DEFLATE, bzip2, LZMA, Zstandard, xz), their CRC-32s
//! checked. Encrypted entries and methods not known are left out (told).

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use aqr_common::{crc32, pool};
use aqr_formula::aqr::{EntryNode, Extra, Reached, Step, WIN_ATTRS_ALLOWED, name_from};
use aqr_zip::zip::{self as z, u16_at, u32_at, u64_at};

use crate::build::{self, Builder};
use crate::{Done, Filter, Sink, codec, stream, sys};

/// Hosts of "version made by" whose names may separate by `\` and whose attributes are MS-DOS's.
fn dos_host(host: u8) -> bool {
    matches!(host, 0 | 11 | 14)
}

/// Hosts whose external attributes hold a Unix mode in their high 16 bits.
fn unix_host(host: u8) -> bool {
    matches!(host, 3 | 19)
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

/// An entry's data to decode: its content, its name (shown), method, body, size and CRC-32.
struct Unit {
    c: usize,
    name: String,
    method: u16,
    body: Range<usize>,
    size: u64,
    crc: u32,
}

pub struct Listing<'d> {
    d: &'d [u8],
    b: Builder,
    units: Vec<Unit>,
    /// The contents of no data to decode (empty files).
    empty: Vec<usize>,
    notes: Vec<String>,
}

/// The blocks of extra field `x`: by id, the data.
fn fields(x: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut p = 0;
    std::iter::from_fn(move || {
        if p + 4 > x.len() {
            return None;
        }
        let (id, n) = (u16_at(x, p), u16_at(x, p + 2) as usize);
        let d = x.get(p + 4..p + 4 + n)?;
        p += 4 + n;
        Some((id, d))
    })
}

/// The times of extra fields `x` (the central directory's and the local header's): NTFS's
/// (modification, access, creation), else Info-ZIP's extended timestamp (of the local header:
/// modification, access, creation).
fn times(x: &mut Extra, central: &[u8], local: &[u8]) {
    for (id, f) in fields(central).chain(fields(local)) {
        match id {
            0x000a if f.len() >= 32 => {
                // (attributes of tag 1, 24 bytes, after 4 reserved)
                let mut p = 4;
                while p + 4 <= f.len() {
                    let (tag, n) = (u16_at(f, p), u16_at(f, p + 2) as usize);
                    if tag == 1 && n >= 24 && p + 4 + 24 <= f.len() {
                        let t = |k: usize| sys::filetime(u64_at(f, p + 4 + 8 * k));
                        (x.mtime, x.atime, x.crtime) = (t(0), t(1), t(2));
                        return;
                    }
                    p += 4 + n;
                }
            }
            _ => {}
        }
    }
    for (id, f) in fields(local).chain(fields(central)) {
        if id == 0x5455 && !f.is_empty() {
            let flags = f[0];
            let mut p = 1;
            let mut next = |bit: u8| {
                if flags & bit == 0 || p + 4 > f.len() {
                    return None;
                }
                p += 4;
                Some(sys::unix(u32_at(f, p - 4) as i32 as i64))
            };
            let (m, a, c) = (next(1), next(2), next(4));
            x.mtime = m.or(x.mtime);
            x.atime = a.or(x.atime);
            x.crtime = c.or(x.crtime);
            return;
        }
    }
}

/// The owner of Info-ZIP's Unix fields (`ux`, else the old `UX`).
fn owner(x: &mut Extra, central: &[u8], local: &[u8]) {
    for (id, f) in fields(local).chain(fields(central)) {
        match id {
            0x7875 if f.len() >= 3 && f[0] == 1 => {
                let le = |b: &[u8]| (b.len() <= 8).then(|| b.iter().rev().fold(0u64, |v, &c| v << 8 | c as u64));
                let n = f[1] as usize;
                let Some(uid) = f.get(2..2 + n).and_then(le) else { continue };
                let Some(&m) = f.get(2 + n) else { continue };
                let Some(gid) = f.get(3 + n..3 + n + m as usize).and_then(le) else { continue };
                (x.uid, x.gid) = (Some(uid), Some(gid));
                return;
            }
            0x7855 if f.len() >= 4 => {
                (x.uid, x.gid) = (Some(u16_at(f, 0) as u64), Some(u16_at(f, 2) as u64));
                return;
            }
            _ => {}
        }
    }
}

/// The name of a central header: UTF-8 where flagged, else that of Info-ZIP's Unicode path
/// field where it is of this name, else [`sys::legacy_name`].
fn entry_name(raw: &[u8], flags: u16, central: &[u8]) -> Vec<u8> {
    if flags & 0x800 != 0 {
        return raw.to_vec();
    }
    for (id, f) in fields(central) {
        if id == 0x7075 && f.len() >= 5 && f[0] == 1 && u32_at(f, 1) == crc32::hash(raw) {
            return f[5..].to_vec();
        }
    }
    sys::legacy_name(raw)
}

impl<'d> Listing<'d> {
    pub fn read(d: &'d [u8]) -> Result<Listing<'d>, String> {
        let (base, cd) = z::find_end(d).ok_or("ZIP: no central directory (not a ZIP archive, or cut short)")?;
        let mut l = Listing { d, b: Builder::default(), units: Vec::new(), empty: Vec::new(), notes: Vec::new() };
        let mut p = cd.start;
        while p < cd.end {
            let c = z::central_at(d, p).ok_or("ZIP: a damaged central directory")?;
            if d[p..p + 4] != z::CENTRAL {
                return Err("ZIP: a damaged central directory".into());
            }
            p = c.comment.end;
            l.entry(&c, base)?;
        }
        Ok(l)
    }

    fn entry(&mut self, c: &z::Central, base: i64) -> Result<(), String> {
        let d = self.d;
        let f = c.fixed(d);
        let (host, flags, method) = ((u16_at(f, 0) >> 8) as u8, u16_at(f, 4), u16_at(f, 6));
        let external = u32_at(f, 34);
        let (offset, info) = c.info(d);
        let raw = &d[c.name.clone()];
        let cx = &d[c.extra.clone()];
        let name = entry_name(raw, flags, cx);
        let shown = String::from_utf8_lossy(&name).into_owned();
        let Some(names) = build::split(&name, dos_host(host)) else {
            self.notes.push(format!("{shown}: a path reaching out of the directory, not extracted"));
            return Ok(());
        };
        // the local header, and the body after it
        let local = usize::try_from((offset as i64).wrapping_add(base))
            .ok()
            .filter(|&at| d.get(at..at + 4) == Some(&z::LOCAL[..]) && at + 30 <= d.len())
            .ok_or_else(|| format!("ZIP: {shown}: no local header where the directory tells"))?;
        let (n, m) = (u16_at(d, local + 26) as usize, u16_at(d, local + 28) as usize);
        let lx = d.get(local + 30 + n..local + 30 + n + m).unwrap_or(&[]);
        let start = local + 30 + n + m;
        let body = usize::try_from(info.csize)
            .ok()
            .and_then(|s| start.checked_add(s))
            .filter(|&e| e <= d.len())
            .map(|e| start..e)
            .ok_or_else(|| format!("ZIP: {shown}: cut short"))?;
        let mode = if unix_host(host) { external >> 16 } else { 0 };
        let dir = name.ends_with(b"/")
            || dos_host(host) && (name.ends_with(b"\\") || external & 0x10 != 0)
            || mode & S_IFMT == S_IFDIR;
        let mut x = Extra { mtime: sys::dos(u16_at(f, 10), u16_at(f, 8)), ..Extra::default() };
        times(&mut x, cx, lx);
        owner(&mut x, cx, lx);
        if mode != 0 {
            x.mode = Some(mode & 0o7777);
        }
        if dos_host(host) && external & WIN_ATTRS_ALLOWED & 0xff != 0 {
            x.win_attrs = Some(external & WIN_ATTRS_ALLOWED & 0xff);
        }
        if dir {
            let r = match names.is_empty() {
                true => self.b.add(&[], None, x),
                false => self.b.add(&names, Some(EntryNode::Dir), x),
            };
            if let Err(e) = r {
                self.notes.push(e);
            }
            return Ok(());
        }
        if flags & 1 != 0 || method == 99 {
            self.notes.push(format!("{shown}: encrypted, not extracted"));
            return Ok(());
        }
        if ![0, 8, 12, 14, 93, 95].contains(&method) {
            let what = match method {
                9 => "Deflate64".into(),
                98 => "PPMd".into(),
                m => format!("method {m}"),
            };
            self.notes.push(format!("{shown}: of {what}, not known here, not extracted"));
            return Ok(());
        }
        let unit = |c| Unit { c, name: shown.clone(), method, body: body.clone(), size: info.usize, crc: info.crc };
        let node = if mode & S_IFMT == S_IFLNK {
            // (the target, the entry's data, read now: the tree has it)
            let target = decode(d, &unit(0)).map_err(|e| format!("ZIP: {shown}: {e}"))?;
            if target.is_empty() {
                self.notes.push(format!("{shown}: a link to nothing, not extracted"));
                return Ok(());
            }
            EntryNode::Link(name_from(&target))
        } else {
            let c = self.b.content();
            match info.usize {
                0 => self.empty.push(c),
                _ => self.units.push(unit(c)),
            }
            EntryNode::File(c)
        };
        if let Err(e) = self.b.add(&names, Some(node), x) {
            self.notes.push(e);
        }
        Ok(())
    }

    pub fn units(&self) -> Vec<String> {
        self.units.iter().map(|u| u.name.clone()).collect()
    }

    pub fn unpack(&self, threads: usize, sink: &dyn Sink) -> Result<Done, String> {
        self.notes.iter().for_each(|n| sink.note(n));
        let tree = self.b.tree.clone();
        let all: Vec<usize> = (0..tree.entries.len()).collect();
        if !all.is_empty() {
            sink.entries(&tree, &all)?;
        }
        for &c in self.empty.iter().filter(|&&c| self.b.live(c)) {
            sink.content(c, Vec::new())?;
        }
        let packed: u64 = self.units.iter().map(|u| u.body.len() as u64).sum();
        let total: u64 = self.units.iter().map(|u| u.size).sum();
        let (done_packed, done) = (AtomicU64::new(0), AtomicU64::new(0));
        let failed = AtomicBool::new(false);
        let made = pool::par_map(self.units.len(), threads, |k| -> Result<(), String> {
            if failed.load(Relaxed) {
                return Ok(());
            }
            let u = &self.units[k];
            // (of a name given again since: left)
            if !self.b.live(u.c) {
                return Ok(());
            }
            sink.step(Step::Started(k));
            let r =
                decode(self.d, u).map_err(|e| format!("ZIP: {}: {e}", u.name)).and_then(|data| sink.content(u.c, data));
            sink.step(Step::Ended(k));
            if r.is_err() {
                failed.store(true, Relaxed);
            }
            let p = done_packed.fetch_add(u.body.len() as u64, Relaxed) + u.body.len() as u64;
            let n = done.fetch_add(u.size, Relaxed) + u.size;
            sink.step(Step::Reached(Reached { decompressed: (p, packed), evaluated: (n, total) }));
            r
        });
        made.into_iter().collect::<Result<(), String>>()?;
        Ok(Done { tree, skipped: self.notes.len() })
    }
}

/// The data of unit `u` of archive `d`, its size and CRC-32 checked.
fn decode(d: &[u8], u: &Unit) -> Result<Vec<u8>, String> {
    let body = &d[u.body.clone()];
    let size = usize::try_from(u.size).map_err(|_| "too large")?;
    let data = match u.method {
        0 if body.len() != size => return Err("not of its size".into()),
        0 => body.to_vec(),
        8 => codec::inflate(body, size, size)?.0,
        12 => codec::bzip2(body, size)?,
        14 => {
            // version (2 bytes), the properties' size (2) and the properties
            let n = body.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or("LZMA: no header")?;
            let props = body.get(4..4 + n).filter(|p| p.len() >= 5).ok_or("LZMA: no header")?;
            codec::lzma(&body[4 + n..], props[0], Some(u.size), size)?.0
        }
        93 => codec::zstd(body, size, size)?,
        95 => {
            let mut out = codec::room(size)?;
            let mut over = false;
            stream::decode(Some(Filter::Xz), body, 1, &mut |v, at| {
                over = out.len() + v.len() - at > size;
                out.extend_from_slice(&v[at..]);
                !over
            })?;
            out
        }
        m => return Err(format!("method {m} not known")),
    };
    if data.len() as u64 != u.size {
        return Err("not of its size".into());
    }
    if crc32::hash(&data) != u.crc {
        return Err("CRC mismatch".into());
    }
    Ok(data)
}
