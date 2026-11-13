//! BPTC's blocks (BC7; FORMAT.md, "BC7"): their fields as they are laid out, read and written
//! back (every block of a mode is the fields of that mode, so that reading and writing are
//! inverses), and what a block decodes to. The partitions' tables are those of `bcdec.h` (Sergii
//! Kudlai, the Unlicense).

/// The subset of each pixel p (bits 2p, 2p + 1) of each partition of two subsets.
pub const P2: [u32; 64] = [
    0x50505050, 0x40404040, 0x54545454, 0x54505040, 0x50404000, 0x55545450, 0x55545040, 0x54504000, 0x50400000,
    0x55555450, 0x55544000, 0x54400000, 0x55555440, 0x55550000, 0x55555500, 0x55000000, 0x55150100, 0x00004054,
    0x15010000, 0x00405054, 0x00004050, 0x15050100, 0x05010000, 0x40505054, 0x00404050, 0x05010100, 0x14141414,
    0x05141450, 0x01155440, 0x00555500, 0x15014054, 0x05414150, 0x44444444, 0x55005500, 0x11441144, 0x05055050,
    0x05500550, 0x11114444, 0x41144114, 0x44111144, 0x15055054, 0x01055040, 0x05041050, 0x05455150, 0x14414114,
    0x50050550, 0x41411414, 0x00141400, 0x00041504, 0x00105410, 0x10541000, 0x04150400, 0x50410514, 0x41051450,
    0x05415014, 0x14054150, 0x41050514, 0x41505014, 0x40011554, 0x54150140, 0x50505500, 0x00555050, 0x15151010,
    0x54540404,
];
/// Of three subsets.
pub const P3: [u32; 64] = [
    0xaa685050, 0x6a5a5040, 0x5a5a4200, 0x5450a0a8, 0xa5a50000, 0xa0a05050, 0x5555a0a0, 0x5a5a5050, 0xaa550000,
    0xaa555500, 0xaaaa5500, 0x90909090, 0x94949494, 0xa4a4a4a4, 0xa9a59450, 0x2a0a4250, 0xa5945040, 0x0a425054,
    0xa5a5a500, 0x55a0a0a0, 0xa8a85454, 0x6a6a4040, 0xa4a45000, 0x1a1a0500, 0x0050a4a4, 0xaaa59090, 0x14696914,
    0x69691400, 0xa08585a0, 0xaa821414, 0x50a4a450, 0x6a5a0200, 0xa9a58000, 0x5090a0a8, 0xa8a09050, 0x24242424,
    0x00aa5500, 0x24924924, 0x24499224, 0x50a50a50, 0x500aa550, 0xaaaa4444, 0x66660000, 0xa5a0a5a0, 0x50a050a0,
    0x69286928, 0x44aaaa44, 0x66666600, 0xaa444444, 0x54a854a8, 0x95809580, 0x96969600, 0xa85454a8, 0x80959580,
    0xaa141414, 0x96960000, 0xaaaa1414, 0xa05050a0, 0xa0a5a5a0, 0x96000000, 0x40804080, 0xa9a8a9a8, 0xaaaaaa44,
    0x2a4a5254,
];
/// The anchor of subset 1 of each partition of two subsets (subset 0's is pixel 0).
pub const A2: [u8; 64] = [
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 2, 8, 2, 2, 8, 8, 15, 2, 8, 2, 2, 8, 8, 2, 2,
    15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6, 6, 2, 6, 8, 15, 15, 2, 2, 15, 15, 15, 15, 15, 2, 2, 15,
];
/// The anchors of subsets 1 and 2 of each partition of three subsets.
pub const A3: [[u8; 2]; 64] = [
    [3, 15],
    [3, 8],
    [15, 8],
    [15, 3],
    [8, 15],
    [3, 15],
    [15, 3],
    [15, 8],
    [8, 15],
    [8, 15],
    [6, 15],
    [6, 15],
    [6, 15],
    [5, 15],
    [3, 15],
    [3, 8],
    [3, 15],
    [3, 8],
    [8, 15],
    [15, 3],
    [3, 15],
    [3, 8],
    [6, 15],
    [10, 8],
    [5, 3],
    [8, 15],
    [8, 6],
    [6, 10],
    [8, 15],
    [5, 15],
    [15, 10],
    [15, 8],
    [8, 15],
    [15, 3],
    [3, 15],
    [5, 10],
    [6, 10],
    [10, 8],
    [8, 9],
    [15, 10],
    [15, 6],
    [3, 15],
    [15, 8],
    [5, 15],
    [15, 3],
    [15, 6],
    [15, 6],
    [15, 8],
    [3, 15],
    [15, 3],
    [5, 15],
    [5, 15],
    [5, 15],
    [8, 15],
    [5, 15],
    [10, 15],
    [5, 15],
    [10, 15],
    [8, 15],
    [13, 15],
    [15, 3],
    [12, 15],
    [3, 15],
    [3, 8],
];

