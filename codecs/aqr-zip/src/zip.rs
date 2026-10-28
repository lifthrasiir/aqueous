//! Reading a ZIP archive (APPNOTE.TXT 6.3.10) into records: local entries (the header, the body
//! and the data descriptor), central directory headers, the zip64 end of central directory
//! record and locator, and the end of central directory record. Whatever is not one of them
//! (data before the archive, padding, signatures, bodies that overlap) is a gap.
//!
//! The file is read from the start, record after record: nothing is read twice and records
//! never overlap, whatever the headers claim. The central directory, found from the end, only
//! tells where bodies end when the local headers do not.

use std::collections::BTreeMap;
use std::ops::Range;

use aqr_flate::inflate::{self, Sink};

pub const LOCAL: [u8; 4] = *b"PK\x03\x04";
pub const CENTRAL: [u8; 4] = *b"PK\x01\x02";
pub const END64: [u8; 4] = *b"PK\x06\x06";
pub const LOCATOR: [u8; 4] = *b"PK\x06\x07";
pub const END: [u8; 4] = *b"PK\x05\x06";
pub const DESC: [u8; 4] = *b"PK\x07\x08";

/// Bytes of the fixed part of each record, after its signature.
pub const LOCAL_FIXED: usize = 26;
pub const CENTRAL_FIXED: usize = 42;
pub const END64_FIXED: usize = 52;
pub const LOCATOR_FIXED: usize = 16;
pub const END_FIXED: usize = 18;

/// General purpose flags: encrypted, and sizes in a data descriptor.
pub const ENCRYPTED: u16 = 1;
pub const DESCRIPTOR: u16 = 8;

pub const STORED: u16 = 0;
pub const DEFLATED: u16 = 8;

pub fn u16_at(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(d[p..p + 2].try_into().unwrap())
}

pub fn u32_at(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

pub fn u64_at(d: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(d[p..p + 8].try_into().unwrap())
}

/// The form of a data descriptor: CRC-32 then the sizes, of 4 or 8 bytes each, with or without
/// its signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desc {
    None = 0,
    Short = 1,
    ShortSig = 2,
    Long = 3,
    LongSig = 4,
}

impl Desc {
    pub fn from_u8(v: u8) -> Option<Desc> {
        [Desc::None, Desc::Short, Desc::ShortSig, Desc::Long, Desc::LongSig].get(v as usize).copied()
    }

    pub fn sig(self) -> bool {
        matches!(self, Desc::ShortSig | Desc::LongSig)
    }

    pub fn long(self) -> bool {
        matches!(self, Desc::Long | Desc::LongSig)
    }

    /// Bytes after the signature.
    pub fn fields(self) -> usize {
        match self {
            Desc::None => 0,
            Desc::Short | Desc::ShortSig => 12,
            Desc::Long | Desc::LongSig => 20,
        }
    }

    pub fn bytes(self) -> usize {
        self.fields() + if self.sig() { 4 } else { 0 }
    }
}

/// A local file header at `pos`, its body and its data descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub pos: usize,
    pub name: Range<usize>,
    pub extra: Range<usize>,
    pub body: Range<usize>,
    pub desc: Desc,
}

impl Local {
    pub fn fixed<'a>(&self, d: &'a [u8]) -> &'a [u8] {
        &d[self.pos + 4..self.pos + 4 + LOCAL_FIXED]
    }

    pub fn flags(&self, d: &[u8]) -> u16 {
        u16_at(d, self.pos + 6)
    }

    pub fn method(&self, d: &[u8]) -> u16 {
        u16_at(d, self.pos + 8)
    }

    /// The fields of the data descriptor, after its signature.
    pub fn desc_fields<'a>(&self, d: &'a [u8]) -> &'a [u8] {
        let at = self.body.end + if self.desc.sig() { 4 } else { 0 };
        &d[at..at + self.desc.fields()]
    }

    pub fn end(&self) -> usize {
        self.body.end + self.desc.bytes()
    }
}

