//! Layer III Huffman coding of one granule's part 3 (big values and count1 regions).

use crate::bits::{BitReader, FastWriter};
use crate::frame::{Granule, Header, sfb_widths};
use crate::huff_tables::{COUNT1A, COUNT1B, LINBITS, PAIR};
use std::sync::OnceLock;

/// First-level lookup width of the pair decoding tables.
const L1: u32 = 9;
const NODE: u32 = 1 << 31;

/// Two-level decoding table of a pair code. Leaf: `len << 8 | x << 4 | y`. Node:
/// `NODE | offset << 8 | k`, with the `k` following bits indexing from `offset`.
pub struct PairDecoder {
    lut: Vec<u32>,
}

impl PairDecoder {
    fn new(codes: &[u32; 256]) -> PairDecoder {
        let mut lut = vec![0u32; 1 << L1];
        // Longest code below each first-level prefix.
        let mut sub_bits = vec![0u32; 1 << L1];
        for (sym, &e) in codes.iter().enumerate() {
            let (code, len) = (e >> 5, e & 31);
            if len > L1 {
                let p = (code >> (len - L1)) as usize;
                sub_bits[p] = sub_bits[p].max(len - L1);
            } else if len > 0 {
                let base = (code << (L1 - len)) as usize;
                for i in 0..1usize << (L1 - len) {
                    lut[base + i] = len << 8 | sym as u32;
                }
            }
        }
        for p in 0..1usize << L1 {
            if sub_bits[p] > 0 {
                let off = lut.len() as u32;
                lut[p] = NODE | off << 8 | sub_bits[p];
                lut.resize(lut.len() + (1 << sub_bits[p]), 0);
            }
        }
        for (sym, &e) in codes.iter().enumerate() {
            let (code, len) = (e >> 5, e & 31);
            if len > L1 {
                let p = (code >> (len - L1)) as usize;
                let (off, k) = ((lut[p] >> 8 & 0x7fffff) as usize, lut[p] & 31);
                let rest = code & ((1 << (len - L1)) - 1);
                let base = off + (rest << (k - (len - L1))) as usize;
                for i in 0..1usize << (k - (len - L1)) {
                    lut[base + i] = len << 8 | sym as u32;
                }
            }
        }
        PairDecoder { lut }
    }

    /// Returns `(len, x << 4 | y)` for the code at the front of `w` (left aligned).
    #[inline(always)]
    pub fn decode(&self, w: u64) -> (u32, u32) {
        let e = self.lut[(w >> (64 - L1)) as usize];
        let e = if e & NODE == 0 {
            e
        } else {
            let k = e & 31;
            let off = (e >> 8 & 0x7fffff) as usize;
            self.lut[off + ((w << L1) >> (64 - k)) as usize]
        };
        (e >> 8, e & 0xff)
    }
}

/// Decoders by table_select (None for 0, 4 and 14), and count1 table A by 6-bit prefix.
pub struct Decoders {
    pub pair: Vec<Option<std::sync::Arc<PairDecoder>>>,
    pub count1a: [u16; 64],
    /// Per table_select, `1 << FL` entries resolving code and signs at once (see
    /// [`fast_lut`]); empty for missing tables.
    pub fast: Vec<std::sync::Arc<Vec<u16>>>,
}

/// Index width of the fast pair tables.
pub const FL: u32 = 11;

