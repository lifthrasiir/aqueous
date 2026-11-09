//! EnCase's images (the Expert Witness Compression Format, EWF-E01: `.E01`, `.E02`, …), read from
//! libyal's description of the format (its document, no code): a header of 13 bytes, then
//! sections one after another, each of a descriptor of 76 bytes (its type, the next one's offset,
//! its size, an Adler-32); the `volume` (or `disk`, `data`) section's chunk size and sectors; each
//! `sectors` section the chunks, the `table` after it their offsets (from a base: 31 bits, the top
//! one telling a compressed chunk), `table2` a copy; `next` or `done` ending the segment file.
//! A chunk is a zlib stream, or its data as it is and an Adler-32.
//!
//! Each segment file is taken by itself: its disk is its chunks one after another (of the image's
//! disk, the part it holds; the first segment's starts it). Not read: EWF2 (`.Ex01`), the table
//! of EnCase 6.7.1 past 2 GiB (offsets of 32 bits: none in order, so no image), older images'
//! chunks of a table of no `sectors` section, `.L01`'s files.

use crate::chunked::{Chunk, Chunked, How};

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

fn le64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

/// Most sections read: of more, no segment file this big.
fn most(d: &[u8]) -> usize {
    d.len() / 76 + 16
}

/// The chunks of EWF-E01 segment file `d`, where it is one whose sections and tables read: each
/// table's chunks in order within the `sectors` section before it, of the chunk size of the volume
/// section (a full one but the last of the image, where the segment file is its last: `done`).
pub fn chunks(d: &[u8]) -> Option<Chunked> {
    let h = d.get(..13)?;
    if &h[..8] != b"EVF\x09\x0d\x0a\xff\x00" || h[8] != 1 || h[11..13] != [0, 0] {
        return None;
    }
    let (mut chunk, mut total) = (None, 0u64);
    let mut sectors: Option<std::ops::Range<usize>> = None;
    let mut tables: Vec<(u64, Vec<u32>, std::ops::Range<usize>)> = Vec::new();
    let mut done = false;
    let mut at = 13usize;
    for _ in 0..most(d) {
        let s = d.get(at..at.checked_add(76)?)?;
        if aqr_common::adler32::hash(&s[..72]) != le32(s, 72) {
            return None;
        }
        let ty = &s[..16];
        let ty = &ty[..ty.iter().position(|&b| b == 0).unwrap_or(16)];
        let (next, size) = (usize::try_from(le64(s, 16)).ok()?, usize::try_from(le64(s, 24)).ok()?);
        let body = d.get(at + 76..at.checked_add(size.max(76))?)?;
        match ty {
            b"volume" | b"disk" | b"data" if body.len() >= 24 => {
                let (spc, bps, n) = (le32(body, 8) as u64, le32(body, 12) as u64, le64(body, 16));
                let c = spc.checked_mul(bps).filter(|&c| c > 0 && c <= 1 << 26)?;
                if chunk.is_some_and(|k| k != c) {
                    return None;
                }
                chunk = Some(c);
                total = n.checked_mul(bps)?;
            }
            b"sectors" => sectors = Some(at + 76..at + size.max(76)),
            b"table" => {
                let sec = sectors.take()?;
                let t = body.get(..24)?;
                if aqr_common::adler32::hash(&t[..20]) != le32(t, 20) {
                    return None;
                }
                let (n, base) = (le32(t, 0) as usize, le64(t, 8));
                let e = body.get(24..24usize.checked_add(n.checked_mul(4)?)?)?;
                tables.push((base, e.as_chunks::<4>().0.iter().map(|e| u32::from_le_bytes(*e)).collect(), sec));
            }
            b"done" => {
                done = true;
                break;
            }
            b"next" => break,
            _ => {}
        }
        if next <= at {
            return None;
        }
        at = next;
    }
    let chunk = chunk?;
    let count: usize = tables.iter().map(|t| t.1.len()).sum();
    if count == 0 {
        return None;
    }
    // (the last chunk of the image: what of its disk the full ones before do not give)
    let last = if done {
        let before = (total / chunk).checked_sub(u64::from(total % chunk == 0))?.checked_mul(chunk)?;
        total.checked_sub(before).filter(|&l| l > 0)?
    } else {
        chunk
    };
    let mut out = Vec::with_capacity(count);
    let mut disk = 0u64;
    for (t, (base, e, sec)) in tables.iter().enumerate() {
        let off = |x: u32| usize::try_from(base.checked_add(u64::from(x & 0x7fff_ffff))?).ok();
        for (i, &x) in e.iter().enumerate() {
            let start = off(x)?;
            let end = match e.get(i + 1) {
                Some(&y) => off(y)?,
                None => sec.end,
            };
            if start < sec.start || end > sec.end || start >= end {
                return None;
            }
            let n = if t + 1 == tables.len() && i + 1 == e.len() { last } else { chunk };
            let (file, how) = if x >> 31 != 0 {
                (start..end, How::Zlib)
            } else {
                let file = start..end.checked_sub(4)?;
                let data = d.get(file.clone())?;
                if file.len() as u64 != n || aqr_common::adler32::hash(data) != le32(d, file.end) {
                    return None;
                }
                (file, How::Raw)
            };
            out.push(Chunk { disk: disk..disk.checked_add(n)?, file, how });
            disk += n;
        }
    }
    Some(Chunked { len: disk, chunks: out })
}
