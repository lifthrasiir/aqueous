//! Mirror blocks (spec, "Mirror block"): copies of the blocks whose damage would lose more than
//! a parcel, made as an archive is written ([`Mirroring`]), checked and restored from as it is
//! read ([`check`], [`repair`]).
//!
//! [`Mirroring`] takes the bytes of an archive as they are written and passes them on, keeping
//! a copy of each metadata, dictionary, import, module, resource and formula block; once the
//! footer has come, it writes a mirror block of each of them before it, in the order of their
//! blocks (the last metadata block's last, furthest from it), and the footer with its offsets
//! moved past them. Nothing before the footer changes, so that it takes any archive written
//! from its start, and the blocks' offsets (the metadata's, the formulas') stay as they are.
//!
//! The mirror blocks all coming after the last metadata block, a block's mirror is as far from
//! it as the blocks after it and the mirrors before its own: far for most, a block near the end
//! as far as the mirrors of the blocks after it (the metadata block, as far as all the others).
//! A mirror block nearer its block than the spec's least distance has a spacer block before it
//! (4 KiB at most); in an encrypted archive's payload, which may have no spacer blocks, it is
//! left nearer. The archive of a segment holds the copies until its end: about as many bytes as
//! its formula, module and metadata blocks.

use std::io::{self, Write};
use std::ops::Range;

use aqr_common::varint;
use xxhash_rust::xxh64::xxh64;

use super::{FOOTER_MAGIC, FOOTER_TYPE, HEADER_MAGIC, OVERLAY_FOOTER_TYPE, OVERLAY_HEADER_MAGIC};

pub const MIRROR_MAGIC: [u8; 8] = [0x02, 0xaa, 0xf5, 0xfe, 0xba, 0x2b, 0xae, 0x23];
pub const OVERLAY_MIRROR_MAGIC: [u8; 8] = [0x01, 0x05, 0x34, 0xfa, 0xbd, 0x11, 0xaf, 0x53];

/// Mirror block types are the types of their target blocks plus this (plus 1 more where the
/// size field is widened).
const MIRROR: u64 = 0xc00;

/// The least bytes between a block and its mirror block (spec: they never share an aligned
/// 4 KiB unit of storage).
pub const DISTANCE: u64 = 4096;

/// The bytes of a chunk of an encrypted archive's payload ([`super::envelope`]): a block and its
/// mirror block have a whole chunk between them in an encrypted archive.
pub const CHUNK: u64 = 64 << 10;

/// Whether blocks of type `ty` may be mirrored: metadata, dictionary, import, module, resource
/// and formula blocks.
pub fn mirrorable(ty: u64) -> bool {
    matches!(ty, 0x10..=0x17 | 0x50..=0x67 | 0x70..=0x77)
}

/// Where a mirror block starting at `at` has to start at least for a block ending at `end`:
/// [`DISTANCE`] after it, or in an encrypted archive's payload (`chunked`, whose offsets are
/// those of the payload, as the chunks are) after a whole [`CHUNK`].
pub fn least_start(end: u64, chunked: bool) -> u64 {
    if chunked { (end.div_ceil(CHUNK) + 1) * CHUNK } else { end + DISTANCE }
}

/// A block kept to be mirrored: where it starts and ends (before its padding) in the archive,
/// its type and data.
struct Target {
    start: u64,
    end: u64,
    ty: u64,
    data: Vec<u8>,
}

/// Where [`Mirroring`] is in the archive written.
enum At {
    /// In the header magic.
    Magic,
    /// In the type and size of a block (or the footer magic), held until it is all there.
    Head,
    /// In the data of a block, so many bytes of it left, then its padding (where `pad`); kept
    /// where `keep` (as the last of the targets).
    Data { left: u64, pad: bool, keep: bool },
    /// In the padding byte of a block.
    Pad,
    /// In the footer, held until it is all there.
    Footer,
    /// Past the footer: nothing more is to come.
    Done,
}

