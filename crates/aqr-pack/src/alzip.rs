//! A tree written as ALZip's archives, ALZ and EGG (the specs of OctopusET, CC0): each file
//! deflated as zlib 1.3.1 does it (aqr-flate's emulation, so that the archive recompresses
//! whole), or stored where that is no smaller; the files of a batch deflated on the pool's
//! threads, then written in order. Neither is tested against ALZip or Bandizip (only against
//! our own reader).
//!
//! - ALZ: the header (version 10), a local header and body for each entry, then the central
//!   directory's 16 bytes and the end record. Sizes are of the narrowest width (1, 2, 4 or 8
//!   bytes) that holds them; directories have none. Names as ALZip writes them, CP949, where
//!   every name can be; else all in UTF-8 (the reader takes UTF-8 where every name is). `/`
//!   separates names. Times are MS-DOS's (local time, 2-second steps); the attribute byte is
//!   Windows' (read-only, hidden, system, archive; 0x10 a directory). No comment section.
//! - EGG: the header with a random id, no prefix records (not solid, not split), a file header
//!   with a UTF-8 filename and Windows file information (the modification time as a FILETIME,
//!   attributes of EGG's own bits: read-only, hidden, system, 0x80 a directory) for each entry,
//!   and a block for each file (one more for each GiB of a larger one, as a block's sizes are
//!   32 bits), then the final end marker. No POSIX file information (modes, owners are lost).
//! - Left out, told: symbolic links (neither format is known to store their targets), FIFOs and
//!   devices, extended attributes and forks. No encryption.

use std::io::{self, Write};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use aqr_common::{crc32, pool};
use aqr_flate::zlib;

use crate::{Bytes, Contents, Format, Item, Keeps, Kind, Options, Progress, Tell, Tree, Written, batches, items, time};

/// The attributes of Windows kept by ALZ (read-only, hidden, system, archive), and its
/// directory's.
const ALZ_KEPT: u32 = 0x27;
const ALZ_DIR: u8 = 0x10;
/// The same in EGG: its own bits (read-only, hidden, system), and a directory's.
const EGG_KEPT: u32 = 0x07;
const EGG_DIR: u8 = 0x80;

/// ALZ's methods.
const ALZ_STORED: u8 = 0;
const ALZ_DEFLATE: u8 = 2;
/// EGG's.
const EGG_STORED: u8 = 0;
const EGG_DEFLATE: u8 = 1;

/// The most bytes of a file in an EGG block (its sizes are 32 bits).
const BLOCK: usize = 1 << 30;

/// The data of a piece of a body.
enum Data {
    Stored(Bytes, Range<usize>),
    Deflated(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Stored(b, r) => &b[r.clone()],
            Data::Deflated(v) => v,
        }
    }
}

/// A piece of a file coded on its own: its CRC-32, whether deflated, its size and its data.
struct Piece {
    crc: u32,
    deflated: bool,
    size: u64,
    data: Data,
}

/// The piece of `r` of file data `d`: deflated at `level`, stored where that is not smaller.
fn piece(d: &Bytes, r: Range<usize>, level: u8) -> Piece {
    let s = &d[r.clone()];
    let (crc, size) = (crc32::hash(s), s.len() as u64);
    if level > 0 && !s.is_empty() {
        let p = zlib::Params { level: level.min(9), ..zlib::Params::default() };
        if let Some(v) = zlib::compress(s, p).filter(|v| v.len() < s.len()) {
            return Piece { crc, deflated: true, size, data: Data::Deflated(v) };
        }
    }
    Piece { crc, deflated: false, size, data: Data::Stored(d.clone(), r) }
}

/// The pieces of a file: one for ALZ, one for each `limit` bytes for EGG (none for an empty
/// file there).
fn pieces(d: Bytes, level: u8, limit: usize, empty_one: bool) -> Vec<Piece> {
    if d.is_empty() {
        return if empty_one { vec![piece(&d, 0..0, level)] } else { Vec::new() };
    }
    let n = d.len();
    (0..n).step_by(limit).map(|s| piece(&d, s..(s + limit).min(n), level)).collect()
}

