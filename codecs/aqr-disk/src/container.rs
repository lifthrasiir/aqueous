//! Containers of virtual disks whose data blocks are stored as they are, a block table telling
//! where (`docs/disk-image-design.md`, stage 4): VHD (dynamic), VHDX, VMDK (a hosted sparse
//! extent), qcow2 and VDI, read from their published descriptions (Microsoft's VHD specification,
//! [MS-VHDX], VMware's Virtual Disk Format 1.1, QEMU's `qcow2.txt`, libyal's notes on VDI). The
//! virtual disk is a [`Map`]: runs of it and where each is in the file, the others (not
//! allocated, of zeros, compressed) read as zeros. A fixed VHD and a flat VMDK extent are the disk
//! as it is, a plain image.
//!
//! Not read: differencing disks and those of a parent or backing file (what they do not hold is
//! their parent's), compressed and encrypted ones (qcow2's compressed clusters are read as zeros:
//! stage 8), VHDX's log (a dirty one's blocks as its table tells), VHD's and VHDX's sector bitmaps
//! (a block allocated is taken whole).

use std::borrow::Cow;
use std::ops::Range;

use crate::image::Image;

/// Most entries of a block table read: a block table of more is no table of a disk of an image
/// this big anyway.
fn most(d: &[u8], block: u64) -> u64 {
    (d.len() as u64 / block.max(1)).saturating_mul(64).saturating_add(1 << 16)
}

fn be32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}

fn be64(b: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(b[p..p + 8].try_into().unwrap())
}

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

fn le64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

/// The kind of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Vhd,
    Vhdx,
    Vmdk,
    Qcow2,
    Vdi,
    /// A UDIF image (`.dmg`) of raw chunks (and zeros) only.
    Dmg,
}

/// A virtual disk in file `file`: its bytes, of runs in order (each a range of the disk and where
/// it starts in the file, None of zeros).
pub struct Map<'a> {
    pub kind: Kind,
    file: &'a [u8],
    len: usize,
    runs: Vec<(Range<usize>, Option<usize>)>,
}

impl<'a> Map<'a> {
    /// The map of blocks of `block` bytes of a disk of `len`, block `i` at `at(i)` of the file (None
    /// of zeros), for blocks below `count`.
    fn of(
        kind: Kind,
        file: &'a [u8],
        len: u64,
        block: u64,
        count: u64,
        at: impl Fn(u64) -> Option<u64>,
    ) -> Option<Map<'a>> {
        let len = usize::try_from(len).ok()?;
        let block = usize::try_from(block).ok().filter(|&b| b > 0)?;
        let mut runs: Vec<(Range<usize>, Option<usize>)> = Vec::new();
        let mut start = 0usize;
        for i in 0..count {
            if start >= len {
                break;
            }
            let end = start.saturating_add(block).min(len);
            let place = at(i).and_then(|a| usize::try_from(a).ok()).filter(|&a| a < file.len());
            match (runs.last_mut(), place) {
                (Some((r, Some(p))), Some(q)) if *p + r.len() == q => r.end = end,
                (Some((r, None)), None) => r.end = end,
                _ => runs.push((start..end, place)),
            }
            start = end;
        }
        if start < len {
            runs.push((start..len, None));
        }
        Some(Map { kind, file, len, runs })
    }

    /// The runs overlapping range `r` of the disk, from the first.
    fn overlapping(&self, r: &Range<usize>) -> impl Iterator<Item = &(Range<usize>, Option<usize>)> {
        let first = self.runs.partition_point(|(s, _)| s.end <= r.start);
        let end = r.end;
        self.runs[first..].iter().take_while(move |(s, _)| s.start < end)
    }

    /// Where the bytes of range `r` of the disk are in the file, in order: those of zeros, and
    /// past the file's end, left out.
    pub fn translate(&self, r: Range<usize>) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (s, at) in self.overlapping(&r) {
            let Some(at) = *at else { continue };
            let (a, b) = (r.start.max(s.start), r.end.min(s.end));
            let (from, to) = (at + (a - s.start), (at + (b - s.start)).min(self.file.len()));
            if from >= to {
                continue;
            }
            match out.last_mut() {
                Some(l) if l.end == from => l.end = to,
                _ => out.push(from..to),
            }
        }
        out
    }
}

