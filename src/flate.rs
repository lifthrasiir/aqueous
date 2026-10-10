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

/// Inflated bytes per DEFLATE byte, estimated from the first block's header: the expected
/// bytes and bits of a symbol when a code of length `l` has probability 2^-l. `None` if
/// the first block is not a dynamic one that parses.
pub fn expansion(data: &[u8]) -> Option<f64> {
    use flate_recons::huffman::{DIST_EXTRA, LEN_BASE, LEN_EXTRA};
    let d = &data[if zlib_header(data) { 2 } else { 0 }..];
    // The header is at most 3 + 14 + 19 * 3 + 320 * 14 bits.
    let mut pos = 0usize;
    let peek = |pos: usize| -> u64 {
        let i = pos >> 3;
        let mut w = [0u8; 8];
        let n = d.len().saturating_sub(i).min(8);
        w[..n].copy_from_slice(&d[i.min(d.len())..i.min(d.len()) + n]);
        u64::from_le_bytes(w) >> (pos & 7)
    };
    let bits = |pos: &mut usize, n: usize| -> Option<u32> {
        if *pos + n > d.len() * 8 {
            return None;
        }
        let v = (peek(*pos) & ((1 << n) - 1)) as u32;
        *pos += n;
        Some(v)
    };
    let _final = bits(&mut pos, 1)?;
    if bits(&mut pos, 2)? != 2 {
        return None;
    }
    let hlit = bits(&mut pos, 5)? as usize + 257;
    let hdist = bits(&mut pos, 5)? as usize + 1;
    let hclen = bits(&mut pos, 4)? as usize + 4;
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut cl = [0u8; 19];
    for &o in &ORDER[..hclen] {
        cl[o] = bits(&mut pos, 3)? as u8;
    }
    // The code-length code as a table of 7 bits (LSB first): (symbol, length).
    let mut table = [(0u8, 0u8); 128];
    let mut count = [0u32; 8];
    for &l in &cl {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 8];
    let mut code = 0;
    for l in 1..8 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    for (s, &l) in cl.iter().enumerate() {
        if l > 0 {
            let c = next[l as usize];
            next[l as usize] += 1;
            if c >= 1 << l {
                return None;
            }
            let r = (c.reverse_bits() >> (32 - l)) as usize;
            for k in 0..1 << (7 - l) {
                table[r | k << l] = (s as u8, l);
            }
        }
    }
    let mut lens = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lens.len() {
        if pos >= d.len() * 8 {
            return None;
        }
        let (sym, l) = table[(peek(pos) & 127) as usize];
        if l == 0 {
            return None;
        }
        pos += l as usize;
        match sym {
            0..=15 => {
                lens[i] = sym;
                i += 1;
            }
            16 => {
                let prev = *lens.get(i.checked_sub(1)?)?;
                for _ in 0..3 + bits(&mut pos, 2)? {
                    *lens.get_mut(i)? = prev;
                    i += 1;
                }
            }
            _ => {
                let n = if sym == 17 { 3 + bits(&mut pos, 3)? } else { 11 + bits(&mut pos, 7)? };
                i += n as usize;
                if i > lens.len() {
                    return None;
                }
            }
        }
    }
    let (ll, dl) = lens.split_at(hlit);
    let p = |l: u8| if l > 0 { (-(l as f64)).exp2() } else { 0.0 };
    let dist_bits: f64 = {
        let tot: f64 = dl.iter().map(|&l| p(l)).sum();
        if tot > 0.0 { dl.iter().enumerate().map(|(c, &l)| p(l) * (l as f64 + DIST_EXTRA.get(c).copied().unwrap_or(0) as f64)).sum::<f64>() / tot } else { 0.0 }
    };
    let (mut bytes, mut nbits, mut tot) = (0f64, 0f64, 0f64);
    for (s, &l) in ll.iter().enumerate() {
        let q = p(l);
        if q == 0.0 || s == 256 {
            continue;
        }
        tot += q;
        if s < 256 {
            bytes += q;
            nbits += q * l as f64;
        } else {
            let c = s - 257;
            let (base, extra) = (*LEN_BASE.get(c)? as f64, *LEN_EXTRA.get(c)? as f64);
            bytes += q * (base + (extra.exp2() - 1.0) / 2.0);
            nbits += q * (l as f64 + extra + dist_bits);
        }
    }
    (tot > 0.0 && nbits > 0.0).then(|| 8.0 * bytes / nbits)
}