/// The archive written to `out` with a mirror block of each of its blocks that may have one
/// ([`mirrorable`]), as the module tells.
pub struct Mirroring<W> {
    out: W,
    /// Whether the archive is an encrypted archive's payload ([`least_start`]).
    chunked: bool,
    at: At,
    /// Bytes passed on to `out` (but the mirror blocks and the footer): where the bytes held
    /// start in the archive.
    written: u64,
    held: Vec<u8>,
    magic: [u8; 8],
    targets: Vec<Target>,
    /// Bytes added to the archive: of the mirror blocks (and spacer blocks), and of the footer
    /// as its offsets grew.
    added: u64,
}

impl<W: Write> Mirroring<W> {
    /// The archive written to `out`, an encrypted archive's payload where `chunked`.
    pub fn new(out: W, chunked: bool) -> Mirroring<W> {
        Mirroring {
            out,
            chunked,
            at: At::Magic,
            written: 0,
            held: Vec::new(),
            magic: MIRROR_MAGIC,
            targets: Vec::new(),
            added: 0,
        }
    }

    /// Bytes added to the archive: of the mirror blocks (and the spacer blocks before them), and
    /// of the footer as its offsets grew.
    pub fn added(&self) -> u64 {
        self.added
    }

    /// Whether the footer has been written (and the mirror blocks before it).
    pub fn is_done(&self) -> bool {
        matches!(self.at, At::Done)
    }

