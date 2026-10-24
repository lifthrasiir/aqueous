//! Multi-symbol rANS with static per-context frequency tables: 64-bit states, 32-bit
//! renormalisation, eight interleaved states (symbol j uses state j % 8), alias decoding.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

pub const PROB_BITS: u32 = 12;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;
const RANS_L: u64 = 1 << 31;
/// Interleaved states: symbol j uses state j % LANES.
const LANES: usize = 8;
/// Contexts up to which all inverse maps (8 KiB each) are built at the first encoding;
/// beyond, only those of the contexts used.
const INV_ALL_MAX: usize = 256;
const _: () = assert!(PROB_BITS == 12, "alias entries hold 12-bit frequencies and ranks");

/// Frequency tables of all contexts. The decoder uses alias tables (one bucket per symbol
/// of the alphabet rounded up to a power of two, at least 4): bucket `i` covers `B` slots,
/// the first `cutoff` of them belong to symbol `i`, the rest to its alias. Within a symbol,
/// ranks count its own bucket's slots first, then its alias slots in bucket order. The
/// encoder maps ranks back to slots with `inv`, built per context on first use. The
/// decoder's and the encoder's tables are built on first use, so that a decoder does not
/// pay for the encoder's.
pub struct Tables {
    /// Context `c` has alphabet `n[c]`, its symbols at `off[c]..off[c] + n[c]`.
    pub off: Vec<u32>,
    pub n: Vec<u16>,
    freqs: Vec<u16>,
    /// The first context with the same frequencies; contexts share tables with it.
    canon: Vec<u32>,
    dec: OnceLock<DecTables>,
    enc: OnceLock<EncTables>,
}

struct EncTables {
    /// By symbol: `freq | rcp_shift << 13 | slot_base << 17`, the symbol's slots
    /// by rank at `inv[slot_base..slot_base + freq]`.
    enc: Vec<u64>,
    /// Reciprocals (see [`reciprocal`]) by frequency.
    rcp: Vec<u64>,
    /// `PROB_SCALE` entries per canonical context, filled by `ensure_inv` under `inv_lock`
    /// before `inv_ready` is set; never reallocated.
    inv: std::cell::UnsafeCell<Vec<u16>>,
    inv_ready: Vec<AtomicBool>,
    inv_lock: Mutex<()>,
    /// Set once every context's inverse map is ready.
    inv_all: AtomicBool,
}

// SAFETY: `inv` is only written under `inv_lock` for contexts whose `inv_ready` flag is not
// yet set, and only read for contexts whose flag is set (with acquire/release ordering).
unsafe impl Sync for EncTables {}

