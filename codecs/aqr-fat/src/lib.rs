//! FAT12, FAT16, FAT32 ([`FAT`]) and exFAT ([`EXFAT`]) volumes read into their files: file
//! systems of aqr-disk, which takes an image apart into the contents of the files and the rest
//! (the boot sectors, the FATs, the directories, free clusters).
//!
//! `fat` and `exfat` read the directory trees into files (their names and extents), whatever the
//! bytes: what does not read is left out, a cluster chain cut where it leaves the volume or loops.

pub mod exfat;
pub mod fat;

pub use aqr_disk::File;

/// FAT12, FAT16 and FAT32 as a file system of aqr-disk.
pub const FAT: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "fat", starts: fat::starts, files: fat::files };

/// exFAT as a file system of aqr-disk.
pub const EXFAT: aqr_disk::FileSystem =
    aqr_disk::FileSystem { name: "exfat", starts: exfat::starts, files: exfat::files };

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

fn le64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

/// The byte ranges of `clusters` (cluster numbers, in order, of `size` bytes each, the first
/// cluster of the data at byte `heap`, numbered from 2), runs of contiguous ones one range, up to
/// `len` bytes in all.
fn extents(clusters: &[u32], heap: usize, size: usize, len: u64) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut left = len;
    for &c in clusters {
        if left == 0 {
            break;
        }
        let Some(at) = (c as usize).checked_sub(2).and_then(|n| n.checked_mul(size)).and_then(|o| o.checked_add(heap))
        else {
            break;
        };
        let n = (size as u64).min(left) as usize;
        left -= n as u64;
        match out.last_mut() {
            Some(r) if r.end == at => r.end = at + n,
            _ => out.push(at..at + n),
        }
    }
    out
}

/// A name of UTF-16 code units as UTF-8, unpaired surrogates replaced.
fn utf16(units: &[u16]) -> Vec<u8> {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>()
        .into_bytes()
}