    pub fn get_ref(&self) -> &W {
        &self.out
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    /// Forgets what was written, to be written again from the start (to `out` as it is then).
    pub fn restart(&mut self) {
        (self.at, self.written, self.added, self.magic) = (At::Magic, 0, 0, MIRROR_MAGIC);
        self.held.clear();
        self.targets.clear();
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    fn pass(&mut self, d: &[u8]) -> io::Result<()> {
        self.out.write_all(d)?;
        self.written += d.len() as u64;
        Ok(())
    }

    /// Takes the bytes held so far as the head of a block or the footer, where they are all
    /// there: whether they were.
    fn head(&mut self) -> io::Result<()> {
        let h = &self.held;
        let mut p = 0;
        let ty = match varint::get(h, &mut p) {
            Ok(ty) => ty,
            // (a type of more bytes to come)
            Err(_) if h.len() < 10 && h.last().is_some_and(|b| b & 0x80 != 0) => return Ok(()),
            Err(_) => return Err(bad("a bad block type")),
        };
        if ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE {
            self.at = At::Footer;
            return self.footer();
        }
        let width = 1 << (ty % 4);
        if h.len() < p + width {
            return Ok(());
        }
        let size = u64::from_le_bytes(std::array::from_fn(|i| if i < width { h[p + i] } else { 0 }));
        let (start, len) = (self.written, h.len() as u64);
        let keep = mirrorable(ty);
        if keep {
            let data = Vec::with_capacity(usize::try_from(size).unwrap_or(0).min(1 << 20));
            self.targets.push(Target { start, end: start + len + size, ty, data });
        }
        let held = std::mem::take(&mut self.held);
        self.pass(&held)?;
        self.held = held;
        self.held.clear();
        self.at = At::Data { left: size, pad: (len + size) % 2 == 1, keep };
        Ok(())
    }

    /// Takes the bytes held so far as the footer, where it is all there: writes the mirror
    /// blocks, then the footer.
    fn footer(&mut self) -> io::Result<()> {
        let h = &self.held;
        if h.len() < 8 {
            return Ok(());
        }
        if h[..6] != FOOTER_MAGIC && h[..6] != super::OVERLAY_FOOTER_MAGIC {
            return Err(bad("a bad footer"));
        }
        let n = u16::from_le_bytes([h[6], h[7]]) as usize;
        if h.len() < 8 + n + 2 {
            return Ok(());
        }
        let fields = &h[8..8 + n];
        let mut p = 0;
        let (size_hint, back) = match (varint::get(fields, &mut p), varint::get(fields, &mut p)) {
            (Ok(s), Ok(b)) => (s, b),
            _ => return Err(bad("a bad footer")),
        };
        let rest = &fields[p..];
        let footer_at = self.written;
        if size_hint != footer_at - HEADER_MAGIC.len() as u64 {
            return Err(bad("a footer of another size"));
        }
        let mut tail = Vec::new();
        for t in &self.targets {
            let at = footer_at + tail.len() as u64;
            let least = least_start(t.end, self.chunked);
            if at < least && !self.chunked {
                spacer(&mut tail, (least - at).next_multiple_of(2).max(2));
            }
            let at = footer_at + tail.len() as u64;
            mirror_block(&mut tail, &self.magic, at, t);
        }
        let added = tail.len() as u64;
        let mut f = Vec::new();
        varint::put(&mut f, size_hint + added);
        varint::put(&mut f, if back == 0 { 0 } else { back + added });
        f.extend_from_slice(rest);
        let n = u16::try_from(f.len()).map_err(|_| bad("a footer too long"))?;
        tail.extend_from_slice(&h[..6]);
        tail.extend_from_slice(&n.to_le_bytes());
        tail.extend_from_slice(&f);
        tail.extend_from_slice(&n.to_le_bytes());
        self.out.write_all(&tail)?;
        // (the footer's fields may have grown too)
        self.added = (tail.len() - h.len()) as u64;
        self.held.clear();
        self.targets = Vec::new();
        self.at = At::Done;
        Ok(())
    }
}

impl<W: Write> Write for Mirroring<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut d = buf;
        while !d.is_empty() {
            match self.at {
                At::Magic => {
                    let n = (HEADER_MAGIC.len() - self.held.len()).min(d.len());
                    self.held.extend_from_slice(&d[..n]);
                    d = &d[n..];
                    if self.held.len() == HEADER_MAGIC.len() {
                        self.magic = match &self.held[..] {
                            h if h == HEADER_MAGIC => MIRROR_MAGIC,
                            h if h == OVERLAY_HEADER_MAGIC => OVERLAY_MIRROR_MAGIC,
                            _ => return Err(bad("not an archive")),
                        };
                        let held = std::mem::take(&mut self.held);
                        self.pass(&held)?;
                        self.at = At::Head;
                    }
                }
                At::Head | At::Footer => {
                    self.held.push(d[0]);
                    d = &d[1..];
                    match self.at {
                        At::Head => self.head()?,
                        _ => self.footer()?,
                    }
                }
                At::Data { left, pad, keep } => {
                    let n = usize::try_from(left).unwrap_or(usize::MAX).min(d.len());
                    self.pass(&d[..n])?;
                    if keep {
                        self.targets.last_mut().expect("a target").data.extend_from_slice(&d[..n]);
                    }
                    d = &d[n..];
                    let left = left - n as u64;
                    self.at = match (left, pad) {
                        (0, true) => At::Pad,
                        (0, false) => At::Head,
                        _ => At::Data { left, pad, keep },
                    };
                }
                At::Pad => {
                    self.pass(&d[..1])?;
                    d = &d[1..];
                    self.at = At::Head;
                }
                At::Done => return Err(bad("bytes after the footer")),
            }
            // (a block of no data: its padding or the next block)
            if let At::Data { left: 0, pad, .. } = self.at {
                self.at = if pad { At::Pad } else { At::Head };
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("mirroring an archive: {what}"))
}

/// Appends a spacer block of `n` bytes (even, at least 2), zeroed.
fn spacer(out: &mut Vec<u8>, n: u64) {
    // (the type and the size field: 2, 3, 5 or 9 bytes, the block as long as `n` without
    // padding, as `n` is even)
    let (ty, head) = match n {
        0..=257 => (0u8, 2),
        258..=65_538 => (1, 3),
        65_539..=0xffff_ffff => (2, 5),
        _ => (3, 9),
    };
    let size = n - head;
    out.push(ty);
    out.extend_from_slice(&size.to_le_bytes()[..head as usize - 1]);
    out.resize(out.len() + size as usize, 0);
}

/// Appends the mirror block of `t`, to start at `at` in the archive (its magic `magic`).
fn mirror_block(out: &mut Vec<u8>, magic: &[u8; 8], at: u64, t: &Target) {
    let archive_offset = at - HEADER_MAGIC.len() as u64;
    let offset = at - t.start;
    let len = |target_offset: u64| {
        (magic.len() + varint::len(archive_offset) + varint::len(target_offset) + t.data.len() + 8) as u64
    };
    let fits = |ty: u64, n: u64| ty % 4 == 3 || n < 1 << (8 << (ty % 4));
    let (ty, target_offset) = match fits(t.ty, len(offset)) {
        true => (MIRROR + t.ty, offset),
        false => (MIRROR + t.ty + 1, offset + 1),
    };
    let n = len(target_offset);
    let start = out.len();
    varint::put(out, ty);
    out.extend_from_slice(&n.to_le_bytes()[..1 << (ty % 4)]);
    out.extend_from_slice(magic);
    varint::put(out, archive_offset);
    varint::put(out, target_offset);
    out.extend_from_slice(&t.data);
    let sum = xxh64(&out[start..], 0);
    out.extend_from_slice(&sum.to_le_bytes());
    if (out.len() - start) % 2 == 1 {
        out.push(0);
    }
}

/// An intact mirror block of an archive (spec: its checksum verifies and it is as the spec
/// requires, but for being equal to its target block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Where it starts in the archive, and where it tells the archive starts.
    pub at: usize,
    pub archive_start: usize,
    /// Where its target block starts, and its type.
    pub target: usize,
    pub ty: u64,
    /// The type and size field its target block is to have, and where its copy of the target
    /// block's data is in the archive.
    pub head: Vec<u8>,
    pub data: Range<usize>,
}

impl Found {
    /// Where its target block ends (before its padding).
    pub fn target_end(&self) -> usize {
        self.target + self.head.len() + self.data.len()
    }

