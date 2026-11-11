//! InstallShield cabinets read as far as the formula needs: where their file data starts and
//! the chunks it is a sequence of (`FORMAT.md`, "The cabinet").

use std::ops::Range;

/// The signature at the start of every header and cabinet file.
pub const MAGIC: [u8; 4] = *b"ISc(";

/// Bytes of the common header, and of the smallest volume header after it (version 5's).
const COMMON: usize = 20;
const VOLUME_V5: usize = 40;

/// The most chunks of one cabinet read.
const CHUNKS: usize = 1 << 22;

/// A cabinet's file data: where it starts, and its chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cab {
    /// Where the data starts (the volume header's first field).
    pub start: usize,
    /// The DEFLATE stream of each chunk, after the two bytes of its length.
    pub chunks: Vec<Range<usize>>,
}

/// Where the file data of cabinet `d` starts, if `d` looks like a data volume: the signature, and
/// the volume header's offset of the data past the headers and within `d` (a header file's is
/// its own size).
fn data_start(d: &[u8]) -> Option<usize> {
    if d.len() < COMMON + VOLUME_V5 || d[..4] != MAGIC {
        return None;
    }
    let at = u32::from_le_bytes(d[COMMON..COMMON + 4].try_into().unwrap()) as usize;
    (COMMON + VOLUME_V5..d.len()).contains(&at).then_some(at)
}

/// Whether `d` starts like a data volume of a cabinet whose data begins with a chunk: the length of
/// a first chunk within `d`, and a DEFLATE block of a type that exists.
pub fn starts(d: &[u8]) -> bool {
    let Some(at) = data_start(d) else { return false };
    let Some(&[l0, l1, b]) = d.get(at..at + 3).map(|s| <&[u8; 3]>::try_from(s).unwrap()) else { return false };
    let n = u16::from_le_bytes([l0, l1]) as usize;
    n > 0 && at + 2 + n <= d.len() && (b >> 1) & 3 != 3
}

/// The chunks of cabinet `d` in order from where its data starts, to the last that lies within it
/// (a chunk is a length of two bytes and that many bytes of a DEFLATE stream, which the formula
/// finds whether it is). None where there is none.
pub fn parse(d: &[u8]) -> Option<Cab> {
    let start = data_start(d)?;
    let (mut chunks, mut at) = (Vec::new(), start);
    while chunks.len() < CHUNKS {
        let Some(n) = d.get(at..at + 2).map(|l| u16::from_le_bytes([l[0], l[1]]) as usize) else { break };
        if n == 0 || at + 2 + n > d.len() {
            break;
        }
        chunks.push(at + 2..at + 2 + n);
        at += 2 + n;
    }
    (!chunks.is_empty()).then_some(Cab { start, chunks })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cabinet of `chunks` after a volume header whose data starts at 0x200.
    pub(crate) fn made(chunks: &[&[u8]]) -> Vec<u8> {
        let mut d = MAGIC.to_vec();
        d.extend([0xb0, 0x04, 0x00, 0x02]); // version
        d.extend([0; 4]);
        d.extend(0x200u32.to_le_bytes()); // the descriptor's offset (in the header file)
        d.extend([0; 4]); // its size: none here
        d.extend(0x200u32.to_le_bytes()); // the volume header: where the data starts
        d.resize(0x200, 0);
        for c in chunks {
            d.extend((c.len() as u16).to_le_bytes());
            d.extend_from_slice(c);
        }
        d
    }

    #[test]
    fn chunks_are_found() {
        let d = made(&[&[0x01, 0x02, 0x00, 0xfd, 0xff, 7, 7], &[0x03, 0x00]]);
        assert!(starts(&d));
        let c = parse(&d).unwrap();
        assert_eq!(c.start, 0x200);
        assert_eq!(c.chunks, [0x202..0x209, 0x20b..0x20d]);
    }

    #[test]
    fn what_is_no_cabinet() {
        let d = made(&[&[0x01, 0x02, 0x00, 0xfd, 0xff, 7, 7]]);
        // a header file (the offset is its own size), another signature, a chunk of a block type
        // that does not exist, a chunk running past the end
        let mut hdr = d.clone();
        hdr[20..24].copy_from_slice(&(d.len() as u32).to_le_bytes());
        assert!(!starts(&hdr) && parse(&hdr).is_none());
        let mut sig = d.clone();
        sig[0] = b'M';
        assert!(!starts(&sig) && parse(&sig).is_none());
        let bad = made(&[&[0x07, 0, 0]]);
        assert!(!starts(&bad));
        assert!(parse(&d[..d.len() - 1]).is_none());
        assert!(!starts(&d[..30]));
    }
}