/// The bits of a block, read and written from bit 0 (of byte 0) up.
pub struct Bits {
    v: u128,
    at: u32,
}

impl Bits {
    pub fn of(b: &[u8; 16]) -> Bits {
        Bits { v: u128::from_le_bytes(*b), at: 0 }
    }

    pub fn new() -> Bits {
        Bits { v: 0, at: 0 }
    }

    #[inline(always)]
    pub fn get(&mut self, n: u32) -> u32 {
        let x = (self.v >> self.at) as u32 & ((1u64 << n) - 1) as u32;
        self.at += n;
        x
    }

    #[inline(always)]
    pub fn put(&mut self, n: u32, x: u32) {
        self.v |= ((x & ((1u64 << n) - 1) as u32) as u128) << self.at;
        self.at += n;
    }

    pub fn bytes(&self) -> [u8; 16] {
        self.v.to_le_bytes()
    }
}

impl Default for Bits {
    fn default() -> Self {
        Bits::new()
    }
}

/// What a BC7 mode's block holds: its subsets, the bits of its partition, rotation and index
/// selection, of each endpoint's colour and alpha channels, whether each endpoint has a p-bit
/// or each subset shares one, and the bits of its indices and its second indices.
#[derive(Debug, Clone, Copy)]
pub struct Mode {
    pub ns: usize,
    pub pb: u32,
    pub rb: u32,
    pub isb: u32,
    pub cb: u32,
    pub ab: u32,
    pub epb: bool,
    pub spb: bool,
    pub ib: u32,
    pub ib2: u32,
}

#[allow(clippy::too_many_arguments)]
const fn mode(
    ns: usize,
    pb: u32,
    rb: u32,
    isb: u32,
    cb: u32,
    ab: u32,
    epb: bool,
    spb: bool,
    ib: u32,
    ib2: u32,
) -> Mode {
    Mode { ns, pb, rb, isb, cb, ab, epb, spb, ib, ib2 }
}

pub const MODES: [Mode; 8] = [
    mode(3, 4, 0, 0, 4, 0, true, false, 3, 0),
    mode(2, 6, 0, 0, 6, 0, false, true, 3, 0),
    mode(3, 6, 0, 0, 5, 0, false, false, 2, 0),
    mode(2, 6, 0, 0, 7, 0, true, false, 2, 0),
    mode(1, 0, 2, 1, 5, 6, false, false, 2, 3),
    mode(1, 0, 2, 0, 7, 8, false, false, 2, 2),
    mode(1, 0, 0, 0, 7, 7, true, false, 4, 0),
    mode(2, 6, 0, 0, 5, 5, true, false, 2, 0),
];

/// The mode of a block of the reserved one (its first byte 0): its bits are kept as they are.
pub const RESERVED: u8 = 8;

/// The interpolation weights of indices of 2, 3 and 4 bits (in 1/64).
pub const WEIGHTS: [&[u8]; 5] = [
    &[],
    &[],
    &[0, 21, 43, 64],
    &[0, 9, 18, 27, 37, 46, 55, 64],
    &[0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64],
];

