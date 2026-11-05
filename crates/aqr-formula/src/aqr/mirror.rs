//! Mirror blocks (spec, "Mirror block"): copies of the blocks whose damage would lose more than
//! a parcel, made as an archive is written ([`Mirroring`], [`mirror_of`]), checked and restored
//! from as it is read ([`check`], [`repair`]).
//!
//! [`Mirroring`] takes the bytes of an archive as they are written and passes them on, keeping
//! a copy of each metadata, dictionary, import, module, resource and formula block; once the
//! footer has come, it writes a mirror block of each of them before it, in the order of their
//! blocks (the last metadata block's last, furthest from it), and the footer with its offsets
//! moved past them. Nothing before the footer changes, so that it takes any archive written
//! from its start, and the blocks' offsets (the metadata's, the formulas') stay as they are. A
//! block whose mirror block came before it ([`super::SegmentWriter::finish_mirrored`] puts the
//! last metadata block's before the last [`FAR`] bytes of the archive, so that a truncation
//! shorter than that leaves it) has none after it.
//!
//! The mirror blocks coming after the last metadata block, a block's mirror is as far from it as
//! the blocks after it and the mirrors before its own: far for most, a block near the end as far
//! as the mirrors of the blocks after it (the metadata block, as far as all the others). A mirror
//! block nearer its block than the spec's least distance has a spacer block before it (4 KiB at
//! most); in an encrypted archive's payload, which may have no spacer blocks, it is left nearer.
//! The archive of a segment holds the copies until its end: about as many bytes as its formula,
//! module and metadata blocks. An archive of several segments has the mirror blocks of each
//! segment written between blocks before the next one's metadata block ([`Mirroring::mirror_before`]),
//! as far from their blocks as that segment is long, the copies of about two segments held.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::ops::Range;

use aqr_common::varint;
use xxhash_rust::xxh64::xxh64;

use super::{FOOTER_MAGIC, FOOTER_TYPE, HEADER_MAGIC, OVERLAY_FOOTER_MAGIC, OVERLAY_FOOTER_TYPE, OVERLAY_HEADER_MAGIC};

pub const MIRROR_MAGIC: [u8; 8] = [0x02, 0xaa, 0xf5, 0xfe, 0xba, 0x2b, 0xae, 0x23];
pub const OVERLAY_MIRROR_MAGIC: [u8; 8] = [0x01, 0x05, 0x34, 0xfa, 0xbd, 0x11, 0xaf, 0x53];

/// Mirror block types are the types of their target blocks plus this (plus 1 more where the
/// size field is widened).
const MIRROR: u64 = 0xc00;

/// The least bytes between a block and its mirror block (spec: they never share an aligned
/// 4 KiB unit of storage).
pub const DISTANCE: u64 = 4096;

/// The bytes between a block and its mirror block that the spec recommends where possible: the
/// bytes of an archive held back before its last metadata block, for that block's mirror block
/// to come before them ([`super::SegmentWriter::finish_mirrored`]).
pub const FAR: u64 = 4 << 20;

/// The bytes of a chunk of an encrypted archive's payload ([`super::envelope`]): a block and its
/// mirror block have a whole chunk between them in an encrypted archive.
pub const CHUNK: u64 = 64 << 10;

/// Whether blocks of type `ty` may be mirrored: metadata, dictionary, import, module, resource
/// and formula blocks.
pub fn mirrorable(ty: u64) -> bool {
    matches!(ty, 0x10..=0x17 | 0x50..=0x67 | 0x70..=0x77)
}

/// Whether blocks of type `ty` are mirror blocks.
fn is_mirror(ty: u64) -> bool {
    (MIRROR + 0x10..=MIRROR + 0x78).contains(&ty)
}

/// Where the later of a block and its mirror block has to start at least, the earlier ending at
/// `end`: [`DISTANCE`] after it, or in an encrypted archive's payload (`chunked`, whose offsets are
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
    /// as the last of the targets, or its start as a mirror block's, as `keep` tells.
    Data { left: u64, pad: bool, keep: Keep },
    /// In the padding byte of a block.
    Pad,
    /// In the footer, held until it is all there.
    Footer,
    /// Past the footer: nothing more is to come.
    Done,
}

/// What [`Mirroring`] keeps of a block's data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    Nothing,
    /// All of it, as a target's.
    Target,
    /// Its start, as a mirror block's (where it starts), for its target block.
    Mirror(u64),
}

