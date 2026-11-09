//! A tree written as a tar archive (POSIX.1-2001's pax interchange format, kept to ustar where
//! that holds an entry, as libarchive's "restricted pax" does), alone or under a compressor
//! ([`crate::filter`]). The files of a batch are read on the pool's threads and written in
//! order; the compressor compresses its parts on them.
//!
//! - Kinds: files, directories (`name/`), symbolic links, hard links (the entries of a link
//!   group after its first: links to its path), FIFOs and devices.
//! - The mode, owner (ids and names) and modification time in the ustar header; a pax header
//!   before it where that does not hold them: a long or non-ASCII path or link (non-UTF-8 ones
//!   under `hdrcharset=BINARY`), a size of 8 GiB or more, large ids, long names of owners, a
//!   time of a fraction of a second or out of ustar's range, extended attributes
//!   (`SCHILY.xattr.`, as GNU tar and libarchive write them).
//! - Not kept, as tar has no room for them: the access, status change and creation times,
//!   Windows' attributes, macOS's flags, forks (told).
//! - The archive ends with two zero blocks, padded to a record of 10,240 bytes as tar writes.

use std::collections::HashMap;
use std::io::{self, Write};

use aqr_common::pool;

use crate::filter::{self, Encoder, Named};
use crate::{
    Bytes, Contents, Extra, Filter, Format, Item, Keeps, Kind, Options, Progress, Tell, Time, Tree, Written, batches,
    items,
};

const BLOCK: usize = 512;
const RECORD: u64 = 10240;

/// Writes tree `t`, the contents of its files read from `c`, as a tar archive under `filter`
/// (none: a tar archive alone) to `w`, telling `tell` how far it is.
pub fn write(
    filter: Option<Filter>,
    t: &Tree,
    c: &dyn Contents,
    o: &Options,
    w: &mut dyn Write,
    tell: &Tell,
) -> io::Result<Written> {
    let mut out = Written::default();
    let keeps = Keeps { special: true, xattrs: true, links: true };
    let items = items(t, "tar", keeps, &mut out.notes);
    let size = |it: &Item| match it.kind {
        Kind::File(k) => c.size(k),
        _ => 0,
    };
    let total: u64 = items.iter().map(size).sum();
    // (about: a header each, a pax header for some, and the bodies' padding)
    let estimate = total + items.len() as u64 * 1024 + RECORD;
    let mut counted = crate::Counted { w, n: 0 };
    let mut enc: Box<dyn Encoder + '_> = match filter {
        None => Box::new(Plain(&mut counted)),
        Some(f) => filter::encoder(f, o.level_of(Format::Tar(Some(f))), o, estimate, Named::default(), &mut counted)?,
    };
    tell(Progress::Done { done: 0, total });
    // the first path of each link group: its other files are links to it
    let mut groups: HashMap<i128, Vec<u8>> = HashMap::new();
    let (mut done, mut at) = (0u64, 0u64);
    for range in batches(&items, o.batch, size) {
        let batch = &items[range];
        let read = pool::par_map(batch.len(), o.threads, |i| match batch[i].kind {
            Kind::File(k) => c.read(k).map(Some),
            _ => Ok(None),
        });
        for (it, d) in batch.iter().zip(read) {
            let d: Option<Bytes> = d?;
            let x = &t.entries[it.entry].extra;
            let hard = match (&it.kind, x.link_group) {
                (Kind::File(_), Some(g)) => match groups.get(&g) {
                    Some(first) => Some(first.clone()),
                    None => {
                        groups.insert(g, it.path.clone());
                        None
                    }
                },
                _ => None,
            };
            let body = if hard.is_some() { 0 } else { d.as_ref().map_or(0, |d| d.len() as u64) };
            let h = header(it, x, hard.as_deref(), body, &mut out.notes);
            enc.write_all(&h)?;
            at += h.len() as u64;
            if let (Kind::File(k), Some(d)) = (&it.kind, &d) {
                tell(Progress::Started(*k));
                if hard.is_none() {
                    enc.write_all(d)?;
                    let pad = d.len().next_multiple_of(BLOCK) - d.len();
                    enc.write_all(&[0; BLOCK][..pad])?;
                    at += (d.len() + pad) as u64;
                }
                out.read += d.len() as u64;
                done += d.len() as u64;
                tell(Progress::Ended(*k));
                tell(Progress::Done { done, total });
            }
        }
    }
    // two zero blocks, then zeros to the end of the record
    let end = (at + 2 * BLOCK as u64).next_multiple_of(RECORD) - at;
    enc.write_all(&vec![0; end as usize])?;
    enc.finish()?;
    out.len = counted.n;
    Ok(out)
}

