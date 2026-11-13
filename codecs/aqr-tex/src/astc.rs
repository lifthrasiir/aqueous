//! ASTC's blocks of 2D footprints (FORMAT.md, "`astc-recons`"): a block read into its fields
//! (its block mode, partitions, endpoint modes, the integer sequences of its colour values and
//! weights, the bits no field holds) and written back; a block that is not written back as it is
//! read (illegal ones, of reserved modes) is an *error* block, kept as it is. Of an integer
//! sequence, each group's packed trits or quints are a *choice* among the codes of its values
//! (some values have two codes, and a group cut short codes its values with bits left 0), so that
//! any bits are of some values and choices. What a block decodes to predicts only.

/// A range of integers of an integer sequence: of trits (×3), quints (×5) or neither, times
/// 2^bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub trits: bool,
    pub quints: bool,
    pub bits: u32,
}

impl Range {
    pub const fn levels(self) -> u32 {
        (if self.trits {
            3
        } else if self.quints {
            5
        } else {
            1
        }) << self.bits
    }

    /// The bits of `n` values of the range.
    pub const fn size(self, n: u32) -> u32 {
        let t = if self.trits { (8 * n).div_ceil(5) } else { 0 };
        let q = if self.quints { (7 * n).div_ceil(3) } else { 0 };
        t + q + n * self.bits
    }

    /// Values a group: 5 of trits, 3 of quints, 1 else.
    pub const fn group(self) -> u32 {
        if self.trits {
            5
        } else if self.quints {
            3
        } else {
            1
        }
    }
}

const fn r(trits: bool, quints: bool, bits: u32) -> Range {
    Range { trits, quints, bits }
}

/// The ranges of colour values, from the least (none below 0 to 5: a block that leaves the colour
/// values fewer bits is illegal).
pub const COLOUR_RANGES: [Range; 17] = [
    r(true, false, 1),
    r(false, false, 3),
    r(false, true, 1),
    r(true, false, 2),
    r(false, false, 4),
    r(false, true, 2),
    r(true, false, 3),
    r(false, false, 5),
    r(false, true, 3),
    r(true, false, 4),
    r(false, false, 6),
    r(false, true, 4),
    r(true, false, 5),
    r(false, false, 7),
    r(false, true, 5),
    r(true, false, 6),
    r(false, false, 8),
];

/// The ranges of weights by ρ (2 to 7) and the precision bit.
pub const WEIGHT_RANGES: [Range; 12] = [
    r(false, false, 1),
    r(true, false, 0),
    r(false, false, 2),
    r(false, true, 0),
    r(true, false, 1),
    r(false, false, 3),
    r(false, true, 1),
    r(true, false, 2),
    r(false, false, 4),
    r(false, true, 2),
    r(true, false, 3),
    r(false, false, 5),
];

/// The trits of each code of 8 bits.
pub const fn trits_of(t: u32) -> [u32; 5] {
    let (c, t4, t3) = if t >> 2 & 7 == 7 {
        ((t >> 5 & 7) << 2 | t & 3, 2, 2)
    } else if t >> 5 & 3 == 3 {
        (t & 31, 2, t >> 7 & 1)
    } else {
        (t & 31, t >> 7 & 1, t >> 5 & 3)
    };
    let (t2, t1, t0) = if c & 3 == 3 {
        (2, c >> 4 & 1, (c >> 3 & 1) << 1 | (c >> 2 & 1 & !(c >> 3) & 1))
    } else if c >> 2 & 3 == 3 {
        (2, 2, c & 3)
    } else {
        (c >> 4 & 1, c >> 2 & 3, (c >> 1 & 1) << 1 | (c & 1 & !(c >> 1) & 1))
    };
    [t0, t1, t2, t3, t4]
}

/// The quints of each code of 7 bits.
pub const fn quints_of(q: u32) -> [u32; 3] {
    let b0 = q & 1;
    if q >> 1 & 3 == 3 && q >> 5 & 3 == 0 {
        let q2 = b0 << 2 | (q >> 4 & 1 & !b0 & 1) << 1 | (q >> 3 & 1 & !b0 & 1);
        return [4, 4, q2];
    }
    let (q2, c) =
        if q >> 1 & 3 == 3 { (4, (q >> 3 & 3) << 3 | (!(q >> 5) & 3) << 1 | b0) } else { (q >> 5 & 3, q & 31) };
    let (q1, q0) = if c & 7 == 5 { (4, c >> 3 & 3) } else { (c >> 3 & 3, c & 7) };
    [q0, q1, q2]
}

/// The bits of the packed trits (or quints) of a group of `k` values present in the stream: of
/// trits 2, 4, 5, 7, 8; of quints 3, 5, 7.
const fn packed_bits(trits: bool, k: u32) -> u32 {
    match trits {
        true => [0, 2, 4, 5, 7, 8][k as usize],
        false => [0, 3, 5, 7][k as usize],
    }
}

