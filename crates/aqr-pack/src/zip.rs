//! A tree written as a ZIP archive (APPNOTE 6.3): each file deflated as zlib 1.3.1 does it
//! (aqr-flate's emulation, so that the archive recompresses whole), or stored where that is no
//! smaller; the files of a batch deflated on the pool's threads (a large one in chunks, `par`),
//! then written in order.
//!
//! - Names are UTF-8, flagged so (bit 11) where they are not ASCII; a name of other bytes is
//!   written as it is, unflagged.
//! - Directories are entries of their own (`name/`); a symbolic link is a stored entry of its
//!   target, of Unix mode `S_IFLNK`, as Info-ZIP writes it. Hard links are files each.
//! - Times: the modification time as MS-DOS's (local time), and the modification, access and
//!   creation times in the extended timestamp (0x5455, seconds) and NTFS (0x000a, 100 ns)
//!   fields. The Unix mode, where it is known, in the external attributes (made by Unix);
//!   Windows' attributes (read-only, hidden, system, archive) in their low byte. The owner's
//!   ids, where both are known, in Info-ZIP's Unix field (0x7875).
//! - Zip64 where a size, an offset or the number of entries needs it.
//! - With a passphrase, files (not directories nor links) are encrypted by WinZip's AES-256,
//!   AE-2 (no CRC-32 told: the authentication code checks the data), each with a salt of its
//!   own.
//! - Left out, told: FIFOs and devices, extended attributes and forks.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use aqr_common::{crc32, pool};

use crate::par;
use crate::{
    Bytes, Contents, Extra, Format, Item, Keeps, Kind, Options, Progress, Tell, Tree, Written, batches, items,
};
use aqr_foreign::attrs::{DOS_DIR, DOS_KEPT, S_IFDIR, S_IFLNK, S_IFREG};
use aqr_foreign::zip as zipf;
use aqr_foreign::{crypt, time};
use aqr_zip::zip::{self, DEFLATED, ENCRYPTED, STORED};

const LOCAL: u32 = u32::from_le_bytes(zip::LOCAL);
const CENTRAL: u32 = u32::from_le_bytes(zip::CENTRAL);
const END: u32 = u32::from_le_bytes(zip::END);
const END64: u32 = u32::from_le_bytes(zip::END64);
const LOCATOR64: u32 = u32::from_le_bytes(zip::LOCATOR);

/// Version 6.3 (made by), and what reading takes: 2.0, 4.5 with Zip64.
const MADE_BY: u16 = 63;
const NEEDS: u16 = 20;
const NEEDS_ZIP64: u16 = 45;
/// Hosts (of "version made by").
const HOST_DOS: u16 = zipf::HOST_DOS;
const HOST_UNIX: u16 = zipf::HOST_UNIX;
const UTF8: u16 = zipf::UTF8;
/// WinZip's AES: the method, and the version needed (5.1).
const AES: u16 = zipf::METHOD_AES;
const NEEDS_AES: u16 = 51;

/// The body of an entry: its CRC-32, method, size and the data written; for WinZip's AES,
/// the method under it.
struct Body {
    crc: u32,
    method: u16,
    size: u64,
    data: Data,
    aes: Option<u16>,
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
    aes: Option<u16>,
    offset: u64,
}

