//! Multi-symbol rANS with static per-context frequency tables: 64-bit state, 32-bit
//! renormalisation, four interleaved states (symbol j uses state j % 4).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

pub const PROB_BITS: u32 = 12;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;
const RANS_L: u64 = 1 << 31;
/// Interleaved states: symbol j uses state j % LANES.
const LANES: usize = 8;
const _: () = assert!(PROB_BITS == 12, "alias entries hold 12-bit frequencies and ranks");

/// Frequency tables of all contexts. The decoder uses alias tables (one bucket per symbol
/// of the alphabet rounded up to a power of two, at least 4): bucket `i` covers `B` slots,
/// the first `cutoff` of them belong to symbol `i`, the rest to its alias. Within a symbol,
/// ranks count its own bucket's slots first, then its alias slots in bucket order. The
/// encoder maps ranks back to slots with `inv`, built per context on first use.
pub struct Tables {
    /// Context `c` has alphabet `n[c]`, its symbols at `off[c]..off[c] + n[c]`.
    pub off: Vec<u32>,
    pub n: Vec<u16>,
    freqs: Vec<u16>,
    /// The first context with the same frequencies; contexts share tables with it.
    canon: Vec<u32>,
    dec: OnceLock<DecTables>,
    /// Encoder, by symbol: `freq | rcp_shift << 13 | slot_base << 17`, the symbol's slots
    /// by rank at `inv[slot_base..slot_base + freq]`.
    enc: Vec<u64>,
    /// Reciprocals (see [`reciprocal`]) by frequency.
    rcp: Vec<u64>,
    /// `PROB_SCALE` entries per canonical context, filled by `ensure_inv` under `inv_lock`
    /// before `inv_ready` is set; never reallocated.
    inv: std::cell::UnsafeCell<Vec<u16>>,
    inv_ready: Vec<AtomicBool>,
    inv_lock: Mutex<()>,
}

// SAFETY: `inv` is only written under `inv_lock` for contexts whose `inv_ready` flag is not
// yet set, and only read for contexts whose flag is set (with acquire/release ordering).
unsafe impl Sync for Tables {}

struct DecTables {
    /// Per context: first bucket in `alias` << 4 | log2 of the bucket size.
    meta: Vec<u32>,
    /// `cutoff | (f_own - 1) << 11 | alias << 23 | (f_alias - 1) << 31 | (rank_start - cutoff) << 43`
    /// (12-bit fields wrap).
    alias: Vec<u64>,
}

/// Alias table of one context: per bucket, the cutoff, the alias, and where the alias's
/// ranks in this bucket start.
struct Alias {
    a: usize,
    bs: usize,
    cutoff: [u16; 256],
    alias: [u8; 256],
    rank_start: [u16; 256],
}

fn build_alias(f: &[u16]) -> Alias {
    let a = f.len().next_power_of_two().max(4);
    let bs = PROB_SCALE as usize / a;
    let mut rem = [0u32; 256];
    for (r, &x) in rem.iter_mut().zip(f) {
        *r = x as u32;
    }
    let mut al = Alias { a, bs, cutoff: [bs as u16; 256], alias: [0; 256], rank_start: [0; 256] };
    // Vose's construction.
    let (mut small, mut large) = ([0u16; 256], [0u16; 256]);
    let (mut ns, mut nl) = (0, 0);
    for i in 0..a {
        if (rem[i] as usize) < bs {
            small[ns] = i as u16;
            ns += 1;
        } else if (rem[i] as usize) > bs {
            large[nl] = i as u16;
            nl += 1;
        }
    }
    while ns > 0 && nl > 0 {
        ns -= 1;
        let s = small[ns] as usize;
        let l = large[nl - 1] as usize;
        al.cutoff[s] = rem[s] as u16;
        al.alias[s] = l as u8;
        rem[l] -= (bs as u32) - rem[s];
        if (rem[l] as usize) < bs {
            nl -= 1;
            small[ns] = l as u16;
            ns += 1;
        } else if rem[l] as usize == bs {
            nl -= 1;
        }
    }
    // Ranks: own slots first, then alias slots in bucket order.
    let mut next = [0u16; 256];
    for i in 0..a {
        next[i] = al.cutoff[i];
    }
    for i in 0..a {
        if (al.cutoff[i] as usize) < bs {
            let l = al.alias[i] as usize;
            al.rank_start[i] = next[l];
            next[l] += (bs - al.cutoff[i] as usize) as u16;
        }
    }
    al
}

