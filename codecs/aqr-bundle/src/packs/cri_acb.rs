//! CRI's ACB cue sheets (`.acb`, CRI ADX2 / CRIWARE), a big-endian `@UTF` table
//! ([`super::cri_utf`]) of one row (the `Header`), whose column `AwbFile` is data: an AFS2
//! archive ([`super::cri_awb`]) embedded in the table's data pool (empty where the audio is in
//! a separate `.awb`, streamed). ACF files (the same container, no `AwbFile`) have no files.
//!
//! The embedded archive's members are taken as `<id>.<ext>` with their extents in the ACB; if
//! the archive cannot be read, the whole of `AwbFile` is one file `AwbFile.awb`. The tables
//! (cues, waveforms, the nested ones) are the rest. A masked table (the XOR of
//! [`super::cri_utf`]) is not taken: its embedded audio would be masked too.

use super::{cri_awb, cri_utf};
use aqr_disk::File;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"@UTF")
}

pub fn files(d: &[u8]) -> Option<Vec<File>> {
    if !starts(d) {
        return None;
    }
    let t = cri_utf::parse(d)?;
    if t.encrypted {
        return None;
    }
    let awb = t.data(0, "AwbFile").filter(|r| !r.is_empty())?;
    let out = match cri_awb::members(&d[awb.clone()]) {
        Some(m) if !m.is_empty() => m
            .into_iter()
            .map(|(id, r)| {
                let r = r.start + awb.start..r.end + awb.start;
                super::file(format!("{id}.{}", cri_awb::ext(&d[r.clone()])).into_bytes(), r)
            })
            .collect(),
        _ => vec![super::file(b"AwbFile.awb".to_vec(), awb)],
    };
    Some(out)
}