/// The entries of `t` to write as format `name`; the CLI refuses a passphrase, but if one is
/// given here, it is an error.
fn kept(t: &Tree, name: &str, o: &Options, notes: &mut Vec<String>) -> io::Result<Vec<Item>> {
    if o.passphrase.is_some() {
        return Err(io::Error::other(format!("{name}: not encrypted by a passphrase")));
    }
    Ok(items(t, name, Keeps { special: false, xattrs: false, links: false }, notes))
}

/// The bodies of the batch's items, read and coded on the pool's threads (in order), and the
/// progress told.
/// How files are cut and deflated: the zlib level, the most bytes of a piece, and whether an empty
/// file has a piece (of no bytes).
struct Cut {
    level: u8,
    limit: usize,
    empty_one: bool,
}

fn bodies(
    batch: &[Item],
    c: &dyn Contents,
    o: &Options,
    cut: &Cut,
    (done, total): (&AtomicU64, u64),
    tell: &Tell,
) -> Vec<io::Result<Vec<Piece>>> {
    pool::par_map(batch.len(), o.threads, |i| match &batch[i].kind {
        &Kind::File(k) => {
            tell(Progress::Started(k));
            let d = c.read(k)?;
            let p = pieces(d, cut.level, cut.limit, cut.empty_one);
            tell(Progress::Ended(k));
            let n = done.fetch_add(c.size(k), Ordering::Relaxed) + c.size(k);
            tell(Progress::Done { done: n, total });
            Ok(p)
        }
        Kind::Dir => Ok(Vec::new()),
        _ => unreachable!("not kept"),
    })
}

/// Writes tree `t`, the contents of its files read from `c`, as an ALZ archive to `w`, telling
/// `tell` how far it is.
pub fn write_alz(t: &Tree, c: &dyn Contents, o: &Options, w: &mut dyn Write, tell: &Tell) -> io::Result<Written> {
    let mut out = Written::default();
    let its = kept(t, "ALZ", o, &mut out.notes)?;
    let names = alz_names(&its, &mut out.notes);
    let level = o.level_of(Format::Alz);
    let size = |it: &Item| match it.kind {
        Kind::File(k) => c.size(k),
        _ => 0,
    };
    let total: u64 = its.iter().map(size).sum();
    let done = AtomicU64::new(0);
    tell(Progress::Done { done: 0, total });
    let mut at = 0u64;
    let mut put = |w: &mut dyn Write, b: &[u8]| -> io::Result<()> {
        at += b.len() as u64;
        w.write_all(b)
    };
    put(w, b"ALZ\x01\x0a\x00\x00\x00")?;
    for range in batches(&its, o.batch, size) {
        let start = range.start;
        let batch = &its[range];
        let made = bodies(batch, c, o, &Cut { level, limit: usize::MAX, empty_one: true }, (&done, total), tell);
        for (k, p) in made.into_iter().enumerate() {
            let (it, mut p) = (&batch[k], p?);
            let name = &names[start + k];
            if name.len() > 0xffff {
                return Err(io::Error::other(format!(
                    "ALZ: the name of {}: too long",
                    String::from_utf8_lossy(&it.path)
                )));
            }
            let x = &t.entries[it.entry].extra;
            let (date, tm) = x.mtime.map_or((0x21, 0), time::dos);
            let mut h = b"BLZ\x01".to_vec();
            h.extend((name.len() as u16).to_le_bytes());
            let dir = matches!(it.kind, Kind::Dir);
            h.push((x.win_attrs.unwrap_or(0) & ALZ_KEPT) as u8 | if dir { ALZ_DIR } else { 0 });
            h.extend(((date as u32) << 16 | tm as u32).to_le_bytes());
            let body = p.pop();
            match &body {
                None => h.extend([0, 0]),
                Some(b) => {
                    let clen = b.data.bytes().len() as u64;
                    let width: u64 = match clen.max(b.size) {
                        0..=0xff => 1,
                        0x100..=0xffff => 2,
                        0x1_0000..=0xffff_ffff => 4,
                        _ => 8,
                    };
                    h.extend([(width as u8) << 4, 0]);
                    h.extend([if b.deflated { ALZ_DEFLATE } else { ALZ_STORED }, 0]);
                    h.extend(b.crc.to_le_bytes());
                    h.extend(&clen.to_le_bytes()[..width as usize]);
                    h.extend(&b.size.to_le_bytes()[..width as usize]);
                    out.read += b.size;
                }
            }
            h.extend(name);
            put(w, &h)?;
            if let Some(b) = &body {
                put(w, b.data.bytes())?;
            }
        }
    }
    // the central directory (its 12 bytes unknown, 0) and the end record
    put(w, b"CLZ\x01\0\0\0\0\0\0\0\0\0\0\0\0CLZ\x02")?;
    out.len = at;
    Ok(out)
}

