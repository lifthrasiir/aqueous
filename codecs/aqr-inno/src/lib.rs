//! Inno Setup installers (6.x, and 5.1.5 on, whose loader has the offset table of version 1):
//! the stream of LZMA that holds the files of a solid installer found by the table the loader
//! carries, and split by aqr-lzma into its data and a record of its parse. The formula is an
//! application of aqr-lzma's `lzma-recons` over a slice of the installer, so this crate has no
//! module of its own; `FORMAT.md` tells what is taken.

use std::ops::Range;

use aqr_lzma::lzma::Props;
use aqr_lzma::recons;

/// The signature of the loader's offset table, 12 bytes (Inno Setup 5.1.5 on).
const ID: &[u8; 12] = b"rDlPtS\xcd\xe6\xd7\x7b\x0b\x2a";

/// Bytes of the table: the signature, eight fields of 4 bytes, the last the CRC-32 of the rest.
const TABLE: usize = 44;

/// The start of a chunk of file data: `zlb`, then 0x1a. The LZMA properties byte and the
/// dictionary size follow, then the stream.
const CHUNK: &[u8; 4] = b"zlb\x1a";

/// Bytes before a chunk's stream: the signature, the properties and the dictionary size.
const CHUNK_HEAD: usize = 9;

/// The most data a stream is split to.
const LIMIT: usize = aqr_lzma::formula::LIMIT;

/// The loader's offset table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table {
    /// The size of the installer without a signature (a certificate that may follow).
    pub total: usize,
    /// Where the setup program that the installer extracts as its own, compressed, starts.
    pub exe: usize,
    /// Where the setup header starts (`Inno Setup Setup Data (x.y.z)`).
    pub header: usize,
    /// Where the file data starts, the chunks.
    pub data: usize,
}

impl Table {
    /// The bytes of the table as the loader holds it (the fields the others are not taken
    /// from are zero), for building installers in tests.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut t = ID.to_vec();
        for v in [1, self.total, self.exe, 0, 0, self.header, self.data] {
            t.extend((v as u32).to_le_bytes());
        }
        t.extend(crc32fast::hash(&t).to_le_bytes());
        t
    }
}

fn u32le(d: &[u8], p: usize) -> Option<usize> {
    Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?) as usize)
}