/// A BC7 block's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Block7 {
    /// 0 to 7, or [`RESERVED`].
    pub mode: u8,
    pub partition: u8,
    pub rotation: u8,
    pub isb: u8,
    /// Endpoint `2s + e` of subset `s`: its channels R, G, B, A, as stored (of `cb` or `ab`
    /// bits), and its p-bit (that of its subset where they are shared).
    pub ep: [[u8; 4]; 6],
    pub p: [u8; 6],
    /// The index of each pixel, and its second index (modes 4 and 5).
    pub ix: [u8; 16],
    pub ix2: [u8; 16],
    /// The bytes of a block of the reserved mode.
    pub raw: [u8; 16],
}

impl Block7 {
    pub fn info(&self) -> &'static Mode {
        &MODES[self.mode as usize]
    }

    /// The subset of pixel `p`, and whether it is an anchor (its index of a bit the less).
    #[inline(always)]
    pub fn subset(&self, p: usize) -> (usize, bool) {
        subset(self.info().ns, self.partition as usize, p)
    }

    /// The fields of block `b`.
    pub fn read(b: &[u8; 16]) -> Block7 {
        let mut x = Block7 { mode: b[0].trailing_zeros().min(8) as u8, ..Default::default() };
        if x.mode == RESERVED {
            x.raw = *b;
            return x;
        }
        let m = *x.info();
        let mut r = Bits::of(b);
        r.get(x.mode as u32 + 1);
        x.partition = r.get(m.pb) as u8;
        x.rotation = r.get(m.rb) as u8;
        x.isb = r.get(m.isb) as u8;
        let n = 2 * m.ns;
        for c in 0..4 {
            let bits = if c < 3 { m.cb } else { m.ab };
            for j in 0..n {
                x.ep[j][c] = r.get(bits) as u8;
            }
        }
        if m.epb {
            for j in 0..n {
                x.p[j] = r.get(1) as u8;
            }
        } else if m.spb {
            for s in 0..m.ns {
                let v = r.get(1) as u8;
                (x.p[2 * s], x.p[2 * s + 1]) = (v, v);
            }
        }
        for p in 0..16 {
            let anchor = x.subset(p).1;
            x.ix[p] = r.get(m.ib - anchor as u32) as u8;
        }
        if m.ib2 > 0 {
            for p in 0..16 {
                x.ix2[p] = r.get(m.ib2 - (p == 0) as u32) as u8;
            }
        }
        debug_assert_eq!(r.at, 128);
        x
    }

    /// The block of these fields (as [`Block7::read`] gave them).
    pub fn write(&self) -> [u8; 16] {
        if self.mode == RESERVED {
            return self.raw;
        }
        let m = *self.info();
        let mut w = Bits::new();
        w.put(self.mode as u32 + 1, 1 << self.mode);
        w.put(m.pb, self.partition as u32);
        w.put(m.rb, self.rotation as u32);
        w.put(m.isb, self.isb as u32);
        let n = 2 * m.ns;
        for c in 0..4 {
            let bits = if c < 3 { m.cb } else { m.ab };
            for j in 0..n {
                w.put(bits, self.ep[j][c] as u32);
            }
        }
        if m.epb {
            for j in 0..n {
                w.put(1, self.p[j] as u32);
            }
        } else if m.spb {
            for s in 0..m.ns {
                w.put(1, self.p[2 * s] as u32);
            }
        }
        for p in 0..16 {
            let anchor = self.subset(p).1;
            w.put(m.ib - anchor as u32, self.ix[p] as u32);
        }
        if m.ib2 > 0 {
            for p in 0..16 {
                w.put(m.ib2 - (p == 0) as u32, self.ix2[p] as u32);
            }
        }
        w.bytes()
    }

    /// Endpoint `j`'s channels expanded to 8 bits (alpha 255 where the mode has none), with
    /// the p-bits; channel A not rotated.
    #[inline]
    pub fn endpoint(&self, j: usize) -> [u8; 4] {
        let m = self.info();
        let pb = (m.epb || m.spb) as u32;
        std::array::from_fn(|c| {
            let bits = if c < 3 { m.cb } else { m.ab };
            if bits == 0 {
                return 255;
            }
            let v = (self.ep[j][c] as u32) << pb | if pb == 1 { self.p[j] as u32 } else { 0 };
            expand(v, bits + pb)
        })
    }

    /// The pixels of the block, RGBA (black and transparent of the reserved mode).
    pub fn pixels(&self) -> [[u8; 4]; 16] {
        if self.mode == RESERVED {
            return [[0; 4]; 16];
        }
        let m = self.info();
        let e: [[u8; 4]; 6] = std::array::from_fn(|j| if j < 2 * m.ns { self.endpoint(j) } else { [0; 4] });
        let (w1, w2) = (WEIGHTS[m.ib as usize], WEIGHTS[m.ib2.max(2) as usize]);
        std::array::from_fn(|p| {
            let s = self.subset(p).0;
            let (a, b) = (e[2 * s], e[2 * s + 1]);
            let mut px: [u8; 4] = if m.ib2 == 0 {
                std::array::from_fn(|c| lerp(a[c], b[c], w1[self.ix[p] as usize]))
            } else {
                // (the colour's and the alpha's weights, swapped by the index selection)
                let (wc, wa) = match self.isb {
                    0 => (w1[self.ix[p] as usize], w2[self.ix2[p] as usize]),
                    _ => (w2[self.ix2[p] as usize], w1[self.ix[p] as usize]),
                };
                let mut v: [u8; 4] = std::array::from_fn(|c| lerp(a[c], b[c], wc));
                v[3] = lerp(a[3], b[3], wa);
                v
            };
            rotate(&mut px, self.rotation);
            px
        })
    }
}

