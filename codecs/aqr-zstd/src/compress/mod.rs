//! An emulation of libzstd's compressor (1.5.x), so that a frame it wrote is made again from
//! its content alone: `ZSTD_compress` (or a stream ended at once, its size pledged) at a level
//! of the double-fast strategy, without dictionary or checksum. Written from what is known of
//! its behaviour and checked against libzstd as a black box; libzstd's code is not read.
//!
//! What is emulated: the parameters a level takes for a size, the frame header, blocks of
//! 128 KiB (not split before: contents of 256 KiB or more are not taken), the double-fast
//! match finder, the literals' Huffman coding and the sequences' FSE coding with libzstd's
//! choices of tables, and when a block is kept raw or RLE.

mod bits;
mod fse;
mod huf;

use bits::Bits;
use fse::{CTable, highbit};
use huf::HTable;

/// Compression parameters (`ZSTD_compressionParameters`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub window_log: u32,
    pub chain_log: u32,
    pub hash_log: u32,
    pub search_log: u32,
    pub min_match: u32,
    pub target_length: u32,
    /// 1 fast, 2 double fast, …
    pub strategy: u32,
}

const DFAST: u32 = 2;

/// The parameters of `level` for `size` bytes of content: None where the level is not
/// emulated (only those of the double-fast strategy are).
pub fn params(level: i32, size: usize) -> Option<Params> {
    let row = (size <= 256 << 10) as usize + (size <= 128 << 10) as usize + (size <= 16 << 10) as usize;
    let p = |w, c, h, s, l, t, st| Params {
        window_log: w,
        chain_log: c,
        hash_log: h,
        search_log: s,
        min_match: l,
        target_length: t,
        strategy: st,
    };
    let mut cp = match (level, row) {
        (3, 0) => p(21, 16, 17, 1, 5, 0, DFAST),
        (3, 1) => p(18, 16, 16, 1, 4, 0, DFAST),
        (3, 2) => p(17, 15, 16, 2, 5, 0, DFAST),
        (3, 3) => p(14, 14, 15, 2, 4, 0, DFAST),
        _ => return None,
    };
    // ZSTD_adjustCParams_internal, no dictionary, the size known
    let src_log = if size < 64 { 6 } else { highbit(size as u32 - 1) + 1 };
    if cp.window_log > src_log {
        cp.window_log = src_log;
    }
    if cp.hash_log > cp.window_log + 1 {
        cp.hash_log = cp.window_log + 1;
    }
    let cycle_log = cp.chain_log - (cp.strategy >= 6) as u32;
    if cycle_log > cp.window_log {
        cp.chain_log -= cycle_log - cp.window_log;
    }
    cp.window_log = cp.window_log.max(10);
    Some(cp)
}

/// The frame libzstd writes of `src` at `level` (`ZSTD_compress`): None where the level or
/// the size is not emulated.
pub fn compress(src: &[u8], level: i32) -> Option<Vec<u8>> {
    let cp = params(level, src.len())?;
    if src.len() >= 256 << 10 {
        return None;
    }
    let mut out = Vec::with_capacity(src.len() / 2 + 64);
    header(&mut out, &cp, src.len() as u64);
    if src.is_empty() {
        out.extend_from_slice(&[1, 0, 0]);
        return Some(out);
    }
    let mut st = State::new(&cp);
    let block_max = (128usize << 10).min(1 << cp.window_log);
    let mut at = 0;
    let mut first = true;
    let mut body = Vec::new();
    while at < src.len() {
        let end = (at + block_max).min(src.len());
        let last = (end == src.len()) as u32;
        body.clear();
        let len = end - at;
        match st.block(src, at, end, first, &mut body) {
            Coded::Raw => {
                out.extend_from_slice(&le24(last | (len as u32) << 3));
                out.extend_from_slice(&src[at..end]);
            }
            Coded::Rle => {
                out.extend_from_slice(&le24(last | 1 << 1 | (len as u32) << 3));
                out.push(src[at]);
            }
            Coded::Compressed => {
                out.extend_from_slice(&le24(last | 2 << 1 | (body.len() as u32) << 3));
                out.extend_from_slice(&body);
            }
        }
        first = false;
        at = end;
    }
    Some(out)
}

fn le24(v: u32) -> [u8; 3] {
    [v as u8, (v >> 8) as u8, (v >> 16) as u8]
}

