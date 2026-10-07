//! Multi-symbol rANS with static per-context frequency tables: 64-bit state, 32-bit
//! renormalisation, four interleaved states (symbol j uses state j % 4).

pub const PROB_BITS: u32 = 12;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;
const RANS_L: u64 = 1 << 31;
/// Interleaved states: symbol j uses state j % LANES.
const LANES: usize = 8;
const _: () = assert!(PROB_BITS == 12, "alias entries hold 12-bit frequencies and ranks");

/// Frequency tables of all contexts, as alias tables (one bucket per symbol of the
/// alphabet rounded up to a power of two, at least 4): bucket `i` covers `B` slots, the
/// first `cutoff` of them belong to symbol `i`, the rest to its alias. Within a symbol,
/// ranks count its own bucket's slots first, then its alias slots in bucket order.
pub struct Tables {
    /// Context `c` has alphabet `n[c]`, its symbols at `off[c]..off[c] + n[c]` of `enc`.
    pub off: Vec<u32>,
    pub n: Vec<u16>,
    /// Per context: first bucket in `alias` << 4 | log2 of the bucket size.
    meta: Vec<u32>,
    /// `cutoff | (f_own - 1) << 11 | alias << 23 | (f_alias - 1) << 31 | (rank_start - cutoff) << 43`
    /// (12-bit fields wrap).
    alias: Vec<u64>,
    /// Encoder, by symbol: `freq | rcp_shift << 13 | slot_base << 17`, the symbol's slots
    /// by rank at `inv[slot_base..slot_base + freq]`.
    enc: Vec<u64>,
    /// Reciprocals (see [`reciprocal`]) by frequency.
    rcp: Vec<u64>,
    inv: Vec<u16>,
}

/// Scales `counts` to frequencies summing to `PROB_SCALE`, each at least 1.
pub fn normalize(counts: &[u64], out: &mut [u16]) {
    let n = counts.len();
    assert!(n >= 1 && n <= 256);
    let total: u64 = counts.iter().sum();
    if total == 0 {
        for (i, o) in out.iter_mut().enumerate() {
            *o = (PROB_SCALE as usize / n + (i < PROB_SCALE as usize % n) as usize) as u16;
        }
        return;
    }
    let spare = PROB_SCALE as u64 - n as u64;
    let mut sum = 0u32;
    for (i, &c) in counts.iter().enumerate() {
        out[i] = (1 + c * spare / total) as u16;
        sum += out[i] as u32;
    }
    // Hand out the rounding remainder to the largest counts, one each in turn.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| counts[b].cmp(&counts[a]).then(a.cmp(&b)));
    let mut k = 0;
    while sum < PROB_SCALE {
        out[order[k % n]] += 1;
        sum += 1;
        k += 1;
    }
}

impl Tables {
    /// `freqs[c]` must sum to `PROB_SCALE` for each context, with every symbol at least 1.
    pub fn new(freqs: &[Vec<u16>]) -> Tables {
        let total: usize = freqs.iter().map(|f| f.len()).sum();
        let mut off = Vec::with_capacity(freqs.len());
        let mut n = Vec::with_capacity(freqs.len());
        let mut meta = Vec::with_capacity(freqs.len());
        let mut alias = Vec::new();
        let mut enc = vec![0u64; total];
        let rcp: Vec<u64> = (0..=PROB_SCALE as u64).map(|f| reciprocal(f).0).collect();
        let mut inv = vec![0u16; freqs.len() * PROB_SCALE as usize];
        let mut o = 0usize;
        for (c, f) in freqs.iter().enumerate() {
            assert!(f.len() <= 256);
            off.push(o as u32);
            n.push(f.len() as u16);
            let a = f.len().next_power_of_two().max(4);
            let bs = PROB_SCALE as usize / a;
            meta.push((alias.len() as u32) << 4 | bs.trailing_zeros());
            // Vose's construction.
            let mut rem: Vec<usize> = (0..a).map(|i| f.get(i).map_or(0, |&x| x as usize)).collect();
            assert_eq!(rem.iter().sum::<usize>(), PROB_SCALE as usize);
            let mut cutoff = vec![bs; a];
            let mut al = vec![0usize; a];
            let mut small: Vec<usize> = (0..a).filter(|&i| rem[i] < bs).collect();
            let mut large: Vec<usize> = (0..a).filter(|&i| rem[i] > bs).collect();
            while let (Some(&s), Some(&l)) = (small.last(), large.last()) {
                small.pop();
                cutoff[s] = rem[s];
                al[s] = l;
                rem[l] -= bs - rem[s];
                if rem[l] < bs {
                    large.pop();
                    small.push(l);
                } else if rem[l] == bs {
                    large.pop();
                }
            }
            // Ranks: own slots first, then alias slots in bucket order.
            let mut next_rank: Vec<usize> = (0..a).map(|i| cutoff[i]).collect();
            let fr = |i: usize| f.get(i).map_or(0, |&x| x as usize);
            let mut cum = 0usize;
            let mut base = vec![0usize; a];
            for i in 0..a {
                base[i] = cum;
                cum += fr(i);
            }
            let ib = c * PROB_SCALE as usize;
            for i in 0..a {
                for j in 0..cutoff[i] {
                    inv[ib + base[i] + j] = (i * bs + j) as u16;
                }
            }
            for i in 0..a {
                let (cu, l) = (cutoff[i], al[i]);
                let rank_start = if cu < bs { next_rank[l] } else { 0 };
                if cu < bs {
                    for j in 0..bs - cu {
                        inv[ib + base[l] + rank_start + j] = (i * bs + cu + j) as u16;
                    }
                    next_rank[l] += bs - cu;
                }
                let f_own = (fr(i) as u64).wrapping_sub(1) & 0xfff;
                let f_al = if cu < bs { (fr(l) as u64 - 1) & 0xfff } else { 0 };
                let roff = (rank_start as u64).wrapping_sub(cu as u64) & 0xfff;
                alias.push(cu as u64 | f_own << 11 | (if cu < bs { l as u64 } else { 0 }) << 23 | f_al << 31 | roff << 43);
            }
            for i in 0..a {
                if i < f.len() {
                    debug_assert_eq!(next_rank[i], fr(i));
                    let (_, sh) = reciprocal(fr(i) as u64);
                    enc[o + i] = fr(i) as u64 | (sh as u64) << 13 | ((ib + base[i]) as u64) << 17;
                }
            }
            o += f.len();
        }
        Tables { off, n, meta, alias, enc, rcp, inv }
    }
}

