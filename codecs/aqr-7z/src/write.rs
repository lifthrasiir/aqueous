//! A simple 7z writer, for tests here and of other crates, where 7-Zip is not to be had: the
//! files with data in one folder of LZMA2 (aqr-lzma's greedy encoder, a dictionary of 1 MiB),
//! with their CRC-32s, and a plain header.

use aqr_common::crc32;

use crate::header::{MAGIC, SIGNATURE};

fn number(out: &mut Vec<u8>, v: u64) {
    // (the first byte's high 1 bits count the bytes after it, little-endian)
    let n = (0..8).find(|&n| v < 1u64 << (7 * (n + 1))).unwrap_or(8);
    let first = if n == 8 { 0xff } else { (0xff00u16 >> n) as u8 | (v >> (8 * n)) as u8 };
    out.push(first);
    out.extend(&v.to_le_bytes()[..n]);
}

/// The archive of `files` (a name and contents each; empty contents make an empty file).
pub fn solid(files: &[(&str, &[u8])]) -> Vec<u8> {
    let data: Vec<u8> = files.iter().flat_map(|f| f.1.iter().copied()).collect();
    let full: Vec<&[u8]> = files.iter().map(|f| f.1).filter(|d| !d.is_empty()).collect();
    let packed = aqr_lzma::greedy::lzma2(&data, 1 << 20);
    let mut h = vec![1];
    if !full.is_empty() {
        h.push(4);
        h.extend([6, 0]);
        number(&mut h, 1);
        h.push(9);
        number(&mut h, packed.len() as u64);
        h.extend([0, 7, 11, 1, 0]);
        // a coder: an id of 1 byte with properties, LZMA2 of a dictionary of 1 MiB
        h.extend([1, 0x21, 0x21, 1, 16]);
        h.push(12);
        number(&mut h, data.len() as u64);
        h.extend([0, 8, 13]);
        number(&mut h, full.len() as u64);
        h.push(9);
        full[..full.len() - 1].iter().for_each(|d| number(&mut h, d.len() as u64));
        h.extend([10, 1]);
        full.iter().for_each(|d| h.extend(crc32::hash(d).to_le_bytes()));
        h.extend([0, 0]);
    }
    h.push(5);
    number(&mut h, files.len() as u64);
    if files.iter().any(|f| f.1.is_empty()) {
        let mut bits = vec![0u8; files.len().div_ceil(8)];
        files.iter().enumerate().filter(|f| f.1.1.is_empty()).for_each(|(i, _)| bits[i / 8] |= 0x80 >> (i % 8));
        h.push(14);
        number(&mut h, bits.len() as u64);
        h.extend(bits);
        // (files, not directories)
        let empty = files.iter().filter(|f| f.1.is_empty()).count();
        h.push(15);
        number(&mut h, empty.div_ceil(8) as u64);
        h.extend(
            (0..empty.div_ceil(8)).map(|i| if 8 * (i + 1) <= empty { 0xff } else { 0xff00u16 >> (empty % 8) } as u8),
        );
    }
    let mut names = vec![0];
    for (n, _) in files {
        n.encode_utf16().chain([0]).for_each(|u| names.extend(u.to_le_bytes()));
    }
    h.push(17);
    number(&mut h, names.len() as u64);
    h.extend(names);
    h.extend([0, 0]);
    let mut start = Vec::with_capacity(20);
    start.extend((if full.is_empty() { 0 } else { packed.len() as u64 }).to_le_bytes());
    start.extend((h.len() as u64).to_le_bytes());
    start.extend(crc32::hash(&h).to_le_bytes());
    let mut out = MAGIC.to_vec();
    out.extend([0, 4]);
    out.extend(crc32::hash(&start).to_le_bytes());
    out.extend(start);
    debug_assert_eq!(out.len(), SIGNATURE);
    if !full.is_empty() {
        out.extend(packed);
    }
    out.extend(h);
    out
}
