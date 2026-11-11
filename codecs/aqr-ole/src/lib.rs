//! OLE compound files (Microsoft's [MS-CFB]: Windows Installer packages `.msi` and `.msp`,
//! Office 97-2003 documents, HWP 5 documents, thumbnail databases, Outlook `.msg`) read into
//! their streams: a file system of aqr-disk ([`FS`]), which takes the file apart into the
//! contents of the streams and the rest (the header, the sector tables, the directory, small
//! streams and free sectors).
//!
//! A stream of the regular sectors is a file, named by its path from the root; its extents are
//! the runs of its sectors. A package's cabinets are such streams: the FAT's own sectors come
//! between theirs every 128 (or 1024) sectors, so that they lie in runs, not as one range, and
//! the contents put the runs together. Streams shorter than the mini stream's cutoff (4096
//! bytes) live in the root's mini stream and are left in the rest.

use std::collections::HashSet;
use std::ops::Range;

pub use aqr_disk::File;
use aqr_disk::Image;

/// OLE compound files as a file system of aqr-disk.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "ole", starts, files };

/// The signature at the start of the header.
const SIGNATURE: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

const FREE: u32 = 0xffff_ffff;
const NO_STREAM: u32 = 0xffff_ffff;
/// Largest sector number of data (past it are the marks of the DIFAT, the FAT and the chain's end).
const MAX_SECTOR: u32 = 0xffff_fffa;

/// Deepest directory read, from the root.
const DEPTH: usize = 256;

/// Most entries read of the directory.
const ENTRIES: usize = 1 << 20;

/// Most bytes of the FAT read: 4 bytes to a sector, for a file of 64 GiB at 512 bytes a sector.
const FAT_BYTES: usize = 1 << 28;

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

/// What the header tells of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    /// Bytes of a sector (512 of version 3, 4096 of version 4).
    sector: usize,
    version: u16,
    fat_sectors: u32,
    dir_start: u32,
    cutoff: u32,
    difat_start: u32,
    difat_sectors: u32,
}

/// The header in `b` (the first 512 bytes), if it is one that holds: the signature, byte order
/// little-endian, version 3 with sectors of 512 bytes or version 4 with 4096, mini sectors of 64.
fn header(b: &[u8]) -> Option<Header> {
    if b.len() < 512 || b[..8] != SIGNATURE || le16(b, 28) != 0xfffe {
        return None;
    }
    let (version, shift, mini) = (le16(b, 26), le16(b, 30), le16(b, 32));
    let sector = match (version, shift) {
        (3, 9) => 512,
        (4, 12) => 4096,
        _ => return None,
    };
    (mini == 6).then_some(Header {
        sector,
        version,
        fat_sectors: le32(b, 44),
        dir_start: le32(b, 48),
        cutoff: le32(b, 56),
        difat_start: le32(b, 68),
        difat_sectors: le32(b, 72),
    })
}

/// Whether `d` starts like a compound file.
pub fn starts(d: &dyn Image) -> bool {
    d.get(0..512).is_some_and(|b| header(&b).is_some())
}

/// The sectors of a file, numbered from the one after the header.
struct Sectors<'a> {
    d: &'a dyn Image,
    h: Header,
    /// The file allocation table: for each sector, the next of its chain.
    fat: Vec<u32>,
}

impl Sectors<'_> {
    /// Where sector `n` starts.
    fn at(&self, n: u32) -> Option<usize> {
        (n as usize).checked_add(1)?.checked_mul(self.h.sector)
    }

    fn read(&self, n: u32) -> Option<std::borrow::Cow<'_, [u8]>> {
        let at = self.at(n)?;
        self.d.get(at..at.checked_add(self.h.sector)?)
    }

    /// The sectors of the chain from `start`, at most `limit` of them: cut where it ends or leaves
    /// the table (or loops, which the limit bounds).
    fn chain(&self, start: u32, limit: usize) -> Vec<u32> {
        let mut out = Vec::new();
        let mut s = start;
        while out.len() < limit && s <= MAX_SECTOR {
            out.push(s);
            let Some(&next) = self.fat.get(s as usize) else { break };
            s = next;
        }
        out
    }
}

/// The file allocation table: the FAT sectors the header (109 of them) and the DIFAT chain list.
fn fat(d: &dyn Image, h: Header) -> Option<Vec<u32>> {
    let b = d.get(0..512)?;
    let mut listed: Vec<u32> = (0..109).map(|i| le32(&b, 76 + 4 * i)).collect();
    // the DIFAT's sectors: the numbers of FAT sectors, and the next DIFAT sector at the end
    let per = h.sector / 4 - 1;
    let at = |n: u32| (n as usize).checked_add(1)?.checked_mul(h.sector);
    let (mut next, mut seen) = (h.difat_start, 0u32);
    while next <= MAX_SECTOR && seen < h.difat_sectors && listed.len() < FAT_BYTES / 4 {
        let a = at(next)?;
        let s = d.get(a..a.checked_add(h.sector)?)?;
        listed.extend((0..per).map(|i| le32(&s, 4 * i)));
        next = le32(&s, 4 * per);
        seen += 1;
    }
    let mut fat = Vec::new();
    for n in listed.into_iter().filter(|&n| n <= MAX_SECTOR).take(h.fat_sectors as usize) {
        if fat.len() * 4 >= FAT_BYTES {
            break;
        }
        let a = at(n)?;
        let Some(s) = a.checked_add(h.sector).and_then(|e| d.get(a..e)) else { break };
        fat.extend((0..h.sector / 4).map(|i| le32(&s, 4 * i)));
    }
    (!fat.is_empty()).then_some(fat)
}

