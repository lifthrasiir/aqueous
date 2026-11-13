//! Garry's Mod addons (`.gma`), little endian: the signature `GMAD`, the version (u8, 1 to 3),
//! the author's Steam ID (u64) and the time (u64), from version 2 the required content (strings
//! until an empty one), the addon's name, description and author (NUL-terminated strings), and
//! its version (i32). The file table follows: for each file a number (u32, 1 on; 0 ends the
//! table), its path (a string), its size (i64) and CRC-32 (u32). The files' data follow back to
//! back in the order of the table, an optional CRC-32 of the addon last.
//!
//! Each file of the table whose data lies within the addon is a file under its path; the rest
//! is the header, the table and the CRC. (Written from the public description; unverified on
//! real files.)

use super::{cstr, file, u32le, u64le};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"GMAD") && d.get(4).is_some_and(|&v| (1..=3).contains(&v))
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let mut at = 5 + 8 + 8;
    if d[4] > 1 {
        loop {
            let (s, next) = cstr(d, at)?;
            at = next;
            if s.is_empty() {
                break;
            }
        }
    }
    for _ in 0..3 {
        at = cstr(d, at)?.1;
    }
    at += 4;
    let mut table = Vec::new();
    loop {
        if u32le(d, at)? == 0 {
            at += 4;
            break;
        }
        let (name, next) = cstr(d, at + 4)?;
        let size = u64le(d, next)?;
        u32le(d, next + 8)?;
        at = next + 12;
        table.push((name, size));
    }
    let mut files = Vec::new();
    for (name, size) in table {
        let Some(end) = usize::try_from(size).ok().and_then(|s| at.checked_add(s)).filter(|&e| e <= d.len()) else {
            break;
        };
        if end > at && !name.is_empty() {
            files.push(file(name.to_vec(), at..end));
        }
        at = end;
    }
    Some(files)
}
