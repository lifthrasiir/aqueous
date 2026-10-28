//! The tiles' zlib streams made again: zlib 1.3 at level 1, windowBits 15, memLevel 9 (what
//! every stream of the files seen is), by aqr-flate's emulation of zlib's compressor.

use aqr_flate::deflate::Body;
use aqr_flate::write::Writer;
use aqr_flate::zlib::{Emulator, Params, Strategy, zlib_block};

pub const PARAMS: Params = Params { level: 1, strategy: Strategy::Default, wbits: 15, mem: 9 };

/// `data` as zlib 1.3 compresses it with [`PARAMS`]; `None` if the emulation cannot go on.
pub fn deflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut e = Emulator::new(data, PARAMS);
    let mut w = Writer::append_to(Vec::with_capacity(data.len() / 8 + 64));
    let mut out = vec![0x78, 0x01];
    loop {
        let b = zlib_block(&mut e, w.pos())?;
        let tokens = match &b.body {
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => &tokens[..],
            Body::Stored { .. } => &[],
        };
        w.block(&b, tokens, data).ok()?;
        if b.last {
            break;
        }
    }
    out.extend(w.finish(0, &adler32(data).to_be_bytes()).ok()?);
    Some(out)
}

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 bytes keep b below 2^32 before the reduction (zlib's NMAX)
    for c in d.chunks(5552) {
        for &x in c {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}
