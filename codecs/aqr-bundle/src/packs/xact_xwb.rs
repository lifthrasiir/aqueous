//! Microsoft XACT's wave banks (`.xwb`, `xact-xwb`), as XNA, FNA and MonoGame games use them.
//!
//! Little endian (`WBND`; the big-endian `DNBW` of the Xbox 360 is read the same with every
//! u32 and the bit fields swapped, unverified: no sample). The header: the magic, the version
//! (the tool's, u32), from version 42 the header version (u32), then the segments, each an
//! offset and a length (u32 each): bank data, entry metadata, seek tables, entry names, wave
//! data (versions up to 3 have four, without the seek tables: not read). Samples are of version
//! 46 (header version 44); the layout of versions 40 to 45 follows the same description but is
//! unverified.
//!
//! The bank data: the flags (u32: 0 buffer or 1 streaming bank in the low bits; 0x10000 entry
//! names, 0x20000 compact entries, 0x40000 sync disabled, 0x80000 seek tables), the number of
//! entries, the bank's name (64 bytes), the entry metadata's element size, the entry names'
//! element size, the alignment (u32 each), the compact format (u32: the format of every wave of
//! a compact bank), and from version 43 the build time (u64).
//!
//! An entry's metadata (element size 24 in the samples; 20 or more is read): the flags and
//! duration (u32), the format (u32), the play region's offset and length in the wave data (u32
//! each), the loop region's start and length. A compact bank (flag 0x20000) has 4 bytes for an
//! entry instead: an offset in units of the alignment (low 21 bits) and the length's deviation
//! (high 11 bits); the wave's length is up to the next wave's offset, less the deviation (the
//! last up to the end of the wave data). The format is a u32 of bit fields: the codec in the
//! low 2 bits (0 PCM, 1 XMA, 2 ADPCM, 3 xWMA).
//!
//! Each entry's play region is a file `NNN.pcm`, `.xma`, `.adpcm` or `.xwma` (raw wave data,
//! without a RIFF header: the header is made from the format fields). The rest is the headers,
//! the tables, and the padding between the waves.

use super::{file, u32le, within};
use aqr_disk::File;

const FLAG_COMPACT: u32 = 0x20000;

pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"WBND") && u32le(d, 4).is_some_and(|v| v > 3 && v < 0x1000)
}

fn segment(d: &[u8], at: usize) -> Option<std::ops::Range<usize>> {
    within(d, u32le(d, at)? as u64, u32le(d, at + 4)? as u64)
}

/// The entries' waves of bank `d`; None where it is not a bank, or is cut short in the headers.
pub fn files(d: &[u8]) -> Option<Vec<File>> {
    Some(waves(d)?.into_iter().map(|w| w.0).collect())
}

/// The layout of the Microsoft ADPCM blocks of a wave of `format` (XACT's ADPCM: its channels,
/// and its block alignment's field plus 22 bytes a channel), if it is one that aqr-msadpcm takes.
pub fn adpcm_layout(format: u32) -> Option<aqr_msadpcm::codec::Layout> {
    let channels = (format >> 2 & 7) as usize;
    (format & 3 == 2)
        .then(|| aqr_msadpcm::codec::Layout::new(channels, ((format >> 23 & 0xff) as usize + 22) * channels))?
}