/// No compressor: the archive as it is written.
struct Plain<'a, 'w>(&'a mut crate::Counted<'w>);

impl Write for Plain<'_, '_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Encoder for Plain<'_, '_> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        Ok(())
    }
}

/// The largest number of `digits` octal digits.
fn octal_max(digits: u32) -> u64 {
    (1u64 << (3 * digits)) - 1
}

/// Writes `v` in octal into `f` (its digits, then a NUL), base-256 where it does not fit.
fn octal(f: &mut [u8], v: u64) {
    let digits = f.len() - 1;
    if v <= octal_max(digits as u32) {
        let s = format!("{v:0digits$o}");
        f[..digits].copy_from_slice(s.as_bytes());
        f[digits] = 0;
    } else {
        // (GNU's base-256: the first byte's high bit, then the number big-endian)
        f.fill(0);
        let n = f.len();
        f[n - 8..].copy_from_slice(&v.to_be_bytes());
        f[0] |= 0x80;
    }
}

/// A pax record `key=value` (with its length before it, counting itself).
fn record(pax: &mut Vec<u8>, key: &[u8], value: &[u8]) {
    let rest = key.len() + value.len() + 3;
    let mut n = rest + 1;
    while n.to_string().len() + rest != n {
        n = n.to_string().len() + rest;
    }
    pax.extend(n.to_string().into_bytes());
    pax.push(b' ');
    pax.extend_from_slice(key);
    pax.push(b'=');
    pax.extend_from_slice(value);
    pax.push(b'\n');
}