impl Image for Map<'_> {
    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, r: Range<usize>) -> Option<Cow<'_, [u8]>> {
        if r.start > r.end || r.end > self.len {
            return None;
        }
        let mut runs = self.overlapping(&r).peekable();
        // (all of it of one run in the file: lent)
        if let Some((s, Some(at))) = runs.peek()
            && s.start <= r.start
            && r.end <= s.end
        {
            let from = at + (r.start - s.start);
            if let Some(b) = self.file.get(from..from + r.len()) {
                return Some(Cow::Borrowed(b));
            }
        }
        let mut out = vec![0u8; r.len()];
        for (s, at) in runs {
            let Some(at) = *at else { continue };
            let (a, b) = (r.start.max(s.start), r.end.min(s.end));
            let from = at + (a - s.start);
            let src = &self.file[from.min(self.file.len())..(from + (b - a)).min(self.file.len())];
            out[a - r.start..a - r.start + src.len()].copy_from_slice(src);
        }
        Some(Cow::Owned(out))
    }
}

/// The virtual disk of container `d`, where it is one of those read.
pub fn open(d: &[u8]) -> Option<Map<'_>> {
    vhdx(d).or_else(|| vhd(d)).or_else(|| vmdk(d)).or_else(|| qcow2(d)).or_else(|| vdi(d)).or_else(|| dmg(d))
}

/// A UDIF image of raw chunks and zeros (UDRO, UDRW with a trailer): the chunks are where the
/// disk's bytes are (a compressed one is none of these: `crate::dmg`).
fn dmg(d: &[u8]) -> Option<Map<'_>> {
    let u = crate::dmg::parse(d)?;
    let mut chunks: Vec<&crate::dmg::Chunk> = u.chunks.iter().collect();
    chunks.sort_by_key(|c| c.disk.start);
    let len = usize::try_from(u.len).ok()?;
    let mut runs: Vec<(Range<usize>, Option<usize>)> = Vec::new();
    let mut at = 0usize;
    for c in chunks {
        let (s, e) = (usize::try_from(c.disk.start).ok()?, usize::try_from(c.disk.end).ok()?);
        if s < at {
            return None;
        }
        if s > at {
            runs.push((at..s, None));
        }
        match c.how {
            crate::dmg::How::Raw if c.file.len() == e - s => runs.push((s..e, Some(c.file.start))),
            crate::dmg::How::Zeros => runs.push((s..e, None)),
            _ => return None,
        }
        at = e;
    }
    if at < len {
        runs.push((at..len, None));
    }
    Some(Map { kind: Kind::Dmg, file: d, len, runs })
}

/// A dynamic VHD: its footer (the copy at the start), its dynamic header and block allocation
/// table (big-endian sectors of blocks, each a sector bitmap then its data).
fn vhd(d: &[u8]) -> Option<Map<'_>> {
    let f = d.get(..512)?;
    if &f[..8] != b"conectix" || be32(f, 60) != 3 {
        return None;
    }
    let size = be64(f, 48);
    let h = usize::try_from(be64(f, 16)).ok().and_then(|p| d.get(p..p.checked_add(1024)?))?;
    if &h[..8] != b"cxsparse" {
        return None;
    }
    let (table, entries, block) = (be64(h, 16), be32(h, 28) as u64, be32(h, 32) as u64);
    if block < 512 || !block.is_power_of_two() {
        return None;
    }
    let bitmap = (block / 512).div_ceil(8).div_ceil(512) * 512;
    let count = entries.min(most(d, block));
    let bat = usize::try_from(table).ok()?;
    let entry = |i: u64| -> Option<u64> {
        let p = bat.checked_add(4 * i as usize)?;
        let s = be32(d.get(p..p + 4)?, 0);
        (s != u32::MAX).then(|| s as u64 * 512 + bitmap)
    };
    Map::of(Kind::Vhd, d, size, block, count, entry)
}

