//! Sizes under zstd -3, the coder the forms are judged by: a compressor per thread, reused.

use std::cell::RefCell;

pub const LEVEL: i32 = 3;

thread_local! {
    static Z: RefCell<(zstd::bulk::Compressor<'static>, Vec<u8>, Vec<u8>)> =
        RefCell::new((zstd::bulk::Compressor::new(LEVEL).unwrap(), Vec::new(), Vec::new()));
}

/// Compressed size of the parts one after another.
pub fn len(parts: &[&[u8]]) -> usize {
    Z.with(|z| {
        let (c, src, out) = &mut *z.borrow_mut();
        let src: &[u8] = if parts.len() == 1 {
            parts[0]
        } else {
            src.clear();
            for p in parts {
                src.extend_from_slice(p);
            }
            src
        };
        out.clear();
        out.reserve(zstd::zstd_safe::compress_bound(src.len()));
        c.compress_to_buffer(src, out).unwrap()
    })
}

/// Code length in bits of byte planes (one after another), less a frame's own bytes.
pub fn plane_bits(planes: &[u8]) -> f64 {
    thread_local!(static EMPTY: usize = len(&[&[]]));
    8.0 * len(&[planes]).saturating_sub(EMPTY.with(|e| *e)) as f64
}
