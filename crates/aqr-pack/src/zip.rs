//! A tree written as a ZIP archive (APPNOTE 6.3): each file deflated as zlib 1.3.1 does it
//! (aqr-flate's emulation, so that the archive recompresses whole), or stored where that is no
//! smaller; the files of a batch deflated on the pool's threads, then written in order.
//!
//! - Names are UTF-8, flagged so (bit 11) where they are not ASCII; a name of other bytes is
//!   written as it is, unflagged.
//! - Directories are entries of their own (`name/`); a symbolic link is a stored entry of its
//!   target, of Unix mode `S_IFLNK`, as Info-ZIP writes it. Hard links are files each.
//! - Times: the modification time as MS-DOS's (local time), and the modification, access and
//!   creation times in the extended timestamp (0x5455, seconds) and NTFS (0x000a, 100 ns)
//!   fields. The Unix mode, where it is known, in the external attributes (made by Unix);
//!   Windows' attributes (read-only, hidden, system, archive) in their low byte.
//! - Zip64 where a size, an offset or the number of entries needs it.
//! - Left out, told: FIFOs and devices, extended attributes and forks.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use aqr_common::{crc32, pool};
use aqr_flate::zlib;

use crate::{Bytes, Contents, EntryNode, Extra, Progress, Tree, Written, path_of, time};

#[derive(Debug, Clone)]
pub struct Options {
    /// zlib's level: 0 stores every file, 1 to 9 deflate (6 is zlib's and Info-ZIP's default).
    pub level: u8,
    pub threads: usize,
    /// About the most bytes of files read and deflated at once (one larger alone).
    pub batch: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options { level: 6, threads: 1, batch: 512 << 20 }
    }
}

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const END64: u32 = 0x0606_4b50;
const LOCATOR64: u32 = 0x0706_4b50;

/// Version 6.3 (made by), and what reading takes: 2.0, 4.5 with Zip64.
const MADE_BY: u16 = 63;
const NEEDS: u16 = 20;
const NEEDS_ZIP64: u16 = 45;
/// Hosts (of "version made by").
const HOST_DOS: u16 = 0;
const HOST_UNIX: u16 = 3;
/// Bit 11 of the flags: the name is UTF-8.
const UTF8: u16 = 0x800;
const STORED: u16 = 0;
const DEFLATED: u16 = 8;

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
/// MS-DOS's attribute of a directory, and those of Windows kept (read-only, hidden, system,
/// archive).
const DOS_DIR: u32 = 0x10;
const DOS_KEPT: u32 = 0x27;

/// An entry to write: its path, whether it is UTF-8, its index in the tree and what it is.
struct Item {
    path: Vec<u8>,
    utf8: bool,
    entry: usize,
    kind: Kind,
}

enum Kind {
    Dir,
    File(usize),
    Link(Vec<u8>),
}

/// The body of an entry: its CRC-32, method, size and the data written.
struct Body {
    crc: u32,
    method: u16,
    size: u64,
    data: Data,
}

enum Data {
    Read(Bytes),
    Deflated(Vec<u8>),
    Of(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Read(b) => b,
            Data::Deflated(v) | Data::Of(v) => v,
        }
    }
}

/// What the central directory repeats of an entry written.
struct Written1 {
    item: usize,
    body: (u32, u16, u64, u64),
    offset: u64,
}