/// The names of `its` as ALZ keeps them: CP949 where all can be (the reader takes UTF-8 where
/// all are), else as they are.
fn alz_names(its: &[Item], notes: &mut Vec<String>) -> Vec<Vec<u8>> {
    let as_is = || its.iter().map(|it| it.path.clone()).collect();
    if !its.iter().all(|it| it.utf8) {
        return as_is();
    }
    let mut v = Vec::with_capacity(its.len());
    for it in its {
        let s = std::str::from_utf8(&it.path).expect("UTF-8");
        let (b, _, bad) = encoding_rs::EUC_KR.encode(s);
        if bad {
            notes.push("ALZ: a name not representable in CP949: all names in UTF-8".into());
            return as_is();
        }
        v.push(b.into_owned());
    }
    v
}

/// Writes tree `t`, the contents of its files read from `c`, as an EGG archive to `w`, telling
/// `tell` how far it is.
pub fn write_egg(t: &Tree, c: &dyn Contents, o: &Options, w: &mut dyn Write, tell: &Tell) -> io::Result<Written> {
    const END: [u8; 4] = 0x08E2_8222u32.to_le_bytes();
    let mut out = Written::default();
    let its = kept(t, "EGG", o, &mut out.notes)?;
    let level = o.level_of(Format::Egg);
    let size = |it: &Item| match it.kind {
        Kind::File(k) => c.size(k),
        _ => 0,
    };
    let total: u64 = its.iter().map(size).sum();
    let done = AtomicU64::new(0);
    tell(Progress::Done { done: 0, total });
    let mut at = 0u64;
    let mut put = |w: &mut dyn Write, b: &[u8]| -> io::Result<()> {
        at += b.len() as u64;
        w.write_all(b)
    };
    let id = u32::from_le_bytes(crate::random()?).max(1);
    let mut h = b"EGGA\x00\x01".to_vec();
    h.extend(id.to_le_bytes());
    h.extend([0; 4]);
    h.extend(END);
    put(w, &h)?;
    let mut id = 0u32;
    for range in batches(&its, o.batch, size) {
        let batch = &its[range];
        let made = bodies(batch, c, o, &Cut { level, limit: BLOCK, empty_one: false }, (&done, total), tell);
        for (k, p) in made.into_iter().enumerate() {
            let (it, p) = (&batch[k], p?);
            let x = &t.entries[it.entry].extra;
            let dir = matches!(it.kind, Kind::Dir);
            let len: u64 = p.iter().map(|b| b.size).sum();
            out.read += len;
            let mut h = 0x0A85_90E3u32.to_le_bytes().to_vec();
            h.extend(id.to_le_bytes());
            h.extend(len.to_le_bytes());
            id += 1;
            // the filename: UTF-8 (flags 0), its size 2 bytes, or 4 (flag bit 0) where longer
            h.extend(0x0A85_91ACu32.to_le_bytes());
            if it.path.len() > 0xffff {
                h.push(1);
                h.extend((it.path.len() as u32).to_le_bytes());
            } else {
                h.push(0);
                h.extend((it.path.len() as u16).to_le_bytes());
            }
            h.extend(&it.path);
            // Windows file information: the modification time and the attributes
            h.extend(0x2C86_950Bu32.to_le_bytes());
            h.push(0);
            h.extend(9u16.to_le_bytes());
            h.extend(x.mtime.map_or(0, time::filetime).to_le_bytes());
            h.push((x.win_attrs.unwrap_or(0) & EGG_KEPT) as u8 | if dir { EGG_DIR } else { 0 });
            h.extend(END);
            put(w, &h)?;
            for b in &p {
                let data = b.data.bytes();
                let mut h = 0x02B5_0C13u32.to_le_bytes().to_vec();
                h.extend([if b.deflated { EGG_DEFLATE } else { EGG_STORED }, 0]);
                h.extend((b.size as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend(b.crc.to_le_bytes());
                h.extend(END);
                put(w, &h)?;
                put(w, data)?;
            }
        }
    }
    put(w, &END)?;
    out.len = at;
    Ok(out)
}