/// Entry: `valid << 15 | len << 10 | x << 6 | y << 2 | sx << 1 | sy`. For tables without
/// linbits, `len` counts the code and the sign bits, which must fit in `FL` bits. For tables
/// with linbits, `len` is the code length alone (the escapes and signs follow it) and the
/// sign fields are zero.
fn fast_lut(codes: &[u32; 256], lin: bool) -> Vec<u16> {
    if lin {
        let mut lut = vec![0u16; 1 << FL];
        for (sym, &e) in codes.iter().enumerate() {
            let (code, len) = (e >> 5, e & 31);
            if len == 0 || len > FL {
                continue;
            }
            let (x, y) = (sym as u32 >> 4, sym as u32 & 15);
            let ent = (1 << 15 | len << 10 | x << 6 | y << 2) as u16;
            let prefix = code << (FL - len);
            for k in 0..1u32 << (FL - len) {
                lut[(prefix | k) as usize] = ent;
            }
        }
        return lut;
    }
    let mut lut = vec![0u16; 1 << FL];
    for (sym, &e) in codes.iter().enumerate() {
        let (code, len) = (e >> 5, e & 31);
        if len == 0 {
            continue;
        }
        let (x, y) = (sym as u32 >> 4, sym as u32 & 15);
        if lin && (x == 15 || y == 15) {
            continue;
        }
        let ns = (x != 0) as u32 + (y != 0) as u32;
        let total = len + ns;
        if total > FL {
            continue;
        }
        for signs in 0..1u32 << ns {
            let (sx, sy) = match (x != 0, y != 0) {
                (true, true) => (signs >> 1, signs & 1),
                (true, false) => (signs, 0),
                (false, true) => (0, signs),
                _ => (0, 0),
            };
            let prefix = (code << ns | signs) << (FL - total);
            let ent = (1 << 15 | total << 10 | x << 6 | y << 2 | sx << 1 | sy) as u16;
            for k in 0..1u32 << (FL - total) {
                lut[(prefix | k) as usize] = ent;
            }
        }
    }
    lut
}

pub fn decoders() -> &'static Decoders {
    static D: OnceLock<Decoders> = OnceLock::new();
    D.get_or_init(|| {
        let mut pair: Vec<Option<std::sync::Arc<PairDecoder>>> = Vec::with_capacity(32);
        for t in 0..32 {
            let d = PAIR[t].map(|codes| {
                // Tables 16..23 and 24..31 share codes.
                if t > 16 && t < 24 || t > 24 {
                    pair[if t < 24 { 16 } else { 24 }].clone().unwrap()
                } else {
                    std::sync::Arc::new(PairDecoder::new(codes))
                }
            });
            pair.push(d);
        }
        let mut count1a = [0u16; 64];
        for (sym, &e) in COUNT1A.iter().enumerate() {
            let (code, len) = (e >> 5, e & 31);
            let base = (code << (6 - len)) as usize;
            for i in 0..1usize << (6 - len) {
                count1a[base + i] = (len << 8 | sym as u32) as u16;
            }
        }
        let mut fast: Vec<std::sync::Arc<Vec<u16>>> = Vec::with_capacity(32);
        for t in 0..32 {
            let f = match PAIR[t] {
                None => std::sync::Arc::new(Vec::new()),
                Some(_) if t > 16 && t < 24 || t > 24 => fast[if t < 24 { 16 } else { 24 }].clone(),
                Some(codes) => std::sync::Arc::new(fast_lut(codes, LINBITS[t] > 0)),
            };
            fast.push(f);
        }
        Decoders { pair, count1a, fast }
    })
}

/// Pair counts of the (up to) three big-values regions.
pub fn regions(h: Header, g: &Granule) -> [usize; 3] {
    let widths = sfb_widths(h, g);
    let counts: [u32; 3] = if g.window_switching {
        [if g.short() && !g.mixed { 8 } else { 7 }, 255, 255]
    } else {
        [g.region0_count as u32, g.region1_count as u32, 255]
    };
    let bv = g.big_values as usize * 2;
    let mut out = [0usize; 3];
    let (mut line, mut sfb) = (0usize, 0usize);
    for r in 0..3 {
        let start = line;
        let mut n = counts[r] + 1;
        while n > 0 && sfb < widths.len() && widths[sfb] != 0 && line < bv {
            line += widths[sfb] as usize;
            sfb += 1;
            n -= 1;
        }
        line = line.min(bv);
        out[r] = (line - start) / 2;
    }
    out
}