/// A central directory header at `pos`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Central {
    pub pos: usize,
    pub name: Range<usize>,
    pub extra: Range<usize>,
    pub comment: Range<usize>,
}

impl Central {
    pub fn fixed<'a>(&self, d: &'a [u8]) -> &'a [u8] {
        &d[self.pos + 4..self.pos + 4 + CENTRAL_FIXED]
    }

    /// The local header's offset, the sizes and the CRC, with those of the zip64 extra field.
    pub fn info(&self, d: &[u8]) -> (u64, Info) {
        let f = self.fixed(d);
        let z =
            zip64(&d[self.extra.clone()]).map_or(&[][..], |r| &d[self.extra.start + r.start..self.extra.start + r.end]);
        let [usize, csize, offset] = resolve([u32_at(f, 20), u32_at(f, 16), u32_at(f, 38)], z);
        (offset, Info { crc: u32_at(f, 12), csize, usize })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rec {
    Gap(Range<usize>),
    Local(Local),
    Central(Central),
    /// The zip64 end of central directory record at `pos`, with its extensible data.
    End64 {
        pos: usize,
        ext: Range<usize>,
    },
    Locator {
        pos: usize,
    },
    /// The end of central directory record at `pos`, with its comment.
    End {
        pos: usize,
        comment: Range<usize>,
    },
}

impl Rec {
    pub fn end(&self) -> usize {
        match self {
            Rec::Gap(r) => r.end,
            Rec::Local(l) => l.end(),
            Rec::Central(c) => c.comment.end,
            Rec::End64 { ext, .. } => ext.end,
            Rec::Locator { pos } => pos + 4 + LOCATOR_FIXED,
            Rec::End { comment, .. } => comment.end,
        }
    }
}

/// What the central directory says of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Info {
    pub crc: u32,
    pub csize: u64,
    pub usize: u64,
}

/// The data of the zip64 extended information (tag 1) of an extra field, if its blocks hold
/// one before they stop making sense.
pub fn zip64(extra: &[u8]) -> Option<Range<usize>> {
    let mut p = 0;
    while p + 4 <= extra.len() {
        let (id, size) = (u16_at(extra, p), u16_at(extra, p + 2) as usize);
        let end = p + 4 + size;
        if end > extra.len() {
            return None;
        }
        if id == 1 {
            return Some(p + 4..end);
        }
        p = end;
    }
    None
}

/// 32-bit fields resolved by the zip64 data `z`: each that is all ones takes the next 8 bytes,
/// while there are any.
pub fn resolve<const N: usize>(fields: [u32; N], z: &[u8]) -> [u64; N] {
    let mut at = 0;
    fields.map(|v| {
        if v == u32::MAX && at + 8 <= z.len() {
            at += 8;
            u64_at(z, at - 8)
        } else {
            v as u64
        }
    })
}

/// The end of central directory record found from the end of `d` whose directory starts with a
/// central header: where the directory is, and its base (where offset 0 is in `d`).
fn find_end(d: &[u8]) -> Option<(i64, Range<usize>)> {
    let last = d.len().checked_sub(4 + END_FIXED)?;
    let low = last.saturating_sub(0xffff + 0x10000);
    let mut p = d.len();
    while let Some(q) = memchr::memmem::rfind(&d[low..p], &END).map(|q| low + q) {
        p = q + 3;
        if q > last {
            continue;
        }
        let (mut size, mut off, mut end) = (u32_at(d, q + 12) as u64, u32_at(d, q + 16) as u64, q);
        if q >= 20 && d[q - 20..q - 16] == LOCATOR {
            let at = u64_at(d, q - 12);
            let tries = [usize::try_from(at).ok(), (q - 20).checked_sub(4 + END64_FIXED)];
            if let Some(e) =
                tries.into_iter().flatten().find(|&e| e + 4 + END64_FIXED <= q - 20 && d[e..e + 4] == END64)
            {
                (size, off, end) = (u64_at(d, e + 40), u64_at(d, e + 48), e);
            }
        }
        let Some(start) = usize::try_from(size).ok().and_then(|s| end.checked_sub(s)) else { continue };
        if size == 0 || d[start..].starts_with(&CENTRAL) {
            return Some(((start as i64).wrapping_sub(off as i64), start..end));
        }
    }
    None
}

