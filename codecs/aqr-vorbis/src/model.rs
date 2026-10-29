//! The model of a stream's packet values, for coding them with a binary arithmetic coder in place
//! of their Huffman codes. One model per stream, fed the packets in order.
//!
//! - A residue VQ entry of a lookup-1 book is coded digit by digit (each dimension's value), a
//!   digit as decisions (zero, sign, magnitude in unary), each predicted by a mixer of the
//!   book's own code (its probabilities factorised over the digits) and counters in contexts of
//!   the coefficients around: those before it in frequency, at its frequency in the previous
//!   packet of its block size and in the previous pass, and in the vector before (another
//!   channel). Decisions the book's code leaves no choice in are not coded.
//! - Any other entry is coded bit by bit down its book's tree, each node a counter starting at
//!   the probability the code gives it.
//! - A field of bits is coded as a binary tree of its value, a counter per node and kind.

use crate::cm::{Apm, Coder, Counter, Mixer, Stretch, p16};
use crate::packet::Field;
use crate::setup::{Child, Codebook, Setup, lookup1_values};

/// The weight of a codeword of `len` bits: 2^(40 − len).
fn weight(len: u8) -> u64 {
    1 << (40 - len as u32)
}

/// A lookup-1 book as digits: per digit and value of the digits before it (as a number), the
/// weights of the code's entries of each value of the digit.
struct Digits {
    q: usize,
    dims: usize,
    /// `rows[j][prefix]`: the weight of each value of digit j.
    rows: Vec<Vec<Vec<u64>>>,
}

impl Digits {
    fn of(b: &Codebook) -> Option<Digits> {
        b.lookup.as_ref().filter(|l| l.kind == 1)?;
        let (q, dims) = (lookup1_values(b.entries(), b.dims), b.dims);
        if !(2..=64).contains(&q) || q.checked_pow(dims as u32)? > b.entries() {
            return None;
        }
        let mut rows: Vec<Vec<Vec<u64>>> = (0..dims).map(|j| vec![vec![0; q]; q.pow(j as u32)]).collect();
        for (e, &len) in b.lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            // entries past q^dims have no digits (and an inexact lookup1 never has them used)
            if e >= q.pow(dims as u32) {
                return None;
            }
            let (mut t, mut prefix, mut scale) = (e, 0, 1);
            for row in rows.iter_mut() {
                let d = t % q;
                t /= q;
                row[prefix][d] += weight(len);
                prefix += d * scale;
                scale *= q;
            }
        }
        Some(Digits { q, dims, rows })
    }
}

/// Counters of a binary tree: the node of a decision is the prefix of decisions made, from 1.
#[derive(Clone)]
struct Tree {
    nodes: Vec<Counter>,
}

impl Tree {
    fn new(bits: u32) -> Tree {
        Tree { nodes: vec![Counter::default(); 1 << bits] }
    }

    /// Codes `v` (of `n` bits, its most significant first).
    fn code(&mut self, c: &mut dyn Coder, n: u32, v: u32) -> u32 {
        let mut node = 1usize;
        let mut out = 0;
        let last = self.nodes.len() - 1;
        for i in (0..n).rev() {
            let k = &mut self.nodes[node.min(last)];
            let b = c.code(p16(k.p12()), v >> i & 1 == 1);
            k.update(b);
            out = out << 1 | b as u32;
            if node < self.nodes.len() / 2 {
                node = node * 2 + b as usize;
            }
        }
        out
    }
}

/// A book's tree of codewords as counters, each starting at the probability the code gives its
/// node.
struct Book {
    counters: Vec<[Counter; 1]>,
}

impl Book {
    fn of(b: &Codebook) -> Book {
        // the weight below each node
        fn below(b: &Codebook, node: usize, w: &mut [u64]) -> u64 {
            let mut t = 0;
            for bit in 0..2 {
                t += match b.child(node, bit) {
                    Child::None => 0,
                    Child::Entry(e) => weight(b.lengths[e as usize]),
                    Child::Node(n) => below(b, n, w),
                };
            }
            w[node] = t;
            t
        }
        let mut w = vec![0u64; b.nodes()];
        if b.nodes() > 0 {
            below(b, 0, &mut w);
        }
        let side = |node: usize, bit: u32| match b.child(node, bit) {
            Child::None => 0,
            Child::Entry(e) => weight(b.lengths[e as usize]),
            Child::Node(n) => w[n],
        };
        let counters = (0..b.nodes())
            .map(|n| {
                let (w0, w1) = (side(n, 0), side(n, 1));
                [Counter::at(((w1 as u128 * 65536) / (w0 + w1).max(1) as u128) as u32, 12)]
            })
            .collect();
        Book { counters }
    }
}

/// Context tables' size (each).
const TABLE: usize = 1 << 16;
/// APM contexts' count (each).
const APMS: usize = 1 << 12;
/// An unknown coefficient, in the tables of magnitudes.
const UNKNOWN: u8 = 255;

fn hash(v: u64) -> usize {
    (v.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 48) as usize & (TABLE - 1)
}