/// Outcome of decoding part 3 of a granule.
pub struct Part3 {
    pub quads: usize,
    /// Bits after the last complete count1 quadruple.
    pub tail: usize,
}

/// Decodes part 3 into quantized values (with signs) from `r` up to bit `end`. Lines past
/// the count1 region are zeroed. The tail bits are left in `r`. Returns None if a table
/// does not exist or the big values run past `end`.
pub fn decode_coefs(h: Header, g: &Granule, r: &mut BitReader, end: usize, coefs: &mut [i32; 576]) -> Option<Part3> {
    let d = decoders();
    if g.big_values > 288 {
        return None;
    }
    let reg = regions(h, g);
    let mut line = 0usize;
    for t in 0..3 {
        let n = reg[t];
        let ts = g.table_select[t] as usize;
        if ts == 0 {
            coefs[line..line + 2 * n].fill(0);
            line += 2 * n;
            continue;
        }
        let dec = d.pair[ts].as_deref()?;
        let fast = &d.fast[ts][..];
        let lin = LINBITS[ts] as u32;
        let mut bb = BitBuf::new(r);
        if lin > 0 {
            for _ in 0..n {
                bb.refill();
                // Code (<= 17 bits), two escapes (<= 2 * 13) and two signs: <= 45 bits.
                let w = bb.acc;
                let e = fast[(w >> (64 - FL)) as usize] as u32;
                let (len, sym) = if e & 0x8000 != 0 { (e >> 10 & 31, e >> 2 & 0xff) } else { dec.decode(w) };
                let mut rest = w << len;
                let mut used = len;
                for v in [sym >> 4, sym & 15] {
                    let l = if v == 15 { lin } else { 0 };
                    let ext = ((rest >> 1) >> (63 - l)) as u32;
                    rest <<= l;
                    let m = (v + ext) as i32;
                    let s = (v != 0) as u32;
                    let neg = s != 0 && rest >> 63 != 0;
                    rest <<= s;
                    used += l + s;
                    coefs[line] = if neg { -m } else { m };
                    line += 1;
                }
                bb.consume(used);
            }
        } else {
            for _ in 0..n {
                bb.refill();
                let w = bb.acc;
                let e = fast[(w >> (64 - FL)) as usize] as u32;
                if e & 0x8000 != 0 {
                    let (x, y) = ((e >> 6 & 15) as i32, (e >> 2 & 15) as i32);
                    coefs[line] = if e & 2 != 0 { -x } else { x };
                    coefs[line + 1] = if e & 1 != 0 { -y } else { y };
                    bb.consume(e >> 10 & 31);
                } else {
                    // Long code: <= 19 bits and two signs.
                    let (len, sym) = dec.decode(w);
                    let (x, y) = ((sym >> 4) as i32, (sym & 15) as i32);
                    let mut rest = w << len;
                    let sx = (x != 0) as u32;
                    let neg_x = sx != 0 && rest >> 63 != 0;
                    rest <<= sx;
                    let sy = (y != 0) as u32;
                    let neg_y = sy != 0 && rest >> 63 != 0;
                    coefs[line] = if neg_x { -x } else { x };
                    coefs[line + 1] = if neg_y { -y } else { y };
                    bb.consume(len + sx + sy);
                }
                line += 2;
            }
        }
        r.pos = bb.pos();
        if r.pos > end {
            return None;
        }
    }
    let mut nq = 0;
    let mut bb = BitBuf::new(r);
    while line + 4 <= 576 {
        bb.refill();
        let (len, sym) = if g.count1table {
            (4, 15 - (bb.acc >> 60) as u32)
        } else {
            let e = d.count1a[(bb.acc >> 58) as usize];
            ((e >> 8) as u32, (e & 0xff) as u32)
        };
        let ns = sym.count_ones();
        if bb.pos() + (len + ns) as usize > end {
            break;
        }
        let mut rest = bb.acc << len;
        for j in 0..4 {
            let nz = (sym >> (3 - j)) & 1;
            let neg = nz != 0 && rest >> 63 != 0;
            rest <<= nz;
            coefs[line + j] = if neg { -(nz as i32) } else { nz as i32 };
        }
        bb.consume(len + ns);
        line += 4;
        nq += 1;
    }
    r.pos = bb.pos();
    coefs[line..].fill(0);
    Some(Part3 { quads: nq, tail: end - r.pos })
}

