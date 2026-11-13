//! FAT12, FAT16, FAT32 ([`FAT`]) and exFAT ([`EXFAT`]) volumes read into their files: file
//! systems of aqr-disk, which takes an image apart into the contents of the files and the rest
//! (the boot sectors, the FATs, the directories, free clusters).
//!
//! `fat` and `exfat` read the directory trees into files (their names and extents), whatever the
//! bytes: what does not read is left out, a cluster chain cut where it leaves the volume or loops.

pub mod exfat;
pub mod fat;

pub use aqr_disk::File;
use aqr_disk::bytes::{le16, le32, le64};

/// FAT12, FAT16 and FAT32 as a file system of aqr-disk.
pub const FAT: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "fat", starts: fat::starts, files: fat::files };

/// exFAT as a file system of aqr-disk.
pub const EXFAT: aqr_disk::FileSystem =
    aqr_disk::FileSystem { name: "exfat", starts: exfat::starts, files: exfat::files };

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
        aqr_disk::push_extent(&mut out, at..at + n);
    }
    out
}