/// The codes of groups of `k` values (of trits or quints): for each tuple of values (of `k`
/// digits, the first the lowest, numbered `Σ d_i · base^i`), the codes of the bits present (the
/// others 0) that decode to it, from the least: `list[k][start[k][t]..start[k][t + 1]]`.
pub struct Codes {
    pub start: [[u16; 244]; 6],
    pub list: [[u8; 256]; 6],
}

impl Codes {
    const fn make(trits: bool) -> Codes {
        let (base, most) = if trits { (3u32, 5u32) } else { (5, 3) };
        let mut c = Codes { start: [[0; 244]; 6], list: [[0; 256]; 6] };
        let mut k = 1;
        while k <= most {
            let codes = 1u32 << packed_bits(trits, k);
            let tuples = base.pow(k);
            // (the codes of each tuple counted, then placed in the order of the codes)
            let mut tuple = [0u16; 256];
            let mut count = [0u16; 244];
            let mut code = 0;
            while code < codes {
                let d = if trits {
                    trits_of(code)
                } else {
                    let q = quints_of(code);
                    [q[0], q[1], q[2], 0, 0]
                };
                let mut n = 0;
                let mut i = k;
                while i > 0 {
                    i -= 1;
                    n = n * base + d[i as usize];
                }
                tuple[code as usize] = n as u16;
                count[n as usize] += 1;
                code += 1;
            }
            let mut t = 0;
            while t < tuples {
                c.start[k as usize][t as usize + 1] = c.start[k as usize][t as usize] + count[t as usize];
                t += 1;
            }
            let mut at = [0u16; 244];
            let mut code = 0;
            while code < codes {
                let n = tuple[code as usize] as usize;
                c.list[k as usize][(c.start[k as usize][n] + at[n]) as usize] = code as u8;
                at[n] += 1;
                code += 1;
            }
            k += 1;
        }
        c
    }

    /// The codes of tuple `t` of `k` values, from the least.
    pub fn of(&self, k: usize, t: usize) -> &[u8] {
        &self.list[k][self.start[k][t] as usize..self.start[k][t + 1] as usize]
    }
}

pub static TRITS: Codes = Codes::make(true);
pub static QUINTS: Codes = Codes::make(false);

/// An integer sequence as read: its values (each its trit or quint above its bits) and the code of
/// each group's packed trits or quints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sequence {
    pub n: u8,
    pub v: [u8; 64],
    pub codes: [u8; 22],
}

impl Default for Sequence {
    fn default() -> Self {
        Sequence { n: 0, v: [0; 64], codes: [0; 22] }
    }
}

/// Reads `n` values of range `r` from `bits` at `at` (a reader from bit 0 up).
fn read_seq(get: &mut impl FnMut(u32) -> u32, n: u32, r: Range) -> Sequence {
    let mut s = Sequence { n: n as u8, ..Default::default() };
    let g = r.group();
    for grp in 0..n.div_ceil(g) {
        let k = (n - grp * g).min(g);
        let (mut code, mut m) = (0u32, [0u32; 5]);
        // the bits of each value, then the packed bits that follow it
        let take: &[u32] = match (r.trits, r.quints) {
            (true, _) => &[2, 2, 1, 2, 1],
            (_, true) => &[3, 2, 2],
            _ => &[0],
        };
        let mut at = 0;
        for i in 0..k as usize {
            m[i] = get(r.bits);
            if r.trits || r.quints {
                let t = take[i];
                code |= get(t) << at;
                at += t;
            }
        }
        if r.trits || r.quints {
            s.codes[grp as usize] = code as u8;
        }
        let digits: [u32; 5] = match (r.trits, r.quints) {
            (true, _) => trits_of(code),
            (_, true) => {
                let q = quints_of(code);
                [q[0], q[1], q[2], 0, 0]
            }
            _ => [0; 5],
        };
        for i in 0..k as usize {
            s.v[(grp * g) as usize + i] = (digits[i] << r.bits | m[i]) as u8;
        }
    }
    s
}

/// Writes sequence `s` of range `r` (as [`read_seq`] read it).
fn write_seq(put: &mut impl FnMut(u32, u32), s: &Sequence, r: Range) {
    let g = r.group();
    let n = s.n as u32;
    for grp in 0..n.div_ceil(g) {
        let k = (n - grp * g).min(g);
        let code = if r.trits || r.quints { s.codes[grp as usize] as u32 } else { 0 };
        let take: &[u32] = match (r.trits, r.quints) {
            (true, _) => &[2, 2, 1, 2, 1],
            (_, true) => &[3, 2, 2],
            _ => &[0],
        };
        let mut at = 0;
        for i in 0..k as usize {
            put(r.bits, s.v[(grp * g) as usize + i] as u32 & ((1 << r.bits) - 1));
            if r.trits || r.quints {
                put(take[i], code >> at & ((1 << take[i]) - 1));
                at += take[i];
            }
        }
    }
}