fn header(out: &mut Vec<u8>, cp: &Params, size: u64) {
    out.extend_from_slice(&crate::MAGIC);
    let window = 1u64 << cp.window_log;
    let single = window >= size;
    let fcs = (size >= 256) as u8 + (size >= 65536 + 256) as u8 + (size >= 0xFFFF_FFFF) as u8;
    out.push((single as u8) << 5 | fcs << 6);
    if !single {
        out.push(((cp.window_log - 10) << 3) as u8);
    }
    match fcs {
        0 => {
            if single {
                out.push(size as u8)
            }
        }
        1 => out.extend_from_slice(&((size - 256) as u16).to_le_bytes()),
        2 => out.extend_from_slice(&(size as u32).to_le_bytes()),
        _ => out.extend_from_slice(&size.to_le_bytes()),
    }
}

enum Coded {
    Raw,
    Rle,
    Compressed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Repeat {
    None,
    Check,
}

#[derive(Clone)]
struct Entropy {
    huf: Option<HTable>,
    huf_repeat: Repeat,
}

#[derive(Clone, Copy)]
struct Seq {
    ll: u32,
    off_base: u32,
    ml: u32,
}

struct State {
    cp: Params,
    long: Vec<u32>,
    short: Vec<u32>,
    rep: [u32; 3],
    entropy: Entropy,
    seqs: Vec<Seq>,
    lits: Vec<u8>,
}

/// Positions in the window are indices from 2 (`ZSTD_WINDOW_START_INDEX`); 0 is no entry.
const START: u32 = 2;

const PRIME4: u32 = 2654435761;
const PRIME5: u64 = 889523592379;
const PRIME6: u64 = 227718039650203;
const PRIME7: u64 = 58295818150454627;
const PRIME8: u64 = 0xCF1BBCDCB7A56463;

#[inline]
fn r32(s: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(s[p..p + 4].try_into().unwrap())
}

#[inline]
fn r64(s: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(s[p..p + 8].try_into().unwrap())
}

#[inline]
fn hash(s: &[u8], p: usize, bits: u32, mls: u32) -> usize {
    match mls {
        4 => (r32(s, p).wrapping_mul(PRIME4) >> (32 - bits)) as usize,
        5 => ((r64(s, p) << 24).wrapping_mul(PRIME5) >> (64 - bits)) as usize,
        6 => ((r64(s, p) << 16).wrapping_mul(PRIME6) >> (64 - bits)) as usize,
        7 => ((r64(s, p) << 8).wrapping_mul(PRIME7) >> (64 - bits)) as usize,
        _ => (r64(s, p).wrapping_mul(PRIME8) >> (64 - bits)) as usize,
    }
}

#[inline]
fn count(s: &[u8], mut a: usize, mut b: usize, end: usize) -> usize {
    let start = a;
    while a + 8 <= end {
        let x = r64(s, a) ^ r64(s, b);
        if x != 0 {
            return a - start + (x.trailing_zeros() / 8) as usize;
        }
        a += 8;
        b += 8;
    }
    while a < end && s[a] == s[b] {
        a += 1;
        b += 1;
    }
    a - start
}

impl State {
    fn new(cp: &Params) -> Self {
        State {
            cp: *cp,
            long: vec![0; 1 << cp.hash_log],
            short: vec![0; 1 << cp.chain_log],
            rep: [1, 4, 8],
            entropy: Entropy { huf: None, huf_repeat: Repeat::None },
            seqs: Vec::new(),
            lits: Vec::new(),
        }
    }

    /// `ZSTD_compressBlock_internal`.
    fn block(&mut self, src: &[u8], start: usize, end: usize, first: bool, out: &mut Vec<u8>) -> Coded {
        let len = end - start;
        if len < 8 {
            return Coded::Raw;
        }
        self.seqs.clear();
        self.lits.clear();
        let mut rep = self.rep;
        let last = self.dfast(src, start, end, &mut rep);
        self.lits.extend_from_slice(&src[end - last..end]);
        let mut next = self.entropy.clone();
        let ok = self.entropy_code(len, &mut next, out);
        let size = if ok { out.len() } else { 0 };
        if !first && size < 25 && src[start..end].iter().all(|&b| b == src[start]) {
            return Coded::Rle;
        }
        if !ok {
            return Coded::Raw;
        }
        self.rep = rep;
        self.entropy = next;
        Coded::Compressed
    }