/// Reciprocal of `freq` for division of rANS states (x < 2^63) by multiplication, as in
/// ryg_rans' rans64: `x / freq == mulhi(x, rcp) >> shift`. For `freq == 1` the result is 0
/// and the caller divides by 1 itself.
pub fn reciprocal(freq: u64) -> (u64, u32) {
    if freq < 2 {
        return (0, 0);
    }
    let mut shift = 0;
    while freq > 1u64 << shift {
        shift += 1;
    }
    let x1 = 1u64 << (shift + 31);
    let t1 = x1 / freq;
    let t0 = (freq - 1 + ((x1 % freq) << 32)) / freq;
    (t0.wrapping_add(t1 << 32), shift - 1)
}

#[inline(always)]
fn div_rcp(x: u64, freq: u64, rcp: u64, shift: u32) -> u64 {
    let q = ((x as u128 * rcp as u128) >> 64) as u64 >> shift;
    if freq == 1 { x } else { q }
}

/// Collects symbols (as indices into `Tables::enc`) and encodes them at the end.
#[derive(Default)]
pub struct Encoder {
    syms: Vec<u32>,
}

impl Encoder {
    pub fn with_capacity(n: usize) -> Self {
        Encoder { syms: Vec::with_capacity(n) }
    }

    /// Makes room for `n` more symbols.
    #[inline(always)]
    pub fn reserve(&mut self, n: usize) {
        self.syms.reserve(n);
    }

    /// Adds a symbol; [`Encoder::reserve`] must have made room for it.
    #[inline(always)]
    pub fn put(&mut self, t: &Tables, ctx: usize, sym: u32) {
        debug_assert!(ctx < t.n.len());
        debug_assert!(sym < t.n[ctx] as u32);
        assert!(self.syms.len() < self.syms.capacity());
        // SAFETY: ctx < number of contexts (checked in debug builds); room was reserved.
        unsafe {
            let n = self.syms.len();
            *self.syms.as_mut_ptr().add(n) = *t.off.get_unchecked(ctx) + sym;
            self.syms.set_len(n + 1);
        }
    }

    pub fn len(&self) -> usize {
        self.syms.len()
    }

    pub fn truncate(&mut self, n: usize) {
        self.syms.truncate(n);
    }

    /// Encodes all symbols; the words come out in decoding order.
    #[inline(never)]
    pub fn finish(&self, t: &Tables) -> Vec<u32> {
        let n = self.syms.len();
        let cap = n + 2 * LANES;
        let mut out = vec![0u32; cap];
        let mut w = cap;
        let mut x = [RANS_L; LANES];
        let o = out.as_mut_ptr();
        let enc = t.enc.as_ptr();
        let inv = t.inv.as_ptr();
        let rcp = t.rcp.as_ptr();
        // SAFETY: symbol indices come from `put` (below the table size), slots of a symbol
        // lie within `inv`, and each symbol writes at most one word, so `w` stays >= 2 * LANES.
        #[inline(always)]
        unsafe fn put(x: &mut u64, (e, rc): (u64, u64), inv: *const u16, o: *mut u32, w: &mut usize) {
            let freq = e & 0x1fff;
            let xmax = ((RANS_L >> PROB_BITS) << 32) * freq;
            // Branchless renormalisation: the word is always stored, and kept if needed.
            let big = *x >= xmax;
            unsafe { *o.add(*w - 1) = *x as u32 };
            *w -= big as usize;
            *x = std::hint::select_unpredictable(big, *x >> 32, *x);
            let q = div_rcp(*x, freq, rc, (e >> 13 & 15) as u32);
            let r = *x - q * freq;
            let slot = unsafe { *inv.add((e >> 17) as usize + r as usize) } as u64;
            *x = (q << PROB_BITS) + slot;
        }
        let full = n - n % LANES;
        unsafe {
            let e = |j: usize| {
                let e = *enc.add(*self.syms.get_unchecked(j) as usize);
                (e, *rcp.add((e & 0x1fff) as usize))
            };
            for j in (full..n).rev() {
                put(&mut x[j % LANES], e(j), inv, o, &mut w);
            }
            let mut j = full;
            while j > 0 {
                j -= LANES;
                for k in (0..LANES).rev() {
                    put(&mut x[k], e(j + k), inv, o, &mut w);
                }
            }
        }
        for k in (0..LANES).rev() {
            w -= 2;
            out[w] = (x[k] >> 32) as u32;
            out[w + 1] = x[k] as u32;
        }
        out.drain(..w);
        out
    }
}