fn bucket(m: u8) -> u64 {
    match m {
        UNKNOWN => 4,
        0..=2 => m as u64,
        _ => 3,
    }
}

/// The model of a stream.
pub struct Model<'s> {
    setup: &'s Setup,
    stretch: Stretch,
    digits: std::rc::Rc<[Option<Digits>]>,
    books: Vec<Book>,
    vq: [Vec<Counter>; 3],
    apms: [Apm; 3],
    mixer: Mixer<8>,
    fields: Vec<Tree>,
    /// The packet's mode, once read.
    mode: usize,
    /// This packet's coefficients' magnitudes, per (vector, pass), by frequency; and the
    /// previous packet's of each block size.
    cur: Vec<Vec<u8>>,
    prev: [Vec<Vec<u8>>; 2],
}

const PASSES: usize = 8;

/// The mixer's learning rate.
const RATE: i32 = 10;

impl<'s> Model<'s> {
    pub fn new(setup: &'s Setup) -> Self {
        let vectors = setup.ident.channels.max(1);
        Model {
            setup,
            stretch: Stretch::default(),
            digits: setup.codebooks.iter().map(Digits::of).collect::<Vec<_>>().into(),
            books: setup.codebooks.iter().map(Book::of).collect(),
            vq: [vec![Counter::default(); TABLE], vec![Counter::default(); TABLE], vec![Counter::default(); TABLE]],
            apms: [Apm::new(APMS, 6), Apm::new(APMS, 6), Apm::new(APMS, 6)],
            mixer: Mixer::new(16, [1 << 14, 0, 0, 0, 1 << 14, 1 << 14, 1 << 14, 0], RATE),
            fields: vec![Tree::new(9); 6],
            mode: 0,
            cur: vec![Vec::new(); vectors * PASSES],
            prev: [vec![Vec::new(); vectors * PASSES], vec![Vec::new(); vectors * PASSES]],
        }
    }

    /// Ends a packet: its coefficients become the previous packet's of its block size.
    pub fn end_packet(&mut self) {
        let long = self.setup.modes.get(self.mode).is_some_and(|m| m.blockflag) as usize;
        for (p, c) in self.prev[long].iter_mut().zip(self.cur.iter_mut()) {
            if !c.is_empty() {
                std::mem::swap(p, c);
                c.clear();
            }
        }
    }

    /// Codes field `f` of `n` bits, `v` its value (any where decoding).
    pub fn bits(&mut self, c: &mut dyn Coder, f: Field, n: u32, v: u32) -> u32 {
        let kind = match f {
            Field::Type => 0,
            Field::Mode => 1,
            Field::Window => 2,
            Field::Nonzero { .. } => 3,
            Field::Y01 { i: 0, .. } => 4,
            _ => 5,
        };
        let v = self.fields[kind].code(c, n, v);
        if f == Field::Mode {
            self.mode = v as usize;
            for m in &mut self.cur {
                m.clear();
            }
        }
        v
    }

    /// Codes entry `e` of book `book` read as field `f` (any `e` where decoding).
    pub fn entry(&mut self, c: &mut dyn Coder, f: Field, book: usize, e: u32) -> u32 {
        if let Field::Vq { submap, v, part, pass, k, .. } = f
            && self.digits[book].is_some()
        {
            return self.vq_entry(c, book, submap, v, part, pass, k, e);
        }
        self.tree_entry(c, book, e)
    }

