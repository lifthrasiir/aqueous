//! Files a file system stores compressed ([`crate::Compressed`]), taken apart into their data and
//! what rebuilds the compressed bytes from it: of WOF's LZX, aqr-lzx's record (its module
//! `lzx-recons` rebuilds the chunks), of WOF's XPRESS, aqr-xpress's (`xpress-recons`); the chunk
//! table kept as it is.

use std::ops::Range;

use crate::Compressed;

/// Bytes of data of a stream of chunks at most, of LZX (as aqr-wim's) and of XPRESS: a file of more
/// is several, so that the memory of a split, and of an application (the modules hold their data
/// whole), is bounded however large the file. (XPRESS's split takes about 6 bytes a byte of data:
/// streams of 32 MiB on 10 threads took the heap past 2 GB; its chunks are compressed apart, so
/// shorter streams cost little.)
pub const STREAM_DATA: usize = 32 << 20;
pub const XPRESS_STREAM_DATA: usize = 8 << 20;

/// A file taken apart: its data, its streams of chunks, and its chunk table (the bytes before
/// the chunks, as they are); whether of LZX (else XPRESS).
pub struct Unpacked {
    pub data: Vec<u8>,
    pub streams: Vec<Part>,
    pub table: Vec<u8>,
    pub lzx: bool,
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
    let chunk = Compressed::wof_chunk(algorithm)?;
    let lzx = algorithm == 1;
    let (table, chunks) = wof_chunks(s, len, chunk)?;
    let chunks: Vec<(&[u8], usize)> = chunks.iter().map(|(r, n)| (&s[r.clone()], *n)).collect();
    let mut data = vec![0; aqr_lzx::recons::data_len(&chunks)];
    let (mut streams, mut at) = (Vec::new(), 0);
    let most = if lzx { STREAM_DATA } else { XPRESS_STREAM_DATA };
    for c in chunks.chunks(most / chunk) {
        let n = aqr_lzx::recons::data_len(c);
        let out = &mut data[at..at + n];
        let record = match lzx {
            true => aqr_lzx::recons::split_into(c, out).ok()?,
            false => aqr_xpress::recons::split_into(c, out).ok()?,
        };
        streams.push(Part { data: n, bytes: c.iter().map(|c| c.0.len()).sum(), record });
        at += n;
    }
    Some(Unpacked { data, streams, table: s[table].to_vec(), lzx })
}