/// Re-encodes part 3 (without the tail) from quantized values: big values per the side
/// information, then `quads` count1 quadruples.
pub fn encode_coefs(h: Header, g: &Granule, coefs: &[i32; 576], quads: usize, w: &mut FastWriter) {
    // At most 47 bits per pair, 10 per quadruple.
    w.reserve(288 * 6 + 144 * 2 + 16);
    let reg = regions(h, g);
    let mut line = 0;
    for t in 0..3 {
        let n = reg[t];
        let ts = g.table_select[t] as usize;
        if ts == 0 {
            line += 2 * n;
            continue;
        }
        let codes = PAIR[ts].unwrap();
        let lin = LINBITS[ts] as u32;
        let pairs = &coefs[line..line + 2 * n];
        if lin == 0 {
            for p in pairs.chunks_exact(2) {
                let (a, b) = (p[0], p[1]);
                let (x, y) = (a.unsigned_abs(), b.unsigned_abs());
                let e = codes[(x * 16 + y) as usize & 255];
                let (sa, sb) = ((x != 0) as u32, (y != 0) as u32);
                let mut bits = (e >> 5) as u64;
                bits = bits << sa | (a < 0) as u64;
                bits = bits << sb | (b < 0) as u64;
                w.put(bits, (e & 31) + sa + sb);
            }
        } else {
            for p in pairs.chunks_exact(2) {
                let (a, b) = (p[0], p[1]);
                let (ma, mb) = (a.unsigned_abs(), b.unsigned_abs());
                let (x, y) = (ma.min(15), mb.min(15));
                let e = codes[(x * 16 + y) as usize];
                let mut bits = (e >> 5) as u64;
                let mut nb = e & 31;
                for (m, v) in [(ma, a), (mb, b)] {
                    let l = if m >= 15 { lin } else { 0 };
                    bits = bits << l | (m.wrapping_sub(15) & ((1u32 << l) - 1)) as u64;
                    let s = (m != 0) as u32;
                    bits = bits << s | (v < 0) as u64;
                    nb += l + s;
                }
                w.put(bits, nb);
            }
        }
        line += 2 * n;
    }
    let tab = if g.count1table { &COUNT1B } else { &COUNT1A };
    for q in coefs[line..line + 4 * quads].chunks_exact(4) {
        let sym = (q[0] != 0) as usize * 8 + (q[1] != 0) as usize * 4 + (q[2] != 0) as usize * 2 + (q[3] != 0) as usize;
        let e = tab[sym];
        let mut bits = (e >> 5) as u64;
        let mut nb = e & 31;
        for &v in q {
            let s = (v != 0) as u32;
            bits = bits << s | (v < 0) as u64;
            nb += s;
        }
        w.put(bits, nb);
    }
}

/// MSB-first bit buffer over a padded byte slice, kept in registers: at least 56 bits are
/// valid after [`BitBuf::refill`].
#[derive(Clone, Copy)]
pub(crate) struct BitBuf<'a> {
    data: &'a [u8],
    /// Next byte to load.
    next: usize,
    pub(crate) acc: u64,
    bits: u32,
}