/// A colour value of range `r` unquantized to 0 to 255.
pub const fn unquantize_colour(v: u32, r: Range) -> i32 {
    if !r.trits && !r.quints {
        // (its bits repeated down from the top)
        let (b, mut u, mut s) = (r.bits as i32, 0u32, 8 - r.bits as i32);
        while s > -b {
            u |= if s >= 0 { v << s } else { v >> -s };
            s -= b;
        }
        return (u & 255) as i32;
    }
    let (d, m) = (v >> r.bits, v & ((1 << r.bits) - 1));
    let a = if m & 1 == 1 { 0x1ff } else { 0 };
    let (b1, b2, b3, b4, b5) = (m >> 1 & 1, m >> 2 & 1, m >> 3 & 1, m >> 4 & 1, m >> 5 & 1);
    let (b, c) = match (r.trits, r.bits) {
        (true, 1) => (0, 204),
        (false, 1) => (0, 113),
        (true, 2) => (b1 * 0b100010110, 93),
        (false, 2) => (b1 * 0b100001100, 54),
        (true, 3) => (b2 * 0b100001010 + b1 * 0b010000101, 44),
        (false, 3) => (b2 * 0b100000101 + b1 * 0b010000010, 26),
        (true, 4) => (b3 * 0b100000101 + b2 * 0b010000010 + b1 * 0b001000001, 22),
        (false, 4) => (b3 * 0b100000010 + b2 * 0b010000001 + b1 * 0b001000000, 13),
        (true, 5) => (b4 * 0b100000010 + b3 * 0b010000001 + b2 * 0b001000000 + b1 * 0b000100000, 11),
        (false, 5) => (b4 * 0b100000001 + b3 * 0b010000000 + b2 * 0b001000000 + b1 * 0b000100000, 6),
        (true, _) => (b5 * 0b100000001 + b4 * 0b010000000 + b3 * 0b001000000 + b2 * 0b000100000 + b1 * 0b000010000, 5),
        (false, _) => (0, 0),
    };
    let u = (d * c + b) ^ a;
    ((a & 0x80) | u >> 2) as i32 & 255
}

/// A weight of range `r` unquantized to 0 to 64.
pub const fn unquantize_weight(v: u32, r: Range) -> i32 {
    let u = if !r.trits && !r.quints {
        let b = r.bits;
        let x = v << (6 - b);
        (x | x >> b | x >> (2 * b) | x >> (3 * b) | x >> (4 * b) | x >> (5 * b)) & 63
    } else if r.bits == 0 {
        if r.trits { [0, 32, 63][v as usize] } else { [0, 16, 32, 47, 63][v as usize] }
    } else {
        let (d, m) = (v >> r.bits, v & ((1 << r.bits) - 1));
        let (b1, b2) = (m >> 1 & 1, m >> 2 & 1);
        let a = if m & 1 == 1 { 0x7f } else { 0 };
        let (b, c) = match (r.trits, r.bits) {
            (true, 1) => (0, 50),
            (false, 1) => (0, 28),
            (true, 2) => (b1 * 0b1000101, 23),
            (false, 2) => (b1 * 0b1000010, 13),
            (true, _) => (b2 * 0b1000010 + b1 * 0b0100001, 11),
            _ => (0, 0),
        };
        let u = (d * c + b) ^ a;
        (a & 0x20) | u >> 2
    };
    (u + (u > 32) as u32) as i32
}

/// The values of a range ordered by what they unquantize to (then by their own): of each value its
/// rank, of each rank its value, and of each value what it unquantizes to.
pub struct Order {
    pub levels: usize,
    pub rank: [u8; 256],
    pub value: [u8; 256],
    pub unq: [i16; 256],
}

impl Order {
    const fn of(r: Range, weight: bool) -> Order {
        let levels = r.levels() as usize;
        let mut o = Order { levels, rank: [0; 256], value: [0; 256], unq: [0; 256] };
        let mut count = [0u16; 257];
        let mut v = 0;
        while v < levels {
            let u = if weight { unquantize_weight(v as u32, r) } else { unquantize_colour(v as u32, r) };
            o.unq[v] = u as i16;
            count[u as usize + 1] += 1;
            v += 1;
        }
        let mut u = 0;
        while u < 256 {
            count[u + 1] += count[u];
            u += 1;
        }
        let mut v = 0;
        while v < levels {
            let u = o.unq[v] as usize;
            let k = count[u] as usize;
            count[u] += 1;
            o.value[k] = v as u8;
            o.rank[v] = k as u8;
            v += 1;
        }
        o
    }

    /// The rank of the value that unquantizes nearest to `u` (the first of them).
    #[inline]
    pub fn nearest(&self, u: i32) -> usize {
        // (the ranks are in order of what they unquantize to: the first not below `u`, or the
        // first of the ones before it, of what unquantizes nearer; the first of the lesser one
        // where they are as near)
        let at = |k: usize| self.unq[self.value[k] as usize] as i32;
        let first = |u: i32| {
            let (mut lo, mut hi) = (0, self.levels);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if at(mid) < u { lo = mid + 1 } else { hi = mid }
            }
            lo
        };
        let k = first(u);
        if k == self.levels {
            return first(at(k - 1));
        }
        if k == 0 || at(k) - u < u - at(k - 1) {
            return k;
        }
        first(at(k - 1))
    }
}