/// The subset of pixel `p` of partition `partition` of `ns` subsets, and whether it is an
/// anchor.
#[inline(always)]
pub fn subset(ns: usize, partition: usize, p: usize) -> (usize, bool) {
    match ns {
        1 => (0, p == 0),
        2 => {
            let s = (P2[partition] >> (2 * p) & 3) as usize;
            (s, p == 0 || p == A2[partition] as usize)
        }
        _ => {
            let s = (P3[partition] >> (2 * p) & 3) as usize;
            let a = A3[partition];
            (s, p == 0 || p == a[0] as usize || p == a[1] as usize)
        }
    }
}

/// A channel of `bits` (4 to 8) expanded to 8, its top bits repeated below.
#[inline(always)]
pub fn expand(v: u32, bits: u32) -> u8 {
    let v = v << (8 - bits);
    (v | v >> bits) as u8
}

#[inline(always)]
pub fn lerp(a: u8, b: u8, w: u8) -> u8 {
    ((a as u32 * (64 - w as u32) + b as u32 * w as u32 + 32) >> 6) as u8
}

/// Rotation `r`'s channel swapped with alpha (none, R, G, B), either way.
#[inline(always)]
pub fn rotate(px: &mut [u8; 4], r: u8) {
    if r > 0 {
        px.swap(r as usize - 1, 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc7_fields() {
        // every mode's blocks read and written back, of random bits
        let mut x = 0x1234_5678_9abc_def0_u64;
        for i in 0..20000 {
            let mut b = [0u8; 16];
            for v in b.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *v = x as u8;
            }
            let m = i % 9;
            b[0] = if m == 8 { 0 } else { (b[0] | 1) << m };
            let f = Block7::read(&b);
            assert_eq!(f.mode as usize, m);
            assert_eq!(f.write(), b);
        }
        // the anchors are of the subsets they are said to be
        for k in 0..64 {
            assert_eq!(P2[k] & 3, 0);
            assert_eq!(P2[k] >> (2 * A2[k]) & 3, 1);
            assert_eq!(P3[k] >> (2 * A3[k][0]) & 3, 1);
            assert_eq!(P3[k] >> (2 * A3[k][1]) & 3, 2);
        }
    }
}