    /// Whether its target block in archive `a` is as it is to be.
    pub fn matches(&self, a: &[u8]) -> bool {
        let h = self.target + self.head.len();
        a.get(self.target..h) == Some(&self.head[..]) && a.get(h..self.target_end()) == Some(&a[self.data.clone()])
    }
}

/// The mirror block in archive `a` starting at `at` of type `ty`, whose data is `data`
/// (a range of `a`), if intact; the archive starting at `start` (after its header magic) where
/// known.
fn mirror_at(a: &[u8], at: usize, ty: u64, data: Range<usize>, start: Option<usize>) -> Option<Found> {
    let d = &a[data.clone()];
    if d.len() < 8 + 2 + 8 || (d[..8] != MIRROR_MAGIC && d[..8] != OVERLAY_MIRROR_MAGIC) {
        return None;
    }
    let sum = u64::from_le_bytes(d[d.len() - 8..].try_into().unwrap());
    if xxh64(&a[at..data.end - 8], 0) != sum {
        return None;
    }
    let mut p = 8;
    let archive_offset = usize::try_from(varint::get(d, &mut p).ok()?).ok()?;
    let target_offset = usize::try_from(varint::get(d, &mut p).ok()?).ok()?;
    if p > d.len() - 8 || target_offset == 0 {
        return None;
    }
    let archive_start = at.checked_sub(archive_offset)?;
    // (an archive within this one: its own start, not this one's)
    if start.is_some_and(|s| archive_start != s) {
        return None;
    }
    let widened = (target_offset % 2) as u64;
    let target_ty = ty.checked_sub(MIRROR + widened).filter(|&t| mirrorable(t))?;
    if widened == 1 && ty.is_multiple_of(4) {
        return None;
    }
    let target = at.checked_sub(target_offset - widened as usize)?;
    if target < archive_start {
        return None;
    }
    let data = data.start + p..data.end - 8;
    let width = 1 << (target_ty % 4);
    if width < 8 && data.len() as u64 >= 1 << (8 * width) {
        return None;
    }
    let mut head = Vec::with_capacity(10 + 8);
    varint::put(&mut head, target_ty);
    head.extend_from_slice(&(data.len() as u64).to_le_bytes()[..width]);
    let f = Found { at, archive_start, target, ty: target_ty, head, data };
    // (the target block before the mirror block)
    (f.target_end() <= at).then_some(f)
}

/// What [`check`] found of an archive's mirror blocks.
#[derive(Debug, Clone, Default)]
pub struct Check {
    /// Whether the blocks were read one after another from the header to the footer (else the
    /// mirror blocks were found by their magic, and `targets` is empty).
    pub walked: bool,
    /// The blocks that may be mirrored, where each starts and its type.
    pub targets: Vec<(usize, u64)>,
    /// The intact mirror blocks.
    pub mirrors: Vec<Found>,
    /// The blocks of a mirror block type that are not intact (as read one after another).
    pub broken: usize,
}

impl Check {
    /// The intact mirror blocks whose target blocks in archive `a` are not as they are to be.
    pub fn differing<'s>(&'s self, a: &'s [u8]) -> impl Iterator<Item = &'s Found> + 's {
        self.mirrors.iter().filter(move |m| !m.matches(a))
    }