const fn orders<const N: usize>(ranges: [Range; N], weight: bool) -> [Order; N] {
    let mut o = [const { Order { levels: 0, rank: [0; 256], value: [0; 256], unq: [0; 256] } }; N];
    let mut i = 0;
    while i < N {
        o[i] = Order::of(ranges[i], weight);
        i += 1;
    }
    o
}

pub static COLOUR_ORDERS: [Order; 17] = orders(COLOUR_RANGES, false);
pub static WEIGHT_ORDERS: [Order; 12] = orders(WEIGHT_RANGES, true);

/// What a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A void-extent block (of one colour, its extents), of LDR or HDR.
    Void,
    /// An error block, kept as it is.
    Error,
    Normal,
}

/// A block's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    pub raw: [u8; 16],
    /// Of normal blocks: the block mode (11 bits), its weight grid, range and planes.
    pub mode: u16,
    pub wx: u8,
    pub wy: u8,
    pub wr: u8,
    pub dual: bool,
    /// The partitions (1 to 4), the partition index (seed), the endpoint mode of each partition.
    pub np: u8,
    pub seed: u16,
    pub cem: [u8; 4],
    /// The CEM field's selector (bits 23–24; 0 of one partition) and the CCS of dual planes.
    pub sel: u8,
    pub ccs: u8,
    /// The colour values' range (an index of [`COLOUR_RANGES`]) and the values.
    pub cr: u8,
    pub colours: Sequence,
    pub weights: Sequence,
    /// The bits between the colour values and what lies below the weights, as they are.
    pub unused: u128,
    pub unused_at: u8,
    pub unused_n: u8,
}

impl Default for Block {
    fn default() -> Self {
        Block {
            kind: Kind::Error,
            raw: [0; 16],
            mode: 0,
            wx: 0,
            wy: 0,
            wr: 0,
            dual: false,
            np: 0,
            seed: 0,
            cem: [0; 4],
            sel: 0,
            ccs: 0,
            cr: 0,
            colours: Sequence::default(),
            weights: Sequence::default(),
            unused: 0,
            unused_at: 0,
            unused_n: 0,
        }
    }
}

/// The weight grid, its range (an index of [`WEIGHT_RANGES`]) and dual planes of 2D block mode
/// `m`, if it is not reserved (nor void-extent).
pub fn block_mode(m: u32) -> Option<(u32, u32, usize, bool)> {
    let b = |i: u32| m >> i & 1;
    let f = |lo: u32, n: u32| m >> lo & ((1 << n) - 1);
    let (wx, wy, rho, high, dual);
    if m & 3 != 0 {
        rho = b(4) | (m & 3) << 1;
        let (a, c) = (f(5, 2), f(7, 2));
        (wx, wy) = match f(2, 2) {
            0 => (c + 4, a + 2),
            1 => (c + 8, a + 2),
            2 => (a + 2, c + 8),
            _ if b(8) == 0 => (a + 2, b(7) + 6),
            _ => (b(7) + 2, a + 2),
        };
        (high, dual) = (b(9), b(10));
    } else {
        if f(0, 4) == 0 {
            return None;
        }
        rho = b(4) | f(2, 2) << 1;
        let a = f(5, 2);
        (wx, wy, high, dual) = match f(7, 2) {
            0 => (12, a + 2, b(9), b(10)),
            1 => (a + 2, 12, b(9), b(10)),
            // (of no dual planes nor high precision: bits 9 and 10 are the height)
            2 => (a + 6, f(9, 2) + 6, 0, 0),
            _ => match a {
                0 => (6, 10, b(9), b(10)),
                1 => (10, 6, b(9), b(10)),
                _ => return None,
            },
        };
    }
    (rho >= 2).then_some((wx, wy, (rho as usize - 2) + 6 * high as usize, dual != 0))
}

/// The pairs of colour values of endpoint mode `cem`: its class plus 1.
pub fn pairs(cem: u8) -> u32 {
    (cem >> 2) as u32 + 1
}

/// The bits of the weights of a normal block of bytes `b` of footprint `fp`, if it is one: of a
/// block mode not reserved, of at most 64 weights in 24 to 96 bits, of a grid within the footprint,
/// of no dual planes of 4 partitions, of at most 18 colour values with 13/5 bits each at least.
pub fn legal(b: &[u8; 16], (fw, fh): (u32, u32)) -> Option<u32> {
    let v = u128::from_le_bytes(*b);
    let f = |lo: u32, n: u32| (v >> lo) as u32 & ((1u32 << n) - 1);
    let mode = f(0, 11);
    if mode & 0x1ff == 0x1fc {
        return None;
    }
    let (wx, wy, wr, dual) = block_mode(mode)?;
    let np = f(11, 2) + 1;
    let nw = wx * wy * (1 + dual as u32);
    let wbits = WEIGHT_RANGES[wr].size(nw);
    if nw > 64 || !(24..=96).contains(&wbits) || wx > fw || wy > fh || (dual && np == 4) {
        return None;
    }
    let (sel, start) = match np {
        1 => (0, 17),
        _ => (f(23, 2), 29),
    };
    let extra = if sel != 0 { 3 * np - 4 } else { 0 };
    let nv: u32 = match (np, sel) {
        (1, _) => 2 * pairs(f(13, 4) as u8),
        (_, 0) => np * 2 * pairs(f(25, 4) as u8),
        _ => {
            let enc = f(25, 4) | f(128 - wbits - extra, extra) << 4;
            (0..np).map(|i| 2 * (sel - 1 + (enc >> i & 1) + 1)).sum()
        }
    };
    let avail = 128 - wbits - extra - 2 * dual as u32 - start;
    (nv <= 18 && avail >= (13 * nv).div_ceil(5)).then_some(wbits)
}