/// Words of zero padding the decoder may read past the end.
pub const SRC_PAD: usize = 64;

pub struct Decoder<'a> {
    x: [u64; LANES],
    src: Vec<u32>,
    pos: usize,
    /// Last valid index of `src`.
    src_max: usize,
    pub t: &'a Tables,
    real_len: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(t: &'a Tables, words: &[u32]) -> Decoder<'a> {
        let mut src = Vec::with_capacity(words.len() + SRC_PAD);
        src.extend_from_slice(words);
        src.resize(words.len().max(2 * LANES) + SRC_PAD, 0);
        let mut x = [0u64; LANES];
        for k in 0..LANES {
            x[k] = (src[2 * k] as u64) << 32 | src[2 * k + 1] as u64;
        }
        let src_max = src.len() - 1;
        Decoder { x, src, pos: 2 * LANES, src_max, t, real_len: words.len() }
    }

    /// True if the decoder has not read past the encoded words.
    pub fn in_bounds(&self) -> bool {
        self.pos <= self.real_len
    }

    /// Decodes a symbol in context `ctx`, which must be below the number of contexts.
    #[inline(always)]
    pub fn get(&mut self, ctx: usize) -> u32 {
        let x = self.x[0];
        let slot = (x & (PROB_SCALE as u64 - 1)) as u32;
        debug_assert!(ctx < self.t.n.len());
        // SAFETY: ctx < number of contexts; slot >> sh is below the context's bucket count.
        let m = unsafe { *self.t.meta.get_unchecked(ctx) };
        let sh = m & 15;
        let i = slot >> sh;
        let pos = slot & ((1 << sh) - 1);
        let e = unsafe { *self.t.alias.get_unchecked((m >> 4) as usize + i as usize) };
        let own = pos < (e & 0x7ff) as u32;
        let sym = std::hint::select_unpredictable(own, i, (e >> 23) as u32 & 0xff);
        let f = std::hint::select_unpredictable(own, e >> 11, e >> 31) & 0xfff;
        let j = std::hint::select_unpredictable(own, pos as u64, (pos as u64 + (e >> 43)) & 0xfff);
        let x = (f + 1) * (x >> PROB_BITS) + j;
        // SAFETY: the position is clamped to the buffer.
        let next = unsafe { *self.src.get_unchecked(self.pos.min(self.src_max)) } as u64;
        let low = x < RANS_L;
        let x = std::hint::select_unpredictable(low, (x << 32) | next, x);
        self.pos += low as usize;
        self.x.rotate_left(1);
        self.x[LANES - 1] = x;
        sym
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reciprocals() {
        let mut seed = 7u64;
        for f in 1..=PROB_SCALE as u64 {
            let (r, sh) = reciprocal(f);
            for k in 0..200 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let x = match k {
                    0 => RANS_L,
                    1 => (1 << 63) - 1,
                    _ => RANS_L + seed % ((1 << 63) - RANS_L),
                };
                assert_eq!(div_rcp(x, f, r, sh), x / f, "x={x} f={f}");
            }
        }
    }

    #[test]
    fn roundtrip() {
        let mut freqs = Vec::new();
        for n in [2usize, 5, 16, 200, 256] {
            let counts: Vec<u64> = (0..n as u64).map(|i| (i * 7919) % 101 + if i == 0 { 5000 } else { 0 }).collect();
            let mut f = vec![0u16; n];
            normalize(&counts, &mut f);
            freqs.push(f);
        }
        let t = Tables::new(&freqs);
        let mut seed = 12345u64;
        let mut syms = Vec::new();
        let mut e = Encoder::with_capacity(100000);
        for _ in 0..100000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let c = (seed >> 60) as usize % freqs.len();
            let s = ((seed >> 20) % t.n[c] as u64) as u32;
            let s = if seed & 1 == 0 { 0 } else { s };
            syms.push((c, s));
            e.put(&t, c, s);
        }
        let words = e.finish(&t);
        let mut d = Decoder::new(&t, &words);
        for &(c, s) in &syms {
            assert_eq!(d.get(c), s);
        }
        assert!(d.in_bounds());
    }
}