    /// The blocks that may be mirrored with no intact mirror block.
    pub fn unmirrored(&self) -> usize {
        let mirrored: std::collections::HashSet<usize> = self.mirrors.iter().map(|m| m.target).collect();
        self.targets.iter().filter(|(at, _)| !mirrored.contains(at)).count()
    }

    /// The blocks with intact mirror blocks all nearer them than the least distance
    /// ([`least_start`]; the offsets those of an encrypted archive's payload where `chunked`).
    pub fn near(&self, chunked: bool) -> usize {
        let mut far: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
        for m in &self.mirrors {
            *far.entry(m.target).or_default() |= m.at as u64 >= least_start(m.target_end() as u64, chunked);
        }
        far.values().filter(|&&f| !f).count()
    }

    /// Whether archive `a` is mirrored as far as its blocks tell (spec, "Mirrored archives"):
    /// read one after another, every block to mirror equal to an intact mirror block, one of them
    /// far enough from it (`chunked` as [`Check::near`] takes it). (An encrypted archive has to
    /// have mirrored headers too, which its envelope tells.)
    pub fn mirrored(&self, a: &[u8], chunked: bool) -> bool {
        let equal: std::collections::HashSet<usize> =
            self.mirrors.iter().filter(|m| m.matches(a)).map(|m| m.target).collect();
        self.walked && self.targets.iter().all(|(at, _)| equal.contains(at)) && self.near(chunked) == 0
    }
}

/// The mirror blocks of archive `a` (from its header magic to the end of its footer: a file of
/// an archive alone, or an encrypted archive's payload), checked: read one after another from
/// the header, else (where that fails, the archive damaged) found by their magic.
pub fn check(a: &[u8]) -> Check {
    let headed = a.starts_with(&HEADER_MAGIC) || a.starts_with(&OVERLAY_HEADER_MAGIC);
    if headed && let Some(c) = walk(a) {
        return c;
    }
    let start = headed.then_some(HEADER_MAGIC.len());
    Check { mirrors: scan(a, start), ..Check::default() }
}