/// Writes tree `t`, the contents of its files read from `c`, as a ZIP archive to `w`, telling
/// `tell` how far it is.
pub fn write(
    t: &Tree,
    c: &dyn Contents,
    o: &Options,
    w: &mut dyn Write,
    tell: &(dyn Fn(Progress) + Sync),
) -> io::Result<Written> {
    let mut out = Written::default();
    let mut items = Vec::new();
    let mut dropped = 0;
    for (i, e) in t.entries.iter().enumerate() {
        let (mut path, utf8) = path_of(t, i);
        let kind = match &e.node {
            EntryNode::Dir => {
                path.push(b'/');
                Kind::Dir
            }
            &EntryNode::File(k) => Kind::File(k),
            EntryNode::Link(to) => Kind::Link(aqr_formula::aqr::name_bytes(to).0.to_vec()),
            EntryNode::Fifo | EntryNode::Device { .. } | EntryNode::Special(_) => {
                out.notes.push(format!("{}: a FIFO or device, not kept in ZIP", String::from_utf8_lossy(&path)));
                continue;
            }
        };
        dropped += (!e.extra.xattrs.is_empty() || !e.extra.forks.is_empty()) as usize;
        items.push(Item { path, utf8, entry: i, kind });
    }
    if dropped > 0 {
        let s = if dropped == 1 { "y" } else { "ies" };
        out.notes.push(format!("the extended attributes and forks of {dropped} entr{s}, not kept in ZIP"));
    }
    let size = |it: &Item| match it.kind {
        Kind::File(k) => c.size(k),
        _ => 0,
    };
    let total: u64 = items.iter().map(size).sum();
    let done = AtomicU64::new(0);
    tell(Progress::Done { done: 0, total });
    let mut at = 0u64;
    let mut written = Vec::with_capacity(items.len());
    let mut start = 0;
    while start < items.len() {
        // (a batch: up to `o.batch` bytes of files, one at least)
        let (mut end, mut bytes) = (start, 0u64);
        while end < items.len() && (end == start || bytes + size(&items[end]) <= o.batch) {
            bytes += size(&items[end]);
            end += 1;
        }
        let batch = &items[start..end];
        let bodies = pool::par_map(batch.len(), o.threads, |i| -> io::Result<Body> {
            match &batch[i].kind {
                Kind::Dir => Ok(Body { crc: 0, method: STORED, size: 0, data: Data::Of(Vec::new()) }),
                Kind::Link(to) => {
                    let data = Data::Of(to.clone());
                    Ok(Body { crc: crc32::hash(to), method: STORED, size: to.len() as u64, data })
                }
                &Kind::File(k) => {
                    tell(Progress::Started(k));
                    let d = c.read(k)?;
                    let b = deflate(d, o.level);
                    tell(Progress::Ended(k));
                    let n = done.fetch_add(c.size(k), Ordering::Relaxed) + c.size(k);
                    tell(Progress::Done { done: n, total });
                    Ok(b)
                }
            }
        });
        for (k, b) in bodies.into_iter().enumerate() {
            let b = b?;
            let it = &batch[k];
            out.read += b.size;
            let header = local(it, &t.entries[it.entry].extra, &b);
            w.write_all(&header)?;
            w.write_all(b.data.bytes())?;
            let clen = b.data.bytes().len() as u64;
            written.push(Written1 { item: start + k, body: (b.crc, b.method, b.size, clen), offset: at });
            at += header.len() as u64 + clen;
        }
        start = end;
    }
    let cd_at = at;
    for wr in &written {
        let it = &items[wr.item];
        let central = central(it, &t.entries[it.entry].extra, wr);
        w.write_all(&central)?;
        at += central.len() as u64;
    }
    let (cd_len, n) = (at - cd_at, written.len() as u64);
    let mut e = Vec::new();
    if n >= 0xffff || cd_len >= 0xffff_ffff || cd_at >= 0xffff_ffff {
        e.extend(END64.to_le_bytes());
        e.extend(44u64.to_le_bytes());
        e.extend((HOST_DOS << 8 | MADE_BY).to_le_bytes());
        e.extend(NEEDS_ZIP64.to_le_bytes());
        e.extend([0; 8]);
        for v in [n, n, cd_len, cd_at] {
            e.extend(v.to_le_bytes());
        }
        e.extend(LOCATOR64.to_le_bytes());
        e.extend(0u32.to_le_bytes());
        e.extend(at.to_le_bytes());
        e.extend(1u32.to_le_bytes());
    }
    e.extend(END.to_le_bytes());
    e.extend([0; 4]);
    let n16 = n.min(0xffff) as u16;
    e.extend(n16.to_le_bytes());
    e.extend(n16.to_le_bytes());
    e.extend((cd_len.min(0xffff_ffff) as u32).to_le_bytes());
    e.extend((cd_at.min(0xffff_ffff) as u32).to_le_bytes());
    e.extend(0u16.to_le_bytes());
    w.write_all(&e)?;
    out.len = at + e.len() as u64;
    Ok(out)
}

/// The body of file data `d`: deflated at `level`, stored where that is not smaller.
fn deflate(d: Bytes, level: u8) -> Body {
    let (crc, size) = (crc32::hash(&d), d.len() as u64);
    if level > 0 && !d.is_empty() {
        let p = zlib::Params { level: level.min(9), ..zlib::Params::default() };
        if let Some(v) = zlib::compress(&d, p).filter(|v| v.len() < d.len()) {
            return Body { crc, method: DEFLATED, size, data: Data::Deflated(v) };
        }
    }
    Body { crc, method: STORED, size, data: Data::Read(d) }
}

/// The host, and the external attributes, of item `it` of metadata `x`.
fn attributes(it: &Item, x: &Extra) -> (u16, u32) {
    let (dir, link) = (matches!(it.kind, Kind::Dir), matches!(it.kind, Kind::Link(_)));
    let mut dos = x.win_attrs.unwrap_or(0) & DOS_KEPT;
    if dir {
        dos |= DOS_DIR;
    }
    if x.mode.is_none() && !link {
        return (HOST_DOS, dos);
    }
    let perm = x.mode.unwrap_or(0o777) & 0o7777;
    if x.win_attrs.is_none() && perm & 0o222 == 0 {
        dos |= 0x1;
    }
    let kind = if dir {
        S_IFDIR
    } else if link {
        S_IFLNK
    } else {
        S_IFREG
    };
    (HOST_UNIX, (kind | perm) << 16 | dos)
}