impl Block {
    /// The block of bytes `b` of footprint (`fw`, `fh`), read.
    pub fn read(b: &[u8; 16], (fw, fh): (u32, u32)) -> Block {
        let mut x = Block { raw: *b, ..Default::default() };
        let v = u128::from_le_bytes(*b);
        let f = |lo: u32, n: u32| (v >> lo) as u32 & ((1u32 << n) - 1);
        let mode = f(0, 11);
        if mode & 0x1ff == 0x1fc {
            // (its reserved bits, 10 and 11, 1; its extents as they are)
            x.kind = if f(10, 2) == 3 { Kind::Void } else { Kind::Error };
            return x;
        }
        let Some(wbits) = legal(b, (fw, fh)) else { return x };
        let (wx, wy, wr, dual) = block_mode(mode).expect("a legal block's mode");
        let np = f(11, 2) + 1;
        let range = WEIGHT_RANGES[wr];
        let nw = wx * wy * (1 + dual as u32);
        let (mut cem, sel, extra, start) = match np {
            1 => ([f(13, 4) as u8; 4], 0, 0, 17),
            _ => {
                let sel = f(23, 2);
                match sel {
                    0 => ([f(25, 4) as u8; 4], 0, 0, 29),
                    _ => ([0; 4], sel as u8, 3 * np - 4, 29),
                }
            }
        };
        let below = 128 - wbits;
        if sel != 0 {
            let enc = f(25, 4) | f(below - extra, extra) << 4;
            for i in 0..np {
                let class = sel as u32 - 1 + (enc >> i & 1);
                cem[i as usize] = (class << 2 | enc >> (np + 2 * i) & 3) as u8;
            }
        }
        let ccs_at = below - extra - 2 * dual as u32;
        let nv: u32 = (0..np as usize).map(|i| 2 * pairs(cem[i])).sum();
        // (the colour values' bits: those from their start up to what is below the weights)
        let avail = ccs_at as i32 - start as i32;
        let Some(cr) = (0..COLOUR_RANGES.len()).rev().find(|&i| COLOUR_RANGES[i].size(nv) as i32 <= avail) else {
            return x;
        };
        x.kind = Kind::Normal;
        x.mode = mode as u16;
        (x.wx, x.wy, x.wr, x.dual) = (wx as u8, wy as u8, wr as u8, dual);
        x.np = np as u8;
        x.seed = if np > 1 { f(13, 10) as u16 } else { 0 };
        x.cem = cem;
        x.sel = sel;
        x.ccs = if dual { f(ccs_at, 2) as u8 } else { 0 };
        x.cr = cr as u8;
        let mut at = start;
        let mut get = |n: u32| {
            let r = (v >> at) as u32 & ((1u64 << n) - 1) as u32;
            at += n;
            r
        };
        x.colours = read_seq(&mut get, nv, COLOUR_RANGES[cr]);
        let end = start + COLOUR_RANGES[cr].size(nv);
        (x.unused_at, x.unused_n) = (end as u8, (ccs_at - end) as u8);
        x.unused = (v >> end) & ((1u128 << (ccs_at - end)) - 1);
        // the weights, from bit 127 down (bit 127 − k of the block is bit k of it reversed)
        let rv = v.reverse_bits();
        let mut at = 0;
        let mut get = |n: u32| {
            let r = (rv >> at) as u32 & ((1u64 << n) - 1) as u32;
            at += n;
            r
        };
        x.weights = read_seq(&mut get, nw, range);
        // (a block that is not as it is written is an error block)
        if x.write() != *b {
            return Block { raw: *b, ..Default::default() };
        }
        x
    }