impl Tables {
    /// `freqs[c]` must sum to `PROB_SCALE` for each context, with every symbol at least 1.
    pub fn new(freqs: &[Vec<u16>]) -> Tables {
        let total: usize = freqs.iter().map(|f| f.len()).sum();
        let mut off = Vec::with_capacity(freqs.len());
        let mut n = Vec::with_capacity(freqs.len());
        let mut flat = Vec::with_capacity(total);
        let mut canon = Vec::with_capacity(freqs.len());
        let mut seen: std::collections::HashMap<&[u16], u32> = Default::default();
        let mut enc = vec![0u64; total];
        for (c, f) in freqs.iter().enumerate() {
            assert!(!f.is_empty() && f.len() <= 256);
            assert_eq!(f.iter().map(|&x| x as u32).sum::<u32>(), PROB_SCALE);
            let o = flat.len();
            off.push(o as u32);
            n.push(f.len() as u16);
            flat.extend_from_slice(f);
            let k = *seen.entry(&f[..]).or_insert(c as u32);
            canon.push(k);
            let mut cum = 0u64;
            for (i, &fr) in f.iter().enumerate() {
                assert!(fr >= 1);
                let (_, sh) = reciprocal(fr as u64);
                enc[o + i] = fr as u64 | (sh as u64) << 13 | (k as u64 * PROB_SCALE as u64 + cum) << 17;
                cum += fr as u64;
            }
        }
        let rcp: Vec<u64> = (0..=PROB_SCALE as u64).map(|f| reciprocal(f).0).collect();
        // Zeroed allocation: pages are only touched for the contexts used.
        let inv = std::cell::UnsafeCell::new(vec![0u16; freqs.len() * PROB_SCALE as usize]);
        let inv_ready = (0..freqs.len()).map(|_| AtomicBool::new(false)).collect();
        Tables { off, n, freqs: flat, canon, dec: OnceLock::new(), enc, rcp, inv, inv_ready, inv_lock: Mutex::new(()) }
    }

    fn freqs(&self, c: usize) -> &[u16] {
        let o = self.off[c] as usize;
        &self.freqs[o..o + self.n[c] as usize]
    }

    fn dec(&self) -> &DecTables {
        self.dec.get_or_init(|| {
            let mut meta = vec![0u32; self.n.len()];
            let mut alias = Vec::new();
            for c in 0..self.n.len() {
                let k = self.canon[c] as usize;
                if k != c {
                    meta[c] = meta[k];
                    continue;
                }
                let f = self.freqs(c);
                let al = build_alias(f);
                meta[c] = (alias.len() as u32) << 4 | al.bs.trailing_zeros();
                let fr = |i: usize| f.get(i).map_or(0, |&x| x as u64);
                for i in 0..al.a {
                    let cu = al.cutoff[i] as u64;
                    let has = (cu as usize) < al.bs;
                    let l = al.alias[i] as usize;
                    let f_own = fr(i).wrapping_sub(1) & 0xfff;
                    let f_al = if has { (fr(l) - 1) & 0xfff } else { 0 };
                    let roff = if has { (al.rank_start[i] as u64).wrapping_sub(cu) & 0xfff } else { 0 };
                    alias.push(cu | f_own << 11 | (if has { l as u64 } else { 0 }) << 23 | f_al << 31 | roff << 43);
                }
            }
            DecTables { meta, alias }
        })
    }

    /// Makes `inv` available for context `c`.
    fn ensure_inv(&self, c: usize) {
        let k = self.canon[c] as usize;
        if self.inv_ready[k].load(Ordering::Acquire) {
            return;
        }
        let _g = self.inv_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.inv_ready[k].load(Ordering::Relaxed) {
            return;
        }
        let f = self.freqs(k);
        let al = build_alias(f);
        let mut base = [0usize; 256];
        let mut cum = 0;
        for i in 0..f.len() {
            base[i] = k * PROB_SCALE as usize + cum;
            cum += f[i] as usize;
        }
        // SAFETY: entries of context k are written only here, under the lock, before the
        // flag is released.
        let p = unsafe { (*self.inv.get()).as_mut_ptr() };
        let put = |i: usize, v: usize| unsafe { *p.add(i) = v as u16 };
        for i in 0..al.a {
            let cu = al.cutoff[i] as usize;
            if i < f.len() {
                for j in 0..cu {
                    put(base[i] + j, i * al.bs + j);
                }
            }
            if cu < al.bs {
                let l = al.alias[i] as usize;
                for j in 0..al.bs - cu {
                    put(base[l] + al.rank_start[i] as usize + j, i * al.bs + cu + j);
                }
            }
        }
        self.inv_ready[k].store(true, Ordering::Release);
    }
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
    /// Contexts used, one bit each.
    used: Vec<u64>,
}

impl Encoder {
    pub fn with_capacity(n: usize) -> Self {
        Encoder { syms: Vec::with_capacity(n), used: Vec::new() }
    }

    /// Makes room for `n` more symbols.
    #[inline(always)]
    pub fn reserve(&mut self, n: usize) {
        self.syms.reserve(n);
    }

    /// The hot state; [`Encoder::reserve`] must have made room for the symbols added
    /// through it before [`Encoder::put_back`].
    #[inline(always)]
    pub fn local(&mut self, t: &Tables) -> EncLocal {
        if self.used.is_empty() {
            self.used = vec![0; t.n.len().div_ceil(64)];
        }
        EncLocal {
            used: self.used.as_mut_ptr(),
            syms: self.syms.as_mut_ptr(),
            len: self.syms.len(),
            room: self.syms.capacity() - self.syms.len(),
            off: t.off.as_ptr(),
            #[cfg(debug_assertions)]
            n_ctx: t.n.len(),
        }
    }