struct DecTables {
    /// Per context: first bucket in `alias` << 4 | log2 of the bucket size.
    meta: Vec<u32>,
    /// `cutoff | (f_own - 1) << 11 | alias << 23 | (f_alias - 1) << 31 | (rank_start - cutoff) << 43`
    /// (12-bit fields wrap).
    alias: Vec<u64>,
    /// The alias entries of context `c` at `c << flat_log` (empty unless all contexts
    /// have the same number of buckets, `1 << flat_log`).
    flat: Vec<u64>,
    flat_log: u32,
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
    for (i, &r) in rem[..a].iter().enumerate() {
        if (r as usize) < bs {
            small[ns] = i as u16;
            ns += 1;
        } else if (r as usize) > bs {
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
    next[..a].copy_from_slice(&al.cutoff[..a]);
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
        for (c, f) in freqs.iter().enumerate() {
            assert!(!f.is_empty() && f.len() <= 256);
            assert_eq!(f.iter().map(|&x| x as u32).sum::<u32>(), PROB_SCALE);
            assert!(f.iter().all(|&x| x >= 1));
            off.push(flat.len() as u32);
            n.push(f.len() as u16);
            flat.extend_from_slice(f);
            canon.push(*seen.entry(&f[..]).or_insert(c as u32));
        }
        Tables { off, n, freqs: flat, canon, dec: OnceLock::new(), enc: OnceLock::new() }
    }

    fn enc(&self) -> &EncTables {
        self.enc.get_or_init(|| {
            let mut enc = vec![0u64; self.freqs.len()];
            for c in 0..self.n.len() {
                let (o, k) = (self.off[c] as usize, self.canon[c] as u64);
                let mut cum = 0u64;
                for (i, &fr) in self.freqs(c).iter().enumerate() {
                    let sh = rcp_shift(fr as u64);
                    enc[o + i] = fr as u64 | (sh as u64) << 13 | (k * PROB_SCALE as u64 + cum) << 17;
                    cum += fr as u64;
                }
            }
            let rcp: Vec<u64> = (0..=PROB_SCALE as u64).map(|f| reciprocal(f).0).collect();
            // Zeroed allocation: pages are only touched for the contexts used.
            let inv = std::cell::UnsafeCell::new(vec![0u16; self.n.len() * PROB_SCALE as usize]);
            let inv_ready = (0..self.n.len()).map(|_| AtomicBool::new(false)).collect();
            EncTables { enc, rcp, inv, inv_ready, inv_lock: Mutex::new(()), inv_all: AtomicBool::new(false) }
        })
    }

    /// log2 of the number of alias buckets of every context, if all have the same
    /// ([`DecLocal::get_flat`]).
    pub fn flat_log(&self) -> Option<u32> {
        (!self.dec().flat.is_empty()).then_some(self.dec().flat_log)
    }

    fn freqs(&self, c: usize) -> &[u16] {
        let o = self.off[c] as usize;
        &self.freqs[o..o + self.n[c] as usize]
    }

    fn dec(&self) -> &DecTables {
        self.dec.get_or_init(|| self.build_dec(1))
    }

    /// Builds the decoder's tables now (otherwise built on first use), on up to `threads`
    /// threads; the same tables whatever the number.
    pub fn prepare_dec(&self, threads: usize) {
        self.dec.get_or_init(|| self.build_dec(threads));
    }

    fn build_dec(&self, threads: usize) -> DecTables {
        let mut meta = vec![0u32; self.n.len()];
        let mut alias = Vec::new();
        let canon: Vec<usize> = (0..self.n.len()).filter(|&c| self.canon[c] as usize == c).collect();
        let threads = threads.clamp(1, canon.len().div_ceil(64).max(1));
        if threads > 1 {
            // Contiguous runs of contexts, their entries concatenated in order.
            let per = canon.len().div_ceil(threads);
            let parts: Vec<(Vec<u64>, Vec<u32>)> = std::thread::scope(|s| {
                let hs: Vec<_> = canon
                    .chunks(per)
                    .map(|cs| {
                        s.spawn(move || {
                            let mut alias = Vec::new();
                            let at = cs
                                .iter()
                                .map(|&c| (alias.len() as u32) << 4 | self.alias_entries(c, &mut alias))
                                .collect();
                            (alias, at)
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
            });
            let mut cs = canon.iter();
            for (part, at) in parts {
                let base = (alias.len() as u32) << 4;
                for m in at {
                    meta[*cs.next().unwrap()] = base + m;
                }
                alias.extend_from_slice(&part);
            }
        } else {
            for &c in &canon {
                meta[c] = (alias.len() as u32) << 4 | self.alias_entries(c, &mut alias);
            }
        }
        for c in 0..self.n.len() {
            meta[c] = meta[self.canon[c] as usize];
        }
        // Every context at a fixed stride when all alphabets have the same size.
        let a = self.n.iter().map(|&n| (n as usize).next_power_of_two().max(4)).max().unwrap_or(4);
        let flat = if self.n.iter().all(|&n| (n as usize).next_power_of_two().max(4) == a) {
            let mut flat = Vec::with_capacity(self.n.len() * a);
            for &m in &meta {
                let base = (m >> 4) as usize;
                flat.extend_from_slice(&alias[base..base + a]);
            }
            flat
        } else {
            Vec::new()
        };
        DecTables { meta, alias, flat, flat_log: a.trailing_zeros() }
    }

    /// Appends the alias entries of context `c`; returns log2 of its bucket size.
    fn alias_entries(&self, c: usize, alias: &mut Vec<u64>) -> u32 {
        let f = self.freqs(c);
        let al = build_alias(f);
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
        al.bs.trailing_zeros()
    }

    /// Makes `inv` available for context `c`.
    fn ensure_inv(&self, c: usize) {
        let k = self.canon[c] as usize;
        let e = self.enc();
        if e.inv_ready[k].load(Ordering::Acquire) {
            return;
        }
        let _g = e.inv_lock.lock().unwrap_or_else(|e| e.into_inner());
        if e.inv_ready[k].load(Ordering::Relaxed) {
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
        let p = unsafe { (*e.inv.get()).as_mut_ptr() };
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
        e.inv_ready[k].store(true, Ordering::Release);
    }
}

/// Scales `counts` to frequencies summing to `PROB_SCALE`, each at least 1.
pub fn normalize(counts: &[u64], out: &mut [u16]) {
    let n = counts.len();
    assert!((1..=256).contains(&n));
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
    let t0 = ((x1 % freq) << 32).div_ceil(freq);
    (t0.wrapping_add(t1 << 32), shift - 1)
}

/// The shift of [`reciprocal`] alone (without its divisions).
#[inline(always)]
fn rcp_shift(freq: u64) -> u32 {
    if freq < 2 { 0 } else { 63 - (freq - 1).leading_zeros() }
}

#[inline(always)]
fn div_rcp(x: u64, freq: u64, rcp: u64, shift: u32) -> u64 {
    let q = ((x as u128 * rcp as u128) >> 64) as u64 >> shift;
    if freq == 1 { x } else { q }
}

/// Collects symbols (as indices into the encoder's tables) and encodes them at the end, or at
/// each [`Encoder::flush`].
#[derive(Default)]
pub struct Encoder {
    syms: Vec<u32>,
    /// The words of one flush, written from the end.
    buf: Vec<u32>,
    /// Contexts used, one bit each, through [`Encoder::local_tracked`].
    used: Vec<u64>,
    /// Whether symbols were added through [`Encoder::local`] (contexts not tracked).
    untracked: bool,
}

impl Encoder {
    pub fn with_capacity(n: usize) -> Self {
        Encoder { syms: Vec::with_capacity(n), buf: Vec::new(), used: Vec::new(), untracked: false }
    }

    /// Makes room for `n` more symbols.
    #[inline(always)]
    pub fn reserve(&mut self, n: usize) {
        self.syms.reserve(n);
    }

    /// The hot state; [`Encoder::reserve`] must have made room for the symbols added
    /// through it before [`Encoder::put_back`]. With many contexts (more than
    /// [`INV_ALL_MAX`]), [`Encoder::local_tracked`] spares [`Encoder::finish`] a pass over
    /// the symbols.
    #[inline(always)]
    pub fn local(&mut self, t: &Tables) -> EncLocal {
        self.untracked = true;
        self.local_with(t)
    }

    /// [`Encoder::local`] that records the contexts used, for tables with many contexts
    /// (only the inverse maps of those are then built).
    #[inline(always)]
    pub fn local_tracked(&mut self, t: &Tables) -> EncLocal<true> {
        if self.used.is_empty() {
            self.used = vec![0; t.n.len().div_ceil(64)];
        }
        self.local_with(t)
    }

    #[inline(always)]
    fn local_with<const TRACK: bool>(&mut self, t: &Tables) -> EncLocal<TRACK> {
        debug_assert!(!TRACK || self.used.len() * 64 >= t.n.len());
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
    pub fn put_back<const TRACK: bool>(&mut self, l: EncLocal<TRACK>) {
        assert!(l.len <= self.syms.capacity());
        // SAFETY: the symbols up to l.len were written through `l`.
        unsafe { self.syms.set_len(l.len) };
    }

    /// Adds a symbol.
    pub fn put(&mut self, t: &Tables, ctx: usize, sym: u32) {
        self.reserve(1);
        if t.n.len() > INV_ALL_MAX {
            let mut l = self.local_tracked(t);
            l.put(ctx, sym);
            self.put_back(l);
        } else {
            let mut l = self.local(t);
            l.put(ctx, sym);
            self.put_back(l);
        }
    }

    pub fn len(&self) -> usize {
        self.syms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.syms.is_empty()
    }

    pub fn truncate(&mut self, n: usize) {
        self.syms.truncate(n);
    }

    /// Encodes all symbols; the words come out in decoding order.
    pub fn finish(&self, t: &Tables) -> Vec<u32> {
        let mut out = vec![0u32; self.syms.len() + 2 * LANES];
        let w = self.encode(t, &mut out);
        out.drain(..w);
        out
    }

    /// Encodes the symbols so far, appending the words to `out` in decoding order as u32 LE,
    /// and starts anew: the decoder takes the words that follow with [`Decoder::restart`].
    /// The symbols' memory is kept for the next ones.
    pub fn flush(&mut self, t: &Tables, out: &mut Vec<u8>) {
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        buf.resize(self.syms.len() + 2 * LANES, 0);
        let w = self.encode(t, &mut buf);
        out.reserve(4 * (buf.len() - w));
        buf[w..].iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes()));
        self.buf = buf;
        self.syms.clear();
        self.untracked = false;
    }

    /// Encodes the symbols into the end of `out` (at least 2 × LANES words more than the
    /// symbols); returns where the words start.
    #[inline(never)]
    fn encode(&self, t: &Tables, out: &mut [u32]) -> usize {
        let et = t.enc();
        // The inverse maps (kept for the process): with few contexts all of them, once,
        // otherwise those of the contexts used.
        let nc = t.n.len();
        if nc <= INV_ALL_MAX {
            if !et.inv_all.load(Ordering::Acquire) {
                for c in 0..nc {
                    t.ensure_inv(c);
                }
                et.inv_all.store(true, Ordering::Release);
            }
        } else {
            let mut used = std::borrow::Cow::Borrowed(&self.used);
            if self.untracked {
                // The canonical contexts of the symbols, from their slot bases.
                let u = used.to_mut();
                u.resize(nc.div_ceil(64), 0);
                for &s in &self.syms {
                    let k = (et.enc[s as usize] >> 17 >> PROB_BITS) as usize;
                    u[k >> 6] |= 1 << (k & 63);
                }
            }
            for (w, &bits) in used.iter().enumerate() {
                let mut b = bits;
                while b != 0 {
                    t.ensure_inv(w * 64 + b.trailing_zeros() as usize);
                    b &= b - 1;
                }
            }
        }
        let n = self.syms.len();
        assert!(out.len() >= n + 2 * LANES);
        let mut w = out.len();
        let mut x = [RANS_L; LANES];
        let o = out.as_mut_ptr();
        let enc = et.enc.as_ptr();
        // SAFETY: the contexts used are ready (above) and no longer written.
        let inv = unsafe { (*et.inv.get()).as_ptr() };
        let rcp = et.rcp.as_ptr();
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
        w
    }
}

/// Encoder state by value, so that it stays in registers.
#[derive(Clone, Copy)]
pub struct EncLocal<const TRACK: bool = false> {
    used: *mut u64,
    syms: *mut u32,
    len: usize,
    room: usize,
    off: *const u32,
    #[cfg(debug_assertions)]
    n_ctx: usize,
}

impl<const TRACK: bool> EncLocal<TRACK> {
    #[inline(always)]
    pub fn put(&mut self, ctx: usize, sym: u32) {
        #[cfg(debug_assertions)]
        assert!(ctx < self.n_ctx);
        assert!(self.room > 0);
        // SAFETY: room was reserved; ctx < number of contexts, and `used` has a bit for each
        // when tracking (`local_tracked`).
        unsafe {
            *self.syms.add(self.len) = *self.off.add(ctx) + sym;
            if TRACK {
                *self.used.add(ctx >> 6) |= 1 << (ctx & 63);
            }
        }
        self.len += 1;
        self.room -= 1;
    }
}

/// Words of zero padding the decoder may read past the end.
pub const SRC_PAD: usize = 64;

pub struct Decoder<'a> {
    x: [u64; LANES],
    lane: usize,
    src: Vec<u32>,
    pos: usize,
    /// Last valid index of `src`.
    src_max: usize,
    pub t: &'a Tables,
    real_len: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(t: &'a Tables, words: &[u32]) -> Decoder<'a> {
        let mut src = Vec::with_capacity(words.len().max(2 * LANES) + SRC_PAD);
        src.extend_from_slice(words);
        Decoder::from_vec(t, src)
    }

    /// [`Decoder::new`] taking the words as they are (padded in place).
    pub fn from_vec(t: &'a Tables, mut src: Vec<u32>) -> Decoder<'a> {
        let real_len = src.len();
        src.resize(real_len.max(2 * LANES) + SRC_PAD, 0);
        let src_max = src.len() - 1;
        let mut d = Decoder { x: [0; LANES], lane: 0, src, pos: 0, src_max, t, real_len };
        d.restart();
        d
    }

    /// [`Decoder::new`] of words as u32 LE (`bytes`, a multiple of 4 long).
    pub fn from_le_bytes(t: &'a Tables, bytes: &[u8]) -> Decoder<'a> {
        assert!(bytes.len().is_multiple_of(4));
        let mut src = Vec::with_capacity((bytes.len() / 4).max(2 * LANES) + SRC_PAD);
        src.extend(bytes.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())));
        Decoder::from_vec(t, src)
    }

    /// Takes the states of the next run of words ([`Encoder::flush`]): the words after the
    /// last one read, 0 past the end.
    pub fn restart(&mut self) {
        for k in 0..LANES {
            let w = |i: usize| self.src.get(self.pos + i).map_or(0, |&w| w as u64);
            self.x[k] = w(2 * k) << 32 | w(2 * k + 1);
        }
        self.pos += 2 * LANES;
        self.lane = 0;
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
            lane: self.lane,
            src: self.src.as_ptr(),
            pos: self.pos,
            src_max: self.src_max,
            meta: self.t.dec().meta.as_ptr(),
            alias: self.t.dec().alias.as_ptr(),
            flat: self.t.dec().flat.as_ptr(),
            #[cfg(debug_assertions)]
            flat_log: self.t.dec().flat_log,
            #[cfg(debug_assertions)]
            n_ctx: self.t.n.len(),
        }
    }

    #[inline(always)]
    pub fn put_back(&mut self, l: DecLocal) {
        self.x = l.x;
        self.lane = l.lane;
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
    /// The state of the next symbol.
    lane: usize,
    src: *const u32,
    pos: usize,
    src_max: usize,
    meta: *const u32,
    alias: *const u64,
    flat: *const u64,
    #[cfg(debug_assertions)]
    flat_log: u32,
    #[cfg(debug_assertions)]
    n_ctx: usize,
}

impl DecLocal {
    /// Words that can still be read without the bounds clamp (`CHECKED` false in
    /// [`DecLocal::get_flat`]); each symbol reads at most one.
    #[inline(always)]
    pub fn room(&self) -> usize {
        self.src_max.saturating_sub(self.pos)
    }

    /// [`DecLocal::get`] for tables whose contexts all have `1 << LOG` alias buckets (see
    /// [`Tables::flat_log`]): one table load less. Without `CHECKED`, [`DecLocal::room`]
    /// must be positive.
    #[inline(always)]
    pub fn get_flat<const LOG: u32, const CHECKED: bool>(&mut self, ctx: usize) -> u32 {
        // SAFETY: lane < LANES.
        let x = unsafe { *self.x.get_unchecked(self.lane) };
        let slot = (x & (PROB_SCALE as u64 - 1)) as u32;
        #[cfg(debug_assertions)]
        assert!(ctx < self.n_ctx && LOG == self.flat_log && (CHECKED || self.pos < self.src_max));
        // SAFETY: as in `get`, with the flat layout; unchecked reads are within the room.
        unsafe {
            let sh = PROB_BITS - LOG;
            let i = slot >> sh;
            let pos = slot & ((1 << sh) - 1);
            let e = *self.flat.add((ctx << LOG) + i as usize);
            let own = pos < (e & 0x7ff) as u32;
            let sym = std::hint::select_unpredictable(own, i, (e >> 23) as u32 & 0xff);
            let f = std::hint::select_unpredictable(own, e >> 11, e >> 31) & 0xfff;
            let j = std::hint::select_unpredictable(own, pos as u64, (pos as u64 + (e >> 43)) & 0xfff);
            let x = (f + 1) * (x >> PROB_BITS) + j;
            let p = if CHECKED { self.pos.min(self.src_max) } else { self.pos };
            let next = *self.src.add(p) as u64;
            let low = x < RANS_L;
            let x = std::hint::select_unpredictable(low, (x << 32) | next, x);
            self.pos += low as usize;
            *self.x.get_unchecked_mut(self.lane) = x;
            self.lane = (self.lane + 1) % LANES;
            sym
        }
    }

    /// Decodes a symbol in context `ctx`, which must be below the number of contexts.
    #[inline(always)]
    pub fn get(&mut self, ctx: usize) -> u32 {
        // SAFETY: lane < LANES.
        let x = unsafe { *self.x.get_unchecked(self.lane) };
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
            *self.x.get_unchecked_mut(self.lane) = x;
            self.lane = (self.lane + 1) % LANES;
            sym
        }
    }
}

/// Frequency tables as data: a layout id (u32 LE), then every context's frequencies (u16 LE);
/// built into a binary compressed with zstd.
#[cfg(feature = "zstd")]
pub mod builtin {
    /// Identifies a context layout (FNV-1a over `vals`).
    pub fn layout_id(vals: impl IntoIterator<Item = u32>) -> u32 {
        let mut h = 0x811c9dc5u32;
        for v in vals {
            h = (h ^ v).wrapping_mul(0x01000193);
        }
        h
    }

    /// The uncompressed form of tables for layout `id`.
    pub fn to_raw(id: u32, f: &[Vec<u16>]) -> Vec<u8> {
        let mut raw = id.to_le_bytes().to_vec();
        for t in f {
            for &x in t {
                raw.extend_from_slice(&x.to_le_bytes());
            }
        }
        raw
    }

    /// Serialises tables for layout `id` (the file form).
    pub fn to_bytes(id: u32, f: &[Vec<u16>]) -> Vec<u8> {
        compress(&to_raw(id, f))
    }

    /// The file form of the uncompressed form `raw`, whatever its layout.
    pub fn compress(raw: &[u8]) -> Vec<u8> {
        zstd::bulk::compress(raw, 19).unwrap()
    }

    /// The uncompressed form in the file `data`.
    pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
        if data.is_empty() {
            return None;
        }
        // The size the frame tells, else a bound (`upper_bound` is only an estimate there).
        let size = match zstd::zstd_safe::get_frame_content_size(data) {
            Ok(Some(n)) => usize::try_from(n).ok()?,
            _ => zstd::bulk::Decompressor::upper_bound(data).unwrap_or(1 << 24),
        };
        zstd::bulk::decompress(data, size).ok()
    }

    /// The tables in the uncompressed form `raw` if they were made for layout `id`, with
    /// `alphabets` giving the number of symbols of each context in turn (each 1..=256). `raw`
    /// must have exactly the size of those tables, and every context valid frequencies (all
    /// nonzero, summing to the probability scale).
    pub fn parse(raw: &[u8], id: u32, alphabets: impl IntoIterator<Item = usize>) -> Option<Vec<Vec<u16>>> {
        if raw.len() < 4 || u32::from_le_bytes(raw[0..4].try_into().unwrap()) != id {
            return None;
        }
        let mut p = 4usize;
        let mut out = Vec::new();
        for n in alphabets {
            let end = p.checked_add(n.checked_mul(2)?)?;
            let f: Vec<u16> = raw.get(p..end)?.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
            // What `Tables::new` requires.
            if f.contains(&0) || f.iter().map(|&x| x as u32).sum::<u32>() != super::PROB_SCALE {
                return None;
            }
            out.push(f);
            p = end;
        }
        (p == raw.len()).then_some(out)
    }

    /// [`parse`] of the file `data`.
    pub fn from_bytes(data: &[u8], id: u32, alphabets: impl IntoIterator<Item = usize>) -> Option<Vec<Vec<u16>>> {
        parse(&decompress(data)?, id, alphabets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "zstd")]
    #[test]
    fn builtin_parse() {
        let f = vec![vec![PROB_SCALE as u16 - 3, 1, 2], vec![PROB_SCALE as u16]];
        let raw = builtin::to_raw(7, &f);
        assert_eq!(raw.len(), 4 + 2 * 4);
        assert_eq!(builtin::parse(&raw, 7, [3, 1]), Some(f.clone()));
        assert_eq!(builtin::from_bytes(&builtin::to_bytes(7, &f), 7, [3, 1]), Some(f));
        assert_eq!(builtin::parse(&raw, 8, [3, 1]), None);
        assert_eq!(builtin::parse(&raw, 7, [3, 2]), None);
        assert_eq!(builtin::parse(&raw, 7, [3]), None); // trailing bytes
        assert_eq!(builtin::parse(&raw[..raw.len() - 1], 7, [3, 1]), None);
        let mut bad = raw.clone();
        bad[4] ^= 1; // sum off the scale
        assert_eq!(builtin::parse(&bad, 7, [3, 1]), None);
        bad[4] = 0;
        bad[5] = 0; // zero frequency
        assert_eq!(builtin::parse(&bad, 7, [3, 1]), None);
    }

    #[test]
    fn reciprocals() {
        let mut seed = 7u64;
        for f in 1..=PROB_SCALE as u64 {
            let (r, sh) = reciprocal(f);
            assert_eq!(rcp_shift(f), sh, "f={f}");
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
    fn prepare_dec_threads() {
        // Many contexts, some repeated (shared tables), mixed and equal alphabet sizes.
        for sizes in [&[2usize, 5, 16, 200, 256][..], &[72][..]] {
            let mut freqs = Vec::new();
            for c in 0..700u64 {
                let n = sizes[c as usize % sizes.len()];
                let counts: Vec<u64> = (0..n as u64).map(|i| (i * 7919 + c % 37 * 31) % 101 + 1).collect();
                let mut f = vec![0u16; n];
                normalize(&counts, &mut f);
                freqs.push(f);
            }
            let one = Tables::new(&freqs);
            one.prepare_dec(1);
            for threads in [2, 3, 8] {
                let t = Tables::new(&freqs);
                t.prepare_dec(threads);
                let (a, b) = (one.dec(), t.dec());
                assert!(a.meta == b.meta && a.alias == b.alias && a.flat == b.flat && a.flat_log == b.flat_log);
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

    /// Runs of symbols flushed one after the other decode with a restart between them, and a
    /// single run as [`Encoder::finish`] gives it.
    #[test]
    fn flush_restart() {
        let freqs: Vec<Vec<u16>> = [3usize, 16, 200]
            .iter()
            .map(|&n| {
                let mut f = vec![0u16; n];
                normalize(&(0..n as u64).map(|i| i * i % 13 + 1).collect::<Vec<_>>(), &mut f);
                f
            })
            .collect();
        let t = Tables::new(&freqs);
        let mut seed = 5u64;
        let runs: Vec<Vec<(usize, u32)>> = [0usize, 1, 7, 8, 1000, 0, 33]
            .iter()
            .map(|&n| {
                (0..n)
                    .map(|_| {
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        let c = (seed >> 50) as usize % 3;
                        (c, (seed >> 20) as u32 % t.n[c] as u32)
                    })
                    .collect()
            })
            .collect();
        let mut e = Encoder::default();
        let mut words = Vec::new();
        for run in &runs {
            for &(c, s) in run {
                e.put(&t, c, s);
            }
            if run.len() == 1000 {
                assert_eq!(e.finish(&t).iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>(), {
                    let mut w = Vec::new();
                    let mut e2 = Encoder::default();
                    run.iter().for_each(|&(c, s)| e2.put(&t, c, s));
                    e2.flush(&t, &mut w);
                    w
                });
            }
            e.flush(&t, &mut words);
        }
        let mut d = Decoder::from_le_bytes(&t, &words);
        for (i, run) in runs.iter().enumerate() {
            if i > 0 {
                d.restart();
            }
            for &(c, s) in run {
                assert_eq!(d.get(c), s);
            }
        }
        assert!(d.in_bounds());
        // One word short: out of bounds.
        let mut d = Decoder::from_le_bytes(&t, &words[..words.len() - 4]);
        for (i, run) in runs.iter().enumerate() {
            if i > 0 {
                d.restart();
            }
            run.iter().for_each(|&(c, _)| _ = d.get(c));
        }
        assert!(!d.in_bounds());
    }

    /// Many contexts (inverse maps of the used ones only), some sharing tables, all of the
    /// same alphabet size (the flat decoder layout).
    #[test]
    fn many_contexts() {
        let freqs: Vec<Vec<u16>> = (0..300u64)
            .map(|c| {
                let counts: Vec<u64> = (0..16).map(|i| 1 + (i * (c % 37 + 1)) % 29).collect();
                let mut f = vec![0u16; 16];
                normalize(&counts, &mut f);
                f
            })
            .collect();
        let t = Tables::new(&freqs);
        assert_eq!(t.flat_log(), Some(4));
        let mut seed = 99u64;
        let mut syms = Vec::new();
        let mut e = Encoder::default();
        for _ in 0..50000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // Every third context unused.
            let c = (seed >> 40) as usize % 200 * 3 / 2;
            let s = (seed >> 20) as u32 % 16;
            syms.push((c, s));
            e.put(&t, c, s);
        }
        let words = e.finish(&t);
        // The same without tracking the contexts (a pass over the symbols instead), on
        // fresh tables.
        let t2 = Tables::new(&freqs);
        let mut e2 = Encoder::with_capacity(syms.len());
        let mut l = e2.local(&t2);
        for &(c, s) in &syms {
            l.put(c, s);
        }
        e2.put_back(l);
        assert_eq!(e2.finish(&t2), words);
        let mut d = Decoder::new(&t, &words);
        let mut l = d.local();
        for &(c, s) in &syms {
            assert_eq!(l.get_flat::<4, true>(c), s);
        }
        d.put_back(l);
        assert!(d.in_bounds());
    }
}
