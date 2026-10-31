//! An Ogg page (RFC 3533 §6): its header's fields, lacing values and body.

use std::ops::Range;

use crate::crc;

/// The capture pattern.
pub const MAGIC: &[u8; 4] = b"OggS";
/// Bytes of a page header before its lacing values.
pub const HEADER: usize = 27;
/// Header type flags: a page continuing a packet, the first page of a stream, its last.
pub const CONTINUED: u8 = 1;
pub const BOS: u8 = 2;
pub const EOS: u8 = 4;

/// A page of data, by where its parts are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub kind: u8,
    pub granule: u64,
    pub serial: u32,
    pub seq: u32,
    /// Its lacing values and its body in the data.
    pub lacing: Range<usize>,
    pub body: Range<usize>,
}

impl Page {
    /// The page at `at` in `d`, where one is there whole: the capture pattern, version 0 and its
    /// CRC right.
    pub fn at(d: &[u8], at: usize) -> Option<Page> {
        let h = d.get(at..)?.first_chunk::<HEADER>()?;
        if &h[..4] != MAGIC || h[4] != 0 {
            return None;
        }
        let le32 = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap());
        let lacing = at + HEADER..at + HEADER + h[26] as usize;
        let body_len: usize = d.get(lacing.clone())?.iter().map(|&l| l as usize).sum();
        let body = lacing.end..lacing.end + body_len;
        let page = d.get(at..body.end)?;
        let mut c = crc::update(0, &page[..22]);
        c = crc::update(c, &[0; 4]);
        if crc::update(c, &page[26..]) != le32(22) {
            return None;
        }
        Some(Page {
            kind: h[5],
            granule: u64::from_le_bytes(h[6..14].try_into().unwrap()),
            serial: le32(14),
            seq: le32(18),
            lacing,
            body,
        })
    }

    /// Where the page ends.
    pub fn end(&self) -> usize {
        self.body.end
    }
}

/// The pages at the start of `d` one after another, up to the first place where none is.
pub fn pages(d: &[u8]) -> Vec<Page> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = Page::at(d, at) {
        at = p.end();
        out.push(p);
    }
    out
}

/// Writes a page: its header of `lacing` values, then `body` (their sum of bytes), its CRC
/// computed.
pub fn write(out: &mut Vec<u8>, kind: u8, granule: u64, serial: u32, seq: u32, lacing: &[u8], body: &[u8]) {
    let start = out.len();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[0, kind]);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(lacing);
    out.extend_from_slice(body);
    let c = crc::update(0, &out[start..]);
    out[start + 22..start + 26].copy_from_slice(&c.to_le_bytes());
}