    #[inline(always)]
    pub fn put_back(&mut self, l: EncLocal) {
        assert!(l.len <= self.syms.capacity());
        // SAFETY: the symbols up to l.len were written through `l`.
        unsafe { self.syms.set_len(l.len) };
    }

    /// Adds a symbol.
    pub fn put(&mut self, t: &Tables, ctx: usize, sym: u32) {
        self.reserve(1);
        let mut l = self.local(t);
        l.put(ctx, sym);
        self.put_back(l);
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
        for (w, &bits) in self.used.iter().enumerate() {
            let mut b = bits;
            while b != 0 {
                t.ensure_inv(w * 64 + b.trailing_zeros() as usize);
                b &= b - 1;
            }
        }
        let n = self.syms.len();
        let cap = n + 2 * LANES;
        let mut out = vec![0u32; cap];
        let mut w = cap;
        let mut x = [RANS_L; LANES];
        let o = out.as_mut_ptr();
        let enc = t.enc.as_ptr();
        // SAFETY: the contexts used are ready (above) and no longer written.
        let inv = unsafe { (*t.inv.get()).as_ptr() };
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

/// Encoder state by value, so that it stays in registers.
#[derive(Clone, Copy)]
pub struct EncLocal {
    used: *mut u64,
    syms: *mut u32,
    len: usize,
    room: usize,
    off: *const u32,
    #[cfg(debug_assertions)]
    n_ctx: usize,
}

impl EncLocal {
    #[inline(always)]
    pub fn put(&mut self, ctx: usize, sym: u32) {
        #[cfg(debug_assertions)]
        assert!(ctx < self.n_ctx);
        assert!(self.room > 0);
        // SAFETY: room was reserved; ctx < number of contexts, and `used` has a bit for each.
        unsafe {
            *self.syms.add(self.len) = *self.off.add(ctx) + sym;
            *self.used.add(ctx >> 6) |= 1 << (ctx & 63);
        }
        self.len += 1;
        self.room -= 1;
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

    /// The hot state, to be used in a loop and returned with [`Decoder::put_back`].
    #[inline(always)]
    pub fn local(&self) -> DecLocal {
        DecLocal {
            x: self.x,
            src: self.src.as_ptr(),
            pos: self.pos,
            src_max: self.src_max,
            meta: self.t.dec().meta.as_ptr(),
            alias: self.t.dec().alias.as_ptr(),
            #[cfg(debug_assertions)]
            n_ctx: self.t.n.len(),
        }
    }

    #[inline(always)]
    pub fn put_back(&mut self, l: DecLocal) {
        self.x = l.x;
        self.pos = l.pos;
    }

    /// Decodes a symbol in context `ctx` (below the number of contexts).
    #[inline(always)]
    pub fn get(&mut self, ctx: usize) -> u32 {
        let mut l = self.local();
        let s = l.get(ctx);
        self.put_back(l);
        s
    }
}

/// Decoder state by value, so that it stays in registers. Valid while its decoder lives
/// and is not used otherwise.
#[derive(Clone, Copy)]
pub struct DecLocal {
    x: [u64; LANES],
    src: *const u32,
    pos: usize,
    src_max: usize,
    meta: *const u32,
    alias: *const u64,
    #[cfg(debug_assertions)]
    n_ctx: usize,
}

impl DecLocal {
    /// Decodes a symbol in context `ctx`, which must be below the number of contexts.
    #[inline(always)]
    pub fn get(&mut self, ctx: usize) -> u32 {
        let x = self.x[0];
        let slot = (x & (PROB_SCALE as u64 - 1)) as u32;
        #[cfg(debug_assertions)]
        assert!(ctx < self.n_ctx);
        // SAFETY: ctx < number of contexts; slot >> sh is below the context's bucket count;
        // the source position is clamped to the buffer.
        unsafe {
            let m = *self.meta.add(ctx);
            let sh = m & 15;
            let i = slot >> sh;
            let pos = slot & ((1 << sh) - 1);
            let e = *self.alias.add((m >> 4) as usize + i as usize);
            let own = pos < (e & 0x7ff) as u32;
            let sym = std::hint::select_unpredictable(own, i, (e >> 23) as u32 & 0xff);
            let f = std::hint::select_unpredictable(own, e >> 11, e >> 31) & 0xfff;
            let j = std::hint::select_unpredictable(own, pos as u64, (pos as u64 + (e >> 43)) & 0xfff);
            let x = (f + 1) * (x >> PROB_BITS) + j;
            let next = *self.src.add(self.pos.min(self.src_max)) as u64;
            let low = x < RANS_L;
            let x = std::hint::select_unpredictable(low, (x << 32) | next, x);
            self.pos += low as usize;
            self.x.rotate_left(1);
            self.x[LANES - 1] = x;
            sym
        }
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