/// The offset table of the installer `d` (a PE image), if it has one that holds: its CRC-32,
/// and offsets in the order the layout has them, within the file.
pub fn table(d: &[u8]) -> Option<Table> {
    // (the table is among the resources, near the start)
    let head = &d[..d.len().min(8 << 20)];
    let mut from = 0;
    while let Some(i) = find(&head[from..], ID) {
        let at = from + i;
        from = at + 1;
        let Some(t) = d.get(at..at + TABLE) else { continue };
        if u32le(t, 12) != Some(1) || u32le(t, 40) != Some(crc32fast::hash(&t[..40]) as usize) {
            continue;
        }
        let (total, exe, header, data) = (u32le(t, 16)?, u32le(t, 20)?, u32le(t, 32)?, u32le(t, 36)?);
        if data >= at + TABLE && data < header && header < exe && exe < total && total <= d.len() {
            return Some(Table { total, exe, header, data });
        }
    }
    None
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Where an installer's own bytes end (before a certificate), if `d` is one: what the
/// extent of its PE image takes in, as the loader reads it.
pub fn end(d: &[u8]) -> Option<usize> {
    table(d).map(|t| t.total)
}

/// A stream of LZMA of the installer taken apart: the bytes it takes, its data and its record
/// (for aqr-lzma's `lzma-recons`).
#[derive(Debug, Default)]
pub struct Piece {
    pub range: Range<usize>,
    pub data: Vec<u8>,
    pub record: Vec<u8>,
}

/// The chunks of file data of the installer `d` that hold LZMA streams ending with a marker
/// (each on its own after the 9 bytes of its head), split on up to `threads` threads, one after
/// another from where the data starts and as far as they go. Empty where `d` is no installer
/// of this layout, or the data is not LZMA (zlib, bzip2, encrypted).
pub fn split(d: &[u8], threads: usize) -> Vec<Piece> {
    let Some(t) = table(d) else { return Vec::new() };
    let (mut pieces, mut at) = (Vec::new(), t.data);
    let mut total = 0usize;
    while d.get(at..at + 4) == Some(CHUNK) && at < t.header {
        let (Some(&p), Some(dict)) = (d.get(at + 4), u32le(d, at + 5)) else { break };
        let Some(props) = Props::from_byte(p) else { break };
        let from = at + CHUNK_HEAD;
        let Some(body) = d.get(from..t.header) else { break };
        let Ok(sp) = recons::split(body, props, dict as u32, None, LIMIT.saturating_sub(total), threads) else {
            break;
        };
        total += sp.data.len();
        at = from + sp.used;
        pieces.push(Piece { range: from..at, data: sp.data, record: sp.record });
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of the right CRC-32 within a made-up file.
    fn made(total: usize, exe: usize, header: usize, data: usize) -> Vec<u8> {
        let mut d = vec![0u8; 0x40];
        d.extend(Table { total, exe, header, data }.to_bytes());
        d.resize(total, 0);
        d
    }

    #[test]
    fn table_is_found() {
        let d = made(1000, 900, 800, 0x80);
        assert_eq!(table(&d), Some(Table { total: 1000, exe: 900, header: 800, data: 0x80 }));
        assert_eq!(end(&d), Some(1000));
    }

    #[test]
    fn table_must_hold() {
        // a bad CRC-32, offsets out of order, a total past the file
        let mut d = made(1000, 900, 800, 0x80);
        d[0x40 + 40] ^= 1;
        assert_eq!(table(&d), None);
        assert_eq!(table(&made(1000, 700, 800, 0x80)), None);
        let d = made(1000, 900, 800, 0x80);
        assert_eq!(table(&d[..999]), None);
        assert_eq!(table(&[0; 100]), None);
    }

    #[test]
    fn no_chunks_without_stream() {
        assert!(split(&made(1000, 900, 800, 0x80), 1).is_empty());
    }

    #[test]
    fn a_chunk_head_past_the_header_ends_the_walk() {
        // the chunk's signature in the last bytes before the setup header
        let mut d = made(1000, 900, 0x86, 0x80);
        d[0x80..0x84].copy_from_slice(CHUNK);
        assert!(split(&d, 1).is_empty());
    }

    /// An installer of the given chunks (data, each an `.lzma` file's stream): the table, then
    /// the chunks, the setup header and the setup program.
    fn installer(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Range<usize>>) {
        let data = 0x80;
        let mut d = made(data, data, data, data);
        let mut ranges = Vec::new();
        for text in chunks {
            let alone = aqr_lzma::greedy::alone(text);
            d.extend_from_slice(CHUNK);
            d.extend_from_slice(&alone[..5]);
            ranges.push(d.len()..d.len() + alone.len() - 13);
            d.extend_from_slice(&alone[13..]);
        }
        let header = d.len();
        d.extend_from_slice(b"Inno Setup Setup Data (6.4.2)");
        let exe = d.len();
        d.extend_from_slice(&[7; 40]);
        let t = Table { total: d.len(), exe, header, data };
        d[0x40..0x40 + TABLE].copy_from_slice(&t.to_bytes());
        (d, ranges)
    }

    #[test]
    fn chunks_are_split_and_rebuilt() {
        let texts: Vec<Vec<u8>> = (0..3u8)
            .map(|k| {
                (0..20000)
                    .map(|i| {
                        format!(
                            "line {} of {k}
",
                            i % 97
                        )
                    })
                    .collect::<String>()
                    .into_bytes()
            })
            .collect();
        let (d, ranges) = installer(&texts.iter().map(|t| &t[..]).collect::<Vec<_>>());
        assert!(table(&d).is_some());
        let pieces = split(&d, 2);
        assert_eq!(pieces.len(), 3);
        for ((p, r), text) in pieces.iter().zip(&ranges).zip(&texts) {
            assert_eq!(&p.range, r);
            assert_eq!(&p.data, text);
            assert_eq!(recons::join(&p.data, &p.record, 1).as_deref(), Some(&d[r.clone()]));
        }
    }

    #[test]
    fn a_broken_chunk_ends_the_walk() {
        let text: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
        let (mut d, ranges) = installer(&[&text, &text]);
        // (the second chunk's stream damaged)
        let at = ranges[1].start + 40;
        d[at] ^= 0xff;
        d[at + 1] ^= 0x5a;
        assert!(split(&d, 1).len() <= 1);
    }
}