/// Writes tree `t`, the contents of its files read from `c`, as a ZIP archive to `w`, telling
/// `tell` how far it is.
pub fn write(t: &Tree, c: &dyn Contents, o: &Options, w: &mut dyn Write, tell: &Tell) -> io::Result<Written> {
    let mut out = Written::default();
    let keeps = Keeps { special: false, xattrs: false, links: true };
    let mut items = items(t, "ZIP", keeps, &mut out.notes);
    for it in &mut items {
        if let Kind::Dir = it.kind {
            it.path.push(b'/');
        }
    }
    let level = o.level_of(Format::Zip);
    let size = |it: &Item| it.size(c);
    let total: u64 = items.iter().map(size).sum();
    let done = AtomicU64::new(0);
    tell(Progress::Done { done: 0, total });
    let mut at = 0u64;
    let mut written = Vec::with_capacity(items.len());
    for range in batches(&items, o.batch, size) {
        let start = range.start;
        let batch = &items[range];
        let bodies = pool::par_map(batch.len(), o.threads, |i| -> io::Result<Body> {
            match &batch[i].kind {
                Kind::Dir => Ok(Body { crc: 0, method: STORED, size: 0, data: Data::Of(Vec::new()), aes: None }),
                Kind::Link(to) => {
                    let data = Data::Of(to.clone());
                    Ok(Body { crc: crc32::hash(to), method: STORED, size: to.len() as u64, data, aes: None })
                }
                Kind::Fifo | Kind::Device { .. } => unreachable!("not kept"),
                &Kind::File(k) => {
                    tell(Progress::Started(k));
                    let d = c.read(k)?;
                    let mut b = deflate(d, level, o.threads);
                    if let Some(p) = &o.passphrase {
                        let salt = crate::random()?;
                        let data = Data::Of(crypt::winzip_aes_encrypt(p, &salt, b.data.bytes()));
                        b = Body { crc: 0, method: AES, data, aes: Some(b.method), ..b };
                    }
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
            written.push(Written1 { item: start + k, body: (b.crc, b.method, b.size, clen), aes: b.aes, offset: at });
            at += header.len() as u64 + clen;
        }
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
fn deflate(d: Bytes, level: u8, threads: usize) -> Body {
    let size = d.len() as u64;
    if let Some((v, crc)) = par::deflate_smaller(&d, level, threads) {
        return Body { crc, method: DEFLATED, size, data: Data::Deflated(v), aes: None };
    }
    let crc = crc32::hash(&d);
    Body { crc, method: STORED, size, data: Data::Read(d), aes: None }
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
    // (a local time of an unknown time zone as local time here, as MS-DOS's)
    let [mtime, atime, crtime] = [x.mtime, x.atime, x.crtime].map(|t| t.and_then(time::instant));
    let mut f = Vec::new();
    let unix = [mtime, atime, crtime].map(|t| t.filter(|t| i32::try_from(t.secs).is_ok()));
    let flags = unix.iter().enumerate().filter(|(_, t)| t.is_some()).fold(0u8, |a, (i, _)| a | 1 << i);
    if flags != 0 {
        // (the central header's has the modification time alone)
        let kept = if central { &unix[..1] } else { &unix[..] };
        let mut d = vec![flags];
        kept.iter().flatten().for_each(|t| d.extend((t.secs as i32).to_le_bytes()));
        field(&mut f, zipf::EXT_TIME, &d);
    }
    if mtime.is_some() || atime.is_some() || crtime.is_some() {
        let mut d = vec![0; 4];
        d.extend(1u16.to_le_bytes());
        d.extend(24u16.to_le_bytes());
        for t in [mtime, atime, crtime] {
            d.extend(t.map_or(0, time::to_filetime).to_le_bytes());
        }
        field(&mut f, zipf::NTFS, &d);
    }
    f
}

/// Info-ZIP's Unix field (0x7875) of `x`'s owner, where both its ids are known: version 1, each
/// id of 4 bytes (8 where larger), the same in local and central headers, as Info-ZIP's `zip`
/// writes it.
fn owner(f: &mut Vec<u8>, x: &Extra) {
    let (Some(uid), Some(gid)) = (x.uid, x.gid) else { return };
    let mut d = vec![1];
    for id in [uid, gid] {
        match u32::try_from(id) {
            Ok(id) => {
                d.push(4);
                d.extend(id.to_le_bytes());
            }
            Err(_) => {
                d.push(8);
                d.extend(id.to_le_bytes());
            }
        }
    }
    field(f, zipf::UNIX_NEW, &d);
}

/// WinZip's AES field (0x9901): AE-2, AES-256, the method under it.
fn aes_field(f: &mut Vec<u8>, method: Option<u16>) {
    if let Some(m) = method {
        field(f, zipf::WINZIP_AES, &crypt::winzip_field(m));
    }
}

fn field(f: &mut Vec<u8>, id: u16, d: &[u8]) {
    f.extend(id.to_le_bytes());
    f.extend((d.len() as u16).to_le_bytes());
    f.extend_from_slice(d);
}

/// The fields of a header that the two kinds share from "version needed" to the CRC-32.
fn common(h: &mut Vec<u8>, it: &Item, x: &Extra, needs: u16, method: u16, crc: u32) {
    let flags = if it.utf8 && !it.path.is_ascii() { UTF8 } else { 0 } | if method == AES { ENCRYPTED } else { 0 };
    let needs = if method == AES { needs.max(NEEDS_AES) } else { needs };
    let (date, time) = x.mtime.map_or((0x21, 0), time::to_dos);
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
        field(&mut extra, zipf::ZIP64, &[b.size.to_le_bytes(), clen.to_le_bytes()].concat());
    }
    extra.extend(times(x, false));
    owner(&mut extra, x);
    aes_field(&mut extra, b.aes);
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
        field(&mut extra, zipf::ZIP64, &z);
    }
    let zip64 = !z.is_empty() || size >= 0xffff_ffff || clen >= 0xffff_ffff;
    extra.extend(times(x, true));
    owner(&mut extra, x);
    aes_field(&mut extra, w.aes);
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
