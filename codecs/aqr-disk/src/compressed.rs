//! Files a file system stores compressed ([`crate::Compressed`]), taken apart into their data and
//! what rebuilds the compressed bytes from it: of WOF's LZX, aqr-lzx's record (its module
//! `lzx-recons` rebuilds the chunks, the chunk table kept as it is). WOF's XPRESS is not taken yet:
//! such a file is its compressed bytes, as any other.

use std::ops::Range;

use crate::Compressed;

/// Chunks of a stream of aqr-lzx at most (32 MiB of data, as aqr-wim's): a file of more is
/// several, so that the memory of a split is bounded however large the file.
pub const STREAM_CHUNKS: usize = 1024;

/// A file taken apart: its data, its streams of chunks, and its chunk table (the bytes before
/// the chunks, as they are).
pub struct Unpacked {
    pub data: Vec<u8>,
    pub streams: Vec<Part>,
    pub table: Vec<u8>,
}

/// A stream of chunks of a file taken apart: its bytes of data, of chunks, and its record.
pub struct Part {
    pub data: usize,
    pub bytes: usize,
    pub record: Vec<u8>,
}

/// Where a WOF stream's chunk table is, and of each chunk where it is and its data's length.
pub type Chunks = (Range<usize>, Vec<(Range<usize>, usize)>);

/// The chunk table and chunks of WOF stream `s` of `len` bytes of data in chunks of `chunk` bytes:
/// where the table is, and of each chunk where it is and its data's length. None where the table
/// does not hold the chunks within `s`, one after another to its end.
pub fn wof_chunks(s: &[u8], len: u64, chunk: usize) -> Option<Chunks> {
    let len = usize::try_from(len).ok()?;
    let k = len.div_ceil(chunk);
    if k == 0 {
        return None;
    }
    let es = if len > u32::MAX as usize { 8 } else { 4 };
    let table = (k - 1).checked_mul(es)?;
    let entry = |i: usize| -> Option<usize> {
        let e = s.get(i * es..i * es + es)?;
        let v = if es == 8 {
            u64::from_le_bytes(e.try_into().unwrap())
        } else {
            u32::from_le_bytes(e.try_into().unwrap()) as u64
        };
        usize::try_from(v).ok()?.checked_add(table)
    };
    let mut chunks = Vec::with_capacity(k);
    let mut at = table;
    for i in 0..k {
        let end = if i + 1 < k { entry(i)? } else { s.len() };
        let n = chunk.min(len - i * chunk);
        if end <= at || end > s.len() || end - at > n {
            return None;
        }
        chunks.push((at..end, n));
        at = end;
    }
    Some((0..table, chunks))
}

/// File stream `s` stored as `how`, taken apart: None where it is not taken (an algorithm not
/// taken, or a stream that does not read).
pub fn unpack(s: &[u8], how: Compressed) -> Option<Unpacked> {
    let Compressed::Wof { algorithm, len } = how;
    // (LZX only, as yet)
    if algorithm != 1 {
        return None;
    }
    let (table, chunks) = wof_chunks(s, len, Compressed::wof_chunk(algorithm)?)?;
    let chunks: Vec<(&[u8], usize)> = chunks.iter().map(|(r, n)| (&s[r.clone()], *n)).collect();
    let mut data = vec![0; aqr_lzx::recons::data_len(&chunks)];
    let (mut streams, mut at) = (Vec::new(), 0);
    for c in chunks.chunks(STREAM_CHUNKS) {
        let n = aqr_lzx::recons::data_len(c);
        let record = aqr_lzx::recons::split_into(c, &mut data[at..at + n]).ok()?;
        streams.push(Part { data: n, bytes: c.iter().map(|c| c.0.len()).sum(), record });
        at += n;
    }
    Some(Unpacked { data, streams, table: s[table].to_vec() })
}