/// [`check`] reading the blocks of `a` one after another, if they are all there up to the
/// footer.
fn walk(a: &[u8]) -> Option<Check> {
    let mut c = Check { walked: true, ..Check::default() };
    let mut at = HEADER_MAGIC.len();
    loop {
        let start = at;
        let ty = varint::get(a, &mut at).ok()?;
        if ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE {
            // (the footer where the archive's end tells it is)
            let n = u16::from_le_bytes(a.get(at..at + 2)?.try_into().unwrap()) as usize;
            return (start + FOOTER_MAGIC.len() + 2 + n + 2 == a.len()).then_some(c);
        }
        let width = 1 << (ty % 4);
        let size = a.get(at..at + width)?;
        at += width;
        let size =
            usize::try_from(u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)))).ok()?;
        let data = at..at.checked_add(size).filter(|&e| e <= a.len())?;
        at = data.end;
        if mirrorable(ty) {
            c.targets.push((start, ty));
        } else if (MIRROR + 0x10..=MIRROR + 0x78).contains(&ty) {
            match mirror_at(a, start, ty, data, Some(HEADER_MAGIC.len())) {
                Some(m) => c.mirrors.push(m),
                None => c.broken += 1,
            }
        }
        if (at - start) % 2 == 1 {
            at += 1;
        }
    }
}

/// The intact mirror blocks of `a` found by their magic, the archive starting at `start` where
/// known; else (its header damaged) where the mirror blocks found tell it starts first.
fn scan(a: &[u8], start: Option<usize>) -> Vec<Found> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(i) = a[from..].windows(8).position(|w| w == MIRROR_MAGIC || w == OVERLAY_MIRROR_MAGIC) {
        let m = from + i;
        from = m + 1;
        // the size field before it of each width, the type of 2 bytes before that
        for width in [1, 2, 4, 8] {
            let Some(at) = m.checked_sub(width + 2) else { continue };
            let mut p = at;
            let Ok(ty) = varint::get(a, &mut p) else { continue };
            if p != m - width || 1 << (ty % 4) != width {
                continue;
            }
            let size = u64::from_le_bytes(std::array::from_fn(|i| if i < width { a[p + i] } else { 0 }));
            let Some(end) = usize::try_from(size).ok().and_then(|n| m.checked_add(n)).filter(|&e| e <= a.len()) else {
                continue;
            };
            found.extend(mirror_at(a, at, ty, m..end, None));
        }
    }
    // (where the archive starts: as known, else the least start a mirror block tells, those
    // of archives within it starting later)
    let start = start.or_else(|| found.iter().map(|f| f.archive_start).min());
    found.into_iter().filter(|f| Some(f.archive_start) == start).collect()
}

/// Restores the target blocks of archive `a` that are not as an intact mirror block of `c`
/// tells (the first of them, where several do): how many were.
pub fn repair(a: &mut [u8], c: &Check) -> usize {
    let mut restored = std::collections::HashSet::new();
    let mut good = std::collections::HashSet::new();
    for m in &c.mirrors {
        if m.matches(a) {
            good.insert(m.target);
        }
    }
    for m in &c.mirrors {
        if good.contains(&m.target) || restored.contains(&m.target) || m.target_end() > a.len() {
            continue;
        }
        let h = m.target + m.head.len();
        a[m.target..h].copy_from_slice(&m.head);
        a.copy_within(m.data.clone(), h);
        // (the padding, as the spec restores it)
        if (m.target_end() - m.target) % 2 == 1 && m.target_end() < a.len() {
            a[m.target_end()] = 0;
        }
        restored.insert(m.target);
    }
    restored.len()
}

#[cfg(test)]
mod tests {
    use super::super::tests::{REV, natives};
    use super::super::{ContentAt, Encoder, Leaf, Level, SegmentWriter, Tree, Value, decode_value};
    use super::*;
    use crate::aqr::tree::{Entry, EntryNode, Extra};
    use crate::{Formula, Node};

    const NAMES: [&str; 3] = ["hello", "xyz", "uvw"];

    /// An archive of three files, each a formula block applying a module (a metadata block,
    /// a module block and three formula blocks to mirror), of `pad` bytes more each.
    fn archive(pad: usize) -> Vec<u8> {
        archive_of(pad, false)
    }