/// The CRC-32 and size of the entries' data that the central directory found from the end of
/// `d` tells, unchecked against local headers: `d` may be the end of an archive alone, holding
/// the directory.
pub fn entry_sums(d: &[u8]) -> Vec<(u32, u64)> {
    let Some((_, cd)) = find_end(d) else { return Vec::new() };
    let (mut out, mut p) = (Vec::new(), cd.start);
    while p < cd.end
        && let Some(c) = central_at(d, p)
    {
        let i = c.info(d).1;
        out.push((i.crc, i.usize));
        p = c.comment.end;
    }
    out
}

/// The central directory found from the end of `d`: its base, and what it says of each entry by
/// the position of its local header (the first header wins). Of the base the end record implies
/// and 0, the one putting more of the entries on local headers is taken, and only the entries
/// it puts on one are kept.
pub fn directory(d: &[u8]) -> (i64, BTreeMap<usize, Info>) {
    let Some((base, cd)) = find_end(d) else { return (0, BTreeMap::new()) };
    let mut entries = Vec::new();
    let mut p = cd.start;
    while p < cd.end
        && let Some(c) = central_at(d, p)
    {
        entries.push(c.info(d));
        p = c.comment.end;
    }
    let map = |base: i64| {
        let mut map = BTreeMap::new();
        for &(off, info) in &entries {
            if let Ok(pos) = usize::try_from((off as i64).wrapping_add(base))
                && d.get(pos..pos.saturating_add(4)) == Some(&LOCAL[..])
            {
                map.entry(pos).or_insert(info);
            }
        }
        map
    };
    let (m, m0) = (map(base), map(0));
    if m0.len() > m.len() { (0, m0) } else { (base, m) }
}

fn central_at(d: &[u8], p: usize) -> Option<Central> {
    let f = d.get(p + 4..p + 4 + CENTRAL_FIXED)?;
    let name = p + 46..p + 46 + u16_at(f, 24) as usize;
    let extra = name.end..name.end + u16_at(f, 26) as usize;
    let comment = extra.end..extra.end + u16_at(f, 28) as usize;
    (comment.end <= d.len()).then_some(Central { pos: p, name, extra, comment })
}

/// A sink counting the bytes of data.
#[derive(Default)]
pub struct Count(pub usize);

impl Sink for Count {
    #[inline(always)]
    fn lit(&mut self, _: u8) {
        self.0 += 1;
    }

    #[inline(always)]
    fn mat(&mut self, len: u16, _: u16, _: bool) {
        self.0 += len as usize;
    }

