//! The tiles' zlib streams made again: zlib 1.3 at level 1, windowBits 15, memLevel 9 (what
//! every stream of the files seen is), by aqr-flate's emulation of zlib's compressor.

use aqr_flate::zlib::{Params, Strategy, compress};

pub const PARAMS: Params = Params { level: 1, strategy: Strategy::Default, wbits: 15, mem: 9 };

/// `data` as zlib 1.3 compresses it with [`PARAMS`].
pub fn deflate(data: &[u8]) -> Option<Vec<u8>> {
    let raw = compress(data, PARAMS)?;
    let mut out = Vec::with_capacity(raw.len() + 6);
    out.extend([0x78, 0x01]);
    out.extend(raw);
    out.extend(aqr_common::adler32::hash(data).to_be_bytes());
    Some(out)
}
