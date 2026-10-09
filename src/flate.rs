//! FlateDecode streams: the zlib wrapper around the DEFLATE data.

use flate_recons::fast::inflate::{self, Sink};

/// Whether `b` starts with a zlib header (deflate, no preset dictionary).
pub fn zlib_header(b: &[u8]) -> bool {
    b.len() >= 2
        && b[0] & 0x0f == 8
        && b[0] >> 4 <= 7
        && b[1] & 0x20 == 0
        && (b[0] as u16 * 256 + b[1] as u16).is_multiple_of(31)
}

pub fn adler32(d: &[u8]) -> u32 {
    let mut a = simd_adler32::Adler32::new();
    a.write(d);
    a.finish()
}

/// The shape of a FlateDecode stream's data, from a parse that keeps nothing.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    /// Bytes of zlib header (0 for a bare DEFLATE stream, or 2).
    pub hdr: usize,
    /// Bytes of DEFLATE data.
    pub deflate: usize,
    /// Bytes of inflated data.
    pub plain: usize,
    pub tokens: usize,
    /// The 4 bytes after the DEFLATE data could be its Adler-32 (checked when coding).
    pub room_adler: bool,
}

#[derive(Default)]
struct Count {
    plain: usize,
    tokens: usize,
}

impl Sink for Count {
    #[inline(always)]
    fn lit(&mut self, _: u8) {
        self.plain += 1;
        self.tokens += 1;
    }
    #[inline(always)]
    fn mat(&mut self, len: u16, _: u16, _: bool) {
        self.plain += len as usize;
        self.tokens += 1;
    }
    fn stored(&mut self, b: &[u8]) {
        self.plain += b.len();
    }
}

/// Measures a stream without inflating it.
pub fn shape(data: &[u8]) -> Option<Shape> {
    let hdr = if zlib_header(data) { 2 } else { 0 };
    let mut c = Count::default();
    let l = inflate::parse_into(&data[hdr..], usize::MAX, &mut c).ok()?;
    let deflate = data.len() - hdr - l.stream.trailing.len();
    Some(Shape {
        hdr,
        deflate,
        plain: c.plain,
        tokens: c.tokens,
        room_adler: l.stream.trailing.len() >= 4,
    })
}