/// GUIDs of VHDX (as stored: the first three fields little-endian).
const BAT: [u8; 16] = guid(0x2dc27766, 0xf623, 0x4200, [0x9d, 0x64, 0x11, 0x5e, 0x9b, 0xfd, 0x4a, 0x08]);
const METADATA: [u8; 16] = guid(0x8b7ca206, 0x4790, 0x4b9a, [0xb8, 0xfe, 0x57, 0x5f, 0x05, 0x0f, 0x88, 0x6e]);
const FILE_PARAMETERS: [u8; 16] = guid(0xcaa16737, 0xfa36, 0x4d43, [0xb3, 0xb6, 0x33, 0xf0, 0xaa, 0x44, 0xe7, 0x6b]);
const DISK_SIZE: [u8; 16] = guid(0x2fa54224, 0xcd1b, 0x4876, [0xb2, 0x11, 0x5d, 0xbe, 0xd8, 0x3b, 0xf4, 0xb8]);
const SECTOR_SIZE: [u8; 16] = guid(0x8141bf1d, 0xa96f, 0x4709, [0xba, 0x47, 0xf2, 0x33, 0xa8, 0xfa, 0xab, 0x5f]);

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let (a, b, c) = (a.to_le_bytes(), b.to_le_bytes(), c.to_le_bytes());
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// A VHDX file: its region table (of the BAT and the metadata), the metadata's file parameters
/// (block size; no parent), the disk's size and its logical sectors; the BAT's entries of payload
/// blocks (a sector bitmap's after every chunk of them), those fully or partly present taken.
fn vhdx(d: &[u8]) -> Option<Map<'_>> {
    if d.get(..8)? != b"vhdxfile" {
        return None;
    }
    // (of the two region tables, the first that reads)
    let regions = [192 << 10, 256 << 10].into_iter().find_map(|at: usize| {
        let t = d.get(at..at + (64 << 10)).filter(|t| &t[..4] == b"regi")?;
        let n = (le32(t, 8) as usize).min(2047);
        let (mut bat, mut meta) = (None, None);
        for e in t[16..16 + 32 * n].as_chunks::<32>().0 {
            let span = (le64(e, 16), le32(e, 24) as u64);
            match &e[..16] {
                g if g == BAT => bat = Some(span),
                g if g == METADATA => meta = Some(span),
                _ => {}
            }
        }
        Some((bat?, meta?))
    })?;
    let ((bat, bat_len), (meta, meta_len)) = regions;
    let m = d.get(usize::try_from(meta).ok()?..usize::try_from(meta.checked_add(meta_len)?).ok()?)?;
    if m.len() < 32 || &m[..8] != b"metadata" {
        return None;
    }
    let item = |id: [u8; 16]| -> Option<&[u8]> {
        let n = (le16(m, 10) as usize).min(2047);
        let e = m.get(32..32 + 32 * n)?.as_chunks::<32>().0.iter().find(|e| e[..16] == id)?;
        let (at, len) = (le32(e, 16) as usize, le32(e, 20) as usize);
        m.get(at..at.checked_add(len)?)
    };
    let params = item(FILE_PARAMETERS).filter(|p| p.len() >= 8)?;
    let (block, has_parent) = (le32(params, 0) as u64, le32(params, 4) & 2 != 0);
    let size = le64(item(DISK_SIZE).filter(|p| p.len() >= 8)?, 0);
    let sector = le32(item(SECTOR_SIZE).filter(|p| p.len() >= 4)?, 0) as u64;
    if has_parent
        || !block.is_power_of_two()
        || !(1 << 20..=256 << 20).contains(&block)
        || !matches!(sector, 512 | 4096)
    {
        return None;
    }
    let chunk = (1u64 << 23) * sector / block;
    let count = size.div_ceil(block).min(most(d, block));
    let table = usize::try_from(bat).ok()?;
    let entry = |i: u64| -> Option<u64> {
        let k = i.checked_add(i / chunk)?;
        if k.checked_mul(8)? + 8 > bat_len {
            return None;
        }
        let p = table.checked_add(usize::try_from(8 * k).ok()?)?;
        let e = le64(d.get(p..p + 8)?, 0);
        matches!(e & 7, 6 | 7).then_some(e >> 20 << 20)
    };
    Map::of(Kind::Vhdx, d, size, block, count, entry)
}

