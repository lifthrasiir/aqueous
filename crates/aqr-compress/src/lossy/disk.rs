//! Disk images (aqr-disk's): `slack`, the bytes that a file system's allocation leaves free
//! zeroed in place, volume by volume ([`aqr_disk::volumes`]): each volume taken by the first of
//! [`FREE`]'s file systems that starts it ([`aqr_disk::FileSystem::starts`], aqr-compress's file
//! systems), which tells its free bytes by its own allocation (its bitmap, its table of
//! clusters, its free sectors). Nothing else is touched: the partition tables, the gaps between
//! volumes, the volumes of no file system known here, the file systems' own structures.
//!
//! A free range that holds any byte of a file as aqr-disk reads the files ([`aqr_disk::files`]:
//! what the file system tells of its files, whatever its allocation says) turns the image down:
//! the two reads disagree, and neither is to be trusted with what is lost. Only plain images:
//! of a container (VHD, VMDK, DMG, …) the free bytes of its disk are not its own.

use std::ops::Range;

use aqr_disk::{FileSystem, Plain, Sub};

use crate::Options;

mod ext;
mod fat;
mod hfs;
mod ntfs;
mod ole;

/// The free bytes of a volume (its bytes, as a plain slice), by its file system: ranges of the
/// volume, in order, apart; why it is not read where it is not one that can be trusted.
type Free = fn(&[u8]) -> Result<Vec<Range<usize>>, String>;

/// The file systems whose free bytes are told, by their [`FileSystem::name`].
const FREE: &[(&str, Free)] = &[
    ("ole", ole::free),
    ("fat", fat::free),
    ("exfat", fat::free_exfat),
    ("ntfs", ntfs::free),
    ("ext", ext::free),
    ("hfs", hfs::free),
];

/// The free ranges of the disk `d` (of the disk, in order, apart), of the volumes of file
/// systems of [`FREE`]; why it is kept as it is otherwise. None of a volume of a file system
/// not of them (ISO 9660, APFS) nor of none.
pub(super) fn free(d: &[u8]) -> Result<Vec<Range<usize>>, String> {
    if aqr_disk::container::open(d).is_some() {
        return Err("a container of a disk".into());
    }
    let fss = crate::bundle::FILE_SYSTEMS;
    let disk = Plain(d);
    let mut out: Vec<Range<usize>> = Vec::new();
    for v in aqr_disk::volumes(&disk) {
        let vd = Sub { of: &disk, span: v.clone() };
        let Some(fs) = fss.iter().find(|fs| (fs.starts)(&vd)) else { continue };
        let Some((_, free)) = FREE.iter().find(|(name, _)| *name == fs.name) else { continue };
        let ranges = free(&d[v.clone()]).map_err(|e| format!("{}: {e}", fs.name))?;
        out.extend(ranges.into_iter().map(|r| r.start + v.start..r.end + v.start));
    }
    out.sort_unstable_by_key(|r| r.start);
    if out.windows(2).any(|w| w[0].end > w[1].start) || out.last().is_some_and(|r| r.end > d.len()) {
        return Err("free ranges of volumes overlapping, or past the disk".into());
    }
    Ok(out)
}

/// Whether any of `files`' extents meets any of `free` (both of the disk; `free` in order, apart).
fn meets(files: &[aqr_disk::File], free: &[Range<usize>]) -> bool {
    files.iter().flat_map(|f| &f.extents).any(|e| {
        let i = free.partition_point(|r| r.end <= e.start);
        free.get(i).is_some_and(|r| r.start < e.end)
    })
}

/// `d` with its free bytes zeroed, None where they are all zero already; why it is kept where a
/// file system is not read whole, or its free bytes hold a file's.
pub(super) fn slack(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    let free = free(d)?;
    if free.is_empty() {
        return Ok(None);
    }
    let files = aqr_disk::files(d, crate::bundle::FILE_SYSTEMS);
    if meets(&files, &free) {
        return Err("free bytes holding a file's, as its file system tells".into());
    }
    let mut r: Option<Vec<u8>> = None;
    for f in &free {
        if d[f.clone()].iter().any(|&b| b != 0) {
            r.get_or_insert_with(|| d.to_vec())[f.clone()].fill(0);
        }
    }
    Ok(r)
}

/// The file systems of aqr-compress, for the check.
pub(super) fn file_systems() -> &'static [FileSystem] {
    crate::bundle::FILE_SYSTEMS
}