impl<'a> BitBuf<'a> {
    #[inline(always)]
    pub(crate) fn new(r: &BitReader<'a>) -> Self {
        let mut b = BitBuf { data: r.data(), next: r.pos >> 3, acc: 0, bits: 0 };
        b.refill();
        let skip = (r.pos & 7) as u32;
        b.acc <<= skip;
        b.bits -= skip;
        b
    }

    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        // Branchless: load eight bytes, keep what fits. Past the end, the last eight bytes
        // are read again (garbage, but no panic).
        let p = self.next.min(self.data.len() - 8);
        // SAFETY: p + 8 <= data.len().
        let w = u64::from_be(unsafe { (self.data.as_ptr().add(p) as *const u64).read_unaligned() });
        self.acc |= w >> self.bits;
        let n = (63 - self.bits) >> 3;
        self.next += n as usize;
        self.bits += n * 8;
    }

    #[inline(always)]
    pub(crate) fn consume(&mut self, n: u32) {
        self.acc <<= n;
        self.bits -= n;
    }

    /// Bit position of the next unread bit.
    #[inline(always)]
    pub(crate) fn pos(&self) -> usize {
        self.next * 8 - self.bits as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::padded;

    #[test]
    fn pair_tables_decode() {
        let d = decoders();
        for t in 1..32 {
            let Some(codes) = PAIR[t] else { continue };
            let dec = d.pair[t].as_deref().unwrap();
            for (sym, &e) in codes.iter().enumerate() {
                if e == 0 {
                    continue;
                }
                let (code, len) = (e >> 5, e & 31);
                // Code followed by ones, so that only the code determines the result.
                let w = ((code as u64) << (64 - len)) | (u64::MAX >> len);
                assert_eq!(dec.decode(w), (len, sym as u32), "table {t}");
            }
        }
    }

    #[test]
    fn count1_decode() {
        let d = decoders();
        for (sym, &e) in COUNT1A.iter().enumerate() {
            let (code, len) = (e >> 5, e & 31);
            let p = ((code << (6 - len)) | ((1 << (6 - len)) - 1)) as usize;
            assert_eq!(d.count1a[p] as u32, len << 8 | sym as u32);
        }
    }

    /// Random values through encode_coefs and decode_coefs, for every table.
    #[test]
    fn coefs_roundtrip() {
        let h = crate::frame::Header(0xfffb9000);
        let mut seed = 5u64;
        let mut rnd = |m: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        for ts in 0..32usize {
            if ts == 4 || ts == 14 {
                continue;
            }
            for _ in 0..20 {
                let g = Granule {
                    big_values: rnd(289) as u16,
                    table_select: [ts as u8, ts as u8, ts as u8],
                    region0_count: rnd(16) as u8,
                    region1_count: rnd(8) as u8,
                    count1table: rnd(2) == 1,
                    ..Default::default()
                };
                let maxv = if ts == 0 {
                    0
                } else if LINBITS[ts] > 0 {
                    15 + (1 << LINBITS[ts]) - 1
                } else {
                    crate::huff_tables::DIM[ts] as u64 - 1
                };
                let mut c = [0i32; 576];
                let bl = g.big_values as usize * 2;
                for v in c[..bl].iter_mut() {
                    let m = if rnd(3) == 0 { rnd(maxv + 1) } else { rnd(maxv.min(3) + 1) } as i32;
                    *v = if rnd(2) == 1 { -m } else { m };
                }
                let quads = ((576 - bl) / 4).min(rnd(40) as usize);
                for v in c[bl..bl + 4 * quads].iter_mut() {
                    *v = rnd(3) as i32 - 1;
                }
                let mut w = FastWriter::with_capacity(4096);
                encode_coefs(h, &g, &c, quads, &mut w);
                let bits = w.len();
                // Padding with 1s makes quadruple-looking garbage after the data.
                let mut buf = w.finish();
                buf.extend_from_slice(&[0xff; 600]);
                let buf = padded(&buf);
                let mut r = BitReader::new(&buf, 0);
                let mut out = [0i32; 576];
                let p3 = decode_coefs(h, &g, &mut r, bits, &mut out).unwrap();
                assert_eq!(p3.quads, quads, "table {ts}");
                assert_eq!(p3.tail, 0);
                assert_eq!(&out[..], &c[..], "table {ts}");
            }
        }
    }
}