    /// [`archive`], with the files' checksums where `sums`.
    fn archive_of(pad: usize, sums: bool) -> Vec<u8> {
        let mut f = Formula::default();
        let datas: Vec<Vec<u8>> = NAMES.iter().map(|s| [s.as_bytes(), &vec![b'.'; pad]].concat()).collect();
        let lits: Vec<_> = (0..3).map(|k| f.push(NAMES[k], datas[k].clone())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let mut t = Tree::default();
        let mut contents = Vec::new();
        for (k, a) in lits.iter().enumerate() {
            let len = datas[k].len() as u64;
            let rev: Vec<u8> = datas[k].iter().rev().copied().collect();
            let sum = sums.then(|| super::super::checksum(&rev));
            let leaves =
                vec![Leaf { outputs: vec![Node::apply(REV, vec![1, 2, 3], vec![a.clone()], len)], sums: vec![sum] }];
            let batch = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&batch);
            out.extend_from_slice(&batch.bytes);
            contents.push(ContentAt { leaf: batch.leaf_at[0], output: 0, size: len });
            let name = format!("f{k}").into_bytes();
            t.entries.push(Entry { parent: None, name, node: EntryNode::File(k), extra: Extra::default() });
        }
        out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        out
    }

    /// `a` written through [`Mirroring`], `step` bytes at a time.
    fn mirrored(a: &[u8], step: usize, chunked: bool) -> Vec<u8> {
        let mut m = Mirroring::new(Vec::new(), chunked);
        a.chunks(step).for_each(|c| m.write_all(c).unwrap());
        assert!(m.is_done());
        let added = m.added();
        let out = m.into_inner();
        assert_eq!(out.len() as u64, a.len() as u64 + added);
        out
    }

    fn files(a: &[u8]) -> Vec<Vec<u8>> {
        match decode_value(a, &natives()).unwrap() {
            Value::Tree(_, files) => files,
            Value::File(_) => panic!("a tree"),
        }
    }

    #[test]
    fn mirrors_every_block_but_data() {
        let a = archive(0);
        for step in [1, 7, a.len()] {
            let m = mirrored(&a, step, false);
            // the blocks before the footer as they were, the files too
            let footer = a.len() - (FOOTER_MAGIC.len() + 4 + (a[a.len() - 2] as usize));
            assert_eq!(m[..footer], a[..footer]);
            assert_eq!(files(&m), files(&a));
            let c = check(&m);
            assert!(c.walked && c.broken == 0);
            // the module block, three formula blocks and the metadata block
            let kinds: Vec<u64> = c.targets.iter().map(|t| t.1 & !7).collect();
            assert_eq!(kinds, [0x60, 0x70, 0x70, 0x70, 0x10]);
            assert_eq!((c.mirrors.len(), c.unmirrored(), c.near(false)), (5, 0, 0));
            assert_eq!(c.differing(&m).count(), 0);
            assert!(c.mirrored(&m, false));
            // the metadata block's mirror last, a spacer before the mirrors (the archive is small)
            assert_eq!(c.mirrors.last().unwrap().target, c.targets.last().unwrap().0);
            assert_eq!(m[footer], 0x01);
        }
        // (an encrypted archive's payload: no spacer blocks, the mirror blocks left near)
        let m = mirrored(&a, 64, true);
        let c = check(&m);
        assert_eq!((c.mirrors.len(), c.unmirrored(), c.near(true)), (5, 0, 5));
        assert!(!c.mirrored(&m, true));
        assert_eq!(files(&m), files(&a));
    }