/// A VMDK hosted sparse extent: its header, grain directory and grain tables (sectors of grains;
/// 0 not allocated, 1 of zeros). Not of compressed grains (streamOptimized: stage 8).
fn vmdk(d: &[u8]) -> Option<Map<'_>> {
    let h = d.get(..512)?;
    if &h[..4] != b"KDMV" || !(1..=3).contains(&le32(h, 4)) {
        return None;
    }
    let (flags, capacity, grain, per_gt, gd) = (le32(h, 8), le64(h, 12), le64(h, 20), le32(h, 44) as u64, le64(h, 56));
    if flags & 0x10000 != 0 || gd == u64::MAX || grain == 0 || !grain.is_power_of_two() || !(1..=4096).contains(&per_gt)
    {
        return None;
    }
    let block = grain.checked_mul(512)?;
    let count = capacity.div_ceil(grain).min(most(d, block));
    let gd = usize::try_from(gd.checked_mul(512)?).ok()?;
    let entry = |i: u64| -> Option<u64> {
        let (t, k) = (i / per_gt, i % per_gt);
        let p = gd.checked_add(usize::try_from(4 * t).ok()?)?;
        let gt = le32(d.get(p..p + 4)?, 0) as u64;
        let p = usize::try_from(gt.checked_mul(512)?.checked_add(4 * k)?).ok()?;
        let s = le32(d.get(p..p + 4)?, 0) as u64;
        (gt != 0 && s > 1).then(|| s * 512)
    };
    Map::of(Kind::Vmdk, d, capacity.checked_mul(512)?, block, count, entry)
}

/// A qcow2 image (versions 2 and 3): its header, L1 and L2 tables (clusters as they are; those
/// compressed, of zeros or not allocated read as zeros). Not of a backing file, encryption, an
/// external data file or extended L2 entries.
fn qcow2(d: &[u8]) -> Option<Map<'_>> {
    let h = d.get(..104)?;
    if &h[..4] != b"QFI\xfb" || !matches!(be32(h, 4), 2 | 3) {
        return None;
    }
    let (backing, bits, size, crypt, l1_size, l1) =
        (be64(h, 8), be32(h, 20), be64(h, 24), be32(h, 32), be32(h, 36), be64(h, 40));
    let incompatible = if be32(h, 4) == 3 { be64(h, 72) } else { 0 };
    if backing != 0 || crypt != 0 || incompatible & !0b11 != 0 || !(9..=21).contains(&bits) {
        return None;
    }
    let cluster = 1u64 << bits;
    let per_l2 = cluster / 8;
    let count = size.div_ceil(cluster).min(most(d, cluster));
    let l1 = usize::try_from(l1).ok()?;
    let mask = 0x00ff_ffff_ffff_fe00u64;
    let entry = |i: u64| -> Option<u64> {
        let (t, k) = (i / per_l2, i % per_l2);
        if t >= l1_size as u64 {
            return None;
        }
        let p = l1.checked_add(usize::try_from(8 * t).ok()?)?;
        let l2 = be64(d.get(p..p + 8)?, 0) & mask;
        if l2 == 0 {
            return None;
        }
        let p = usize::try_from(l2.checked_add(8 * k)?).ok()?;
        let e = be64(d.get(p..p + 8)?, 0);
        // (compressed, or of zeros: as zeros)
        let at = e & mask;
        (e & 1 << 62 == 0 && e & 1 == 0 && at != 0).then_some(at)
    };
    Map::of(Kind::Qcow2, d, size, cluster, count, entry)
}

