//! Inno Setup installers (4.0 on: the offset table of the loader, of version 1 from 5.1.5,
//! earlier ones before): the stream of LZMA that holds the files of a solid installer found by the table the loader
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

/// Bytes of an earlier table (versions 04, 05 and 07, Inno Setup 4.0 to 5.1.4): the signature,
/// seven fields of 4 bytes (07: six and the CRC-32 of the rest).
const OLD_TABLE: usize = 40;

/// The start of a chunk of file data: `zlb`, then 0x1a. The LZMA properties byte and the
/// dictionary size follow, then the stream.
const CHUNK: &[u8; 4] = b"zlb\x1a";

/// Bytes before a chunk's stream: the signature, the properties and the dictionary size.
const CHUNK_HEAD: usize = 9;

/// The most data a stream is split to.

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
/// and offsets in the order the layout has them, within the file. Of 5.1.5 on, the table of
/// version 1 among the resources; before, the earlier one ([`old_table`]).
pub fn table(d: &[u8]) -> Option<Table> {
    new_table(d).or_else(|| old_table(d))
}

fn new_table(d: &[u8]) -> Option<Table> {
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

/// The table of Inno Setup 4.0 to 5.1.4: `Inno` at 0x30, then the table's offset and its
/// complement; the table (versions 04 and 05: the setup program's offset, its compressed and
/// uncompressed sizes, its CRC-32, the setup header's and the data's offsets after the total;
/// 07: the same but the compressed size, then the CRC-32 of the rest) after the setup program and
/// the setup header, which follow the data in either order.
fn old_table(d: &[u8]) -> Option<Table> {
    let (at, not) = (u32le(d, 0x34)?, u32le(d, 0x38)?);
    if d.get(0x30..0x34) != Some(b"Inno") || at as u32 != !(not as u32) {
        return None;
    }
    let t = d.get(at..at + OLD_TABLE)?;
    if &t[..7] != b"rDlPtS0" || &t[8..12] != b"\x87eVx" {
        return None;
    }
    let (total, exe, header, data) = match t[7] {
        b'4' | b'5' => (u32le(t, 12)?, u32le(t, 16)?, u32le(t, 32)?, u32le(t, 36)?),
        b'7' if u32le(t, 36) == Some(crc32fast::hash(&t[..36]) as usize) => {
            (u32le(t, 12)?, u32le(t, 16)?, u32le(t, 28)?, u32le(t, 32)?)
        }
        _ => return None,
    };
    let ok = 0x40 <= data && data < exe.min(header) && exe.max(header) < at && at + OLD_TABLE <= total;
    (ok && total <= d.len()).then_some(Table { total, exe, header, data })
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Where an installer's own bytes end (before a certificate), if `d` is one: what the
/// extent of its PE image takes in, as the loader reads it. Before 5.1.5, only where its data
/// starts with a chunk of LZMA: of zlib or bzip2 (4.0's default), nothing of it is taken, and
/// what the extent leaves out archives smaller apart.
pub fn end(d: &[u8]) -> Option<usize> {
    new_table(d).or_else(|| old_table(d).filter(|t| lzma_at(d, t.data))).map(|t| t.total)
}

/// Whether a chunk of LZMA starts at `at` of `d`: its signature, a valid properties byte, a
/// dictionary of 4 KiB to 1 GiB, and the range coder's first byte, 0.
fn lzma_at(d: &[u8], at: usize) -> bool {
    let dict = u32le(d, at + 5).unwrap_or(0);
    d.get(at..at + 4) == Some(CHUNK)
        && d.get(at + 4).and_then(|&p| Props::from_byte(p)).is_some()
        && (4 << 10..=1 << 30).contains(&dict)
        && d.get(at + CHUNK_HEAD) == Some(&0)
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
    // (the most the streams decode to, all together: aqr-lzma's `-Z lzma:limit`)
    let limit = aqr_lzma::formula::MAX.get();
    // (the data ends where the setup header or, before 4.1, the setup program starts)
    let end = t.header.min(t.exe);
    let mut total = 0usize;
    while d.get(at..at + 4) == Some(CHUNK) && at < end {
        let (Some(&p), Some(dict)) = (d.get(at + 4), u32le(d, at + 5)) else { break };
        let Some(props) = Props::from_byte(p) else { break };
        let from = at + CHUNK_HEAD;
        let Some(body) = d.get(from..end) else { break };
        let Ok(sp) = recons::split(body, props, dict as u32, None, limit.saturating_sub(total), threads) else {
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

    /// An installer of before 5.1.5 of the given chunks: the table of version `v` (b'4', b'5'
    /// or b'7') at its end, found by the pointer at 0x30; the setup program before the setup
    /// header (4.0) or after it (4.1 on).
    fn old_installer(v: u8, chunks: &[&[u8]]) -> (Vec<u8>, Vec<Range<usize>>) {
        let data = 0x80;
        let mut d = vec![0u8; data];
        let mut ranges = Vec::new();
        for text in chunks {
            let alone = aqr_lzma::greedy::alone(text);
            d.extend_from_slice(CHUNK);
            d.extend_from_slice(&alone[..5]);
            ranges.push(d.len()..d.len() + alone.len() - 13);
            d.extend_from_slice(&alone[13..]);
        }
        let (header, exe);
        if v == b'7' {
            header = d.len();
            d.extend_from_slice(b"Inno Setup Setup Data (4.1.8)");
            exe = d.len();
            d.extend_from_slice(&[7; 40]);
        } else {
            exe = d.len();
            d.extend_from_slice(&[7; 40]);
            header = d.len();
            d.extend_from_slice(b"Inno Setup Setup Data (4.0.5)");
        }
        let at = d.len();
        let total = at + OLD_TABLE;
        let mut t = b"rDlPtS0".to_vec();
        t.push(v);
        t.extend_from_slice(b"\x87eVx");
        let fields: &[usize] = match v {
            b'7' => &[total, exe, 1000, 0, header, data],
            _ => &[total, exe, 40, 1000, 0, header, data],
        };
        for &f in fields {
            t.extend((f as u32).to_le_bytes());
        }
        if v == b'7' {
            t.extend(crc32fast::hash(&t).to_le_bytes());
        }
        d.extend(t);
        d[0x30..0x34].copy_from_slice(b"Inno");
        d[0x34..0x38].copy_from_slice(&(at as u32).to_le_bytes());
        d[0x38..0x3c].copy_from_slice(&(!(at as u32)).to_le_bytes());
        (d, ranges)
    }

    #[test]
    fn old_tables_are_found_and_split() {
        let text: Vec<u8> = (0..30000).flat_map(|i: u32| format!("{} ", i % 113).into_bytes()).collect();
        for v in [b'4', b'5', b'7'] {
            let (d, ranges) = old_installer(v, &[&text, &text[..9000]]);
            let t = table(&d).unwrap_or_else(|| panic!("version 0{}", v as char));
            assert_eq!((t.total, t.data), (d.len(), 0x80));
            assert_eq!(end(&d), Some(d.len()));
            let pieces = split(&d, 2);
            assert_eq!(pieces.len(), 2, "version 0{}", v as char);
            for (p, r) in pieces.iter().zip(&ranges) {
                assert_eq!(&p.range, r);
                assert_eq!(recons::join(&p.data, &p.record, 1).as_deref(), Some(&d[r.clone()]));
            }
        }
    }

    #[test]
    fn old_zlib_installers_have_no_end() {
        // (a chunk of zlib: what the image's extent leaves out is kept so, nothing taken of it)
        let (mut d, ranges) = old_installer(b'5', &[b"abc"]);
        assert!(end(&d).is_some());
        let at = ranges[0].start - 5;
        d[at..at + 6].copy_from_slice(&[0x78, 0xda, 0xcc, 0xbd, 0x7d, 0x5c]);
        assert!(table(&d).is_some());
        assert_eq!(end(&d), None);
    }

    #[test]
    fn old_tables_must_hold() {
        let (d, _) = old_installer(b'7', &[b"abc"]);
        let at = d.len() - OLD_TABLE;
        // a bad CRC-32 (version 07), a pointer of a bad complement, an unknown version
        let mut e = d.clone();
        e[at + 20] ^= 1;
        assert_eq!(table(&e), None);
        let mut e = d.clone();
        e[0x38] ^= 1;
        assert_eq!(table(&e), None);
        let mut e = d.clone();
        e[at + 7] = b'3';
        assert_eq!(table(&e), None);
        assert!(table(&d).is_some());
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