    /// Each block mirrored is restored when damaged, read one after another or (the size of the
    /// metadata block damaged too, as the blocks can no longer be read up to the footer) found by
    /// their magic.
    #[test]
    fn restores_blocks() {
        let m = mirrored(&archive(0), 4096, false);
        let c = check(&m);
        for t in &c.mirrors {
            let mut d = m.clone();
            d[t.target_end() - 3] ^= 0x55;
            let cd = check(&d);
            assert!(cd.walked);
            assert_eq!(cd.differing(&d).count(), 1);
            assert_eq!(repair(&mut d, &cd), 1);
            assert_eq!(d, m);
        }
        let meta = c.mirrors.last().unwrap();
        let mut d = m.clone();
        // (its size field: the metadata block runs past the archive)
        d[meta.target + 1] = 0xff;
        d[meta.target + 4] ^= 1;
        let cd = check(&d);
        // (read one after another, the spacer block's zeros resynchronising the blocks)
        assert!(cd.walked && cd.mirrors.len() == 5);
        assert_eq!(repair(&mut d, &cd), 1);
        assert_eq!(d, m);
        assert_eq!(files(&d), files(&m));
        // the header damaged too: found by their magic, the archive starting where they tell
        d[meta.target + 1] = 0xff;
        d[0] ^= 1;
        let cd = check(&d);
        assert!(!cd.walked && cd.mirrors.len() == 5);
        assert_eq!(repair(&mut d, &cd), 1);
        d[0] ^= 1;
        assert_eq!(d, m);
    }

    /// A damaged mirror block is told and not used; the archive is read as it is.
    #[test]
    fn tells_damaged_mirrors() {
        let m = mirrored(&archive(0), 4096, false);
        let c = check(&m);
        let mut d = m.clone();
        let x = &c.mirrors[1];
        d[x.data.start + 1] ^= 1;
        let cd = check(&d);
        assert_eq!((cd.mirrors.len(), cd.broken, cd.unmirrored()), (4, 1, 1));
        assert!(!cd.mirrored(&d, false));
        assert_eq!(repair(&mut d.clone(), &cd), 0);
        assert_eq!(files(&d), files(&m));
    }

    /// A mirror block of an archive within the archive (as a file of it, stored) is not taken
    /// as one of the archive's.
    #[test]
    fn leaves_inner_archives_alone() {
        let inner = mirrored(&archive(0), 4096, false);
        let mut f = Formula::default();
        f.root = f.push("inner", inner.clone());
        let outer = super::super::encode(&f, None, Level::STORED).unwrap();
        let c = check(&outer);
        assert!(c.walked && c.mirrors.is_empty());
        assert!(scan(&outer, Some(HEADER_MAGIC.len())).is_empty());
        // (as found by their magic alone, the archive starting where its header is)
        assert_eq!(scan(&outer, None).len(), 5);
    }

    /// The archival profile, as the formula blocks declare checksums of their outputs or not.
    #[test]
    fn archival() {
        let natives = natives();
        let of = |sums| super::super::open(&archive_of(0, sums), &natives).unwrap().archival().unwrap();
        assert_eq!(of(false), super::super::Archival { unchecked_leaves: 0, unchecked_formulas: 3 });
        assert!(of(true).conforms());
    }

    /// A block whose mirror block would not fit its size field has a widened one.
    #[test]
    fn widens() {
        let t = Target { start: 6, end: 6 + 2 + 250, ty: 0x70, data: vec![7; 250] };
        let mut out = HEADER_MAGIC.to_vec();
        out.extend_from_slice(&[0x70, 250]);
        out.extend_from_slice(&t.data);
        let at = out.len() as u64;
        mirror_block(&mut out, &MIRROR_MAGIC, at, &t);
        assert_eq!(&out[at as usize..at as usize + 2], &[0xf1, 0x18]);
        let mut p = at as usize;
        let ty = varint::get(&out, &mut p).unwrap();
        let n = u16::from_le_bytes([out[p], out[p + 1]]) as usize;
        let f = mirror_at(&out, at as usize, ty, p + 2..p + 2 + n, Some(6)).unwrap();
        assert!(f.target == 6 && f.ty == 0x70 && f.matches(&out));
    }
}