/// The chunks of a qcow2 image of any compressed cluster (`crate::chunked`), as `qcow2` reads it
/// otherwise: its clusters by its L2 tables, those allocated raw, compressed ones raw DEFLATE
/// streams of zlib's (compression type 0; qcow2.txt's descriptor: the offset and the sectors from
/// it, which may hold the next one's start: qemu packs them by bytes), the others zeros (an L2 table
/// not allocated, its clusters at once). Runs of raw clusters one after another in the disk and
/// the file, and of zeros, are a chunk each. None where no cluster is compressed.
pub fn qcow2_chunks(d: &[u8]) -> Option<crate::chunked::Chunked> {
    use crate::chunked::{Chunk, How};
    let h = d.get(..104)?;
    if &h[..4] != b"QFI\xfb" || !matches!(be32(h, 4), 2 | 3) {
        return None;
    }
    let (backing, bits, size, crypt, l1_size, l1) =
        (be64(h, 8), be32(h, 20), be64(h, 24), be32(h, 32), be32(h, 36) as u64, be64(h, 40));
    let incompatible = if be32(h, 4) == 3 { be64(h, 72) } else { 0 };
    if backing != 0 || crypt != 0 || incompatible & !0b11 != 0 || !(9..=21).contains(&bits) {
        return None;
    }
    let (cluster, x) = (1u64 << bits, 62 - (bits - 8));
    let per_l2 = cluster / 8;
    let count = size.div_ceil(cluster);
    if l1_size.checked_mul(per_l2)? < count {
        return None;
    }
    let mask = 0x00ff_ffff_ffff_fe00u64;
    let l1 = usize::try_from(l1).ok()?;
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut push = |c: Chunk| match chunks.last_mut() {
        Some(l)
            if l.how == c.how
                && l.disk.end == c.disk.start
                && matches!(c.how, How::Zeros | How::Raw)
                && (c.how == How::Zeros || l.file.end == c.file.start) =>
        {
            l.disk.end = c.disk.end;
            l.file.end = c.file.end;
        }
        _ => chunks.push(c),
    };
    let mut compressed = false;
    for t in 0..count.div_ceil(per_l2) {
        let p = l1.checked_add(usize::try_from(8 * t).ok()?)?;
        let l2 = be64(d.get(p..p + 8)?, 0) & mask;
        let first = t * per_l2;
        let n = per_l2.min(count - first);
        if l2 == 0 {
            let disk = first * cluster..((first + n) * cluster).min(size);
            push(Chunk { disk, file: 0..0, how: How::Zeros });
            continue;
        }
        for k in 0..n {
            let p = usize::try_from(l2.checked_add(8 * k)?).ok()?;
            let e = be64(d.get(p..p + 8)?, 0);
            let disk = (first + k) * cluster..((first + k + 1) * cluster).min(size);
            let len = (disk.end - disk.start) as usize;
            let c = if e & 1 << 62 != 0 {
                compressed = true;
                let at = e & ((1 << x) - 1);
                let sectors = ((e >> x) & ((1 << (62 - x)) - 1)) + 1;
                let end = (at & !511).checked_add(sectors * 512)?;
                let file = usize::try_from(at).ok()?..usize::try_from(end).ok()?.min(d.len());
                if file.start >= file.end {
                    return None;
                }
                Chunk { disk, file, how: How::Deflate }
            } else if e & mask != 0 && e & 1 == 0 {
                let at = usize::try_from(e & mask).ok()?;
                let file = at..at.checked_add(len)?;
                if file.end > d.len() {
                    return None;
                }
                Chunk { disk, file, how: How::Raw }
            } else {
                Chunk { disk, file: 0..0, how: How::Zeros }
            };
            push(c);
        }
    }
    compressed.then_some(crate::chunked::Chunked { len: size, chunks })
}

