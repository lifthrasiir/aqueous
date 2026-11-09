//! Apple's disk images (UDIF, `.dmg`): a `koly` trailer of 512 bytes at the end, telling where
//! the data fork (the chunks) and an XML property list are; the list's `blkx` entries, each a
//! `mish` block table (base64), of chunks: of a run of sectors of the disk, where its bytes are in
//! the data fork and how they are stored (zeros, raw, or compressed by zlib, bzip2, LZFSE, LZMA,
//! ADC). Read from the format's public descriptions; the disk's sectors are of 512 bytes.

use std::ops::Range;

fn be32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}

fn be64(b: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(b[p..p + 8].try_into().unwrap())
}

pub use crate::chunked::{Chunk, How};

/// A UDIF image: its disk's bytes, its chunks (in the order of the tables), where its property
/// list is, and its trailer.
#[derive(Debug, Clone)]
pub struct Udif {
    pub len: u64,
    pub chunks: Vec<Chunk>,
    pub xml: Range<usize>,
    pub koly: Range<usize>,
}

/// Most chunks read (of a table and in all), as a table of more is no table of an image this big.
fn most(d: &[u8]) -> usize {
    d.len() / 16 + 1024
}

/// The UDIF image of `d`, where it ends with a trailer whose tables read: None where a chunk is
/// outside the data fork, past the disk, or of a kind not known.
pub fn parse(d: &[u8]) -> Option<Udif> {
    let at = d.len().checked_sub(512)?;
    let k = &d[at..];
    if &k[..4] != b"koly" || be32(k, 4) != 4 || be32(k, 8) != 512 {
        return None;
    }
    let (fork, fork_len) = (usize::try_from(be64(k, 24)).ok()?, usize::try_from(be64(k, 32)).ok()?);
    let (xml, xml_len) = (usize::try_from(be64(k, 216)).ok()?, usize::try_from(be64(k, 224)).ok()?);
    let fork = fork..fork.checked_add(fork_len)?;
    let xml = xml..xml.checked_add(xml_len)?;
    if fork.end > at || xml.end > at || xml.is_empty() {
        return None;
    }
    let len = be64(k, 492).checked_mul(512)?;
    let mut chunks = Vec::new();
    for table in blkx(&d[xml.clone()]) {
        if table.len() < 204 || &table[..4] != b"mish" {
            return None;
        }
        let (first, data) = (be64(&table, 8), be64(&table, 24));
        let n = be32(&table, 200) as usize;
        for i in 0..n.min(most(d)) {
            let c = table.get(204 + 40 * i..244 + 40 * i)?;
            let ty = be32(c, 0);
            let how = match ty {
                0 | 2 => How::Zeros,
                1 => How::Raw,
                0x8000_0004 => How::Adc,
                0x8000_0005 => How::Zlib,
                0x8000_0006 => How::Bzip2,
                0x8000_0007 => How::Lzfse,
                0x8000_0008 => How::Lzma,
                // (a comment, the end of the table)
                0x7fff_fffe => continue,
                0xffff_ffff => break,
                _ => return None,
            };
            let sector = first.checked_add(be64(c, 8))?;
            let disk = sector.checked_mul(512)?..sector.checked_add(be64(c, 16))?.checked_mul(512)?;
            let off = usize::try_from(be64(c, 24).checked_add(data)?).ok()?;
            let file = fork.start.checked_add(off)?
                ..fork.start.checked_add(off)?.checked_add(usize::try_from(be64(c, 32)).ok()?)?;
            let bytes = match how {
                How::Zeros => 0..0,
                _ => file,
            };
            if bytes.end > fork.end || disk.end > len || (how != How::Zeros && bytes.is_empty()) {
                return None;
            }
            if !disk.is_empty() {
                chunks.push(Chunk { disk, file: bytes, how });
            }
            if chunks.len() > most(d) {
                return None;
            }
        }
    }
    Some(Udif { len, chunks, xml, koly: at..d.len() })
}

/// The block tables of property list `xml`: the base64 data of each dictionary of the array of
/// key `blkx` (a plain scan for the tags, as hdiutil writes the list).
fn blkx(xml: &[u8]) -> Vec<Vec<u8>> {
    let find = |h: &[u8], n: &[u8], from: usize| h.get(from..)?.windows(n.len()).position(|w| w == n).map(|p| p + from);
    let Some(start) = find(xml, b"<key>blkx</key>", 0) else { return Vec::new() };
    let end = find(xml, b"</array>", start).unwrap_or(xml.len());
    let mut out = Vec::new();
    let mut at = start;
    while let Some(k) = find(xml, b"<key>Data</key>", at).filter(|&k| k < end) {
        let Some(open) = find(xml, b"<data>", k) else { break };
        let Some(close) = find(xml, b"</data>", open) else { break };
        out.extend(base64(&xml[open + 6..close]));
        at = close;
    }
    out
}

/// The bytes of base64 `s` (whitespace skipped), where it decodes.
fn base64(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
