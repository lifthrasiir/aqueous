//! PNG chunk layer.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ihdr {
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub color: u8,
    pub compression: u8,
    pub filter: u8,
    pub interlace: u8,
}

impl aqr_common::wire::Wire for Ihdr {
    fn put(&self, out: &mut Vec<u8>) {
        (self.width, self.height).put(out);
        [self.depth, self.color, self.compression, self.filter, self.interlace].put(out);
    }

    fn get(b: &mut &[u8]) -> Option<Self> {
        let (width, height) = <(u32, u32)>::get(b)?;
        let [depth, color, compression, filter, interlace] = <[u8; 5]>::get(b)?;
        Some(Ihdr { width, height, depth, color, compression, filter, interlace })
    }
}

pub const SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

#[derive(Debug, Clone, Copy)]
pub struct Chunk<'a> {
    pub kind: [u8; 4],
    pub data: &'a [u8],
    pub crc: u32,
}

/// Splits the file after the signature into chunks; returns the chunks and any trailing bytes
/// that do not form a complete chunk.
pub fn chunks(file: &[u8]) -> Option<(Vec<Chunk<'_>>, &[u8])> {
    if file.get(..8)? != SIGNATURE {
        return None;
    }
    let mut v = vec![];
    let mut at = 8;
    while at + 12 <= file.len() {
        let n = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        if n > file.len() - at - 12 {
            break;
        }
        let kind = file[at + 4..at + 8].try_into().unwrap();
        let data = &file[at + 8..at + 8 + n];
        let crc = u32::from_be_bytes(file[at + 8 + n..at + 12 + n].try_into().unwrap());
        v.push(Chunk { kind, data, crc });
        at += 12 + n;
        if &kind == b"IEND" {
            break;
        }
    }
    Some((v, &file[at..]))
}

impl Ihdr {
    pub fn parse(d: &[u8]) -> Option<Ihdr> {
        if d.len() != 13 {
            return None;
        }
        Some(Ihdr {
            width: u32::from_be_bytes(d[0..4].try_into().unwrap()),
            height: u32::from_be_bytes(d[4..8].try_into().unwrap()),
            depth: d[8],
            color: d[9],
            compression: d[10],
            filter: d[11],
            interlace: d[12],
        })
    }
    pub fn channels(&self) -> usize {
        match self.color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 0,
        }
    }
}
