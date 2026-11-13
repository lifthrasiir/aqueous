//! FMOD Studio's banks (`.bank`, `fmod-bank`): a RIFF file of form type `FEV `, little endian.
//!
//! The chunks are a 4-byte tag, a u32 size and the contents, padded to an even size (RIFF's
//! rule); `FMT ` and the `LIST` chunks (`PROJ` with the events, buses and the like, their
//! contents) hold the bank's metadata. The sound data is the chunk `SND `: it starts with zeros
//! that align what follows, then an FSB5 bank (`FSB5`, version 0 or 1) up to the chunk's end. A
//! bank may have several `SND ` chunks (read at the top level and within `LIST`s).
//!
//! The FSB5 bank's size is its header (60 bytes for version 1, 64 for version 0: the magic,
//! the version, the sample count, the sizes of the sample headers, the name table and the data,
//! the mode, and the zeros, hash and dummy) and these three sizes; if that does not fit the
//! chunk, the chunk's end. Each bank is a file `NNN.fsb` (by the order of its chunk), which
//! aqr-fsb recompresses. The rest is the metadata, and a `SND ` chunk that holds no FSB5 (older
//! FSB versions are not read).

use super::{file, u32le};
use aqr_disk::File;

/// The zeros before the FSB5 bank in a `SND ` chunk: at most this many are skipped.
const MAX_PAD: usize = 64;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"RIFF") && d.get(8..12) == Some(b"FEV ")
}

/// The size of the FSB5 bank at the start of `d`, if it is one.
fn fsb5_size(d: &[u8]) -> Option<u64> {
    if !d.starts_with(b"FSB5") {
        return None;
    }
    let header = match u32le(d, 4)? {
        0 => 64,
        1 => 60,
        _ => return None,
    };
    let sum = header + u32le(d, 12)? as u64 + u32le(d, 16)? as u64 + u32le(d, 20)? as u64;
    Some(sum)
}

fn walk(d: &[u8], mut at: usize, end: usize, files: &mut Vec<File>, depth: u32) {
    while let Some(next) = at.checked_add(8).filter(|&n| n <= end) {
        let Some(size) = u32le(d, at + 4) else { return };
        let body = next;
        let Some(stop) = body.checked_add(size as usize).filter(|&e| e <= end) else { return };
        match &d[at..at + 4] {
            b"SND " => {
                let pad = d[body..stop].iter().take(MAX_PAD).take_while(|&&b| b == 0).count();
                let start = body + pad;
                if let Some(len) = fsb5_size(&d[start..stop]) {
                    let take = (len as usize).min(stop - start);
                    files.push(file(format!("{:03}.fsb", files.len()).into_bytes(), start..start + take));
                }
            }
            b"LIST" if depth < 8 && stop - body >= 4 => walk(d, body + 4, stop, files, depth + 1),
            _ => {}
        }
        at = stop + (size as usize & 1);
    }
}

/// The FSB5 banks of the bank `d`; None where it is not one.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let mut files = Vec::new();
    walk(d, 12, d.len(), &mut files, 0);
    Some(files)
}