    /// The block of these fields (as [`Block::read`] gave them).
    pub fn write(&self) -> [u8; 16] {
        if self.kind != Kind::Normal {
            return self.raw;
        }
        let mut v: u128 = self.mode as u128;
        let np = self.np as u32;
        v |= ((np - 1) as u128) << 11;
        let range = WEIGHT_RANGES[self.wr as usize];
        let nw = self.wx as u32 * self.wy as u32 * (1 + self.dual as u32);
        let wbits = range.size(nw);
        let below = 128 - wbits;
        let (start, extra) = match (np, self.sel) {
            (1, _) => {
                v |= (self.cem[0] as u128) << 13;
                (17, 0)
            }
            (_, 0) => {
                v |= (self.seed as u128) << 13 | (self.cem[0] as u128) << 25;
                (29, 0)
            }
            (_, sel) => {
                v |= (self.seed as u128) << 13 | (sel as u128) << 23;
                let mut enc = 0u32;
                for i in 0..np {
                    let c = self.cem[i as usize];
                    enc |= (((c >> 2) as u32 + 1 - sel as u32) & 1) << i | ((c & 3) as u32) << (np + 2 * i);
                }
                v |= ((enc & 15) as u128) << 25;
                let extra = 3 * np - 4;
                v |= ((enc >> 4) as u128) << (below - extra);
                (29, extra)
            }
        };
        let ccs_at = below - extra - 2 * self.dual as u32;
        if self.dual {
            v |= (self.ccs as u128) << ccs_at;
        }
        let mut at = start;
        let mut put = |n: u32, x: u32| {
            v |= (x as u128) << at;
            at += n;
        };
        write_seq(&mut put, &self.colours, COLOUR_RANGES[self.cr as usize]);
        v |= self.unused << self.unused_at;
        // (the weights from bit 127 down: written up from bit 0, then reversed)
        let (mut rv, mut at) = (0u128, 0u32);
        let mut put = |n: u32, x: u32| {
            rv |= ((x & ((1u64 << n) - 1) as u32) as u128) << at;
            at += n;
        };
        write_seq(&mut put, &self.weights, range);
        (v | rv.reverse_bits()).to_le_bytes()
    }

    /// The bits of the block below its weights (of a void-extent or error block, all of them).
    pub fn head_bits(&self) -> u32 {
        match self.kind {
            Kind::Normal => {
                let nw = self.wx as u32 * self.wy as u32 * (1 + self.dual as u32);
                128 - WEIGHT_RANGES[self.wr as usize].size(nw)
            }
            _ => 128,
        }
    }
}

/// ASTC's hash of a partition's seed.
fn hash52(mut p: u32) -> u32 {
    p ^= p >> 15;
    p = p.wrapping_sub(p << 17);
    p = p.wrapping_add(p << 7);
    p = p.wrapping_add(p << 4);
    p ^= p >> 5;
    p = p.wrapping_add(p << 16);
    p ^= p >> 7;
    p ^= p >> 3;
    p ^= p << 6;
    p ^= p >> 17;
    p
}

/// The partition of texel (x, y) of a block of `texels` texels, of seed `seed` and `n` partitions.
pub fn partition(seed: u32, x: u32, y: u32, n: u32, texels: u32) -> u32 {
    Partitions::new(seed, n, texels).of(x, y)
}

/// What [`partition`] takes of a block's seed, partitions and texels, once for its texels.
#[derive(Clone, Copy)]
pub struct Partitions {
    n: u32,
    small: bool,
    s: [u32; 8],
    rnum: u32,
}

impl Partitions {
    pub fn new(seed: u32, n: u32, texels: u32) -> Partitions {
        let mut p = Partitions { n, small: texels < 31, s: [0; 8], rnum: 0 };
        if n == 1 {
            return p;
        }
        let seed = seed + (n - 1) * 1024;
        let rnum = hash52(seed);
        let (sh1, sh2) = match seed & 1 {
            1 => (if seed & 2 != 0 { 4 } else { 5 }, if n == 3 { 6 } else { 5 }),
            _ => (if n == 3 { 6 } else { 5 }, if seed & 2 != 0 { 4 } else { 5 }),
        };
        // (of the 12 seeds of the hash, the first 8 are of the texel's place; the rest unused)
        p.s = std::array::from_fn(|i| {
            let v = rnum >> (4 * i) & 15;
            (v * v) >> if i % 2 == 0 { sh1 } else { sh2 }
        });
        p.rnum = rnum;
        p
    }

    /// The partition of texel (x, y).
    #[inline]
    pub fn of(&self, x: u32, y: u32) -> u32 {
        let (n, s, rnum) = (self.n, &self.s, self.rnum);
        if n == 1 {
            return 0;
        }
        let (x, y) = if self.small { (x << 1, y << 1) } else { (x, y) };
        let a = (s[0] * x + s[1] * y + (rnum >> 14)) & 0x3f;
        let b = (s[2] * x + s[3] * y + (rnum >> 10)) & 0x3f;
        let c = if n >= 3 { (s[4] * x + s[5] * y + (rnum >> 6)) & 0x3f } else { 0 };
        let d = if n >= 4 { (s[6] * x + s[7] * y + (rnum >> 2)) & 0x3f } else { 0 };
        if a >= b && a >= c && a >= d {
            0
        } else if b >= c && b >= d {
            1
        } else if c >= d {
            2
        } else {
            3
        }
    }
}

fn bit_transfer_signed(a: &mut i32, b: &mut i32) {
    *b >>= 1;
    *b |= *a & 0x80;
    *a >>= 1;
    *a &= 0x3f;
    if *a & 0x20 != 0 {
        *a -= 0x40;
    }
}

