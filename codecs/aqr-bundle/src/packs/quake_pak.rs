//! id Software's PACK (Quake, Quake II and their descendants' `pakN.pak`; not Unreal's, which is
//! `unreal-pak`), little endian: the signature `PACK`, the directory's offset and size (i32
//! each). The directory is entries of 64 bytes: the file's path (56 bytes, NUL-terminated, `/`
//! as separator), its offset and size (i32 each).
//!
//! Each entry of size over 0 whose data lies within the pack is a file under its path; the rest
//! is the header and the directory. (Written from the public description; unverified on real
//! files.)

use super::{cstr, file, u32le, within};
use aqr_disk::File;

const ENTRY: usize = 64;

pub fn starts(d: &[u8]) -> bool {
    // (the directory within the data, of whole entries: a git pack starts `PACK` too, then its
    // version and count, big endian)
    d.starts_with(b"PACK")
        && matches!((u32le(d, 4), u32le(d, 8)), (Some(o), Some(n))
            if o >= 12 && n > 0 && (n as usize).is_multiple_of(ENTRY) && o as u64 + n as u64 <= d.len() as u64)
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let dir = within(d, u32le(d, 4)? as u64, u32le(d, 8)? as u64)?;
    let mut files = Vec::new();
    for e in d[dir].as_chunks::<ENTRY>().0 {
        let name = cstr(&e[..56], 0).map_or(&e[..56], |(n, _)| n);
        let (offset, size) = (u32le(e, 56)?, u32le(e, 60)?);
        if size == 0 || name.is_empty() {
            continue;
        }
        if let Some(body) = within(d, offset as u64, size as u64) {
            files.push(file(name.to_vec(), body));
        }
    }
    Some(files)
}