/// Time `t` as pax writes it: seconds, and the fraction where it has one.
fn pax_time(t: Time) -> Vec<u8> {
    // (a negative time's fraction counts up from its seconds: -1.25 is secs -2, 0.75)
    let (secs, nanos) =
        if t.secs < 0 && t.nanos > 0 { (t.secs + 1, 1_000_000_000 - t.nanos) } else { (t.secs, t.nanos) };
    let sign = if t.secs < 0 && secs == 0 { "-" } else { "" };
    let mut s = format!("{sign}{secs}");
    if nanos > 0 {
        let f = format!("{nanos:09}");
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
    s.into_bytes()
}

/// Splits `path` into ustar's prefix and name, where it fits them.
fn split_ustar(path: &[u8]) -> Option<(&[u8], &[u8])> {
    if path.len() <= 100 {
        return Some((&[], path));
    }
    // (at a `/` that leaves at most 155 bytes before it and 100 after it)
    let cut = (0..path.len()).rev().find(|&i| path[i] == b'/' && i <= 155 && path.len() - i - 1 <= 100)?;
    (cut > 0 && cut + 1 < path.len()).then(|| (&path[..cut], &path[cut + 1..]))
}

/// The header blocks of item `it` of metadata `x` (a pax header first where needed), a hard
/// link to `hard` where given, of a body of `size` bytes.
fn header(it: &Item, x: &Extra, hard: Option<&[u8]>, size: u64, notes: &mut Vec<String>) -> Vec<u8> {
    let mut path = it.path.clone();
    let (typeflag, link): (u8, &[u8]) = match (&it.kind, hard) {
        (_, Some(to)) => (b'1', to),
        (Kind::File(_), None) => (b'0', &[]),
        (Kind::Dir, _) => {
            path.push(b'/');
            (b'5', &[])
        }
        (Kind::Link(to), _) => (b'2', to),
        (Kind::Fifo, _) => (b'6', &[]),
        (Kind::Device { block, .. }, _) => (if *block { b'4' } else { b'3' }, &[]),
    };
    let (major, minor) = match it.kind {
        Kind::Device { major, minor, .. } => (major, minor),
        _ => (0, 0),
    };
    let mode = x.mode.unwrap_or(match it.kind {
        Kind::Dir => 0o755,
        Kind::Link(_) => 0o777,
        _ => 0o644,
    }) & 0o7777;
    let (uid, gid) = (x.uid.unwrap_or(0), x.gid.unwrap_or(0));
    // (a local time of an unknown time zone as local time here)
    let mtime = x.mtime.and_then(aqr_foreign::time::instant).unwrap_or(Time::new(0, 0));
    let (uname, gname) = (x.uname.as_deref().unwrap_or(b""), x.gname.as_deref().unwrap_or(b""));

    let mut pax = Vec::new();
    let utf8 = std::str::from_utf8(&path).is_ok() && std::str::from_utf8(link).is_ok();
    let ustar = split_ustar(&path).filter(|_| path.is_ascii());
    if ustar.is_none() || link.len() > 100 || !link.is_ascii() {
        if !utf8 {
            record(&mut pax, b"hdrcharset", b"BINARY");
        }
        if ustar.is_none() {
            record(&mut pax, b"path", &path);
        }
        if link.len() > 100 || !link.is_ascii() {
            record(&mut pax, b"linkpath", link);
        }
    }
    if size > octal_max(11) {
        record(&mut pax, b"size", size.to_string().as_bytes());
    }
    if mtime.nanos != 0 || mtime.secs < 0 || mtime.secs as u64 > octal_max(11) {
        record(&mut pax, b"mtime", &pax_time(mtime));
    }
    for (k, v) in [(&b"uid"[..], uid), (b"gid", gid), (b"SCHILY.devmajor", major), (b"SCHILY.devminor", minor)] {
        if v > octal_max(7) {
            record(&mut pax, k, v.to_string().as_bytes());
        }
    }
    for (k, v) in [(&b"uname"[..], uname), (b"gname", gname)] {
        if v.len() > 31 || !v.is_ascii() {
            record(&mut pax, k, v);
        }
    }
    let mut dropped = 0;
    for (k, v) in &x.xattrs {
        if k.contains(&b'=') || k.contains(&0) {
            dropped += 1;
            continue;
        }
        record(&mut pax, &[&b"SCHILY.xattr."[..], k].concat(), v);
    }
    if dropped > 0 {
        let p = String::from_utf8_lossy(&it.path);
        notes.push(format!("{p}: {dropped} extended attributes of names with = or NUL, not kept in tar"));
    }

    let mut out = Vec::new();
    if !pax.is_empty() {
        // (named as libarchive names them: PaxHeader/ and the name)
        let name = it.path.rsplit(|&b| b == b'/').next().unwrap_or(&[]);
        let mut pname = b"PaxHeader/".to_vec();
        pname.extend(name.iter().take(100 - pname.len()).filter(|b| b.is_ascii() && **b != 0));
        let h = block(
            &pname,
            &[],
            b'x',
            &[],
            mode,
            uid.min(octal_max(7)),
            gid.min(octal_max(7)),
            pax.len() as u64,
            mtime,
            b"",
            b"",
            0,
            0,
        );
        out.extend(h);
        out.extend(&pax);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
    }
    let (prefix, name) = ustar.unwrap_or((&[], &path[..path.len().min(100)]));
    let link = &link[..link.len().min(100)];
    let (uname, gname) = (&uname[..uname.len().min(31)], &gname[..gname.len().min(31)]);
    out.extend(block(name, prefix, typeflag, link, mode, uid, gid, size, mtime, uname, gname, major, minor));
    out
}

/// A ustar header block.
#[allow(clippy::too_many_arguments)]
fn block(
    name: &[u8],
    prefix: &[u8],
    typeflag: u8,
    link: &[u8],
    mode: u32,
    uid: u64,
    gid: u64,
    size: u64,
    mtime: Time,
    uname: &[u8],
    gname: &[u8],
    major: u64,
    minor: u64,
) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    b[..name.len()].copy_from_slice(name);
    octal(&mut b[100..108], mode as u64);
    octal(&mut b[108..116], uid);
    octal(&mut b[116..124], gid);
    octal(&mut b[124..136], size);
    octal(&mut b[136..148], mtime.secs.clamp(0, octal_max(11) as i64) as u64);
    b[156] = typeflag;
    b[157..157 + link.len()].copy_from_slice(link);
    b[257..265].copy_from_slice(b"ustar\x0000");
    b[265..265 + uname.len()].copy_from_slice(uname);
    b[297..297 + gname.len()].copy_from_slice(gname);
    if matches!(typeflag, b'3' | b'4') {
        octal(&mut b[329..337], major.min(octal_max(7)));
        octal(&mut b[337..345], minor.min(octal_max(7)));
    }
    b[345..345 + prefix.len()].copy_from_slice(prefix);
    // the checksum: of the block with its field as spaces, six digits, a NUL and a space
    b[148..156].fill(b' ');
    let sum: u32 = b.iter().map(|&v| v as u32).sum();
    b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    b
}