fn blue_contract(r: i32, g: i32, b: i32, a: i32) -> [i32; 4] {
    [(r + b) >> 1, (g + b) >> 1, b, a]
}

/// The two endpoints (RGBA, 8 bits) of endpoint mode `cem` of unquantized values `v`: of LDR modes
/// exactly, of HDR ones their 12-bit values' top 8 bits (an approximation, for predictions).
pub fn endpoints(cem: u8, v: &[i32]) -> [[i32; 4]; 2] {
    let clamp = |e: [i32; 4]| e.map(|c| c.clamp(0, 255));
    let mut v: [i32; 8] = std::array::from_fn(|i| v.get(i).copied().unwrap_or(0));
    match cem {
        0 => [[v[0], v[0], v[0], 255], [v[1], v[1], v[1], 255]],
        1 => {
            let l0 = (v[0] >> 2) | (v[1] & 0xc0);
            let l1 = (l0 + (v[1] & 0x3f)).min(255);
            [[l0, l0, l0, 255], [l1, l1, l1, 255]]
        }
        4 => [[v[0], v[0], v[0], v[2]], [v[1], v[1], v[1], v[3]]],
        5 => {
            let (mut a, mut b) = (v[1], v[0]);
            bit_transfer_signed(&mut a, &mut b);
            let (mut c, mut d) = (v[3], v[2]);
            bit_transfer_signed(&mut c, &mut d);
            [clamp([b, b, b, d]), clamp([b + a, b + a, b + a, d + c])]
        }
        6 => [[(v[0] * v[3]) >> 8, (v[1] * v[3]) >> 8, (v[2] * v[3]) >> 8, 255], [v[0], v[1], v[2], 255]],
        8 | 12 => {
            let (a0, a1) = if cem == 12 { (v[6], v[7]) } else { (255, 255) };
            if v[1] + v[3] + v[5] >= v[0] + v[2] + v[4] {
                [[v[0], v[2], v[4], a0], [v[1], v[3], v[5], a1]]
            } else {
                [blue_contract(v[1], v[3], v[5], a1), blue_contract(v[0], v[2], v[4], a0)]
            }
        }
        9 | 13 => {
            for k in 0..(if cem == 13 { 4 } else { 3 }) {
                let (mut a, mut b) = (v[2 * k + 1], v[2 * k]);
                bit_transfer_signed(&mut a, &mut b);
                (v[2 * k + 1], v[2 * k]) = (a, b);
            }
            let (a0, a1) = if cem == 13 { (v[6], v[6] + v[7]) } else { (255, 255) };
            if v[1] + v[3] + v[5] >= 0 {
                [clamp([v[0], v[2], v[4], a0]), clamp([v[0] + v[1], v[2] + v[3], v[4] + v[5], a1])]
            } else {
                [
                    clamp(blue_contract(v[0] + v[1], v[2] + v[3], v[4] + v[5], a1)),
                    clamp(blue_contract(v[0], v[2], v[4], a0)),
                ]
            }
        }
        10 => [[(v[0] * v[3]) >> 8, (v[1] * v[3]) >> 8, (v[2] * v[3]) >> 8, v[4]], [v[0], v[1], v[2], v[5]]],
        // (HDR: approximately, the values as they are)
        2 | 3 => [[v[0], v[0], v[0], 255], [v[1], v[1], v[1], 255]],
        7 => {
            [[v[0] - v[3] / 2, v[1] - v[3] / 2, v[2] - v[3] / 2, 255].map(|c| c.clamp(0, 255)), [v[0], v[1], v[2], 255]]
        }
        11 => [[v[0], v[2], v[4], 255], [v[1], v[3], v[5], 255]],
        _ => [[v[0], v[2], v[4], v[6]], [v[1], v[3], v[5], v[7]]],
    }
}

/// The texels of a block of footprint (`fw`, `fh`), row by row, as 8-bit RGBA (of a void-extent
/// block its colour's top bits; of an error block magenta).
pub fn pixels(b: &Block, (fw, fh): (u32, u32), out: &mut [[u8; 4]]) {
    let n = (fw * fh) as usize;
    match b.kind {
        Kind::Error => out[..n].fill([255, 0, 255, 255]),
        Kind::Void => {
            let c = |i: usize| b.raw[8 + 2 * i + 1];
            out[..n].fill([c(0), c(1), c(2), c(3)]);
        }
        Kind::Normal => {
            let ends = colour_ends(b);
            let w = texel_weights(b, (fw, fh));
            let parts = Partitions::new(b.seed as u32, b.np as u32, fw * fh);
            for y in 0..fh {
                for x in 0..fw {
                    let t = (y * fw + x) as usize;
                    let p = parts.of(x, y) as usize;
                    let (e0, e1) = (ends[p][0], ends[p][1]);
                    out[t] = std::array::from_fn(|c| {
                        let i = if b.dual && c == b.ccs as usize { w[1][t] } else { w[0][t] };
                        let (c0, c1) = (e0[c] * 257, e1[c] * 257);
                        (((c0 * (64 - i) + c1 * i + 32) / 64) >> 8) as u8
                    });
                }
            }
        }
    }
}