/// The chunks of a streamOptimized VMDK extent (VMware's Virtual Disk Format 1.1: compressed grains
/// with markers, deflate), by its markers from its first data sector on: each grain a zlib stream
/// (its marker's LBA, in sectors, and bytes), each marker on a sector of its own; a metadata marker
/// (a grain table, the directory, the footer) skipped by its sectors, the end-of-stream one ending
/// it. What no grain gives is zeros. None where grains overlap or none is read.
pub fn vmdk_stream_chunks(d: &[u8]) -> Option<crate::chunked::Chunked> {
    use crate::chunked::{Chunk, How};
    let h = d.get(..512)?;
    if &h[..4] != b"KDMV" || !(1..=3).contains(&le32(h, 4)) {
        return None;
    }
    let (flags, capacity, grain, over) = (le32(h, 8), le64(h, 12), le64(h, 20), le64(h, 64));
    if flags & 0x3_0000 != 0x3_0000 || le16(h, 77) != 1 || grain == 0 || !grain.is_power_of_two() {
        return None;
    }
    let (len, grain) = (capacity.checked_mul(512)?, grain.checked_mul(512)?);
    let mut grains = Vec::new();
    let mut p = usize::try_from(over.checked_mul(512)?).ok()?;
    while let Some(m) = d.get(p..p + 16) {
        let (lba, size) = (le64(m, 0), le32(m, 8) as usize);
        if size > 0 {
            let start = lba.checked_mul(512)?;
            let file = p + 12..(p + 12).checked_add(size)?;
            if start >= len || file.end > d.len() {
                return None;
            }
            p = file.end.div_ceil(512).checked_mul(512)?;
            grains.push(Chunk { disk: start..start.saturating_add(grain).min(len), file, how: How::Zlib });
        } else if le32(m, 12) == 0 {
            break;
        } else {
            p = p.checked_add(usize::try_from(lba.checked_add(1)?.checked_mul(512)?).ok()?)?;
        }
    }
    if grains.is_empty() {
        return None;
    }
    grains.sort_by_key(|c| c.disk.start);
    let mut chunks = Vec::with_capacity(grains.len() + 1);
    let mut at = 0;
    for c in grains {
        if c.disk.start < at {
            return None;
        }
        if c.disk.start > at {
            chunks.push(Chunk { disk: at..c.disk.start, file: 0..0, how: How::Zeros });
        }
        at = c.disk.end;
        chunks.push(c);
    }
    if at < len {
        chunks.push(Chunk { disk: at..len, file: 0..0, how: How::Zeros });
    }
    Some(crate::chunked::Chunked { len, chunks })
}

/// A VDI image of VirtualBox (header version 1.1, dynamic or fixed): its block map (indices of
/// blocks in the data; free and zero blocks read as zeros).
fn vdi(d: &[u8]) -> Option<Map<'_>> {
    let h = d.get(..0x190)?;
    if le32(h, 0x40) != 0xbeda_107f || le32(h, 0x44) != 0x0001_0001 {
        return None;
    }
    let (blocks, data, size, block, extra, count) = (
        le32(h, 0x154) as usize,
        le32(h, 0x158) as u64,
        le64(h, 0x170),
        le32(h, 0x178) as u64,
        le32(h, 0x17c) as u64,
        le32(h, 0x180) as u64,
    );
    if block == 0 || !block.is_power_of_two() {
        return None;
    }
    let count = count.min(most(d, block));
    let entry = |i: u64| -> Option<u64> {
        let p = blocks.checked_add(usize::try_from(4 * i).ok()?)?;
        let n = le32(d.get(p..p + 4)?, 0);
        (n < 0xffff_fffe).then(|| data + n as u64 * (block + extra) + extra)
    };
    Map::of(Kind::Vdi, d, size, block, count, entry)
}
