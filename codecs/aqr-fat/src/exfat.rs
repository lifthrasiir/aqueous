//! exFAT: the directory tree walked from the root directory, entry sets (a File entry, its Stream
//! Extension and File Name entries) read into files. The contents of a file are its first
//! ValidDataLength bytes, in a contiguous run (NoFatChain) or the cluster chain of the first FAT.
//!
//! Left out (they stay in the rest): the allocation bitmap, up-case table, volume label and the
//! other entries, directories as files, and the bytes between ValidDataLength and DataLength
//! (undefined on disk). Checksums (the sets', the boot region's) are not checked: a damaged one
//! does not stop the reading.

use crate::{File, le16, le32, le64, utf16};
use aqr_disk::Image;
use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;

/// How deep directories are followed.
const DEPTH: usize = 256;

/// What the main boot sector tells of the volume.
struct Volume {
    /// Bytes of a cluster.
    size: usize,
    /// The byte offset of the cluster heap, where cluster 2 is.
    heap: usize,
    /// The byte offset of the first FAT.
    fat: usize,
    /// How many FAT entries lie in the first FAT (by its length).
    entries: usize,
    /// The number of clusters.
    count: u32,
    /// The first cluster of the root directory.
    root: u32,
    /// Clusters still to be followed through the FAT in all: of a volume whose chains meet or
    /// loop, as damaged or made ones do, the time stays that of the clusters.
    steps: std::cell::Cell<usize>,
}

/// The main boot sector read, if it is one.
fn volume(d: &dyn Image) -> Option<Volume> {
    let d = &*d.get(0..120)?;
    if &d[3..11] != b"EXFAT   " || d[11..64].iter().any(|&b| b != 0) {
        return None;
    }
    let (sector, per) = (d[108] as u32, d[109] as u32);
    if !(9..=12).contains(&sector) || sector + per > 25 || !matches!(d[110], 1 | 2) {
        return None;
    }
    let (fat, length, heap) = (le32(d, 80) as u64, le32(d, 84) as u64, le32(d, 88) as u64);
    let (count, root) = (le32(d, 92), le32(d, 96));
    // (the FAT's entries are 4 bytes, the clusters 0, 1 and the count ones)
    if fat < 1 || length < 1 || fat + length * d[110] as u64 > heap || !(1..=0xFFFF_FFF5).contains(&count) {
        return None;
    }
    if !(2..=count + 1).contains(&root) || (length << sector) / 4 < 2 {
        return None;
    }
    Some(Volume {
        size: 1 << (sector + per),
        heap: usize::try_from(heap << sector).ok()?,
        fat: usize::try_from(fat << sector).ok()?,
        entries: usize::try_from((length << sector) / 4).ok()?,
        count,
        root,
        steps: std::cell::Cell::new(2 * count as usize + 1024),
    })
}

/// Whether the main boot sector of an exFAT volume is at the start.
pub fn starts(d: &dyn Image) -> bool {
    volume(d).is_some()
}

impl Volume {
    fn valid(&self, c: u32) -> bool {
        (2..=self.count.saturating_add(1)).contains(&c)
    }

    /// The byte ranges of `len` bytes from cluster `first`: contiguous with `contiguous`, else its
    /// chain in the first FAT (cut where it leaves the clusters or loops).
    fn stream(&self, d: &dyn Image, fat: &[u8], first: u32, contiguous: bool, len: u64) -> Vec<Range<usize>> {
        if !self.valid(first) || len == 0 {
            return Vec::new();
        }
        let need = len.div_ceil(self.size as u64).min(self.count as u64) as usize;
        let at = |c: u32| (c as usize - 2).checked_mul(self.size).and_then(|o| o.checked_add(self.heap));
        if contiguous {
            // (cut at the image, as the clusters may be many)
            let Some(start) = at(first).filter(|&s| s < d.len()) else { return Vec::new() };
            let n = (len.min((d.len() - start) as u64)) as usize;
            return std::iter::once(start..start + n).collect();
        }
        // (a chain is not longer than the FAT has entries, which ends loops)
        let mut clusters = Vec::new();
        let mut c = first;
        let most = need.min(self.entries).min(self.steps.get());
        while clusters.len() < most && self.valid(c) {
            clusters.push(c);
            let Some(e) = (c as usize).checked_mul(4).and_then(|p| fat.get(p..p.checked_add(4)?)) else { break };
            c = u32::from_le_bytes(e.try_into().unwrap());
        }
        self.steps.set(self.steps.get() - clusters.len());
        crate::extents(&clusters, self.heap, self.size, len)
    }
}

struct Walk<'a> {
    d: &'a dyn Image,
    /// The bytes of the first FAT (as many as there are).
    fat: &'a [u8],
    v: &'a Volume,
    /// The first clusters of the directories read.
    seen: HashSet<u32>,
    /// The directory bytes still allowed to be read.
    budget: usize,
    out: Vec<File>,
}

impl Walk<'_> {
    fn dir(&mut self, path: &[u8], extents: &[Range<usize>], depth: usize) {
        let mut bytes = Vec::new();
        for r in extents {
            let part = self.d.cut(r.start..r.end.min(r.start.saturating_add(self.budget)));
            bytes.extend_from_slice(&part);
            self.budget -= part.len();
        }
        let mut i = 0;
        while let Some(e) = bytes.get(i * 32..i * 32 + 32) {
            i += 1;
            if e[0] == 0 {
                break;
            }
            if e[0] != 0x85 {
                continue;
            }
            // the set: the stream extension next, then the names
            let secondary = (e[1] as usize).min(bytes.len() / 32 - i);
            let set = &bytes[i * 32..(i + secondary) * 32];
            let Some(s) = set.get(..32).filter(|s| s[0] == 0xC0) else { continue };
            let attributes = le16(e, 4);
            let (flags, units) = (s[1], s[3] as usize);
            let (valid, first, length) = (le64(s, 8), le32(s, 20), le64(s, 24));
            let mut name = Vec::new();
            for n in set.as_chunks::<32>().0.iter().skip(1).take_while(|n| n[0] == 0xC1) {
                name.extend((0..15).map(|k| le16(n, 2 + 2 * k)));
            }
            name.truncate(units);
            if name.is_empty() {
                continue;
            }
            i += secondary;
            let mut full = path.to_vec();
            if !full.is_empty() {
                full.push(b'/');
            }
            full.extend(utf16(&name).into_iter().map(|b| if b == b'/' { b'_' } else { b }));
            let contiguous = flags & 2 != 0;
            if attributes & 0x10 != 0 {
                // each directory once, and not too deep
                if depth < DEPTH && self.v.valid(first) && self.seen.insert(first) {
                    let extents = self.v.stream(self.d, self.fat, first, contiguous, length);
                    self.dir(&full, &extents, depth + 1);
                }
            } else {
                let extents = self.v.stream(self.d, self.fat, first, contiguous, valid.min(length));
                self.out.push(File { name: full, extents, rank: 0, compressed: None });
            }
        }
    }
}

/// The files of the directory tree, empty ones too.
pub fn files(d: &dyn Image) -> Vec<File> {
    let Some(v) = volume(d) else { return Vec::new() };
    // (the first FAT, once: of a container every read puts its bytes together)
    let fat: Cow<[u8]> = d.cut(v.fat..v.fat.saturating_add(v.entries.saturating_mul(4)));
    let mut w = Walk { d, fat: &fat, v: &v, seen: HashSet::new(), budget: d.len(), out: Vec::new() };
    w.seen.insert(v.root);
    let root = v.stream(d, &fat, v.root, false, u64::MAX);
    w.dir(&[], &root, 0);
    w.out
}