/// The endpoints of each partition of a normal block.
pub fn colour_ends(b: &Block) -> [[[i32; 4]; 2]; 4] {
    let r = COLOUR_RANGES[b.cr as usize];
    let mut at = 0;
    std::array::from_fn(|p| {
        if p >= b.np as usize {
            return [[0; 4]; 2];
        }
        let n = (2 * pairs(b.cem[p]) as usize).min(8);
        let mut v = [0i32; 8];
        for k in 0..n {
            v[k] = unquantize_colour(b.colours.v[(at + k).min(63)] as u32, r);
        }
        at += n;
        endpoints(b.cem[p], &v[..n])
    })
}

/// The weights of each texel of a normal block (0 to 64), of each plane, by the infill of its
/// grid.
pub fn texel_weights(b: &Block, (fw, fh): (u32, u32)) -> [[i32; 144]; 2] {
    let r = WEIGHT_RANGES[b.wr as usize];
    let (wx, wy) = (b.wx as u32, b.wy as u32);
    let planes = 1 + b.dual as usize;
    // (each plane's grid unquantized, and 0 past its end, where the infill of its last row and
    // column reaches with a weight of 0)
    let n = (wx * wy) as usize;
    let mut grid = [[0i32; 64 + 16]; 2];
    for plane in 0..planes {
        for i in 0..n.min(64) {
            grid[plane][i] = unquantize_weight(b.weights.v[(i * planes + plane).min(63)] as u32, r);
        }
    }
    let (ds, dt) = ((1024 + fw / 2) / (fw - 1).max(1), (1024 + fh / 2) / (fh - 1).max(1));
    let mut out = [[0i32; 144]; 2];
    for y in 0..fh {
        let gt = (dt * y * (wy - 1) + 32) >> 6;
        let (jt, ft) = (gt >> 4, gt & 15);
        for x in 0..fw {
            let gs = (ds * x * (wx - 1) + 32) >> 6;
            let (js, fs) = (gs >> 4, gs & 15);
            let v0 = (js + jt * wx) as usize;
            let w11 = ((fs * ft + 8) >> 4) as i32;
            let (w10, w01) = (ft as i32 - w11, fs as i32 - w11);
            let w00 = 16 - fs as i32 - ft as i32 + w11;
            for plane in 0..planes {
                let g = &grid[plane];
                let at = |d: usize| g[(v0 + d).min(64 + 15)];
                let i = (at(0) * w00 + at(1) * w01 + at(wx as usize) * w10 + at(wx as usize + 1) * w11 + 8) >> 4;
                out[plane][(y * fw + x) as usize] = i;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences() {
        // every code of a group decodes to a tuple of which it is one of the codes
        for (codes, base, most, trits) in [(&TRITS, 3u32, 5, true), (&QUINTS, 5, 3, false)] {
            for k in 1..=most {
                let tuples = base.pow(k as u32) as usize;
                assert_eq!(codes.start[k][tuples] as u32, 1 << packed_bits(trits, k as u32));
                for t in 0..tuples {
                    assert!(!codes.of(k, t).is_empty(), "{base} {k}");
                    assert!(codes.of(k, t).windows(2).all(|w| w[0] < w[1]));
                }
            }
        }
        // the unquantized weights of each range are within 0 to 64, its ends 0 and 64
        for r in WEIGHT_RANGES {
            let u: Vec<i32> = (0..r.levels()).map(|v| unquantize_weight(v, r)).collect();
            assert!(u.iter().all(|&w| (0..=64).contains(&w)));
            assert!(u.contains(&0) && u.contains(&64), "{r:?} {u:?}");
        }
        for r in COLOUR_RANGES {
            let u: Vec<i32> = (0..r.levels()).map(|v| unquantize_colour(v, r)).collect();
            assert!(u.contains(&0) && u.contains(&255), "{r:?}");
        }
    }

    /// [`Order::nearest`] is the first rank of the least distance, of every order and value.
    #[test]
    fn nearest() {
        for o in COLOUR_ORDERS.iter().chain(&WEIGHT_ORDERS) {
            for u in -20..300 {
                let d = |k: usize| (o.unq[o.value[k] as usize] as i32 - u).abs();
                let want = (0..o.levels).min_by_key(|&k| (d(k), k)).unwrap();
                assert_eq!(o.nearest(u), want, "{} levels, {u}", o.levels);
            }
        }
    }

    #[test]
    fn blocks() {
        // random blocks read and written back (normal ones, or kept as error blocks)
        let mut x = 0x5eed_u64 | 1;
        let mut normal = 0;
        for i in 0..100000 {
            let mut b = [0u8; 16];
            for v in b.iter_mut() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *v = x as u8;
            }
            let fp = [(4, 4), (6, 5), (8, 8), (12, 12)][i % 4];
            let blk = Block::read(&b, fp);
            assert_eq!(blk.write(), b);
            normal += (blk.kind == Kind::Normal) as usize;
        }
        assert!(normal > 5000, "{normal}");
    }
}
