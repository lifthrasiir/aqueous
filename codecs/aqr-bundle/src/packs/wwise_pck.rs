//! Audiokinetic Wwise's file packages (`.pck`, `wwise-pck`, magic `AKPK`), little endian.
//!
//! UNVERIFIED on real files: there is no sample on the machine this was written on. It follows
//! public descriptions of the format, and its tests read packages written from them.
//!
//! The header: `AKPK`, the size of the rest of the header (u32, from the next field to the end
//! of the tables, where the first file's padding may follow), the version (u32, 1), the sizes
//! in bytes of the language map, the sound bank table and the streamed files table (u32 each)
//! and, in later versions, of the external files table (u32); then the four. The sizes must
//! add up to the header's, which tells whether the external files' size is there.
//!
//! The language map: a count (u32), that many pairs (offset of a name from the map's start,
//! language id; u32 each), then the names in UTF-16 with a NUL. Each table is a count (u32) and
//! that many entries of the file's id, its block size, its size, its offset in blocks and its
//! language id (u32 each; the id is u64 in the external files table, and where the table's
//! size says entries are 24 bytes). A file is at its offset times its block size (a block size
//! of 0 is 1).
//!
//! The sound banks are files `<id>.bnk` (aqr-bundle's `wwise-bnk` takes their media apart), the
//! streamed and external files `<id>.wem`; those of a language other than 0 (the SFX) are under
//! the language's name (`english(us)/<id>.wem`, or `lang<id>/` where the map does not name it).
//! The rest is the header and tables and the padding between files.

use super::{file, u32le, within};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"AKPK") && u32le(d, 4).is_some_and(|n| n >= 16 && (n as usize) < d.len())
}

/// The name of language `id` by the map at `at..end`.
fn language(d: &[u8], map: std::ops::Range<usize>, id: u32) -> Option<String> {
    let m = &d[map];
    let n = u32le(m, 0)? as usize;
    for k in 0..n.min(m.len() / 8) {
        if u32le(m, 8 + 8 * k)? != id {
            continue;
        }
        let at = u32le(m, 4 + 8 * k)? as usize;
        let units =
            m.get(at..)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0);
        let s: String = char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect();
        return (!s.is_empty() && !s.contains(['/', '\\'])).then_some(s);
    }
    None
}

/// The files of package `d` (the sound banks, the streamed files and the external files); None
/// where it is not one, or its tables do not fit.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let header_end = (u32le(d, 4)? as usize).checked_add(8)?;
    let (langs, banks, sounds) = (u32le(d, 12)? as usize, u32le(d, 16)? as usize, u32le(d, 20)? as usize);
    let sum = langs.checked_add(banks)?.checked_add(sounds)?;
    let (at, externals) = if sum.checked_add(24) == Some(header_end) {
        (24, 0)
    } else {
        let externals = u32le(d, 24)? as usize;
        if sum.checked_add(externals)?.checked_add(28)? != header_end {
            return None;
        }
        (28, externals)
    };
    if header_end > d.len() {
        return None;
    }
    let lang_map = within(d, at as u64, langs as u64)?;
    let mut files = Vec::new();
    let mut at = at + langs;
    for (size, ext) in [(banks, "bnk"), (sounds, "wem"), (externals, "wem")] {
        let table = within(d, at as u64, size as u64)?;
        at += size;
        if size < 4 {
            continue;
        }
        let n = u32le(d, table.start)? as usize;
        if n == 0 {
            continue;
        }
        // (an entry of 24 bytes has a 64-bit id)
        let width = (size - 4) / n;
        if (size - 4) % n != 0 || !(20..=24).contains(&width) || !width.is_multiple_of(4) {
            return None;
        }
        for k in 0..n {
            let e = table.start + 4 + k * width;
            let (id, e) = if width == 24 { (super::u64le(d, e)?, e + 4) } else { (u32le(d, e)? as u64, e) };
            let (block, len, offset, lang) =
                (u32le(d, e + 4)? as u64, u32le(d, e + 8)? as u64, u32le(d, e + 12)? as u64, u32le(d, e + 16)?);
            let Some(range) = within(d, offset * block.max(1), len) else { continue };
            // (a streamed file's extension by its codec, as a bank's media)
            let ext = if ext == "wem" { super::wwise_bnk::medium(&d[range.clone()]) } else { ext };
            let name = match lang {
                0 => format!("{id}.{ext}"),
                _ => {
                    let dir = language(d, lang_map.clone(), lang).unwrap_or(format!("lang{lang}"));
                    format!("{dir}/{id}.{ext}")
                }
            };
            files.push(file(name.into_bytes(), range));
        }
    }
    Some(files)
}
