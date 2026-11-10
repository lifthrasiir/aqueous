//! One large file deflated on several threads (pigz's way): its data cut in chunks, each
//! deflated on the pool with the 32 KiB before it as its preset dictionary and ended on a byte
//! boundary by an empty stored block (`Z_SYNC_FLUSH`), the last one final; the chunks' bytes
//! in order are one raw DEFLATE stream, and their CRC-32s are combined.
//!
//! Only data of at least [`MIN`] bytes is cut, and only where there are threads for it; any
//! other is deflated whole, as zlib 1.3.1 does it (so that a small file's output does not
//! depend on this).

use aqr_common::{crc32, pool};
use aqr_flate::zlib;

/// The bytes of a chunk.
pub(crate) const CHUNK: usize = 512 << 10;
/// The fewest bytes of data to cut in chunks.
pub(crate) const MIN: usize = 4 << 20;
/// The preset dictionary of a chunk: the window's size.
const DICT: usize = 32 << 10;

/// `d` as a raw DEFLATE stream with its CRC-32: deflated at `params` as zlib does, or in chunks
/// (cut at multiples of `chunk`) on up to `threads` threads where `d` has `min` bytes at least
/// and `threads` is above 1. `None` for invalid parameters.
pub(crate) fn deflate_crc(d: &[u8], params: zlib::Params, threads: usize) -> Option<(Vec<u8>, u32)> {
    deflate_crc_in(d, params, threads, CHUNK, MIN)
}

pub(crate) fn deflate_crc_in(
    d: &[u8],
    params: zlib::Params,
    threads: usize,
    chunk: usize,
    min: usize,
) -> Option<(Vec<u8>, u32)> {
    if threads < 2 || d.len() < min.max(2 * chunk) {
        return Some((zlib::compress(d, params)?, crc32::hash(d)));
    }
    let n = d.len().div_ceil(chunk);
    let parts = pool::par_map(n, threads, |k| {
        let (a, b) = (k * chunk, ((k + 1) * chunk).min(d.len()));
        let from = a.saturating_sub(DICT);
        let mut h = crc32::Hasher::new();
        h.update(&d[a..b]);
        (zlib::compress_part(&d[from..b], a - from, params, k + 1 == n), h)
    });
    let mut out = Vec::with_capacity(parts.iter().map(|(v, _)| v.as_ref().map_or(0, Vec::len)).sum());
    let mut crc = crc32::Hasher::new();
    for (v, h) in parts {
        out.extend_from_slice(&v?);
        crc.combine(&h);
    }
    Some((out, crc.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chunk sizes against time and size: `AQR_PACK_FILE=path cargo rt -p aqr-pack par -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn chunk_sizes() {
        let d = std::fs::read(std::env::var("AQR_PACK_FILE").unwrap()).unwrap();
        let p = zlib::Params::default();
        let threads = std::thread::available_parallelism().unwrap().get();
        for (chunk, threads) in [
            (usize::MAX, 1),
            (1 << 30, threads),
            (128 << 10, threads),
            (256 << 10, threads),
            (512 << 10, threads),
            (1 << 20, threads),
            (2 << 20, threads),
        ] {
            let t = std::time::Instant::now();
            let (v, _) = deflate_crc_in(&d, p, threads, chunk.min(d.len()), 0).unwrap();
            println!("chunk {chunk} threads {threads}: {} bytes, {:?}", v.len(), t.elapsed());
        }
    }
}
