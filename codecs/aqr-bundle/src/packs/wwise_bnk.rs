//! Audiokinetic Wwise's SoundBanks (`.bnk`, `wwise-bnk`).
//!
//! A bank is a sequence of chunks: a 4-byte tag, a u32 size and the contents (`BKHD` first, the
//! bank header, whose first u32 is the bank version; then `DIDX`, `DATA`, `HIRC`, `STID`, `INIT`,
//! `STMG`, `ENVS`, `PLAT` and others, in no fixed order). `DIDX` lists the embedded media, 12
//! bytes each: the media's id, its offset in the contents of `DATA`, and its size (u32 each).
//! `DATA` holds them. PC banks are little endian; a bank whose version, read little endian, is
//! implausible (over 0xFFFF) and plausible big endian is read big endian (consoles; unverified,
//! no sample).
//!
//! Each medium is a file `<id>.wem` (a RIFF WAVE of Wwise's Vorbis), `<id>.wav` (of its other
//! codecs: PCM, ADPCM, Opus), or `<id>.bin` where it is not RIFF (7 of 16775 in the samples):
//! [`medium`].
//! Banks of versions 134 to 145 are verified on samples; `DIDX` and `DATA` have the same layout
//! in the older versions seen documented (since 2011). The rest is the other chunks and the
//! padding between the media.

use super::{file, u32be, u32le, within};
use aqr_disk::File;

/// A reader of u32s in the bank's byte order.
fn reader(big: bool) -> fn(&[u8], usize) -> Option<u32> {
    if big { u32be } else { u32le }
}

/// The byte order of bank `d` (big endian or not), if it starts as a bank does.
fn order(d: &[u8]) -> Option<bool> {
    if d.get(..4)? != b"BKHD" {
        return None;
    }
    let version = u32le(d, 8)?;
    if version <= 0xFFFF && version != 0 {
        return Some(false);
    }
    let version = u32be(d, 8)?;
    (version <= 0xFFFF && version != 0).then_some(true)
}

pub fn starts(d: &[u8]) -> bool {
    order(d).is_some()
}

/// The embedded media of bank `d`, none where it holds none; None where it is not a bank.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    let u32 = reader(order(d)?);
    let (mut didx, mut data) = (None, None);
    let mut at = 0usize;
    while let (Some(tag), Some(size)) = (d.get(at..at.saturating_add(4)), u32(d, at.saturating_add(4))) {
        // (a chunk cut short is read as far as it goes)
        let range = at + 8..(at + 8).saturating_add(size as usize).min(d.len());
        match tag {
            b"DIDX" if didx.is_none() => didx = Some(range.clone()),
            b"DATA" if data.is_none() => data = Some(range.clone()),
            _ => {}
        }
        at = (at + 8).saturating_add(size as usize);
    }
    let mut files = Vec::new();
    if let (Some(didx), Some(data)) = (didx, data) {
        for e in d[didx].as_chunks::<12>().0 {
            let (id, offset, size) = (u32(e, 0)?, u32(e, 4)?, u32(e, 8)?);
            if let Some(r) = within(d, data.start as u64 + offset as u64, size as u64).filter(|r| r.end <= data.end) {
                files.push(file(format!("{id}.{}", medium(&d[r.clone()])).into_bytes(), r));
            }
        }
    }
    Some(files)
}

/// The extension of a medium `m` of Wwise's: `wem` of its Vorbis (format `FFFF`, which aqr-wwise
/// recompresses), `wav` of the others (PCM, ADPCM, ...) of RIFF, `bin` of the plugins' media
/// (impulse responses), which are not RIFF.
pub fn medium(m: &[u8]) -> &'static str {
    match (m.starts_with(b"RIFF"), m.get(12..16) == Some(b"fmt "), m.get(20..22)) {
        (false, ..) => "bin",
        (true, true, Some(&[0xff, 0xff])) => "wem",
        (true, ..) => "wav",
    }
}