/// The bytes of a mirror block's data that tell its target block: the magic and two LEB128s.
const MIRROR_HEAD: usize = 8 + 10 + 10;

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
    /// The start of a mirror block's data, as it comes ([`Keep::Mirror`]).
    mirror_head: Vec<u8>,
    /// The blocks (where they start) of a mirror block written before them.
    mirrored: HashSet<u64>,
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
            mirror_head: Vec::new(),
            mirrored: HashSet::new(),
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
        self.mirror_head.clear();
        self.mirrored.clear();
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    fn pass(&mut self, d: &[u8]) -> io::Result<()> {
        self.out.write_all(d)?;
        self.written += d.len() as u64;
        Ok(())
    }

    /// Writes now, between blocks, the mirror blocks of the blocks kept that end by `end` (those
    /// of segments closed, say: an archive of several segments has those of each segment after
    /// the next, rather than all of them at its end, holding the copies of about two segments'
    /// blocks); with a spacer block before one nearer its block than the spec's least distance
    /// but in an encrypted archive's payload. Returns the bytes written, which the blocks after
    /// them are moved by ([`super::SegmentWriter::skip`]). Fails in a block, and where `out` does.
    pub fn mirror_before(&mut self, end: u64) -> io::Result<u64> {
        if !matches!(self.at, At::Head) || !self.held.is_empty() {
            return Err(bad("mirror blocks not between blocks"));
        }
        let mut tail = Vec::new();
        let (now, later): (Vec<Target>, Vec<Target>) =
            std::mem::take(&mut self.targets).into_iter().partition(|t| t.end <= end);
        self.targets = later;
        for t in now.iter().filter(|t| !self.mirrored.contains(&t.start)) {
            let least = least_start(t.end, self.chunked);
            let at = self.written + tail.len() as u64;
            if at < least && !self.chunked {
                spacer(&mut tail, (least - at).next_multiple_of(2).max(2));
            }
            let at = self.written + tail.len() as u64;
            mirror_block(&mut tail, &self.magic, at, t.start, t.ty, &t.data);
        }
        self.pass(&tail)?;
        self.added += tail.len() as u64;
        Ok(tail.len() as u64)
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
        let keep = match ty {
            ty if mirrorable(ty) => Keep::Target,
            ty if is_mirror(ty) => Keep::Mirror(start),
            _ => Keep::Nothing,
        };
        if keep == Keep::Target {
            let data = Vec::with_capacity(usize::try_from(size).unwrap_or(0).min(1 << 20));
            self.targets.push(Target { start, end: start + len + size, ty, data });
        }
        let held = std::mem::take(&mut self.held);
        self.pass(&held)?;
        self.held = held;
        self.held.clear();
        self.mirror_head.clear();
        self.at = At::Data { left: size, pad: (len + size) % 2 == 1, keep };
        Ok(())
    }

    /// Takes the start of a mirror block's data, all there, as telling a block that has a
    /// mirror block (where the block follows it).
    fn mirror_ends(&mut self, at: u64) {
        let d = &self.mirror_head;
        let mut p = 8;
        if let (Ok(_), Ok(t)) = (varint::get(d, &mut p), varint::get(d, &mut p))
            && t % 2 == 1
        {
            self.mirrored.insert(at + t - 1);
        }
    }

    /// Takes the bytes held so far as the footer, where it is all there: writes the mirror
    /// blocks, then the footer.
    fn footer(&mut self) -> io::Result<()> {
        let h = &self.held;
        if h.len() < 8 {
            return Ok(());
        }
        if h[..6] != FOOTER_MAGIC && h[..6] != OVERLAY_FOOTER_MAGIC {
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
        for t in self.targets.iter().filter(|t| !self.mirrored.contains(&t.start)) {
            let at = footer_at + tail.len() as u64;
            let least = least_start(t.end, self.chunked);
            if at < least && !self.chunked {
                spacer(&mut tail, (least - at).next_multiple_of(2).max(2));
            }
            let at = footer_at + tail.len() as u64;
            mirror_block(&mut tail, &self.magic, at, t.start, t.ty, &t.data);
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
        self.added += (tail.len() - h.len()) as u64;
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
                    match keep {
                        Keep::Target => self.targets.last_mut().expect("a target").data.extend_from_slice(&d[..n]),
                        Keep::Mirror(_) => {
                            let k = (MIRROR_HEAD - self.mirror_head.len().min(MIRROR_HEAD)).min(n);
                            self.mirror_head.extend_from_slice(&d[..k]);
                        }
                        Keep::Nothing => {}
                    }
                    d = &d[n..];
                    let left = left - n as u64;
                    self.at = match (left, pad) {
                        (0, true) => At::Pad,
                        (0, false) => At::Head,
                        _ => At::Data { left, pad, keep },
                    };
                    if let (0, Keep::Mirror(at)) = (left, keep) {
                        self.mirror_ends(at);
                    }
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
pub fn spacer(out: &mut Vec<u8>, n: u64) {
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

/// The mirror block of block `b` (its bytes from its type on, padding or not) starting at
/// `target` in the archive, to start at `at` (before or after it) in an archive that is an overlay
/// archive or not.
pub fn mirror_of(b: &[u8], target: u64, at: u64, overlay: bool) -> Vec<u8> {
    let mut p = 0;
    let ty = varint::get(b, &mut p).expect("a block");
    let width = 1 << (ty % 4);
    let size = u64::from_le_bytes(std::array::from_fn(|i| if i < width { b[p + i] } else { 0 })) as usize;
    let data = &b[p + width..p + width + size];
    let mut out = Vec::new();
    let magic = if overlay { OVERLAY_MIRROR_MAGIC } else { MIRROR_MAGIC };
    mirror_block(&mut out, &magic, at, target, ty, data);
    out
}

/// Appends the mirror block of the block of type `ty` and `data` starting at `target`, to start
/// at `at` in the archive (its magic `magic`): its archive offset odd where it is widened, its
/// target offset odd where the block follows it.
fn mirror_block(out: &mut Vec<u8>, magic: &[u8; 8], at: u64, target: u64, ty: u64, data: &[u8]) {
    let archive_offset = at - HEADER_MAGIC.len() as u64;
    let target_offset = if target < at { at - target } else { target - at + 1 };
    // (an even archive offset plus 1 is as long, as the LEB128s of each length end at odd ones)
    let n = (magic.len() + varint::len(archive_offset) + varint::len(target_offset) + data.len() + 8) as u64;
    let fits = ty % 4 == 3 || n < 1 << (8 << (ty % 4));
    let (ty, archive_offset) = if fits { (MIRROR + ty, archive_offset) } else { (MIRROR + ty + 1, archive_offset + 1) };
    let start = out.len();
    varint::put(out, ty);
    out.extend_from_slice(&n.to_le_bytes()[..1 << (ty % 4)]);
    out.extend_from_slice(magic);
    varint::put(out, archive_offset);
    varint::put(out, target_offset);
    out.extend_from_slice(data);
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
    /// Where it starts and ends (before its padding) in the archive, and where it tells the
    /// archive starts.
    pub at: usize,
    pub end: usize,
    pub archive_start: usize,
    /// Where its target block starts, and its type.
    pub target: usize,
    pub ty: u64,
    /// The type and size field its target block is to have, and where its copy of the target
    /// block's data is in the archive.
    pub head: Vec<u8>,
    pub data: Range<usize>,
    /// Whether its magic is an overlay archive's.
    pub overlay: bool,
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

    /// Whether it is as far from its target block as the spec asks ([`least_start`]).
    pub fn far(&self, chunked: bool) -> bool {
        match self.target < self.at {
            true => self.at as u64 >= least_start(self.target_end() as u64, chunked),
            false => self.target as u64 >= least_start(self.end as u64, chunked),
        }
    }
}

/// The mirror block in archive `a` starting at `at` of type `ty`, whose data is `data`
/// (a range of `a`), if intact; the archive starting at `start` (after its header magic) where
/// known. Its target block may be past the end of `a`.
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
    if p > d.len() - 8 {
        return None;
    }
    let widened = (archive_offset % 2) as u64;
    let archive_start = at.checked_sub(archive_offset - widened as usize)?;
    // (an archive within this one: its own start, not this one's)
    if start.is_some_and(|s| archive_start != s) {
        return None;
    }
    let target_ty = ty.checked_sub(MIRROR + widened).filter(|&t| mirrorable(t))?;
    if widened == 1 && ty.is_multiple_of(4) {
        return None;
    }
    let follows = target_offset % 2 == 1;
    let distance = target_offset - follows as usize;
    if distance == 0 {
        return None;
    }
    let target = if follows { at.checked_add(distance)? } else { at.checked_sub(distance)? };
    if target < archive_start {
        return None;
    }
    let end = data.end;
    let data = data.start + p..data.end - 8;
    let width = 1 << (target_ty % 4);
    if width < 8 && data.len() as u64 >= 1 << (8 * width) {
        return None;
    }
    let mut head = Vec::with_capacity(10 + 8);
    varint::put(&mut head, target_ty);
    head.extend_from_slice(&(data.len() as u64).to_le_bytes()[..width]);
    let overlay = d[..8] == OVERLAY_MIRROR_MAGIC;
    let f = Found { at, end, archive_start, target, ty: target_ty, head, data, overlay };
    // (the two not overlapping)
    let apart = if follows { end <= target } else { f.target_end() <= at };
    apart.then_some(f)
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
        let mirrored: HashSet<usize> = self.mirrors.iter().map(|m| m.target).collect();
        self.targets.iter().filter(|(at, _)| !mirrored.contains(at)).count()
    }

    /// The blocks with intact mirror blocks all nearer them than the least distance
    /// ([`least_start`]; the offsets those of an encrypted archive's payload where `chunked`).
    pub fn near(&self, chunked: bool) -> usize {
        let mut far: HashMap<usize, bool> = HashMap::new();
        for m in &self.mirrors {
            *far.entry(m.target).or_default() |= m.far(chunked);
        }
        far.values().filter(|&&f| !f).count()
    }

    /// Whether archive `a` is mirrored as far as its blocks tell (spec, "Mirrored archives"):
    /// read one after another, every block to mirror equal to an intact mirror block, one of them
    /// far enough from it (`chunked` as [`Check::near`] takes it). (An encrypted archive has to
    /// have mirrored headers too, which its envelope tells.)
    pub fn mirrored(&self, a: &[u8], chunked: bool) -> bool {
        let equal: HashSet<usize> = self.mirrors.iter().filter(|m| m.matches(a)).map(|m| m.target).collect();
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

/// How reading the blocks of an archive one after another ended ([`blocks`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// At a footer where the archive's end tells it is.
    Footer,
    /// At the archive's end, in a block or the footer or between blocks: where the archive is
    /// cut short.
    Cut,
    /// At bytes that are no block.
    Bad,
}

/// The blocks of archive `a` read one after another from its header on (where each starts, its
/// type and data), as far as they go, and how that ended.
fn blocks(a: &[u8]) -> (Vec<(usize, u64, Range<usize>)>, Ended) {
    let mut out = Vec::new();
    let mut at = HEADER_MAGIC.len();
    loop {
        let start = at;
        if at >= a.len() {
            return (out, Ended::Cut);
        }
        let Ok(ty) = varint::get(a, &mut at) else {
            // (a type running past the end, or too long)
            let cut = a[start..].iter().all(|b| b & 0x80 != 0) && a.len() - start < 10;
            return (out, if cut { Ended::Cut } else { Ended::Bad });
        };
        if ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE {
            let ended = match a.get(at..at + 2) {
                Some(n) if start + 6 + 2 + u16::from_le_bytes(n.try_into().unwrap()) as usize + 2 == a.len() => {
                    Ended::Footer
                }
                // (a footer cut short, or not ending at the end)
                _ => Ended::Cut,
            };
            return (out, ended);
        }
        if at - start != varint::len(ty) {
            return (out, Ended::Bad);
        }
        let width = 1 << (ty % 4);
        let Some(size) = a.get(at..at + width) else { return (out, Ended::Cut) };
        at += width;
        let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
        let Some(data) = usize::try_from(size).ok().and_then(|n| at.checked_add(n)).filter(|&e| e <= a.len()) else {
            return (out, Ended::Cut);
        };
        let data = at..data;
        at = data.end;
        out.push((start, ty, data));
        if (at - start) % 2 == 1 {
            at += 1;
        }
    }
}

/// [`check`] reading the blocks of `a` one after another, if they are all there up to the
/// footer.
fn walk(a: &[u8]) -> Option<Check> {
    let (blocks, ended) = blocks(a);
    if ended != Ended::Footer {
        return None;
    }
    let mut c = Check { walked: true, ..Check::default() };
    for (start, ty, data) in blocks {
        if mirrorable(ty) {
            c.targets.push((start, ty));
        } else if is_mirror(ty) {
            match mirror_at(a, start, ty, data, Some(HEADER_MAGIC.len())) {
                Some(m) => c.mirrors.push(m),
                None => c.broken += 1,
            }
        }
    }
    Some(c)
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

/// An archive repaired ([`repair`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repaired {
    pub bytes: Vec<u8>,
    /// The blocks restored from mirror blocks.
    pub restored: usize,
    /// Whether the header magic was restored (damaged, the mirror blocks telling the archive
    /// starts after it).
    pub header: bool,
    /// Whether the footer was made again (the archive cut short after its last metadata block,
    /// or its footer not to be trusted).
    pub footer: bool,
    /// The bytes lost, salvaging: before a block restored past the end (the archive cut short
    /// before it), the blocks of bytes not to be trusted ([`repair`]'s `erased`), now spacer
    /// blocks (or non-leaf blocks that do not verify), and the blocks of a segment cut short
    /// after the last metadata block whole, now cut off: the archive is to be salvaged
    /// ([`super::open_salvaging`]).
    pub lost: Vec<Range<usize>>,
    /// Of those, the blocks of a segment cut short, cut off: files whose names are not known
    /// are lost with them.
    pub cut_off: Option<Range<usize>>,
}

/// Whether ranges `a` and `b` overlap.
fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Ranges `of` but for those in `but`, each sorted and apart.
fn subtract(of: &[Range<usize>], but: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for r in of {
        let mut at = r.start;
        for b in but.iter().filter(|b| overlap(b, r)) {
            if b.start > at {
                out.push(at..b.start);
            }
            at = at.max(b.end);
        }
        if at < r.end {
            out.push(at..r.end);
        }
    }
    out
}

/// Appends a non-leaf data block of `n` bytes (even, at least 10) whose checksum does not
/// verify: in place of one lost, so that the non-leaf blocks after it keep their ordinals.
fn failing_non_leaf(out: &mut Vec<u8>, n: usize) {
    let (ss, width) = match n {
        0..=257 => (0u8, 1),
        258..=65_538 => (1, 2),
        65_539..=0xffff_ffff => (2, 4),
        _ => (3, 8),
    };
    let size = n - 1 - width;
    out.push(super::NON_LEAF_RAW as u8 | ss);
    out.extend_from_slice(&(size as u64).to_le_bytes()[..width]);
    let zeros = vec![0; size - 8];
    out.extend_from_slice(&zeros);
    out.extend_from_slice(&(xxh64(&zeros, 0) ^ 1).to_le_bytes());
}

/// Archive `a` repaired (spec, "Repair") by what [`check`] found of it (`c`), where anything
/// was:
///
/// - The header magic restored where the mirror blocks (found by their magic) tell the archive
///   starts after it.
/// - The target blocks not as an intact mirror block tells restored from the first of them (a
///   block past the end of `a` too, where it starts within it; where `salvage`, one starting
///   further too, the bytes from the end of the last block whole to it a spacer block).
/// - Bytes of `a` `erased` (an encrypted archive's chunks that do not authenticate, decrypted
///   without it): mirror blocks there not used; salvaging, the blocks there not restored lost,
///   each a spacer block (a non-leaf data block a non-leaf block that does not verify, for those
///   after it to keep their ordinals). The blocks are read through them only to tell where they
///   are, checked by reaching the start of a block a mirror block tells; where they do not, the
///   bytes up to the next such start are lost. The footer there is made again.
/// - Where the blocks no longer reach a footer at the end but are cut short after the last
///   metadata block (with only spacer blocks and blocks of non-critical types after it), the
///   footer made again after the last block whole (and the size hint block first, if any, set).
///   Salvaging, also where blocks of a later segment come after it (cut short before that
///   segment's metadata block): those blocks lost, cut off, and the footer made again after the
///   metadata block.
pub fn repair(a: &[u8], c: &Check, salvage: bool, erased: &[Range<usize>]) -> Option<Repaired> {
    // (read one after another up to a footer at the end, as all its blocks are as they are to be)
    if c.walked && erased.is_empty() && c.differing(a).next().is_none() {
        return None;
    }
    let mirrors: Vec<&Found> =
        c.mirrors.iter().filter(|m| !erased.iter().any(|e| overlap(e, &(m.at..m.end)))).collect();
    let mut v: Option<Vec<u8>> = None;
    let mut header = false;
    let mut headed = a.starts_with(&HEADER_MAGIC) || a.starts_with(&OVERLAY_HEADER_MAGIC);
    if !headed
        && a.len() >= HEADER_MAGIC.len()
        && !mirrors.is_empty()
        && mirrors.iter().all(|m| m.archive_start == HEADER_MAGIC.len())
    {
        let overlay = mirrors[0].overlay;
        let b = v.get_or_insert_with(|| a.to_vec());
        b[..6].copy_from_slice(if overlay { &OVERLAY_HEADER_MAGIC } else { &HEADER_MAGIC });
        (header, headed) = (true, true);
    }
    // (the bytes restored or found as a mirror block tells: to be trusted, erased or not)
    let mut trusted: Vec<Range<usize>> = Vec::new();
    let good: HashSet<usize> = mirrors
        .iter()
        .filter(|m| m.matches(a))
        .map(|m| {
            trusted.push(m.target..m.target_end());
            m.target
        })
        .collect();
    let mut restored = HashSet::new();
    let mut lost = Vec::new();
    for m in &mirrors {
        if good.contains(&m.target) || restored.contains(&m.target) {
            continue;
        }
        let b = v.get_or_insert_with(|| a.to_vec());
        if m.target > b.len() {
            if !(salvage && headed) {
                continue;
            }
            // (the blocks whole before the cut kept, the rest up to the block a spacer block)
            let (blocks, _) = blocks(b);
            let e = blocks.last().map_or(HEADER_MAGIC.len(), |l| l.2.end + (l.2.end - l.0) % 2);
            if e > m.target || (m.target - e) % 2 == 1 {
                continue;
            }
            b.truncate(e);
            if m.target > e {
                spacer(b, (m.target - e) as u64);
                // (nothing lost where only a padding byte was cut off)
                lost.push(e..m.target);
            }
        }
        let end = m.target_end();
        if b.len() < end {
            b.resize(end, 0);
        }
        let h = m.target + m.head.len();
        b[m.target..h].copy_from_slice(&m.head);
        b[h..end].copy_from_slice(&a[m.data.clone()]);
        // (the padding, as the spec restores it)
        if (end - m.target) % 2 == 1 && end < b.len() {
            b[end] = 0;
        }
        trusted.push(m.target..end);
        restored.insert(m.target);
    }
    trusted.sort_by_key(|r| r.start);
    let untrusted = subtract(erased, &trusted);
    let mut footer = false;
    if headed && salvage && !untrusted.is_empty() {
        let b = v.get_or_insert_with(|| a.to_vec());
        let known: Vec<usize> = {
            let mut k: Vec<usize> = mirrors.iter().flat_map(|m| [m.at, m.target]).collect();
            // (the footer, where the end tells it is and it is to be trusted)
            let n = b.len().checked_sub(2).map(|e| u16::from_le_bytes([b[e], b[e + 1]]) as usize);
            if let Some(f) = n.and_then(|n| b.len().checked_sub(10 + n))
                && (b[f..f + 6] == FOOTER_MAGIC || b[f..f + 6] == OVERLAY_FOOTER_MAGIC)
                && !untrusted.iter().any(|u| overlap(u, &(f..b.len())))
            {
                k.push(f);
            }
            k.sort_unstable();
            k.dedup();
            k
        };
        let (spans, cut_at) = erased_blocks(b, &untrusted, &known);
        for (r, non_leaf) in spans {
            // (an odd number of bytes only at the end, of a block cut short: its last byte zero)
            let n = r.len() & !1;
            let mut fill = Vec::with_capacity(r.len());
            match (non_leaf && n >= 10, n) {
                (true, _) => failing_non_leaf(&mut fill, n),
                (false, 0) => {}
                (false, _) => spacer(&mut fill, n as u64),
            }
            fill.resize(r.len(), 0);
            b[r.clone()].copy_from_slice(&fill);
            lost.push(r);
        }
        if let Some(f) = cut_at {
            b.truncate(f);
        }
    }
    let mut cut_off = None;
    if headed {
        let b = v.as_deref().unwrap_or(a);
        let (blocks, ended) = blocks(b);
        let last_meta = blocks.iter().rposition(|b| (0x10..=0x17).contains(&b.1));
        // (a block cut short at the end, of a critical type but a spacer block's or the footer's:
        // a content block of a segment cut short)
        let cut = blocks.last().map_or(HEADER_MAGIC.len(), |l| l.2.end + (l.2.end - l.0) % 2);
        let mut p = cut;
        let cut_content = varint::get(b, &mut p)
            .is_ok_and(|ty| ty > 0x03 && super::critical(ty) && ty != FOOTER_TYPE && ty != OVERLAY_FOOTER_TYPE);
        let after_ok = |i: usize| !cut_content && blocks[i + 1..].iter().all(|b| b.1 <= 0x03 || !super::critical(b.1));
        // (salvaging, an archive cut short in a segment after the last metadata block whole: that
        // segment's blocks lost, the footer made again after the metadata block)
        if ended == Ended::Cut
            && let Some(i) = last_meta.filter(|&i| salvage || after_ok(i))
        {
            let meta = blocks[i].0;
            let last = if after_ok(i) { blocks.last().unwrap() } else { &blocks[i] };
            let e = last.2.end + (last.2.end - last.0) % 2;
            let first = blocks.first().filter(|f| f.1 == super::SIZE_HINT && f.2.len() == 10).map(|f| f.2.start);
            let b = v.get_or_insert_with(|| a.to_vec());
            if b.len() > e && !after_ok(i) {
                lost.push(e..b.len());
                cut_off = Some(e..b.len());
            }
            // (its padding, where cut off, restored as a zero byte)
            b.resize(e, 0);
            let mut f = Vec::new();
            varint::put(&mut f, (e - HEADER_MAGIC.len()) as u64);
            varint::put(&mut f, (e + 8 - meta) as u64);
            let overlay = b.starts_with(&OVERLAY_HEADER_MAGIC);
            b.extend_from_slice(if overlay { &OVERLAY_FOOTER_MAGIC } else { &FOOTER_MAGIC });
            b.extend_from_slice(&(f.len() as u16).to_le_bytes());
            b.extend_from_slice(&f);
            b.extend_from_slice(&(f.len() as u16).to_le_bytes());
            // (a size hint block of a padded LEB128, as an encrypted archive's payload begins)
            if let Some(at) = first {
                let mut v = (e - HEADER_MAGIC.len()) as u64;
                for (i, x) in b[at..at + 10].iter_mut().enumerate() {
                    *x = (v & 0x7f) as u8 | if i < 9 { 0x80 } else { 0 };
                    v >>= 7;
                }
            }
            footer = true;
        }
    }
    if restored.is_empty() && !footer && !header && lost.is_empty() {
        return None;
    }
    Some(Repaired { bytes: v.expect("changed"), restored: restored.len(), header, footer, lost, cut_off })
}

/// The blocks of archive `a` (repaired or not: those before `cut` do not matter) from the start
/// of `cut` (the blocks of a segment cut short, [`Repaired::cut_off`]) as far as they are whole,
/// as an archive of their own without a metadata block, for its leaf blocks to be salvaged each
/// alone ([`super::open_salvaging`] and [`super::Archive::each_leaf`]): files of no name known.
/// Its blocks stop before the first with a byte `erased` (not to be trusted). None where no
/// leaf block is whole.
pub fn cut_segment(a: &[u8], cut: &Range<usize>, erased: &[Range<usize>]) -> Option<Vec<u8>> {
    let mut at = cut.start;
    let mut leaves = 0;
    while at < a.len() {
        let mut p = at;
        let Ok(ty) = varint::get(a, &mut p) else { break };
        if p - at != varint::len(ty) || ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE {
            break;
        }
        let width = 1 << (ty % 4);
        let Some(size) = a.get(p..p + width) else { break };
        let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
        let Some(end) = usize::try_from(size).ok().and_then(|n| (p + width).checked_add(n)) else { break };
        let end = end + (end - at) % 2;
        if end > a.len() || erased.iter().any(|e| overlap(e, &(at..end))) {
            break;
        }
        leaves += matches!(ty, 0x20..=0x37 | 0x70..=0x77) as usize;
        at = end;
    }
    if leaves == 0 {
        return None;
    }
    // (a standalone archive whatever `a` is: its leaves are evaluated alone)
    let mut out = HEADER_MAGIC.to_vec();
    out.extend_from_slice(&a[cut.start..at]);
    super::footer(&mut out, at - cut.start);
    Some(out)
}

/// Blocks (or bytes) where they are, padding too, and whether each is a non-leaf data block.
type Spans = Vec<(Range<usize>, bool)>;

/// The blocks of archive `a` that bytes `untrusted` (sorted, apart) make lost (where each is,
/// its padding too, and whether it is a non-leaf data block), and where the footer starts if it
/// is among them. The blocks are read one after another; those from one whose type or size is
/// among them on are not trusted until the reading reaches the start of a block that `known`
/// (sorted: of mirror blocks and their targets, and the footer) has; where it passes one, fails,
/// or ends first, the bytes from that block to the next known start (or the end) are lost as one.
fn erased_blocks(a: &[u8], untrusted: &[Range<usize>], known: &[usize]) -> (Spans, Option<usize>) {
    let bad = |r: &Range<usize>| untrusted.iter().any(|u| overlap(u, r));
    let next_known = |from: usize| known.iter().copied().find(|&k| k > from);
    let mut lost: Spans = Vec::new();
    // (the blocks read since one of untrusted type or size, and where it starts)
    let mut suspect: Option<(Spans, usize)> = None;
    let mut at = HEADER_MAGIC.len();
    loop {
        if suspect.is_some() && known.binary_search(&at).is_ok() {
            // (reaching a block known: those read since as they tell)
            let (read, _) = suspect.take().unwrap();
            lost.extend(read.into_iter().filter(|(r, _)| bad(r)));
        }
        // (the reading gone astray: from where it is not trusted to the next block known)
        let astray = match &suspect {
            Some((_, s)) => next_known(*s).is_none_or(|k| k < at) || at >= a.len(),
            None => false,
        };
        let block = (!astray && at < a.len()).then(|| {
            let mut p = at;
            let ty = varint::get(a, &mut p).ok().filter(|&ty| p - at == varint::len(ty))?;
            if ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE {
                return Some((ty, p, at));
            }
            let width = 1 << (ty % 4);
            let size = a.get(p..p + width)?;
            let size =
                usize::try_from(u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)))).ok()?;
            let end = (p + width).checked_add(size).filter(|&e| e <= a.len())?;
            Some((ty, p + width, end + (end - at) % 2))
        });
        match block {
            None if suspect.is_none() && at >= a.len() => return (lost, None),
            Some(Some((ty, _, _))) if (ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE) && suspect.is_none() => {
                return (lost, bad(&(at..a.len())).then_some(at));
            }
            Some(Some((ty, data, end))) if !(ty == FOOTER_TYPE || ty == OVERLAY_FOOTER_TYPE) => {
                let b = (at..end.min(a.len()), (0x40..=0x4b).contains(&ty));
                match &mut suspect {
                    Some((read, _)) => read.push(b),
                    None if bad(&(at..data)) => suspect = Some((vec![b], at)),
                    None if bad(&b.0) => lost.push(b),
                    None => {}
                }
                at = end;
            }
            // (not readable where nothing is to be distrusted: left for the reading to fail on)
            Some(None) if suspect.is_none() && !bad(&(at..(at + 10).min(a.len()))) => return (lost, None),
            _ => {
                let s = suspect.take().map_or(at, |(_, s)| s);
                match next_known(s).filter(|&k| k <= a.len()) {
                    Some(k) => {
                        lost.push((s..k, false));
                        at = k;
                    }
                    None => {
                        lost.push((s..a.len(), false));
                        return (lost, None);
                    }
                }
            }
        }
    }
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
    /// a module block and three formula blocks to mirror).
    fn archive() -> Vec<u8> {
        segment(&[0; 3], false, None)
    }

    /// Bytes that do not compress.
    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect()
    }

    /// [`archive`], the files of `pads[k]` bytes of noise more each, with their checksums where
    /// `sums`; where `front` is given, the metadata block's mirror block before the files from
    /// that one on ([`SegmentWriter::finish_mirrored`]).
    fn segment(pads: &[usize; 3], sums: bool, front: Option<usize>) -> Vec<u8> {
        let mut f = Formula::default();
        let datas: Vec<Vec<u8>> = (0..3).map(|k| [NAMES[k].as_bytes(), &noise(pads[k], k as u64)].concat()).collect();
        let lits: Vec<_> = (0..3).map(|k| f.push(NAMES[k], datas[k].clone())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let mut t = Tree::default();
        let mut contents = Vec::new();
        let mut at_front = None;
        for (k, a) in lits.iter().enumerate() {
            if front == Some(k) {
                at_front = Some(out.len());
            }
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
        match at_front {
            None => out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0),
            Some(p) => {
                let (mirror, end) = w
                    .finish_mirrored(&enc, &t, &contents, p, false, crate::aqr::Status::Complete)
                    .unwrap()
                    .expect("far enough");
                out.splice(p..p, mirror);
                out.extend_from_slice(&end);
            }
        }
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
        let a = archive();
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
        let m = mirrored(&archive(), 4096, false);
        let c = check(&m);
        for t in &c.mirrors {
            let mut d = m.clone();
            d[t.target_end() - 3] ^= 0x55;
            let cd = check(&d);
            assert!(cd.walked);
            assert_eq!(cd.differing(&d).count(), 1);
            let r = repair(&d, &cd, false, &[]).unwrap();
            assert_eq!((r.restored, r.footer), (1, false));
            assert_eq!(r.bytes, m);
        }
        let meta = c.mirrors.last().unwrap();
        let mut d = m.clone();
        // (its size field: the metadata block runs past the archive)
        d[meta.target + 1] = 0xff;
        d[meta.target + 4] ^= 1;
        let cd = check(&d);
        // (read one after another, the spacer block's zeros resynchronising the blocks)
        assert!(cd.walked && cd.mirrors.len() == 5);
        let r = repair(&d, &cd, false, &[]).unwrap();
        assert_eq!((r.restored, &r.bytes), (1, &m));
        assert_eq!(files(&r.bytes), files(&m));
        // the header damaged too: found by their magic, the archive starting where they tell, its
        // header magic restored
        d[0] ^= 1;
        let cd = check(&d);
        assert!(!cd.walked && cd.mirrors.len() == 5);
        let r = repair(&d, &cd, false, &[]).unwrap();
        assert!(r.restored == 1 && r.header);
        assert_eq!(r.bytes, m);
    }

    /// A damaged mirror block is told and not used; the archive is read as it is.
    #[test]
    fn tells_damaged_mirrors() {
        let m = mirrored(&archive(), 4096, false);
        let c = check(&m);
        let mut d = m.clone();
        let x = &c.mirrors[1];
        d[x.data.start + 1] ^= 1;
        let cd = check(&d);
        assert_eq!((cd.mirrors.len(), cd.broken, cd.unmirrored()), (4, 1, 1));
        assert!(!cd.mirrored(&d, false));
        assert_eq!(repair(&d, &cd, false, &[]), None);
        assert_eq!(files(&d), files(&m));
    }

    /// The metadata block's mirror block before the last files (its target following it), and
    /// none after them; a truncation into the metadata block restores it, and the footer.
    #[test]
    fn mirrors_the_metadata_block_before() {
        let a = segment(&[0, 3000, 3000], false, Some(1));
        let m = mirrored(&a, 4096, false);
        assert_eq!(files(&m), files(&segment(&[0, 3000, 3000], false, None)));
        let c = check(&m);
        assert!(c.walked && c.mirrored(&m, false), "{c:?}");
        let meta = c.targets.last().unwrap().0;
        let front: Vec<&Found> = c.mirrors.iter().filter(|f| f.target == meta).collect();
        assert!(front.len() == 1 && front[0].at < meta && front[0].far(false));
        // (cut in the metadata block: restored past the end, the footer made again)
        for cut in [meta + 3, meta, m.len() - 5] {
            let d = &m[..cut];
            let cd = check(d);
            assert!(!cd.walked);
            let r = repair(d, &cd, false, &[]).unwrap();
            assert_eq!((r.restored, r.footer), ((cut <= meta + 3) as usize, true));
            assert_eq!(files(&r.bytes), files(&m));
        }
        // (cut before the metadata block: not repaired, as files' blocks are lost)
        let d = &m[..meta - 1];
        assert!(repair(d, &check(d), false, &[]).is_none_or(|r| !r.footer));
        // in the archive without it, a truncation into the metadata block is lost
        let m = mirrored(&segment(&[0, 3000, 3000], false, None), 4096, false);
        let meta = check(&m).targets.last().unwrap().0;
        let d = &m[..meta + 3];
        assert_eq!(repair(d, &check(d), false, &[]), None);
    }

    /// A truncation before the metadata block, salvaged: the metadata block restored past the
    /// bytes lost (a spacer block now), the files before them given back, those after lost.
    #[test]
    fn salvages() {
        let full = segment(&[0, 3000, 3000], false, Some(1));
        let m = mirrored(&full, 4096, false);
        let want = files(&m);
        let c = check(&m);
        let meta = c.targets.last().unwrap().0;
        // (the last formula block's: the third file, its blocks after it; cut in it, in its
        // non-leaf block, and just before the metadata block)
        let third = c.targets.iter().rev().nth(1).unwrap().0;
        for cut in [third + 1, third + 40, meta - 1] {
            let d = &m[..cut];
            assert!(repair(d, &check(d), false, &[]).is_none_or(|r| r.lost.is_empty() && !r.footer));
            let r = repair(d, &check(d), true, &[]).unwrap();
            assert!(r.restored == 1 && r.footer && r.lost.len() == 1 && r.lost[0].end == meta, "{cut}");
            // (not an archive as the spec reads it: a file at no block, or a formula block referring
            // to a block not there)
            assert!(decode_value(&r.bytes, &natives()).is_err());
            let natives = natives();
            let a = super::super::open_salvaging(&r.bytes, &natives, false).unwrap();
            let got = std::sync::Mutex::new(vec![None; 3]);
            let done = a.contents(1, |k, d| {
                got.lock().unwrap()[k] = Some(d);
                Ok(())
            });
            assert!(done.is_err());
            let got = got.into_inner().unwrap();
            assert_eq!(got[..2], [Some(want[0].clone()), Some(want[1].clone())], "{cut}");
            assert_eq!(got[2], None);
        }
    }

    /// An archive of two segments (the first file's, then the others'), cut short in the second
    /// before its metadata block: not repaired, but salvaged as the first segment alone (the
    /// second's blocks lost, the footer made again after the first metadata block).
    #[test]
    fn salvages_segments() {
        let mut f = Formula::default();
        let lits: Vec<_> = (0..3).map(|k| f.push(NAMES[k], NAMES[k].as_bytes().to_vec())).collect();
        let enc = Encoder::new(Level::Zstd(3));
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let (mut t, mut contents) = (Tree::default(), Vec::new());
        let mut second = 0;
        for (k, a) in lits.iter().enumerate() {
            let len = NAMES[k].len() as u64;
            let leaves =
                vec![Leaf { outputs: vec![Node::apply(REV, vec![1, 2, 3], vec![a.clone()], len)], sums: vec![None] }];
            let batch = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&batch);
            out.extend_from_slice(&batch.bytes);
            contents.push(ContentAt { leaf: batch.leaf_at[0], output: 0, size: len });
            let name = format!("f{k}").into_bytes();
            t.entries.push(Entry {
                parent: None,
                name,
                node: EntryNode::File(contents.len() - 1),
                extra: Extra::default(),
            });
            if k == 0 {
                out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
                second = out.len();
                (t, contents) = (Tree::default(), Vec::new());
            }
        }
        out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        let rev = |s: &str| s.bytes().rev().collect::<Vec<u8>>();
        assert_eq!(files(&out), [rev(NAMES[0]), rev(NAMES[1]), rev(NAMES[2])]);
        let metas = blocks(&out).0.iter().filter(|b| (0x10..=0x17).contains(&b.1)).count();
        assert_eq!(metas, 2);
        let natives = natives();
        assert_eq!(super::super::open(&out, &natives).unwrap().status(), Some(crate::aqr::Status::Complete));
        // (cut just after the first metadata block: repaired as an archive of the first segment,
        // which does not tell it was meant to be the last)
        let d = &out[..second];
        let r = repair(d, &check(d), false, &[]).unwrap();
        assert!(r.footer && r.lost.is_empty() && r.cut_off.is_none());
        assert_eq!(super::super::open(&r.bytes, &natives).unwrap().status(), Some(crate::aqr::Status::None));
        for cut in [second + 1, out.len() - 20] {
            let d = &out[..cut];
            assert!(repair(d, &check(d), false, &[]).is_none_or(|r| !r.footer));
            let r = repair(d, &check(d), true, &[]).unwrap();
            assert!(r.footer && r.lost.len() == 1 && r.lost[0] == (second..cut), "{cut}: {r:?}");
            assert_eq!(r.cut_off, Some(second..cut));
            // the second segment's leaves whole, each alone: a file of no name each (of the
            // cut in the second file's blocks, none)
            let c = cut_segment(d, &(second..cut), &[]);
            let leaves = std::sync::Mutex::new(Vec::new());
            if let Some(c) = &c {
                let a = super::super::open_salvaging(c, &natives, false).unwrap();
                a.each_leaf(1, |i, r| {
                    leaves.lock().unwrap().push((i, r.ok()));
                    Ok(())
                })
                .unwrap();
            }
            let mut leaves = leaves.into_inner().unwrap();
            leaves.sort_by_key(|l| l.0);
            let given: Vec<Vec<Vec<u8>>> = leaves.into_iter().filter_map(|l| l.1).collect();
            match cut {
                c if c == second + 1 => assert!(given.is_empty()),
                _ => assert_eq!(given, [vec![rev(NAMES[1])], vec![rev(NAMES[2])]]),
            }
            assert_eq!(files(&r.bytes), [rev(NAMES[0])]);
        }
    }

    /// Bytes not to be trusted (an encrypted archive's chunk that does not authenticate): a block
    /// there restored from its mirror block; a non-leaf block there lost (one that does not verify
    /// in its place), its file with it; its type or size there too, the blocks up to the next one
    /// a mirror block tells lost.
    #[test]
    fn salvages_erased() {
        let m = mirrored(&segment(&[0, 3000, 3000], false, Some(1)), 4096, false);
        let want = files(&m);
        let c = check(&m);
        let meta = c.targets.last().unwrap().0;
        let salvaged = |d: &[u8], erased: &[Range<usize>]| {
            let r = repair(d, &check(d), true, erased).unwrap();
            let natives = natives();
            let a = super::super::open_salvaging(&r.bytes, &natives, false).unwrap();
            let got = std::sync::Mutex::new(vec![None; 3]);
            let done = a.contents(1, |k, d| {
                got.lock().unwrap()[k] = Some(d);
                Ok(())
            });
            (r, done.is_ok(), got.into_inner().unwrap())
        };
        // the metadata block, garbled: restored from its mirror block before
        let mut d = m.clone();
        let e = meta..meta + 40;
        d[e.clone()].iter_mut().for_each(|b| *b ^= 0xa5);
        let (r, ok, got) = salvaged(&d, std::slice::from_ref(&e));
        assert!(r.restored == 1 && r.lost.is_empty() && ok);
        assert_eq!(got, want.iter().cloned().map(Some).collect::<Vec<_>>());
        // the last non-leaf block's data, garbled: lost, the third file with it
        let (start, _, data) = blocks(&m).0.into_iter().rfind(|b| (0x40..0x4c).contains(&b.1)).unwrap();
        let mut d = m.clone();
        d[data.start + 100..data.start + 200].iter_mut().for_each(|b| *b ^= 0xa5);
        let e = data.start + 64..data.start + 300;
        let (r, ok, got) = salvaged(&d, std::slice::from_ref(&e));
        assert!(r.restored == 0 && r.lost.len() == 1 && r.lost[0].start == start && !ok);
        assert!(r.bytes[start] == 0x41 && r.bytes[start..].len() > data.end - start);
        assert_eq!(got[..2], [Some(want[0].clone()), Some(want[1].clone())]);
        assert_eq!(got[2], None);
        // its size garbled too: the blocks read astray, those up to the metadata block lost
        let mut d = m.clone();
        d[start + 1..start + 3].copy_from_slice(&[0x10, 0]);
        let e = start..start + 10;
        let (r, ok, got) = salvaged(&d, std::slice::from_ref(&e));
        assert!(r.lost.len() == 1 && r.lost[0] == (start..meta) && !ok, "{:?}", r.lost);
        assert_eq!(got[..2], [Some(want[0].clone()), Some(want[1].clone())]);
    }

    /// A mirror block of an archive within the archive (as a file of it, stored) is not taken
    /// as one of the archive's.
    #[test]
    fn leaves_inner_archives_alone() {
        let inner = mirrored(&archive(), 4096, false);
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
        let of = |sums| super::super::open(&segment(&[0; 3], sums, None), &natives).unwrap().archival().unwrap();
        assert_eq!(of(false), super::super::Archival { unchecked_leaves: 0, unchecked_formulas: 3 });
        assert!(of(true).conforms());
    }

    /// A block whose mirror block would not fit its size field has a widened one, its archive
    /// offset odd; a block following its mirror block has an odd target offset.
    #[test]
    fn widens_and_follows() {
        let data = vec![7; 250];
        let mut out = HEADER_MAGIC.to_vec();
        out.extend_from_slice(&[0x70, 250]);
        out.extend_from_slice(&data);
        let at = out.len() as u64;
        mirror_block(&mut out, &MIRROR_MAGIC, at, 6, 0x70, &data);
        assert_eq!(&out[at as usize..at as usize + 2], &[0xf1, 0x18]);
        let read = |out: &[u8], at: usize| {
            let mut p = at;
            let ty = varint::get(out, &mut p).unwrap();
            let width = 1 << (ty % 4);
            let n = u64::from_le_bytes(std::array::from_fn(|i| if i < width { out[p + i] } else { 0 })) as usize;
            let d = &out[p + width..p + width + n];
            let mut q = 8;
            let fields = (varint::get(d, &mut q).unwrap(), varint::get(d, &mut q).unwrap());
            (mirror_at(out, at, ty, p + width..p + width + n, Some(6)).unwrap(), fields)
        };
        let (f, (archive_offset, target_offset)) = read(&out, at as usize);
        assert!(f.target == 6 && f.ty == 0x70 && f.matches(&out));
        assert_eq!((archive_offset, target_offset), (at - 6 + 1, at - 6));
        // (the block after its mirror block: odd)
        let mut out = HEADER_MAGIC.to_vec();
        out.extend_from_slice(&mirror_of(&[0x70, 3, 1, 2, 3], 6 + 30, 6, false));
        let pad = 6 + 30 - out.len();
        out.extend_from_slice(&vec![0; pad]);
        out.extend_from_slice(&[0x70, 3, 1, 2, 3]);
        let (f, (archive_offset, target_offset)) = read(&out, 6);
        assert!(f.target == 36 && f.matches(&out));
        assert_eq!((archive_offset, target_offset), (0, 31));
    }
}
