//! The tiles' zlib streams made again: zlib 1.3 at level 1, windowBits 15, memLevel 9 (what
//! every stream of the files seen is), by aqr-flate's emulation of zlib's compressor.

use aqr_flate::zlib::{Params, Strategy, compress_onto};

pub const PARAMS: Params = Params { level: 1, strategy: Strategy::Default, wbits: 15, mem: 9 };

/// `data` as zlib 1.3 compresses it with [`PARAMS`].
pub fn deflate(data: &[u8]) -> Option<Vec<u8>> {
    deflate_onto(data, Vec::with_capacity(data.len() / 4 + 64 + 6))
}

/// [`deflate`] appended to `out`, which is given back: with room for the stream and
/// [`ROOM`] bytes more, it is not grown.
pub fn deflate_onto(data: &[u8], mut out: Vec<u8>) -> Option<Vec<u8>> {
    out.extend([0x78, 0x01]);
    let mut out = compress_onto(data, PARAMS, out)?;
    out.extend(aqr_common::adler32::hash(data).to_be_bytes());
    Some(out)
}

/// Bytes that making a stream asks past its end in the vector it is appended to.
pub const ROOM: usize = aqr_flate::write::TOKEN_SLACK;

/// The `len` bytes of the zlib stream `z` (of no preset dictionary, ending where `z` does), by
/// aqr-flate's inflater; `None` where it is not that.
#[cfg(feature = "encode")]
pub fn inflate(z: &[u8], len: usize) -> Option<Vec<u8>> {
    let (&[cmf, flg], rest) = z.split_first_chunk::<2>()?;
    let (raw, &sum) = rest.split_last_chunk::<4>()?;
    if cmf & 15 != 8 || cmf >> 4 > 7 || flg & 0x20 != 0 || !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
        return None;
    }
    let data = aqr_flate::inflate::data(raw, len).ok()?;
    (data.len() == len && aqr_common::adler32::hash(&data) == u32::from_be_bytes(sum)).then_some(data)
}