/// A directory entry.
struct Entry {
    name: Vec<u8>,
    /// 1 storage, 2 stream, 5 root.
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

fn entries(s: &Sectors) -> Vec<Entry> {
    let per = s.h.sector / 128;
    let mut out = Vec::new();
    for n in s.chain(s.h.dir_start, ENTRIES / per) {
        let Some(b) = s.read(n) else { break };
        for e in b.as_chunks::<128>().0 {
            let units: Vec<u16> = (0..(le16(e, 64) as usize).clamp(2, 64) / 2 - 1).map(|i| le16(e, 2 * i)).collect();
            // (version 3 files have only the low half of the size)
            let size = match s.h.version {
                3 => le32(e, 120) as u64,
                _ => u64::from_le_bytes(e[120..128].try_into().unwrap()),
            };
            out.push(Entry {
                name: utf16(&units),
                kind: e[66],
                left: le32(e, 68),
                right: le32(e, 72),
                child: le32(e, 76),
                start: le32(e, 116),
                size,
            });
        }
    }
    out
}

/// A name of UTF-16 code units as UTF-8, unpaired surrogates replaced, and `/` too.
fn utf16(units: &[u16]) -> Vec<u8> {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .map(|c| if c == '/' { '_' } else { c })
        .collect::<String>()
        .into_bytes()
}

/// The byte ranges of `sectors`, runs of consecutive ones a range, up to `len` bytes in all.
fn extents(s: &Sectors, sectors: &[u32], len: u64) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut left = len;
    for &n in sectors {
        let (Some(at), true) = (s.at(n), left > 0) else { break };
        let take = (s.h.sector as u64).min(left) as usize;
        left -= take as u64;
        match out.last_mut() {
            Some(r) if r.end == at => r.end = at + take,
            _ => out.push(at..at + take),
        }
    }
    out
}