    /// `ZSTD_compressBlock_doubleFast_noDict_generic`: the sequences and literals of
    /// `src[start..end]`, the bytes of literals left at the end.
    fn dfast(&mut self, s: &[u8], start: usize, end: usize, rep: &mut [u32; 3]) -> usize {
        let hbl = self.cp.hash_log;
        let hbs = self.cp.chain_log;
        let mls = self.cp.min_match;
        let idx = |p: usize| p as u32 + START;
        let pos = |i: u32| (i - START) as usize;
        let ilimit = end - 8;
        let mut ip = start;
        let mut anchor = start;
        ip += (ip == 0) as usize;
        let (mut o1, mut o2) = (rep[0], rep[1]);
        let (mut saved1, mut saved2) = (0, 0);
        {
            let max_rep = ip as u32;
            if o2 > max_rep {
                saved2 = o2;
                o2 = 0;
            }
            if o1 > max_rep {
                saved1 = o1;
                o1 = 0;
            }
        }
        let seqs = &mut self.seqs;
        let lits = &mut self.lits;
        let long = &mut self.long[..];
        let short = &mut self.short[..];
        let mut store = |ll: usize, anchor: usize, off_base: u32, ml: usize| {
            lits.extend_from_slice(&s[anchor..anchor + ll]);
            seqs.push(Seq { ll: ll as u32, off_base, ml: ml as u32 });
        };
        'outer: loop {
            let mut step = 1;
            let mut next_step = ip + 256;
            let mut ip1 = ip + step;
            if ip1 > ilimit {
                break;
            }
            let mut hl0 = hash(s, ip, hbl, 8);
            let mut idxl0 = long[hl0];
            let curr;
            let mut ml;
            let offset;
            let hl1;
            // the inner loop: one search for each position
            loop {
                let hs0 = hash(s, ip, hbs, mls);
                let idxs0 = short[hs0];
                let c = idx(ip);
                long[hl0] = c;
                short[hs0] = c;
                if o1 > 0 && r32(s, ip + 1 - o1 as usize) == r32(s, ip + 1) {
                    ml = count(s, ip + 5, ip + 5 - o1 as usize, end) + 4;
                    ip += 1;
                    store(ip - anchor, anchor, 1, ml);
                    // _match_stored
                    ip += ml;
                    anchor = ip;
                    self::after_match(
                        s,
                        &mut ip,
                        &mut anchor,
                        ilimit,
                        end,
                        c,
                        &mut o1,
                        &mut o2,
                        long,
                        short,
                        hbl,
                        hbs,
                        mls,
                        &mut store,
                    );
                    continue 'outer;
                }
                let h1 = hash(s, ip1, hbl, 8);
                if idxl0 >= START {
                    let mut m = pos(idxl0);
                    if r64(s, m) == r64(s, ip) {
                        ml = count(s, ip + 8, m + 8, end) + 8;
                        offset = (ip - m) as u32;
                        while ip > anchor && m > 0 && s[ip - 1] == s[m - 1] {
                            ip -= 1;
                            m -= 1;
                            ml += 1;
                        }
                        curr = c;
                        hl1 = h1;
                        break;
                    }
                }
                let idxl1 = long[h1];
                if idxs0 >= START && r32(s, pos(idxs0)) == r32(s, ip) {
                    // _search_next_long
                    let mut m = pos(idxs0);
                    ml = count(s, ip + 4, m + 4, end) + 4;
                    let mut off = (ip - m) as u32;
                    if idxl1 > START && r64(s, pos(idxl1)) == r64(s, ip1) {
                        let ml1 = pos(idxl1);
                        let l1 = count(s, ip1 + 8, ml1 + 8, end) + 8;
                        if l1 > ml {
                            ip = ip1;
                            ml = l1;
                            off = (ip - ml1) as u32;
                            m = ml1;
                        }
                    }
                    while ip > anchor && m > 0 && s[ip - 1] == s[m - 1] {
                        ip -= 1;
                        m -= 1;
                        ml += 1;
                    }
                    offset = off;
                    curr = c;
                    hl1 = h1;
                    break;
                }
                if ip1 >= next_step {
                    step += 1;
                    next_step += 256;
                }
                ip = ip1;
                ip1 += step;
                hl0 = h1;
                idxl0 = idxl1;
                if ip1 > ilimit {
                    break 'outer;
                }
            }
            // _match_found
            o2 = o1;
            o1 = offset;
            if step < 4 {
                long[hl1] = idx(ip1);
            }
            store(ip - anchor, anchor, offset + 3, ml);
            ip += ml;
            anchor = ip;
            self::after_match(
                s,
                &mut ip,
                &mut anchor,
                ilimit,
                end,
                curr,
                &mut o1,
                &mut o2,
                long,
                short,
                hbl,
                hbs,
                mls,
                &mut store,
            );
        }
        saved2 = if saved1 != 0 && o1 != 0 { saved1 } else { saved2 };
        rep[0] = if o1 != 0 { o1 } else { saved1 };
        rep[1] = if o2 != 0 { o2 } else { saved2 };
        end - anchor
    }