    fn stored(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
}

/// The most bytes of data that a DEFLATE stream holds per byte of it: matches of 258 bytes in
/// two bits.
pub const MAX_EXPANSION: u64 = 1032;

/// Bytes of the data of the DEFLATE stream `d`, which ends with `d` or before, without keeping
/// the data.
pub fn inflated_len(d: &[u8]) -> Option<usize> {
    let mut n = Count(0);
    inflate::parse_head(d, usize::MAX, &mut n).ok()?;
    Some(n.0)
}

/// Bytes of the DEFLATE stream at the start of `d` and of its data, without keeping the data,
/// where it is not known where the stream ends (what follows it is not copied).
pub fn deflate_len(d: &[u8]) -> Option<(usize, usize)> {
    let mut n = Count(0);
    let l = inflate::parse_head(d, usize::MAX, &mut n).ok()?;
    Some((l.end, n.0))
}

/// The data descriptor after a body of `len` bytes ending at `at`, if one is there: by its
/// signature, or without one where a record or the end of `d` follows. The long form is
/// preferred where the local header has zip64 data.
fn descriptor(d: &[u8], at: usize, len: usize, long_first: bool) -> Desc {
    let fits = |desc: Desc| {
        let q = at + if desc.sig() { 4 } else { 0 };
        let e = at + desc.bytes();
        e <= d.len()
            && (!desc.sig() || d[at..at + 4] == DESC)
            && if desc.long() { u64_at(d, q + 4) == len as u64 } else { u32_at(d, q + 4) == len as u32 }
            && (desc.sig() || e == d.len() || d[e..].starts_with(b"PK"))
    };
    let order = if long_first {
        [Desc::LongSig, Desc::ShortSig, Desc::Long, Desc::Short]
    } else {
        [Desc::ShortSig, Desc::LongSig, Desc::Short, Desc::Long]
    };
    order.into_iter().find(|&desc| fits(desc)).unwrap_or(Desc::None)
}

fn local_at(d: &[u8], p: usize, map: &BTreeMap<usize, Info>) -> Option<Local> {
    let f = d.get(p + 4..p + 4 + LOCAL_FIXED)?;
    let (flags, method) = (u16_at(f, 2), u16_at(f, 4));
    let name = p + 30..p + 30 + u16_at(f, 22) as usize;
    let extra = name.end..name.end + u16_at(f, 24) as usize;
    let start = extra.end;
    if start > d.len() {
        return None;
    }
    let z = zip64(&d[extra.clone()]);
    let zd = z.as_ref().map_or(&[][..], |r| &d[extra.start + r.start..extra.start + r.end]);
    let [_, csize] = resolve([u32_at(f, 18), u32_at(f, 14)], zd);
    let rest = d.len() - start;
    let fits = |n: u64| usize::try_from(n).ok().filter(|&n| n <= rest);
    let len = map
        .get(&p)
        .and_then(|i| fits(i.csize))
        .or_else(|| fits(csize).filter(|&n| n > 0 || flags & DESCRIPTOR == 0))
        .or_else(|| match method {
            DEFLATED if flags & ENCRYPTED == 0 => deflate_len(&d[start..]).map(|(n, _)| n),
            // a stored body is known by the descriptor after it, with its own length
            _ if flags & DESCRIPTOR != 0 => memchr::memmem::find_iter(&d[start..], &DESC)
                .find(|&q| start + q + 12 <= d.len() && u32_at(d, start + q + 8) == q as u32),
            _ => None,
        })?;
    let body = start..start + len;
    let desc = if flags & DESCRIPTOR != 0 { descriptor(d, body.end, len, z.is_some()) } else { Desc::None };
    Some(Local { pos: p, name, extra, body, desc })
}

/// The record at `p`, if one is there and fits in `d`. One that the central directory does not
/// know of must not run over a local header that it does: signatures turn up in other data
/// (a self-extractor's code, a comment), with whatever fields follow.
fn record_at(d: &[u8], p: usize, map: &BTreeMap<usize, Info>) -> Option<Rec> {
    let r = any_record_at(d, p, map)?;
    let known = map.contains_key(&p) && matches!(r, Rec::Local(_));
    let next = map.range(p + 1..).next().map_or(usize::MAX, |(&q, _)| q);
    (known || r.end() <= next).then_some(r)
}

fn any_record_at(d: &[u8], p: usize, map: &BTreeMap<usize, Info>) -> Option<Rec> {
    let sig: [u8; 4] = d.get(p..p + 4)?.try_into().unwrap();
    match sig {
        LOCAL => local_at(d, p, map).map(Rec::Local),
        CENTRAL => central_at(d, p).map(Rec::Central),
        END64 => {
            let size = usize::try_from(u64_at(d.get(p..p + 12)?, 4)).ok()?;
            let end = size.checked_sub(END64_FIXED - 8).and_then(|_| (p + 12).checked_add(size))?;
            (end <= d.len()).then_some(Rec::End64 { pos: p, ext: p + 4 + END64_FIXED..end })
        }
        LOCATOR => (p + 4 + LOCATOR_FIXED <= d.len()).then_some(Rec::Locator { pos: p }),
        END => {
            let at = p + 4 + END_FIXED;
            let comment = at..at + u16_at(d.get(p..at)?, 20) as usize;
            (comment.end <= d.len()).then_some(Rec::End { pos: p, comment })
        }
        _ => None,
    }
}

/// The records of `d`, from its start, with gaps between them (every byte is in one), and the
/// base and entries of the central directory ([`directory`]).
pub fn records(d: &[u8]) -> (i64, BTreeMap<usize, Info>, Vec<Rec>) {
    let (base, map) = directory(d);
    let mut out = Vec::new();
    let (mut p, mut gap) = (0, 0);
    while p < d.len() {
        if let Some(r) = record_at(d, p, &map) {
            if gap < p {
                out.push(Rec::Gap(gap..p));
            }
            p = r.end();
            gap = p;
            out.push(r);
        } else {
            p = memchr::memchr(b'P', &d[p + 1..]).map_or(d.len(), |q| p + 1 + q);
        }
    }
    if gap < d.len() {
        out.push(Rec::Gap(gap..d.len()));
    }
    (base, map, out)
}

/// Bytes of the archive at the start of `d`: as far as its records follow each other without a
/// gap up to the end of central directory record, or else to the end record found from the end
/// of `d` whose directory starts with a central header (a gap may hold anything), or else to the
/// end of `d` where its records, a whole local entry first, follow each other up to one that
/// `d` cuts short ([`cut_at`]): an archive cut short, as downloads are. `None` where there is
/// none of them. An end record whose comment runs past `d` ends with `d`. The bodies that the
/// directory found from the end of `d` knows of are of the sizes it says, rather than parsed
/// (as one with a descriptor after it is).
pub fn extent(d: &[u8]) -> Option<usize> {
    if !d.starts_with(&LOCAL) {
        return None;
    }
    let (_, map) = directory(d);
    let (mut p, mut last) = (0, None);
    while let Some(r) = record_at(d, p, &map) {
        if let Rec::End { .. } = r {
            return Some(r.end());
        }
        p = r.end();
        last = Some(r);
    }
    let Some((_, cd)) = find_end(d) else {
        return last.is_some_and(|r| cut_at(d, p, &r)).then_some(d.len());
    };
    let at = cd.end + 4 + END_FIXED;
    let end = (d[cd.end..cd.end + 4] == END).then(|| at + u16_at(d, at - 2) as usize);
    // after a zip64 end record, the end record follows the locator
    end.or_else(|| {
        let q = memchr::memmem::find(&d[cd.end..], &END)? + cd.end;
        Some(q + 4 + END_FIXED + u16_at(d, q + 20) as usize)
    })
    .map(|e| e.min(d.len()))
}

/// Whether the record at `p`, after `last`, is one that `d` cuts short: `d` ends there, or in
/// a signature, or in the data descriptor of `last` (which it lacks, or has in a shorter form
/// than the one cut short), or the record runs past `d`. A local entry that is not taken runs
/// past `d` (its fields, a body of the size its header says, or one whose end is not found), or
/// else is broken: either way the archive goes on to the end of `d`.
fn cut_at(d: &[u8], p: usize, last: &Rec) -> bool {
    let rest = &d[p..];
    if let Rec::Local(l) = last
        && l.flags(d) & DESCRIPTOR != 0
        && l.desc.bytes() + rest.len() < Desc::LongSig.bytes()
    {
        return true;
    }
    let Some(sig) = rest.first_chunk::<4>() else {
        return [LOCAL, CENTRAL, END64, LOCATOR, END].iter().any(|s| s.starts_with(rest));
    };
    match *sig {
        // these are not taken only where they run past `d`
        LOCAL | CENTRAL | LOCATOR | END => true,
        END64 => rest.get(4..12).is_none_or(|s| u64::from_le_bytes(s.try_into().unwrap()) >= (END64_FIXED - 8) as u64),
        _ => false,
    }
}
