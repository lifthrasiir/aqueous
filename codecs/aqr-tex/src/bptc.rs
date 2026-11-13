//! BPTC's blocks (BC7 and BC6H; FORMAT.md, "`bc7-recons`", "`bc6h-recons`"): their fields as
//! they are laid out, read and written back (every block of a mode is the fields of that mode,
//! so that reading and writing are inverses), and what a block decodes to. The partitions'
//! tables, and the layouts of BC6H's modes, are those of `bcdec.h` (Sergii Kudlai, the
//! Unlicense).

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

/// A segment of a BC6H mode's fields: the field (endpoint `e` · 3 + channel `c`, or 12 the
/// partition), the lowest of its bits there, how many, and whether they are stored in reverse.
type Seg = (u8, u8, u8, bool);

const fn s(f: u8, lo: u8, n: u8) -> Seg {
    (f, lo, n, false)
}

const fn rev(f: u8, lo: u8, n: u8) -> Seg {
    (f, lo, n, true)
}

// (fields: rw gw bw = 0 1 2, rx gx bx = 3 4 5, ry gy by = 6 7 8, rz gz bz = 9 10 11, d = 12)
const S0: &[Seg] = &[
    s(7, 4, 1),
    s(8, 4, 1),
    s(11, 4, 1),
    s(0, 0, 10),
    s(1, 0, 10),
    s(2, 0, 10),
    s(3, 0, 5),
    s(10, 4, 1),
    s(7, 0, 4),
    s(4, 0, 5),
    s(11, 0, 1),
    s(10, 0, 4),
    s(5, 0, 5),
    s(11, 1, 1),
    s(8, 0, 4),
    s(6, 0, 5),
    s(11, 2, 1),
    s(9, 0, 5),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S1: &[Seg] = &[
    s(7, 5, 1),
    s(10, 4, 1),
    s(10, 5, 1),
    s(0, 0, 7),
    s(11, 0, 1),
    s(11, 1, 1),
    s(8, 4, 1),
    s(1, 0, 7),
    s(8, 5, 1),
    s(11, 2, 1),
    s(7, 4, 1),
    s(2, 0, 7),
    s(11, 3, 1),
    s(11, 5, 1),
    s(11, 4, 1),
    s(3, 0, 6),
    s(7, 0, 4),
    s(4, 0, 6),
    s(10, 0, 4),
    s(5, 0, 6),
    s(8, 0, 4),
    s(6, 0, 6),
    s(9, 0, 6),
    s(12, 0, 5),
];
const S2: &[Seg] = &[
    s(0, 0, 10),
    s(1, 0, 10),
    s(2, 0, 10),
    s(3, 0, 5),
    s(0, 10, 1),
    s(7, 0, 4),
    s(4, 0, 4),
    s(1, 10, 1),
    s(11, 0, 1),
    s(10, 0, 4),
    s(5, 0, 4),
    s(2, 10, 1),
    s(11, 1, 1),
    s(8, 0, 4),
    s(6, 0, 5),
    s(11, 2, 1),
    s(9, 0, 5),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S3: &[Seg] = &[
    s(0, 0, 10),
    s(1, 0, 10),
    s(2, 0, 10),
    s(3, 0, 4),
    s(0, 10, 1),
    s(10, 4, 1),
    s(7, 0, 4),
    s(4, 0, 5),
    s(1, 10, 1),
    s(10, 0, 4),
    s(5, 0, 4),
    s(2, 10, 1),
    s(11, 1, 1),
    s(8, 0, 4),
    s(6, 0, 4),
    s(11, 0, 1),
    s(11, 2, 1),
    s(9, 0, 4),
    s(7, 4, 1),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S4: &[Seg] = &[
    s(0, 0, 10),
    s(1, 0, 10),
    s(2, 0, 10),
    s(3, 0, 4),
    s(0, 10, 1),
    s(8, 4, 1),
    s(7, 0, 4),
    s(4, 0, 4),
    s(1, 10, 1),
    s(11, 0, 1),
    s(10, 0, 4),
    s(5, 0, 5),
    s(2, 10, 1),
    s(8, 0, 4),
    s(6, 0, 4),
    s(11, 1, 1),
    s(11, 2, 1),
    s(9, 0, 4),
    s(11, 4, 1),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S5: &[Seg] = &[
    s(0, 0, 9),
    s(8, 4, 1),
    s(1, 0, 9),
    s(7, 4, 1),
    s(2, 0, 9),
    s(11, 4, 1),
    s(3, 0, 5),
    s(10, 4, 1),
    s(7, 0, 4),
    s(4, 0, 5),
    s(11, 0, 1),
    s(10, 0, 4),
    s(5, 0, 5),
    s(11, 1, 1),
    s(8, 0, 4),
    s(6, 0, 5),
    s(11, 2, 1),
    s(9, 0, 5),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S6: &[Seg] = &[
    s(0, 0, 8),
    s(10, 4, 1),
    s(8, 4, 1),
    s(1, 0, 8),
    s(11, 2, 1),
    s(7, 4, 1),
    s(2, 0, 8),
    s(11, 3, 1),
    s(11, 4, 1),
    s(3, 0, 6),
    s(7, 0, 4),
    s(4, 0, 5),
    s(11, 0, 1),
    s(10, 0, 4),
    s(5, 0, 5),
    s(11, 1, 1),
    s(8, 0, 4),
    s(6, 0, 6),
    s(9, 0, 6),
    s(12, 0, 5),
];
const S7: &[Seg] = &[
    s(0, 0, 8),
    s(11, 0, 1),
    s(8, 4, 1),
    s(1, 0, 8),
    s(7, 5, 1),
    s(7, 4, 1),
    s(2, 0, 8),
    s(10, 5, 1),
    s(11, 4, 1),
    s(3, 0, 5),
    s(10, 4, 1),
    s(7, 0, 4),
    s(4, 0, 6),
    s(10, 0, 4),
    s(5, 0, 5),
    s(11, 1, 1),
    s(8, 0, 4),
    s(6, 0, 5),
    s(11, 2, 1),
    s(9, 0, 5),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S8: &[Seg] = &[
    s(0, 0, 8),
    s(11, 1, 1),
    s(8, 4, 1),
    s(1, 0, 8),
    s(8, 5, 1),
    s(7, 4, 1),
    s(2, 0, 8),
    s(11, 5, 1),
    s(11, 4, 1),
    s(3, 0, 5),
    s(10, 4, 1),
    s(7, 0, 4),
    s(4, 0, 5),
    s(11, 0, 1),
    s(10, 0, 4),
    s(5, 0, 6),
    s(8, 0, 4),
    s(6, 0, 5),
    s(11, 2, 1),
    s(9, 0, 5),
    s(11, 3, 1),
    s(12, 0, 5),
];
const S9: &[Seg] = &[
    s(0, 0, 6),
    s(10, 4, 1),
    s(11, 0, 1),
    s(11, 1, 1),
    s(8, 4, 1),
    s(1, 0, 6),
    s(7, 5, 1),
    s(8, 5, 1),
    s(11, 2, 1),
    s(7, 4, 1),
    s(2, 0, 6),
    s(10, 5, 1),
    s(11, 3, 1),
    s(11, 5, 1),
    s(11, 4, 1),
    s(3, 0, 6),
    s(7, 0, 4),
    s(4, 0, 6),
    s(10, 0, 4),
    s(5, 0, 6),
    s(8, 0, 4),
    s(6, 0, 6),
    s(9, 0, 6),
    s(12, 0, 5),
];
const S10: &[Seg] = &[s(0, 0, 10), s(1, 0, 10), s(2, 0, 10), s(3, 0, 10), s(4, 0, 10), s(5, 0, 10)];
const S11: &[Seg] =
    &[s(0, 0, 10), s(1, 0, 10), s(2, 0, 10), s(3, 0, 9), s(0, 10, 1), s(4, 0, 9), s(1, 10, 1), s(5, 0, 9), s(2, 10, 1)];
const S12: &[Seg] = &[
    s(0, 0, 10),
    s(1, 0, 10),
    s(2, 0, 10),
    s(3, 0, 8),
    rev(0, 10, 2),
    s(4, 0, 8),
    rev(1, 10, 2),
    s(5, 0, 8),
    rev(2, 10, 2),
];
const S13: &[Seg] = &[
    s(0, 0, 10),
    s(1, 0, 10),
    s(2, 0, 10),
    s(3, 0, 4),
    rev(0, 10, 6),
    s(4, 0, 4),
    rev(1, 10, 6),
    s(5, 0, 4),
    rev(2, 10, 6),
];

/// A BC6H mode: its header (2 bits, or 5) and their value, its subsets, the bits of the first
/// endpoint's channels (W) and of the others' (of R, G, B), whether those are differences from
/// the first (transformed), and its fields' segments.
#[derive(Debug, Clone, Copy)]
pub struct Mode6 {
    pub header: u8,
    pub hbits: u32,
    pub ns: usize,
    pub w: u32,
    pub d: [u32; 3],
    pub transformed: bool,
    pub segs: &'static [Seg],
}

const fn mode6(
    header: u8,
    hbits: u32,
    ns: usize,
    w: u32,
    d: [u32; 3],
    transformed: bool,
    segs: &'static [Seg],
) -> Mode6 {
    Mode6 { header, hbits, ns, w, d, transformed, segs }
}

pub const MODES6: [Mode6; 14] = [
    mode6(0b00, 2, 2, 10, [5, 5, 5], true, S0),
    mode6(0b01, 2, 2, 7, [6, 6, 6], true, S1),
    mode6(0b00010, 5, 2, 11, [5, 4, 4], true, S2),
    mode6(0b00110, 5, 2, 11, [4, 5, 4], true, S3),
    mode6(0b01010, 5, 2, 11, [4, 4, 5], true, S4),
    mode6(0b01110, 5, 2, 9, [5, 5, 5], true, S5),
    mode6(0b10010, 5, 2, 8, [6, 5, 5], true, S6),
    mode6(0b10110, 5, 2, 8, [5, 6, 5], true, S7),
    mode6(0b11010, 5, 2, 8, [5, 5, 6], true, S8),
    mode6(0b11110, 5, 2, 6, [6, 6, 6], false, S9),
    mode6(0b00011, 5, 1, 10, [10, 10, 10], false, S10),
    mode6(0b00111, 5, 1, 11, [9, 9, 9], true, S11),
    mode6(0b01011, 5, 1, 12, [8, 8, 8], true, S12),
    mode6(0b01111, 5, 1, 16, [4, 4, 4], true, S13),
];

/// The mode of a BC6H block of a reserved header (`10011`, `10111`, `11011`, `11111`): its bits
/// are kept as they are.
pub const RESERVED6: u8 = 14;

/// A BC6H block's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Block6 {
    /// 0 to 13, or [`RESERVED6`].
    pub mode: u8,
    pub partition: u8,
    /// Endpoint `e`'s channels R, G, B as stored: the first's of W bits, the others' of theirs.
    pub v: [[u16; 3]; 4],
    pub ix: [u8; 16],
    /// The bytes of a block of a reserved mode.
    pub raw: [u8; 16],
}

/// `v`'s low `n` bits in reverse.
fn reversed(v: u32, n: u8) -> u32 {
    v.reverse_bits() >> (32 - n as u32)
}

impl Block6 {
    pub fn info(&self) -> &'static Mode6 {
        &MODES6[self.mode as usize]
    }

    /// The mode of a block starting with byte `b0`.
    pub fn mode_of(b0: u8) -> u8 {
        match b0 & 3 {
            m @ (0 | 1) => m,
            _ => MODES6.iter().position(|m| m.hbits == 5 && m.header == b0 & 31).map_or(RESERVED6, |m| m as u8),
        }
    }

    /// The subset of pixel `p`, and whether it is an anchor.
    #[inline(always)]
    pub fn subset(&self, p: usize) -> (usize, bool) {
        subset(self.info().ns, self.partition as usize, p)
    }

    /// The bits of the indices of the block's mode.
    pub fn index_bits(&self) -> u32 {
        if self.info().ns == 2 { 3 } else { 4 }
    }

    /// The fields of block `b`.
    pub fn read(b: &[u8; 16]) -> Block6 {
        let mut x = Block6 { mode: Self::mode_of(b[0]), ..Default::default() };
        if x.mode == RESERVED6 {
            x.raw = *b;
            return x;
        }
        let m = *x.info();
        let mut r = Bits::of(b);
        r.get(m.hbits);
        for &(f, lo, n, back) in m.segs {
            let v = r.get(n as u32);
            let v = if back { reversed(v, n) } else { v };
            match f {
                12 => x.partition |= (v << lo) as u8,
                _ => x.v[f as usize / 3][f as usize % 3] |= (v << lo) as u16,
            }
        }
        let ib = x.index_bits();
        for p in 0..16 {
            x.ix[p] = r.get(ib - x.subset(p).1 as u32) as u8;
        }
        debug_assert_eq!(r.at, 128);
        x
    }

    /// The block of these fields (as [`Block6::read`] gave them).
    pub fn write(&self) -> [u8; 16] {
        if self.mode == RESERVED6 {
            return self.raw;
        }
        let m = *self.info();
        let mut w = Bits::new();
        w.put(m.hbits, m.header as u32);
        for &(f, lo, n, back) in m.segs {
            let v = match f {
                12 => self.partition as u32,
                _ => self.v[f as usize / 3][f as usize % 3] as u32,
            } >> lo;
            let v = v & ((1 << n) - 1);
            w.put(n as u32, if back { reversed(v, n) } else { v });
        }
        let ib = self.index_bits();
        for p in 0..16 {
            w.put(ib - self.subset(p).1 as u32, self.ix[p] as u32);
        }
        w.bytes()
    }

    /// Endpoint `e`'s channels as integers (the first's and the differences sign-extended where
    /// the format is signed or the mode transformed, the others added to the first), of W bits.
    pub fn integer(&self, e: usize, signed: bool) -> [i32; 3] {
        let m = self.info();
        let sext = |v: i32, n: u32| (v << (32 - n)) >> (32 - n);
        std::array::from_fn(|c| {
            let w0 = self.v[0][c] as i32;
            let w0 = if signed { sext(w0, m.w) } else { w0 };
            if e == 0 {
                return w0;
            }
            let v = self.v[e][c] as i32;
            let v = if signed || m.transformed { sext(v, m.d[c]) } else { v };
            match m.transformed {
                true => {
                    let v = (v + w0) & ((1 << m.w) - 1);
                    if signed { sext(v, m.w) } else { v }
                }
                false => v,
            }
        })
    }

    /// The channels of endpoint `e` unquantized: of 16 bits (unsigned), or in −0x7fff to 0x7fff
    /// (signed).
    pub fn endpoint(&self, e: usize, signed: bool) -> [i32; 3] {
        let w = self.info().w;
        self.integer(e, signed).map(|v| unquantize(v, w, signed))
    }

    /// The pixels of the block, each channel interpolated between the endpoints unquantized
    /// (before it is scaled to a half float): of signed formats, 0x8000 added (all 0 or 0x8000 of a
    /// reserved mode).
    pub fn pixels(&self, signed: bool) -> [[u16; 3]; 16] {
        let bias = if signed { 0x8000 } else { 0 };
        if self.mode == RESERVED6 {
            return [[bias as u16; 3]; 16];
        }
        let m = self.info();
        let e: [[i32; 3]; 4] = std::array::from_fn(|j| if j < 2 * m.ns { self.endpoint(j, signed) } else { [0; 3] });
        let w = WEIGHTS[self.index_bits() as usize];
        std::array::from_fn(|p| {
            let s = self.subset(p).0;
            let (a, z, wt) = (e[2 * s], e[2 * s + 1], w[self.ix[p] as usize] as i32);
            std::array::from_fn(|c| (((a[c] * (64 - wt) + z[c] * wt + 32) >> 6) + bias) as u16)
        })
    }
}

/// An endpoint's channel `v` of `bits` unquantized (as BC6H's decoders do).
pub fn unquantize(v: i32, bits: u32, signed: bool) -> i32 {
    if !signed {
        return match v {
            _ if bits >= 15 => v,
            0 => 0,
            _ if v == (1 << bits) - 1 => 0xffff,
            _ => ((v << 16) + 0x8000) >> bits,
        };
    }
    if bits >= 16 {
        return v;
    }
    let (s, a) = (v < 0, v.abs());
    let u = match a {
        0 => 0,
        _ if a >= (1 << (bits - 1)) - 1 => 0x7fff,
        _ => ((a << 15) + 0x4000) >> (bits - 1),
    };
    if s { -u } else { u }
}

/// The integer of `bits` that unquantizes nearest to (or just below) `u` (as [`unquantize`]
/// gives: of 16 bits, or signed).
pub fn quantize(u: i32, bits: u32, signed: bool) -> i32 {
    if !signed {
        return if bits >= 15 {
            u.clamp(0, (1 << bits) - 1)
        } else {
            ((u.clamp(0, 0xffff) as i64) << bits >> 16) as i32
        };
    }
    if bits >= 16 {
        return u;
    }
    let a = ((u.unsigned_abs().min(0x7fff) as i64) << (bits - 1) >> 15) as i32;
    let a = a.min((1 << (bits - 1)) - 1);
    if u < 0 { -a } else { a }
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
        // and BC6H's, of every mode's header (the reserved ones too)
        let headers: Vec<u8> = (0..32).collect();
        for i in 0..20000 {
            let mut b = [0u8; 16];
            for v in b.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *v = x as u8;
            }
            b[0] = b[0] & !31 | headers[i % 32];
            let f = Block6::read(&b);
            assert_eq!(f.write(), b, "{i}");
        }
        // the segments of each mode cover its bits once
        for m in MODES6 {
            let n: u32 = m.segs.iter().map(|s| s.2 as u32).sum::<u32>() + m.hbits;
            assert_eq!(n + if m.ns == 2 { 46 } else { 63 }, 128);
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