    /// `ZSTD_entropyCompressSeqStore`: false where the block is better kept raw.
    fn entropy_code(&mut self, len: usize, next: &mut Entropy, out: &mut Vec<u8>) -> bool {
        let nb_seq = self.seqs.len();
        let suspect = nb_seq == 0 || self.lits.len() / nb_seq >= 20;
        literals(out, &self.lits, &self.entropy, next, suspect);
        if nb_seq < 128 {
            out.push(nb_seq as u8);
        } else if nb_seq < 0x7F00 {
            out.push(((nb_seq >> 8) + 0x80) as u8);
            out.push(nb_seq as u8);
        } else {
            out.push(0xFF);
            out.extend_from_slice(&((nb_seq - 0x7F00) as u16).to_le_bytes());
        }
        if nb_seq > 0 && sequences(out, &self.seqs).is_none() {
            return false;
        }
        let max = len - ((len >> 6) + 2);
        out.len() < max
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn after_match(
    s: &[u8],
    ip: &mut usize,
    anchor: &mut usize,
    ilimit: usize,
    end: usize,
    curr: u32,
    o1: &mut u32,
    o2: &mut u32,
    long: &mut [u32],
    short: &mut [u32],
    hbl: u32,
    hbs: u32,
    mls: u32,
    store: &mut impl FnMut(usize, usize, u32, usize),
) {
    if *ip > ilimit {
        return;
    }
    let ins = curr + 2;
    let ins_p = (ins - START) as usize;
    long[hash(s, ins_p, hbl, 8)] = ins;
    long[hash(s, *ip - 2, hbl, 8)] = (*ip - 2) as u32 + START;
    short[hash(s, ins_p, hbs, mls)] = ins;
    short[hash(s, *ip - 1, hbs, mls)] = (*ip - 1) as u32 + START;
    while *ip <= ilimit && *o2 > 0 && r32(s, *ip) == r32(s, *ip - *o2 as usize) {
        let rl = count(s, *ip + 4, *ip + 4 - *o2 as usize, end) + 4;
        std::mem::swap(o1, o2);
        short[hash(s, *ip, hbs, mls)] = *ip as u32 + START;
        long[hash(s, *ip, hbl, 8)] = *ip as u32 + START;
        store(0, *anchor, 1, rl);
        *ip += rl;
        *anchor = *ip;
    }
}

/// `ZSTD_compressLiterals` at the double-fast strategy.
fn literals(out: &mut Vec<u8>, lits: &[u8], prev: &Entropy, next: &mut Entropy, suspect: bool) {
    let n = lits.len();
    let lh = 3 + (n >= 1024) as usize + (n >= 16 << 10) as usize;
    let single = n < 256;
    next.huf = prev.huf.clone();
    next.huf_repeat = prev.huf_repeat;
    if n < 64 {
        return raw_literals(out, lits);
    }
    let prefer_repeat = n <= 1024;
    let start = out.len();
    out.extend_from_slice(&[0; 5][..lh]);
    let at = out.len();
    let mut repeat = prev.huf_repeat;
    let c = huf_compress(out, lits, !single, &mut next.huf, &mut repeat, prefer_repeat, suspect);
    let h_type = if repeat != Repeat::None { 3 } else { 2 };
    let min_gain = (n >> 6) + 2;
    if c == 0 || c >= n - min_gain {
        out.truncate(start);
        next.huf = prev.huf.clone();
        next.huf_repeat = prev.huf_repeat;
        return raw_literals(out, lits);
    }
    if c == 1 && (n >= 8 || lits.iter().all(|&b| b == lits[0])) {
        out.truncate(start);
        next.huf = prev.huf.clone();
        next.huf_repeat = prev.huf_repeat;
        return rle_literals(out, lits);
    }
    if h_type == 2 {
        next.huf_repeat = Repeat::Check;
    }
    debug_assert_eq!(out.len() - at, c);
    let (n, c) = (n as u32, c as u32);
    match lh {
        3 => {
            let v = h_type + ((!single as u32) << 2) + (n << 4) + (c << 14);
            out[start..start + 3].copy_from_slice(&le24(v));
        }
        4 => {
            let v = h_type + (2 << 2) + (n << 4) + (c << 18);
            out[start..start + 4].copy_from_slice(&v.to_le_bytes());
        }
        _ => {
            let v = h_type + (3 << 2) + (n << 4) + (c << 22);
            out[start..start + 4].copy_from_slice(&v.to_le_bytes());
            out[start + 4] = (c >> 10) as u8;
        }
    }
}

fn raw_literals(out: &mut Vec<u8>, lits: &[u8]) {
    lit_header(out, 0, lits.len());
    out.extend_from_slice(lits);
}

fn rle_literals(out: &mut Vec<u8>, lits: &[u8]) {
    lit_header(out, 1, lits.len());
    out.push(lits[0]);
}

fn lit_header(out: &mut Vec<u8>, t: u32, n: usize) {
    let n = n as u32;
    if n <= 31 {
        out.push((t + (n << 3)) as u8);
    } else if n <= 4095 {
        out.extend_from_slice(&((t + (1 << 2) + (n << 4)) as u16).to_le_bytes());
    } else {
        out.extend_from_slice(&le24(t + (3 << 2) + (n << 4)));
    }
}

/// `HUF_compress_internal`: the literals' bytes at the end of `out` (the table's
/// description, then the streams), their count, or 0 (not compressed: nothing appended) or
/// 1 (a single symbol: its byte appended).
fn huf_compress(
    out: &mut Vec<u8>,
    src: &[u8],
    four: bool,
    old: &mut Option<HTable>,
    repeat: &mut Repeat,
    prefer_repeat: bool,
    suspect: bool,
) -> usize {
    let n = src.len();
    let start = out.len();
    let mut count = [0u32; 256];
    if suspect && n >= 4096 * 10 {
        let (_, b) = fse::histogram(&mut count, &src[..4096]);
        let (_, e) = fse::histogram(&mut count, &src[n - 4096..]);
        if (b + e) as usize <= ((2 * 4096) >> 7) + 4 {
            return 0;
        }
    }
    let (max, largest) = fse::histogram(&mut count, src);
    if largest as usize == n {
        out.push(src[0]);
        return 1;
    }
    if largest as usize <= (n >> 7) + 4 {
        return 0;
    }
    if *repeat == Repeat::Check && !old.as_ref().is_some_and(|t| huf::valid(t, &count, max)) {
        *repeat = Repeat::None;
    }
    if prefer_repeat && *repeat != Repeat::None {
        return huf::compress_with(out, start, src, four, old.as_ref().unwrap());
    }
    let log = fse::optimal_table_log(huf::LIT_LOG, n, max, 1);
    let t = huf::build(&count, max, log);
    let mut desc = Vec::new();
    let Some(hsize) = huf::write_table(&mut desc, &t) else { return 0 };
    if *repeat != Repeat::None {
        let o = old.as_ref().unwrap();
        let old_size = huf::estimate(o, &count, max);
        let new_size = huf::estimate(&t, &count, max);
        if old_size <= hsize + new_size || hsize + 12 >= n {
            return huf::compress_with(out, start, src, four, o);
        }
    }
    if hsize + 12 >= n {
        return 0;
    }
    out.extend_from_slice(&desc);
    *repeat = Repeat::None;
    let c = huf::compress_with(out, start, src, four, &t);
    *old = Some(t);
    c
}

const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 128, 256, 512, 1024,
    2048, 4096, 8192, 16384, 32768, 65536,
];
const LL_BITS: [u8; 36] =
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027, 2051, 4099, 8195, 16387, 32771, 65539,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2,
    3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const LL_NORM: [i16; 36] =
    [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
const ML_NORM: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_NORM: [i16; 29] = [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];

fn code_of(base: &[u32], v: u32) -> u8 {
    (base.partition_point(|&b| b <= v) - 1) as u8
}

/// What a kind of code (literal lengths, offsets, match lengths) is coded by.
struct Kind {
    log: u32,
    max: usize,
    default_norm: &'static [i16],
    default_log: u32,
    /// The largest code the default table has.
    default_max: usize,
}

const LL: Kind = Kind { log: 9, max: 35, default_norm: &LL_NORM, default_log: 6, default_max: 35 };
const OF: Kind = Kind { log: 8, max: 31, default_norm: &OF_NORM, default_log: 5, default_max: 28 };
const ML: Kind = Kind { log: 9, max: 52, default_norm: &ML_NORM, default_log: 6, default_max: 52 };

/// The table of `codes` (`ZSTD_selectEncodingType` at the double-fast strategy, then
/// `ZSTD_buildCTable`), its description appended to `out`: (type, table, the description's
/// bytes where it is of type compressed).
fn table(out: &mut Vec<u8>, codes: &[u8], k: &Kind) -> (u8, CTable, usize) {
    let nb_seq = codes.len();
    let mut count = vec![0u32; k.max + 1];
    for &c in codes {
        count[c as usize] += 1;
    }
    let mut max = k.max;
    while count[max] == 0 {
        max -= 1;
    }
    let most = *count.iter().max().unwrap() as usize;
    let default_allowed = max <= k.default_max;
    if most == nb_seq {
        if default_allowed && nb_seq <= 2 {
            return (0, CTable::new(&k.default_norm[..=k.default_max], k.default_log), 0);
        }
        out.push(codes[0]);
        return (1, CTable::rle(max as u8), 0);
    }
    if default_allowed {
        let dyn_min = ((1usize << k.default_log) * 8) >> 3;
        if nb_seq < dyn_min || most < (nb_seq >> (k.default_log - 1)) {
            return (0, CTable::new(&k.default_norm[..=k.default_max], k.default_log), 0);
        }
    }
    let log = fse::optimal_table_log(k.log, nb_seq, max as u32, 2);
    let mut total = nb_seq;
    let last = codes[nb_seq - 1] as usize;
    if count[last] > 1 {
        count[last] -= 1;
        total -= 1;
    }
    let norm = fse::normalize(&count[..=max], total, log, total >= 2048).expect("normalised");
    let n = fse::write_ncount(out, &norm, log);
    (2, CTable::new(&norm, log), n)
}

/// The sequences section after its count (`ZSTD_buildSequencesStatistics`,
/// `ZSTD_encodeSequences`): None where the block is kept raw for old decoders' sake.
fn sequences(out: &mut Vec<u8>, seqs: &[Seq]) -> Option<()> {
    let n = seqs.len();
    let llc: Vec<u8> = seqs.iter().map(|q| code_of(&LL_BASE, q.ll)).collect();
    let mlc: Vec<u8> = seqs.iter().map(|q| code_of(&ML_BASE, q.ml)).collect();
    let ofc: Vec<u8> = seqs.iter().map(|q| highbit(q.off_base) as u8).collect();
    let head = out.len();
    out.push(0);
    let (llt, ll_t, ll_n) = table(out, &llc, &LL);
    let (oft, of_t, of_n) = table(out, &ofc, &OF);
    let (mlt, ml_t, ml_n) = table(out, &mlc, &ML);
    out[head] = (llt << 6) + (oft << 4) + (mlt << 2);
    let last_count =
        [(llt, ll_n), (oft, of_n), (mlt, ml_n)].iter().filter(|t| t.0 == 2).map(|t| t.1).next_back().unwrap_or(0);
    let mut bits = Bits::new(out);
    let l = n - 1;
    let mut ml_s = ml_t.init(mlc[l]);
    let mut of_s = of_t.init(ofc[l]);
    let mut ll_s = ll_t.init(llc[l]);
    let extra = |bits: &mut Bits, i: usize| {
        let q = &seqs[i];
        bits.add(q.ll - LL_BASE[llc[i] as usize], LL_BITS[llc[i] as usize] as u32);
        bits.add(q.ml - ML_BASE[mlc[i] as usize], ML_BITS[mlc[i] as usize] as u32);
        bits.add(q.off_base, ofc[i] as u32);
    };
    extra(&mut bits, l);
    for i in (0..l).rev() {
        of_t.encode(&mut bits, &mut of_s, ofc[i]);
        ml_t.encode(&mut bits, &mut ml_s, mlc[i]);
        ll_t.encode(&mut bits, &mut ll_s, llc[i]);
        extra(&mut bits, i);
    }
    ml_t.flush(&mut bits, ml_s);
    of_t.flush(&mut bits, of_s);
    ll_t.flush(&mut bits, ll_s);
    let size = bits.close();
    if last_count != 0 && last_count + size < 4 {
        return None;
    }
    Some(())
}