    fn tree_entry(&mut self, c: &mut dyn Coder, book: usize, e: u32) -> u32 {
        let b = &self.setup.codebooks[book];
        let (code, len) =
            (b.codewords.get(e as usize).copied().unwrap_or(0), b.lengths.get(e as usize).copied().unwrap_or(0));
        let mut node = 0;
        let mut depth = 0;
        loop {
            let (c0, c1) = (b.child(node, 0), b.child(node, 1));
            // a node of one way is no choice
            let bit = if c0 == Child::None {
                true
            } else if c1 == Child::None {
                false
            } else {
                let want = depth < len as u32 && code >> (len as u32 - 1 - depth) & 1 == 1;
                let k = &mut self.books[book].counters[node][0];
                let bit = c.code(p16(k.p12()), want);
                k.update(bit);
                bit
            };
            depth += 1;
            match b.child(node, bit as u32) {
                Child::Entry(e) => return e,
                Child::Node(n) => node = n,
                Child::None => return 0,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn vq_entry(
        &mut self,
        c: &mut dyn Coder,
        book: usize,
        submap: usize,
        v: usize,
        part: usize,
        pass: usize,
        k: usize,
        e: u32,
    ) -> u32 {
        let s = self.setup;
        let mode = &s.modes[self.mode];
        let map = &s.mappings[mode.mapping];
        let r = &s.residues[map.submaps[submap].1];
        let dims = s.codebooks[book].dims;
        let (q, ndig) = {
            let d = self.digits[book].as_ref().expect("digits");
            (d.q, d.dims)
        };
        let half = q / 2;
        let long = mode.blockflag as usize;
        let n = s.ident.blocksizes[long] / 2;
        let size = if r.kind == 2 { n * map.mux.iter().filter(|&&m| m == submap).count() } else { n };
        let slot = v * PASSES + pass;
        if self.cur[slot].is_empty() {
            self.cur[slot] = vec![UNKNOWN; size];
        }
        let base = r.begin + part * r.partition;
        let step = r.partition / dims;
        let (mut t, mut prefix, mut scale, mut out) = (e as usize, 0, 1, 0usize);
        for j in 0..ndig {
            let want = t % q;
            t /= q;
            let freq = if r.kind == 0 { base + k + j * step } else { base + k * dims + j };
            let at = |m: &Vec<u8>, i: usize| m.get(i).copied().unwrap_or(UNKNOWN);
            let cur = &self.cur[slot];
            let f1 = if freq > 0 { bucket(at(cur, freq - 1)) } else { 5 };
            let f2 = if freq > 1 { bucket(at(cur, freq - 2)) } else { 5 };
            let pp = bucket(at(&self.prev[long][slot], freq));
            let pv = if pass > 0 { bucket(at(&self.cur[slot - 1], freq)) } else { 5 };
            let nb = if v > 0 { bucket(at(&self.cur[slot - PASSES], freq)) } else { 5 };
            let band = (freq * 8 / r.end.max(1)).min(7) as u64;
            let ctx = [
                (book as u64) << 32 | (j as u64) << 24 | f1 << 8 | f2,
                1 << 60 | (book as u64) << 32 | pp << 8 | pv,
                2 << 60 | (book as u64) << 32 | nb << 8 | band,
            ];
            let digits = self.digits.clone();
            let row = &digits[book].as_ref().expect("digits").rows[j][prefix];
            let total: u64 = row.iter().sum();
            let d = self.digit(c, row, total, half, ctx, pass, want);
            out += d * scale;
            prefix += d * scale;
            scale *= q;
            let m = (d as i32 - half as i32).unsigned_abs().min(254) as u8;
            if let Some(x) = self.cur[slot].get_mut(freq) {
                *x = m;
            }
        }
        out as u32
    }

    /// Codes a digit of the values weighted `row` (`total` their sum, `half` the centre).
    #[allow(clippy::too_many_arguments)]
    fn digit(
        &mut self,
        c: &mut dyn Coder,
        row: &[u64],
        total: u64,
        half: usize,
        ctx: [u64; 3],
        pass: usize,
        want: usize,
    ) -> usize {
        let wz = row[half];
        // zero?
        if self.decide(c, wz, total - wz, 0, ctx, pass, want == half) {
            return half;
        }
        let neg: u64 = row[..half].iter().sum();
        let pos: u64 = row[half + 1..].iter().sum();
        let negative = self.decide(c, neg, pos, 1, ctx, pass, want < half);
        let at = |m: usize| if negative { half - m } else { half + m };
        let mut m = 1;
        loop {
            let eq = row[at(m)];
            let beyond: u64 = (m + 1..=half).map(|x| row[at(x)]).sum();
            if !self.decide(
                c,
                beyond,
                eq,
                2 + (m - 1).min(5),
                ctx,
                pass,
                (want as i64 - half as i64).unsigned_abs() as usize > m,
            ) {
                return at(m);
            }
            m += 1;
        }
    }

    /// Codes a decision whose code gives `w1` to yes and `w0` to no; a decision of no choice is
    /// not coded.
    #[allow(clippy::too_many_arguments)]
    fn decide(
        &mut self,
        c: &mut dyn Coder,
        w1: u64,
        w0: u64,
        node: usize,
        ctx: [u64; 3],
        pass: usize,
        want: bool,
    ) -> bool {
        if w0 == 0 || w1 == 0 {
            return w1 > 0;
        }
        // (weights cut to 50 bits, so that the product fits)
        let cut = (64 - (w0 + w1).leading_zeros()).saturating_sub(50);
        let (w0, w1) = (w0 >> cut, w1 >> cut);
        let prior = (w1 * 4096 / (w0 + w1).max(1)) as i32;
        let idx: [usize; 3] = std::array::from_fn(|i| hash(ctx[i] ^ (node as u64) << 16));
        let st = &self.stretch;
        let sp = st.get(prior);
        let a: [i32; 3] = std::array::from_fn(|i| st.get(self.apms[i].p(sp, idx[i] & (APMS - 1))));
        let inputs = [
            sp,
            st.get(self.vq[0][idx[0]].p12()),
            st.get(self.vq[1][idx[1]].p12()),
            st.get(self.vq[2][idx[2]].p12()),
            a[0],
            a[1],
            a[2],
            256,
        ];
        let set = node * 2 + (pass > 0) as usize;
        let p = self.mixer.mix(set, inputs);
        let bit = c.code(p16(p), want);
        self.mixer.update(bit);
        for apm in &mut self.apms {
            apm.update(bit);
        }
        for (t, &i) in self.vq.iter_mut().zip(&idx) {
            t[i].update(bit);
        }
        bit
    }
}