/// The entries' waves of bank `d`, each a file and its format; as [`files`].
pub fn waves(d: &[u8]) -> Option<Vec<(File, u32)>> {
    if !starts(d) {
        return None;
    }
    let version = u32le(d, 4)?;
    let at = if version >= 42 { 12 } else { 8 };
    let bank = segment(d, at)?;
    let meta = segment(d, at + 8)?;
    let data = segment(d, at + 32)?;
    let b = &d[bank.clone()];
    let flags = u32le(b, 0)?;
    let count = u32le(b, 4)? as usize;
    let size = u32le(b, 72)? as usize;
    let align = u32le(b, 80)? as u64;
    let compact_format = u32le(b, 84).unwrap_or(0);
    let compact = flags & FLAG_COMPACT != 0;
    if count > meta.len() / size.max(1) || size < if compact { 4 } else { 20 } {
        return None;
    }
    let ext = |format: u32| match format & 3 {
        0 => "pcm",
        1 => "xma",
        2 => "adpcm",
        _ => "xwma",
    };
    let m = &d[meta];
    let mut files = Vec::with_capacity(count);
    for i in 0..count {
        let (range, format) = if compact {
            let this = u32le(m, i * size)?;
            let start = (this & 0x1F_FFFF) as u64 * align;
            let next = match i + 1 < count {
                true => (u32le(m, (i + 1) * size)? & 0x1F_FFFF) as u64 * align,
                false => data.len() as u64,
            };
            let len = next.checked_sub(start)?.saturating_sub((this >> 21) as u64);
            (within(&d[data.clone()], start, len), compact_format)
        } else {
            let (format, offset, len) = (u32le(m, i * size + 4)?, u32le(m, i * size + 8)?, u32le(m, i * size + 12)?);
            (within(&d[data.clone()], offset as u64, len as u64), format)
        };
        if let Some(r) = range.filter(|r| !r.is_empty()) {
            let r = data.start + r.start..data.start + r.end;
            files.push((file(format!("{i:03}.{}", ext(format)).into_bytes(), r), format));
        }
    }
    Some(files)
}

/// The formula of bank `d` where any of its waves is of ADPCM that aqr-msadpcm takes: those
/// waves' whole blocks rebuilt by `msadpcm-recons` from their records, the other waves contents
/// of their kinds by `kind` (a stream each), the rest (headers, tables, padding, a wave's partial
/// block) the stream `xact-xwb/rest`, stream 1, as aqr-disk's formula has it. None where no wave
/// is of such ADPCM (the bank is then aqr-disk's formula).
pub fn encode_with(d: &[u8], kind: fn(&[u8]) -> aqr_formula::Kind) -> Option<aqr_formula::Formula> {
    use aqr_formula::{Formula, Kind, Node};
    let mut waves = waves(d)?;
    if !waves.iter().any(|w| adpcm_layout(w.1).is_some()) {
        return None;
    }
    waves.sort_by_key(|w| w.0.extents[0].start);
    let mut f = Formula::default();
    f.push_content("raw", Kind::Unknown, Vec::new());
    f.push(aqr_disk::formula::rest("xact-xwb"), Vec::new());
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    let kept = |rest: &mut Vec<u8>, parts: &mut Vec<Node>, b: &[u8]| {
        if !b.is_empty() {
            parts.push(Node::slice(1, rest.len() as u64, b.len() as u64));
            rest.extend_from_slice(b);
        }
    };
    for (file, format) in &waves {
        let r = file.extents[0].clone();
        // (waves overlapping one before are the rest's)
        if r.start < at {
            continue;
        }
        kept(&mut rest, &mut parts, &d[at..r.start]);
        let w = &d[r.clone()];
        match adpcm_layout(*format).and_then(|l| aqr_msadpcm::formula::blocks(&mut f, l, w)) {
            Some((node, used)) => {
                parts.push(node);
                kept(&mut rest, &mut parts, &w[used..]);
            }
            None => {
                let k = kind(&file.name);
                let class = match k {
                    Kind::Unknown => "raw".to_string(),
                    Kind::Format(name) => format!("xact-xwb/{name}"),
                    k => format!("xact-xwb/{k}"),
                };
                parts.push(f.push_content(class, k, w.to_vec()));
            }
        }
        at = r.end;
    }
    kept(&mut rest, &mut parts, &d[at..]);
    f.streams[1].data = rest.into();
    f.root = Node::concat(parts);
    debug_assert_eq!(f.root.len(), d.len() as u64);
    Some(f)
}