/// The streams of compound file `d` of the regular sectors, as files (in order of where they
/// start): not the root's mini stream nor the streams in it.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(h) = d.get(0..512).and_then(|b| header(&b)) else { return Vec::new() };
    let Some(fat) = fat(d, h) else { return Vec::new() };
    let s = Sectors { d, h, fat };
    let dir = entries(&s);
    let mut out = Vec::new();
    // (the red-black tree of a storage's children, each entry once)
    let mut seen = HashSet::new();
    let mut stack = vec![(dir.first().map_or(NO_STREAM, |r| r.child), Vec::new(), 0usize)];
    while let Some((id, path, depth)) = stack.pop() {
        let Some(e) = dir.get(id as usize).filter(|_| id != NO_STREAM && depth <= DEPTH) else { continue };
        if !seen.insert(id) {
            continue;
        }
        // (the siblings at this level, then the entry's children below it)
        stack.push((e.left, path.clone(), depth + 1));
        stack.push((e.right, path.clone(), depth + 1));
        let mut here = path;
        if !here.is_empty() {
            here.push(b'/');
        }
        here.extend_from_slice(&e.name);
        match e.kind {
            1 => stack.push((e.child, here, depth + 1)),
            2 if e.size >= h.cutoff as u64 && e.start != FREE => {
                let need = usize::try_from(e.size.div_ceil(h.sector as u64)).unwrap_or(usize::MAX);
                let ex = extents(&s, &s.chain(e.start, need), e.size);
                if !ex.is_empty() {
                    out.push(File { name: here, extents: ex, rank: 0, compressed: None });
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|f| f.extents[0].start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_disk::Plain;

    fn name(entry: &mut [u8], s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        for (i, u) in units.iter().enumerate() {
            entry[2 * i..2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
        entry[64..66].copy_from_slice(&(2 * units.len() as u16 + 2).to_le_bytes());
    }

    /// A version 3 file of sector 0 the FAT, 1 the directory, then the sectors of streams "A"
    /// (5120 bytes) and "B" (4096) alternating, A's last two sectors after B's; and "S", a small
    /// stream in the mini stream. Returns the file, the contents of A and of B.
    fn made() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let a: Vec<u8> = (0..5120u32).map(|i| (i % 251) as u8).collect();
        let b: Vec<u8> = (0..4096u32).map(|i| (i % 241) as u8 + 1).collect();
        // sectors 2.. alternate A0 B0 A1 B1 ... (A has 10, B has 8)
        let (mut order, mut fat) = (Vec::new(), vec![0xffff_ffffu32; 128]);
        let (mut a_ix, mut b_ix) = (0, 0);
        for k in 0..18 {
            if (k % 2 == 0 && a_ix < 10) || b_ix == 8 {
                order.push(('A', a_ix));
                a_ix += 1;
            } else {
                order.push(('B', b_ix));
                b_ix += 1;
            }
        }
        let sector_of = |c, i| 2 + order.iter().position(|&o| o == (c, i)).unwrap() as u32;
        for (c, n) in [('A', 10), ('B', 8)] {
            for i in 0..n {
                fat[sector_of(c, i) as usize] = if i + 1 < n { sector_of(c, i + 1) } else { 0xffff_fffe };
            }
        }
        fat[0] = 0xffff_fffd; // the FAT's own sector
        fat[1] = 0xffff_fffe; // the directory
        let mut d = vec![0u8; 512];
        d[..8].copy_from_slice(&SIGNATURE);
        d[24..26].copy_from_slice(&0x3eu16.to_le_bytes());
        d[26..28].copy_from_slice(&3u16.to_le_bytes());
        d[28..30].copy_from_slice(&0xfffeu16.to_le_bytes());
        d[30..32].copy_from_slice(&9u16.to_le_bytes());
        d[32..34].copy_from_slice(&6u16.to_le_bytes());
        d[44..48].copy_from_slice(&1u32.to_le_bytes()); // one FAT sector
        d[48..52].copy_from_slice(&1u32.to_le_bytes()); // the directory at sector 1
        d[56..60].copy_from_slice(&4096u32.to_le_bytes());
        d[60..64].copy_from_slice(&0xffff_fffeu32.to_le_bytes());
        d[68..72].copy_from_slice(&0xffff_fffeu32.to_le_bytes());
        d[76..80].copy_from_slice(&0u32.to_le_bytes()); // FAT sector 0
        d[80..76 + 109 * 4].fill(0xff);
        d.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
        // the directory: root, then A, B and S as a tree (A's right is B, B's right is S)
        let mut dir = vec![0u8; 512];
        for (i, (n, kind, left, right, child, start, size)) in [
            ("Root Entry", 5u8, NO_STREAM, NO_STREAM, 1, 0xffff_fffe, 0u32),
            ("A", 2, NO_STREAM, 2, NO_STREAM, sector_of('A', 0), 5120),
            ("B", 2, NO_STREAM, 3, NO_STREAM, sector_of('B', 0), 4096),
            ("S", 2, NO_STREAM, NO_STREAM, NO_STREAM, 0, 100),
        ]
        .into_iter()
        .enumerate()
        {
            let e = &mut dir[128 * i..128 * i + 128];
            name(e, n);
            e[66] = kind;
            e[68..72].copy_from_slice(&left.to_le_bytes());
            e[72..76].copy_from_slice(&right.to_le_bytes());
            e[76..80].copy_from_slice(&child.to_le_bytes());
            e[116..120].copy_from_slice(&start.to_le_bytes());
            e[120..124].copy_from_slice(&size.to_le_bytes());
        }
        d.extend(dir);
        for &(c, i) in &order {
            let (src, at) = if c == 'A' { (&a, i) } else { (&b, i) };
            let mut s = [0u8; 512];
            let part = &src[at as usize * 512..((at as usize + 1) * 512).min(src.len())];
            s[..part.len()].copy_from_slice(part);
            d.extend(s);
        }
        (d, a, b)
    }

    fn gather(d: &[u8], f: &File) -> Vec<u8> {
        f.extents.iter().flat_map(|r| d[r.clone()].iter().copied()).collect()
    }

    #[test]
    fn streams_are_files_in_runs() {
        let (d, a, b) = made();
        assert!(starts(&Plain(&d)));
        let files = files(&Plain(&d));
        // (S is small and left in the rest)
        let names: Vec<&[u8]> = files.iter().map(|f| &f.name[..]).collect();
        assert_eq!(names, [&b"A"[..], b"B"]);
        assert_eq!(gather(&d, &files[0]), a);
        assert_eq!(gather(&d, &files[1]), b);
        // interleaved: A's runs are single sectors until the two last, which join
        assert_eq!(files[0].extents.len(), 9);
        assert_eq!(files[0].extents[0].len(), 512);
        assert_eq!(files[0].extents[8].len(), 1024);
    }

    #[test]
    fn damage_gives_what_reads() {
        let (d, _, _) = made();
        assert!(!starts(&Plain(&d[..100])));
        // the file cut inside the streams: what reads is left
        let cut = &d[..d.len() - 1000];
        let files = files(&Plain(cut));
        assert!(files.iter().all(|f| f.extents.iter().all(|r| r.end <= cut.len() || r.start < cut.len())));
        // a chain looping on itself
        let mut loop_ = d.clone();
        let fat = 512;
        for k in 0..128 {
            loop_[fat + 4 * k..fat + 4 * k + 4].copy_from_slice(&2u32.to_le_bytes());
        }
        let _ = files_of(&loop_);
        // a directory without the signature
        let mut bad = d;
        bad[0] ^= 1;
        assert!(files_of(&bad).is_empty());
    }

    fn files_of(d: &[u8]) -> Vec<File> {
        files(&Plain(d))
    }
}