/// The fields of times of `x` (as in local headers, or `central` ones): the extended
/// timestamp and NTFS's.
fn times(x: &Extra, central: bool) -> Vec<u8> {
    let x = &*x.instants();
    let mut f = Vec::new();
    let unix = [x.mtime, x.atime, x.crtime].map(|t| t.filter(|t| i32::try_from(t.secs).is_ok()));
    let flags = unix.iter().enumerate().filter(|(_, t)| t.is_some()).fold(0u8, |a, (i, _)| a | 1 << i);
    if flags != 0 {
        // (the central header's has the modification time alone)
        let kept = if central { &unix[..1] } else { &unix[..] };
        let mut d = vec![flags];
        kept.iter().flatten().for_each(|t| d.extend((t.secs as i32).to_le_bytes()));
        field(&mut f, 0x5455, &d);
    }
    if x.mtime.is_some() || x.atime.is_some() || x.crtime.is_some() {
        let mut d = vec![0; 4];
        d.extend(1u16.to_le_bytes());
        d.extend(24u16.to_le_bytes());
        for t in [x.mtime, x.atime, x.crtime] {
            d.extend(t.map_or(0, time::filetime).to_le_bytes());
        }
        field(&mut f, 0x000a, &d);
    }
    f
}

fn field(f: &mut Vec<u8>, id: u16, d: &[u8]) {
    f.extend(id.to_le_bytes());
    f.extend((d.len() as u16).to_le_bytes());
    f.extend_from_slice(d);
}

/// The fields of a header that the two kinds share from "version needed" to the CRC-32.
fn common(h: &mut Vec<u8>, it: &Item, x: &Extra, needs: u16, method: u16, crc: u32) {
    let flags = if it.utf8 && !it.path.is_ascii() { UTF8 } else { 0 };
    let (date, time) = x.mtime.map_or((0x21, 0), time::dos);
    for v in [needs, flags, method, time, date] {
        h.extend(v.to_le_bytes());
    }
    h.extend(crc.to_le_bytes());
}

fn local(it: &Item, x: &Extra, b: &Body) -> Vec<u8> {
    let clen = b.data.bytes().len() as u64;
    let zip64 = b.size >= 0xffff_ffff || clen >= 0xffff_ffff;
    let mut extra = Vec::new();
    if zip64 {
        field(&mut extra, 0x0001, &[b.size.to_le_bytes(), clen.to_le_bytes()].concat());
    }
    extra.extend(times(x, false));
    let mut h = Vec::with_capacity(30 + it.path.len() + extra.len());
    h.extend(LOCAL.to_le_bytes());
    common(&mut h, it, x, if zip64 { NEEDS_ZIP64 } else { NEEDS }, b.method, b.crc);
    let len32 = |n: u64| if zip64 { 0xffff_ffff } else { n as u32 };
    h.extend(len32(clen).to_le_bytes());
    h.extend(len32(b.size).to_le_bytes());
    h.extend((it.path.len() as u16).to_le_bytes());
    h.extend((extra.len() as u16).to_le_bytes());
    h.extend(&it.path);
    h.extend(extra);
    h
}

fn central(it: &Item, x: &Extra, w: &Written1) -> Vec<u8> {
    let (crc, method, size, clen) = w.body;
    let mut z = Vec::new();
    let mut len32 = |n: u64| {
        if n < 0xffff_ffff {
            return n as u32;
        }
        z.extend(n.to_le_bytes());
        0xffff_ffff
    };
    let (size32, clen32, offset32) = (len32(size), len32(clen), len32(w.offset));
    let mut extra = Vec::new();
    if !z.is_empty() {
        field(&mut extra, 0x0001, &z);
    }
    let zip64 = !z.is_empty() || size >= 0xffff_ffff || clen >= 0xffff_ffff;
    extra.extend(times(x, true));
    let (host, attrs) = attributes(it, x);
    let mut h = Vec::with_capacity(46 + it.path.len() + extra.len());
    h.extend(CENTRAL.to_le_bytes());
    h.extend((host << 8 | MADE_BY).to_le_bytes());
    common(&mut h, it, x, if zip64 { NEEDS_ZIP64 } else { NEEDS }, method, crc);
    h.extend(clen32.to_le_bytes());
    h.extend(size32.to_le_bytes());
    for v in [it.path.len() as u16, extra.len() as u16, 0, 0, 0] {
        h.extend(v.to_le_bytes());
    }
    h.extend(attrs.to_le_bytes());
    h.extend(offset32.to_le_bytes());
    h.extend(&it.path);
    h.extend(extra);
    h
}
