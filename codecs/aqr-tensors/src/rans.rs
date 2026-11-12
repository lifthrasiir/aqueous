//! The symbols' coder (FORMAT.md §3): static rANS of 32 interleaved 32-bit states with 16-bit
//! renormalisation, over an alphabet of 16 symbols, the symbols coded in *groups* of up to 32,
//! one to a state, all of a group with one table. As every state of a group has the same table,
//! a symbol is found by comparing the slot with the table's 15 cumulative frequencies, which
//! SIMD does on all the states at once with no gather: AVX2 natively (where the processor has
//! it), NEON on AArch64, SIMD128 in Wasm, else one state at a time. All of them read the same words in the same
//! order, so their results are the same.

use crate::{Result, corrupt};

/// States, and symbols of a full group.
pub const LANES: usize = 32;
/// Bits of the probabilities: the frequencies of a table add up to `1 << PROB_BITS`.
pub const PROB_BITS: u32 = 15;
pub const PROB: u32 = 1 << PROB_BITS;
/// The least a state is, between symbols.
const L: u32 = 1 << 16;
/// Bytes of the states at the start of a stream.
pub const HEAD: usize = 4 * LANES;
/// Bytes a group reads at most.
pub const GROUP_BYTES: usize = 2 * LANES;

/// A table of 16 frequencies, as the decoder takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Table {
    pub freq: [u32; 16],
    /// `cum[s]`: the frequencies of the symbols before `s`; `cum[16] = PROB`.
    pub cum: [u32; 17],
}

impl Table {
    /// The table of frequencies `freq`, which must add up to [`PROB`].
    pub fn new(freq: [u32; 16]) -> Result<Table> {
        let mut cum = [0; 17];
        for s in 0..16 {
            cum[s + 1] = cum[s] + freq[s];
        }
        if cum[16] != PROB {
            return Err(corrupt());
        }
        Ok(Table { freq, cum })
    }

    /// The table of counts `counts` (not all zero) normalised: every symbol counted has a
    /// frequency of at least 1 (FORMAT.md §3 leaves the encoder its choice).
    pub fn of_counts(counts: &[u64; 16]) -> Table {
        let total: u64 = counts.iter().sum();
        assert!(total > 0);
        let mut freq = [0u32; 16];
        for s in 0..16 {
            if counts[s] > 0 {
                freq[s] = ((counts[s] as u128 * PROB as u128 / total as u128) as u32).max(1);
            }
        }
        // (the difference taken from or given to the largest, then any left as fits)
        let mut sum: i64 = freq.iter().map(|&f| f as i64).sum();
        while sum != PROB as i64 {
            let big = (0..16).max_by_key(|&s| (freq[s], counts[s])).unwrap();
            if sum < PROB as i64 {
                freq[big] += (PROB as i64 - sum) as u32;
                sum = PROB as i64;
            } else {
                let room = freq[big] as i64 - 1;
                let take = (sum - PROB as i64).min(room);
                if take > 0 {
                    freq[big] -= take as u32;
                    sum -= take;
                } else {
                    // (the largest is 1: every counted symbol is; there are more than PROB)
                    unreachable!("16 symbols fit");
                }
            }
        }
        Table::new(freq).unwrap()
    }

    /// Bits that coding `counts` with the table takes (the symbols counted must have a
    /// frequency).
    pub fn cost(&self, counts: &[u64; 16]) -> f64 {
        (0..16)
            .filter(|&s| counts[s] > 0)
            .map(|s| counts[s] as f64 * (PROB_BITS as f64 - (self.freq[s] as f64).log2()))
            .sum()
    }
}

/// The order-0 entropy of `counts`, in bits.
pub fn entropy(counts: &[u64; 16]) -> f64 {
    let total: u64 = counts.iter().sum();
    let t = total as f64;
    counts.iter().filter(|&&c| c > 0).map(|&c| c as f64 * (t / c as f64).log2()).sum()
}

/// The encoder: groups gathered in order ([`Encoder::group`]), coded at the end, last first.
#[derive(Default)]
pub struct Encoder {
    syms: Vec<u8>,
    /// Of each group, its table (an index into the tables given to [`Encoder::finish`]) and its
    /// symbols.
    groups: Vec<(u32, u8)>,
}

/// What the encoder needs of a table: of each symbol, its frequency, its start, the state at
/// which a word is put out first, and `ceil(2^64 / freq)` (0 for a frequency of 1), by which a
/// state is divided (Lemire's: exact for every 32-bit state).
#[derive(Clone, Copy)]
struct EncSym {
    freq: u32,
    cum: u32,
    max: u64,
    magic: u64,
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder::default()
    }

    /// An encoder of about `n` symbols (more grow it).
    pub fn with_capacity(n: usize) -> Encoder {
        Encoder { syms: Vec::with_capacity(n), groups: Vec::with_capacity(n / LANES + 16) }
    }

    /// A group of `syms` (1 to 32, each below 16) of table `table`.
    pub fn group(&mut self, table: u32, syms: &[u8]) {
        debug_assert!(!syms.is_empty() && syms.len() <= LANES && syms.iter().all(|&s| s < 16));
        self.syms.extend_from_slice(syms);
        self.groups.push((table, syms.len() as u8));
    }

    /// Groups of 32 symbols each, all of `table`: `syms.len()` a multiple of 32.
    pub fn groups(&mut self, table: u32, syms: &[u8]) {
        debug_assert!(syms.len().is_multiple_of(LANES));
        self.syms.extend_from_slice(syms);
        self.groups.extend(std::iter::repeat_n((table, LANES as u8), syms.len() / LANES));
    }

    /// The symbols gathered.
    pub fn symbols(&self) -> usize {
        self.syms.len()
    }

    /// Where the encoder is: its symbols and groups so far, to go back to ([`Encoder::rewind`]).
    pub fn mark(&self) -> (usize, usize) {
        (self.syms.len(), self.groups.len())
    }

    /// Back to `mark`, the groups after it dropped.
    pub fn rewind(&mut self, mark: (usize, usize)) {
        self.syms.truncate(mark.0);
        self.groups.truncate(mark.1);
    }

    /// `n` more symbols, zero, for the caller to fill and then tell the groups of
    /// ([`Encoder::told`]).
    pub fn append(&mut self, n: usize) -> &mut [u8] {
        let at = self.syms.len();
        self.syms.resize(at + n, 0);
        &mut self.syms[at..]
    }

    /// `count` full groups of table `table` of the symbols appended.
    pub fn told(&mut self, table: u32, count: usize) {
        self.groups.extend(std::iter::repeat_n((table, LANES as u8), count));
    }

    /// `n` symbols of table `table` of the symbols appended, in groups of 32 but the last.
    pub fn told_syms(&mut self, table: u32, n: usize) {
        self.told(table, n / LANES);
        if !n.is_multiple_of(LANES) {
            self.groups.push((table, (n % LANES) as u8));
        }
    }

    /// The symbols from `mark` on.
    pub fn since(&self, mark: (usize, usize)) -> &[u8] {
        &self.syms[mark.0..]
    }

    /// The stream of the groups, with `tables` (every symbol coded must have a frequency in its
    /// table). Of no group, it is empty.
    pub fn finish(self, tables: &[Table]) -> Vec<u8> {
        self.finish_with(tables, Kernel::best())
    }

    /// [`Encoder::finish`] by the kernel `kernel` (all of them make the same stream).
    pub fn finish_with(self, tables: &[Table], kernel: Kernel) -> Vec<u8> {
        if self.groups.is_empty() {
            return Vec::new();
        }
        let enc: Vec<[EncSym; 16]> = (tables.iter())
            .map(|t| {
                std::array::from_fn(|s| EncSym {
                    freq: t.freq[s],
                    cum: t.cum[s],
                    max: (((L >> PROB_BITS) as u64) << 16) * t.freq[s] as u64,
                    magic: if t.freq[s] > 1 { u64::MAX / t.freq[s] as u64 + 1 } else { 0 },
                })
            })
            .collect();
        #[cfg(target_arch = "aarch64")]
        let neon: Vec<neon::Enc> =
            if kernel == Kernel::Neon { enc.iter().map(neon::Enc::new).collect() } else { Vec::new() };
        let mut x = [L; LANES];
        // (the words, last first: one at most for each symbol, written whether or not it is
        // put out, which a renormalisation of about one symbol in four would mispredict; with
        // room for a group's and a vector's store past them, grown as they come)
        let mut words: Vec<u16> = vec![0; self.syms.len() / 4 + 2 * LANES];
        let mut w = 0;
        let mut end = self.syms.len();
        for &(t, n) in self.groups.iter().rev() {
            if words.len() - w < LANES + 16 {
                words.resize(words.len() * 2, 0);
            }
            let n = n as usize;
            let syms = &self.syms[end - n..end];
            end -= n;
            match kernel {
                // SAFETY: AVX2 was found ([`Kernel::best`]); `words` has room for a group's
                // words past `w` and 8 more
                #[cfg(target_arch = "x86_64")]
                Kernel::Avx2 if n == LANES => unsafe {
                    avx2::encode(&mut x, &tables[t as usize], syms, &mut words, &mut w)
                },
                // SAFETY: `words` has room for a group's words past `w` and 4 more
                #[cfg(target_arch = "aarch64")]
                Kernel::Neon if n == LANES => unsafe {
                    neon::encode(&mut x, &neon[t as usize], syms, &mut words, &mut w)
                },
                _ => {
                    let e = &enc[t as usize];
                    for lane in (0..n).rev() {
                        let s = e[syms[lane] as usize];
                        debug_assert!(s.freq > 0, "a symbol of no frequency");
                        let mut v = x[lane];
                        let put = (v as u64 >= s.max) as usize;
                        words[w] = v as u16;
                        w += put;
                        v >>= 16 * put as u32;
                        let q = ((s.magic as u128 * v as u128) >> 64) as u32;
                        let q = if s.magic == 0 { v } else { q };
                        x[lane] = (q << PROB_BITS) + (v - q * s.freq) + s.cum;
                    }
                }
            }
        }
        words.truncate(w);
        words.reverse();
        let mut out = Vec::with_capacity(HEAD + 2 * words.len());
        for v in x {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let at = out.len();
        out.resize(at + 2 * words.len(), 0);
        for (o, w) in out[at..].as_chunks_mut::<2>().0.iter_mut().zip(&words) {
            *o = w.to_le_bytes();
        }
        out
    }
}

/// How the decoder runs: the kernel picked once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kernel {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "aarch64")]
    Neon,
    #[cfg(target_family = "wasm")]
    Simd128,
}

impl Kernel {
    /// The fastest the processor has.
    pub fn best() -> Kernel {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            return Kernel::Avx2;
        }
        #[cfg(target_arch = "aarch64")]
        return Kernel::Neon;
        #[cfg(target_family = "wasm")]
        return Kernel::Simd128;
        #[allow(unreachable_code)]
        Kernel::Scalar
    }
}

/// The decoder's states. The words are read from a buffer the caller holds, at least
/// [`GROUP_BYTES`] bytes past the position for each group decoded (zeros past the stream's
/// end, which [`Decoder::finish`] tells of).
pub struct Decoder {
    x: [u32; LANES],
    kernel: Kernel,
}

impl Decoder {
    /// The states from the head of a stream ([`HEAD`] bytes).
    pub fn new(head: &[u8], kernel: Kernel) -> Result<Decoder> {
        let mut x = [0; LANES];
        for (k, v) in x.iter_mut().enumerate() {
            *v = u32::from_le_bytes(head[4 * k..4 * k + 4].try_into().unwrap());
            if *v < L {
                return Err(corrupt());
            }
        }
        Ok(Decoder { x, kernel })
    }

    /// Whether the states are back where the encoder started.
    pub fn finish(&self) -> bool {
        self.x.iter().all(|&v| v == L)
    }

    /// Decodes `out.len() / 32` full groups of table `t` into `out`, reading words from
    /// `words` at `*pos` (which must have [`GROUP_BYTES`] bytes for each group past it).
    #[inline]
    // (groups as slices: of arrays, the scalar kernel is unrolled for them, tensors-recons 787
    // bytes larger)
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn groups(&mut self, t: &Table, words: &[u8], pos: &mut usize, out: &mut [u8]) {
        debug_assert!(out.len().is_multiple_of(LANES));
        assert!(words.len() >= *pos + out.len() * 2);
        match self.kernel {
            Kernel::Scalar => {
                for g in out.chunks_exact_mut(LANES) {
                    scalar(&mut self.x, t, words, pos, g);
                }
            }
            // SAFETY: AVX2 was found ([`Kernel::best`]); the words were asserted above.
            #[cfg(target_arch = "x86_64")]
            Kernel::Avx2 => unsafe { avx2::groups(&mut self.x, t, words, pos, out) },
            // SAFETY: the words were asserted above (AArch64 has NEON always).
            #[cfg(target_arch = "aarch64")]
            Kernel::Neon => unsafe { neon::groups(&mut self.x, t, words, pos, out) },
            // SAFETY: the words were asserted above.
            #[cfg(target_family = "wasm")]
            Kernel::Simd128 => unsafe { simd128::groups(&mut self.x, t, words, pos, out) },
        }
    }

    /// Decodes a group of `out.len()` symbols (1 to 32) of table `t`: the states of the lanes
    /// below it.
    pub fn partial(&mut self, t: &Table, words: &[u8], pos: &mut usize, out: &mut [u8]) {
        assert!(words.len() >= *pos + GROUP_BYTES && out.len() <= LANES);
        scalar(&mut self.x, t, words, pos, out);
    }
}

/// The symbol of `slot` in `t`, and its start and frequency.
#[inline(always)]
fn find(t: &Table, slot: u32) -> usize {
    let mut s = 0;
    for k in 1..16 {
        s += (slot >= t.cum[k]) as usize;
    }
    s
}

/// A group of `out.len()` symbols, a state at a time.
#[inline]
fn scalar(x: &mut [u32; LANES], t: &Table, words: &[u8], pos: &mut usize, out: &mut [u8]) {
    for (lane, o) in out.iter_mut().enumerate() {
        let v = x[lane];
        let slot = v & (PROB - 1);
        let s = find(t, slot);
        let mut v = t.freq[s].wrapping_mul(v >> PROB_BITS).wrapping_add(slot - t.cum[s]);
        if v < L {
            v = v << 16 | u16::from_le_bytes([words[*pos], words[*pos + 1]]) as u32;
            *pos += 2;
        }
        x[lane] = v;
        *o = s as u8;
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::{L, LANES, PROB, PROB_BITS, Table};
    use std::arch::x86_64::*;

    /// Of each mask of 8 lanes, the index of the word each lane takes: the lanes below it that
    /// take one.
    static TAKE: [[u32; 8]; 256] = {
        let mut t = [[0u32; 8]; 256];
        let mut m = 0;
        while m < 256 {
            let mut n = 0;
            let mut i = 0;
            while i < 8 {
                t[m][i] = n;
                if m >> i & 1 == 1 {
                    n += 1;
                }
                i += 1;
            }
            m += 1;
        }
        t
    };

    /// Of each mask of 8 lanes, the lanes set, from the highest down (then zeros).
    static PUT: [[u32; 8]; 256] = {
        let mut t = [[0u32; 8]; 256];
        let mut m = 0;
        while m < 256 {
            let mut n = 0;
            let mut i = 8;
            while i > 0 {
                i -= 1;
                if m >> i & 1 == 1 {
                    t[m][n] = i as u32;
                    n += 1;
                }
            }
            m += 1;
        }
        t
    };

    /// The frequency or start of each lane's symbol `s` (0 to 15), of the 16 of `lo` and `hi`.
    #[inline(always)]
    unsafe fn look(lo: __m256i, hi: __m256i, s: __m256i) -> __m256i {
        unsafe {
            let upper = _mm256_cmpgt_epi32(s, _mm256_set1_epi32(7));
            _mm256_blendv_epi8(_mm256_permutevar8x32_epi32(lo, s), _mm256_permutevar8x32_epi32(hi, s), upper)
        }
    }

    /// The encoder of a full group (FORMAT.md §3), lanes from the last: as the scalar one, its
    /// words put out after `w` in `words` (which has room for 8 more past a group's).
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn encode(x: &mut [u32; LANES], t: &Table, syms: &[u8], words: &mut [u16], w: &mut usize) {
        unsafe {
            let f_lo = _mm256_loadu_si256(t.freq.as_ptr() as *const __m256i);
            let f_hi = _mm256_loadu_si256(t.freq.as_ptr().add(8) as *const __m256i);
            let c_lo = _mm256_loadu_si256(t.cum.as_ptr() as *const __m256i);
            let c_hi = _mm256_loadu_si256(t.cum.as_ptr().add(8) as *const __m256i);
            let sign = _mm256_set1_epi32(i32::MIN);
            let two31 = _mm256_set1_pd(2147483648.0);
            let one = _mm256_set1_epi32(1);
            // (of each 32-bit lane, its low 16 bits, packed in each half)
            let pack = _mm256_setr_epi8(
                0, 1, 4, 5, 8, 9, 12, 13, -1, -1, -1, -1, -1, -1, -1, -1, 0, 1, 4, 5, 8, 9, 12, 13, -1, -1, -1, -1, -1,
                -1, -1, -1,
            );
            for k in (0..4).rev() {
                let s = _mm256_cvtepu8_epi32(_mm_loadl_epi64(syms.as_ptr().add(8 * k) as *const __m128i));
                let f = look(f_lo, f_hi, s);
                let c = look(c_lo, c_hi, s);
                let mut v = _mm256_loadu_si256(x.as_ptr().add(8 * k) as *const __m256i);
                // (a word out where v >= f << 17, that is v >> 17 >= f)
                let keep = _mm256_cmpgt_epi32(f, _mm256_srli_epi32::<17>(v));
                let m = !_mm256_movemask_ps(_mm256_castsi256_ps(keep)) as usize & 0xff;
                let out = _mm256_permutevar8x32_epi32(v, _mm256_loadu_si256(PUT[m].as_ptr() as *const __m256i));
                let out = _mm256_permute4x64_epi64::<0b1000>(_mm256_shuffle_epi8(out, pack));
                _mm_storeu_si128(words.as_mut_ptr().add(*w) as *mut __m128i, _mm256_castsi256_si128(out));
                *w += m.count_ones() as usize;
                v = _mm256_blendv_epi8(_mm256_srli_epi32::<16>(v), v, keep);
                // (v / f by doubles, exact below 2^32; a frequency of 1 leaves v)
                let half = |a: __m128i| _mm256_add_pd(_mm256_cvtepi32_pd(a), two31);
                let vs = _mm256_xor_si256(v, sign);
                let lo = _mm256_div_pd(half(_mm256_castsi256_si128(vs)), _mm256_cvtepi32_pd(_mm256_castsi256_si128(f)));
                let hi = _mm256_div_pd(
                    half(_mm256_extracti128_si256::<1>(vs)),
                    _mm256_cvtepi32_pd(_mm256_extracti128_si256::<1>(f)),
                );
                let q = _mm256_set_m128i(_mm256_cvttpd_epi32(hi), _mm256_cvttpd_epi32(lo));
                let q = _mm256_blendv_epi8(q, v, _mm256_cmpeq_epi32(f, one));
                let r = _mm256_sub_epi32(v, _mm256_mullo_epi32(q, f));
                let nx = _mm256_add_epi32(_mm256_add_epi32(_mm256_slli_epi32::<{ PROB_BITS as i32 }>(q), r), c);
                _mm256_storeu_si256(x.as_mut_ptr().add(8 * k) as *mut __m256i, nx);
            }
        }
    }

    // (the 4 vectors of the states by index, as the lanes they hold)
    #[allow(clippy::needless_range_loop)]
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn groups(x: &mut [u32; LANES], t: &Table, words: &[u8], pos: &mut usize, out: &mut [u8]) {
        unsafe {
            // (the cumulative frequencies less one, 1 to 15, to compare by greater-than; the
            // frequencies and starts by symbol, in two halves of 8)
            let c: [__m256i; 15] = std::array::from_fn(|k| _mm256_set1_epi32(t.cum[k + 1] as i32 - 1));
            let f_lo = _mm256_loadu_si256(t.freq.as_ptr() as *const __m256i);
            let f_hi = _mm256_loadu_si256(t.freq.as_ptr().add(8) as *const __m256i);
            let s_lo = _mm256_loadu_si256(t.cum.as_ptr() as *const __m256i);
            let s_hi = _mm256_loadu_si256(t.cum.as_ptr().add(8) as *const __m256i);
            let mask = _mm256_set1_epi32(PROB as i32 - 1);
            let seven = _mm256_set1_epi32(7);
            let below = _mm256_set1_epi32(L as i32);
            let mut st: [__m256i; 4] =
                std::array::from_fn(|k| _mm256_loadu_si256(x.as_ptr().add(8 * k) as *const __m256i));
            let mut p = *pos;
            let w = words.as_ptr();
            for g in out.as_chunks_mut::<LANES>().0 {
                let mut syms = [_mm256_setzero_si256(); 4];
                for k in 0..4 {
                    let v = st[k];
                    let slot = _mm256_and_si256(v, mask);
                    let mut s = _mm256_setzero_si256();
                    for ck in &c {
                        // (all ones where slot >= cum)
                        s = _mm256_sub_epi32(s, _mm256_cmpgt_epi32(slot, *ck));
                    }
                    // (the symbol's frequency and start: of the lower or upper eight)
                    let hi = _mm256_cmpgt_epi32(s, seven);
                    let freq = _mm256_blendv_epi8(
                        _mm256_permutevar8x32_epi32(f_lo, s),
                        _mm256_permutevar8x32_epi32(f_hi, s),
                        hi,
                    );
                    let start = _mm256_blendv_epi8(
                        _mm256_permutevar8x32_epi32(s_lo, s),
                        _mm256_permutevar8x32_epi32(s_hi, s),
                        hi,
                    );
                    let v = _mm256_add_epi32(
                        _mm256_mullo_epi32(freq, _mm256_srli_epi32::<{ PROB_BITS as i32 }>(v)),
                        _mm256_sub_epi32(slot, start),
                    );
                    // (the states below L, as unsigned: L - 1 = min(v, L - 1))
                    let need =
                        _mm256_cmpeq_epi32(_mm256_min_epu32(v, _mm256_sub_epi32(below, _mm256_set1_epi32(1))), v);
                    let m = _mm256_movemask_ps(_mm256_castsi256_ps(need)) as usize;
                    let ws = _mm256_cvtepu16_epi32(_mm_loadu_si128(w.add(p) as *const __m128i));
                    let idx = _mm256_loadu_si256(TAKE[m].as_ptr() as *const __m256i);
                    let ws = _mm256_permutevar8x32_epi32(ws, idx);
                    let renorm = _mm256_or_si256(_mm256_slli_epi32::<16>(v), ws);
                    st[k] = _mm256_blendv_epi8(v, renorm, need);
                    p += 2 * m.count_ones() as usize;
                    syms[k] = s;
                }
                // (the symbols packed to bytes, in order)
                let a = _mm256_packus_epi32(syms[0], syms[1]);
                let b = _mm256_packus_epi32(syms[2], syms[3]);
                let ab = _mm256_packus_epi16(a, b);
                // (packing interleaves the 128-bit halves: dwords 0 4 1 5 2 6 3 7 of the bytes)
                let ab = _mm256_permutevar8x32_epi32(ab, _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7));
                _mm256_storeu_si256(g.as_mut_ptr() as *mut __m256i, ab);
            }
            for k in 0..4 {
                _mm256_storeu_si256(x.as_mut_ptr().add(8 * k) as *mut __m256i, st[k]);
            }
            *pos = p;
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::{L, LANES, PROB, PROB_BITS, Table};
    use std::arch::aarch64::*;

    /// Of each mask of 4 lanes, the bytes of the words (of 4, the 8 bytes from the position)
    /// each lane takes: the lanes below it that take one (indices past 15 give 0).
    static TAKE: [[u8; 16]; 16] = {
        let mut t = [[0u8; 16]; 16];
        let mut m = 0;
        while m < 16 {
            let mut n = 0;
            let mut i = 0;
            while i < 4 {
                let mut b = 0;
                while b < 4 {
                    t[m][4 * i + b] = if b < 2 { (2 * n + b) as u8 } else { 0xff };
                    b += 1;
                }
                if m >> i & 1 == 1 {
                    n += 1;
                }
                i += 1;
            }
            m += 1;
        }
        t
    };

    /// Of each mask of 4 lanes, the low two bytes of the lanes set, from the highest down (then
    /// zeros).
    static PUT: [[u8; 16]; 16] = {
        let mut t = [[0xffu8; 16]; 16];
        let mut m = 0;
        while m < 16 {
            let mut n = 0;
            let mut i = 4;
            while i > 0 {
                i -= 1;
                if m >> i & 1 == 1 {
                    t[m][2 * n] = 4 * i as u8;
                    t[m][2 * n + 1] = 4 * i as u8 + 1;
                    n += 1;
                }
            }
            m += 1;
        }
        t
    };

    /// The bits of the lanes set of a mask.
    #[inline(always)]
    unsafe fn bits(m: uint32x4_t) -> usize {
        unsafe {
            const B: [u32; 4] = [1, 2, 4, 8];
            vaddvq_u32(vandq_u32(m, vld1q_u32(B.as_ptr()))) as usize
        }
    }

    /// Of a table's 16 values of 16 bits, their low and high bytes.
    #[inline(always)]
    unsafe fn bytes(a: &[u32]) -> (uint8x16_t, uint8x16_t) {
        let lo: [u8; 16] = std::array::from_fn(|s| a[s] as u8);
        let hi: [u8; 16] = std::array::from_fn(|s| (a[s] >> 8) as u8);
        unsafe { (vld1q_u8(lo.as_ptr()), vld1q_u8(hi.as_ptr())) }
    }

    /// The 16-bit values of the symbols `s` (bytes, 0 to 15) of the table of bytes `lo`, `hi`:
    /// of the lower 8 and the upper 8 of `s`.
    #[inline(always)]
    unsafe fn look(lo: uint8x16_t, hi: uint8x16_t, s: uint8x16_t) -> (uint16x8_t, uint16x8_t) {
        unsafe {
            let (l, h) = (vqtbl1q_u8(lo, s), vqtbl1q_u8(hi, s));
            (vreinterpretq_u16_u8(vzip1q_u8(l, h)), vreinterpretq_u16_u8(vzip2q_u8(l, h)))
        }
    }

    /// A table as the encoder takes it: by symbol, the bytes of the frequencies (low, high), of
    /// the starts and of the 32-bit halves of the dividers' magic numbers ([`super::EncSym`]).
    pub(super) struct Enc {
        freq: [uint8x16_t; 2],
        cum: [uint8x16_t; 2],
        lo: [uint8x16_t; 4],
        hi: [uint8x16_t; 4],
    }

    impl Enc {
        pub(super) fn new(e: &[super::EncSym; 16]) -> Enc {
            let b = |f: &dyn Fn(&super::EncSym) -> u64, k: u32| {
                let a: [u8; 16] = std::array::from_fn(|s| (f(&e[s]) >> (8 * k)) as u8);
                // SAFETY: AArch64 has NEON always.
                unsafe { vld1q_u8(a.as_ptr()) }
            };
            Enc {
                freq: std::array::from_fn(|k| b(&|e| e.freq as u64, k as u32)),
                cum: std::array::from_fn(|k| b(&|e| e.cum as u64, k as u32)),
                lo: std::array::from_fn(|k| b(&|e| e.magic, k as u32)),
                hi: std::array::from_fn(|k| b(&|e| e.magic, 4 + k as u32)),
            }
        }
    }

    /// The 32-bit values of the symbols `s` (bytes, 0 to 15) of the table of bytes `t`, of 16
    /// lanes, 4 to a vector.
    #[inline(always)]
    unsafe fn look32(t: &[uint8x16_t; 4], s: uint8x16_t) -> [uint32x4_t; 4] {
        unsafe {
            let (l, h) = look(t[0], t[1], s);
            let (l2, h2) = look(t[2], t[3], s);
            let z = |a: uint16x8_t, b: uint16x8_t| (vzip1q_u16(a, b), vzip2q_u16(a, b));
            let (a, b) = z(l, l2);
            let (c, d) = z(h, h2);
            [a, b, c, d].map(|v| vreinterpretq_u32_u16(v))
        }
    }

    /// The encoder of a full group (FORMAT.md §3), lanes from the last: as the scalar one, its
    /// words put out after `w` in `words` (which has room for 4 more past a group's).
    #[allow(clippy::needless_range_loop)]
    pub(super) unsafe fn encode(x: &mut [u32; LANES], t: &Enc, syms: &[u8], words: &mut [u16], w: &mut usize) {
        unsafe {
            // (the frequencies, starts and magic numbers of the lanes, 4 to a vector)
            let mut fq = [vdupq_n_u32(0); 8];
            let mut cm = [vdupq_n_u32(0); 8];
            let mut ml = [vdupq_n_u32(0); 8];
            let mut mh = [vdupq_n_u32(0); 8];
            for h in 0..2 {
                let s = vld1q_u8(syms.as_ptr().add(16 * h));
                let (fa, fb) = look(t.freq[0], t.freq[1], s);
                let (ca, cb) = look(t.cum[0], t.cum[1], s);
                for (j, (f, c)) in [(fa, ca), (fb, cb)].into_iter().enumerate() {
                    fq[4 * h + 2 * j] = vmovl_u16(vget_low_u16(f));
                    fq[4 * h + 2 * j + 1] = vmovl_high_u16(f);
                    cm[4 * h + 2 * j] = vmovl_u16(vget_low_u16(c));
                    cm[4 * h + 2 * j + 1] = vmovl_high_u16(c);
                }
                ml[4 * h..4 * h + 4].copy_from_slice(&look32(&t.lo, s));
                mh[4 * h..4 * h + 4].copy_from_slice(&look32(&t.hi, s));
            }
            let zero = vdupq_n_u32(0);
            let mut o = *w;
            for k in (0..8).rev() {
                let (f, c) = (fq[k], cm[k]);
                let mut v = vld1q_u32(x.as_ptr().add(4 * k));
                // (a word out where v >= f << 17, that is v >> 17 >= f)
                let put = vcgeq_u32(vshrq_n_u32::<17>(v), f);
                let m = bits(put);
                let out = vqtbl1q_u8(vreinterpretq_u8_u32(v), vld1q_u8(PUT[m].as_ptr()));
                vst1_u8(words.as_mut_ptr().add(o) as *mut u8, vget_low_u8(out));
                o += m.count_ones() as usize;
                v = vbslq_u32(put, vshrq_n_u32::<16>(v), v);
                // (v / f: (magic * v) >> 64, of the magic's halves; a magic of 0, of a frequency
                // of 1, leaves v)
                let (lo, hi) = (ml[k], mh[k]);
                let a = vshrq_n_u64::<32>(vmull_u32(vget_low_u32(v), vget_low_u32(lo)));
                let b = vshrq_n_u64::<32>(vmull_high_u32(v, lo));
                let a = vmlal_u32(a, vget_low_u32(v), vget_low_u32(hi));
                let b = vmlal_high_u32(b, v, hi);
                let q = vshrn_high_n_u64::<32>(vshrn_n_u64::<32>(a), b);
                let q = vbslq_u32(vceqq_u32(hi, zero), v, q);
                let r = vmlsq_u32(v, q, f);
                let nx = vaddq_u32(vaddq_u32(vshlq_n_u32::<{ PROB_BITS as i32 }>(q), r), c);
                vst1q_u32(x.as_mut_ptr().add(4 * k), nx);
            }
            *w = o;
        }
    }

    // (the 8 vectors of the states by index, as the lanes they hold)
    #[allow(clippy::needless_range_loop)]
    pub(super) unsafe fn groups(x: &mut [u32; LANES], t: &Table, words: &[u8], pos: &mut usize, out: &mut [u8]) {
        unsafe {
            // (the slots and the cumulative frequencies, below 2^15 and to 2^15, compared as 16
            // bits)
            let c: [uint16x8_t; 15] = std::array::from_fn(|k| vdupq_n_u16(t.cum[k + 1] as u16));
            let (f0, f1) = bytes(&t.freq);
            let (s0, s1) = bytes(&t.cum[..16]);
            let mask = vdupq_n_u16(PROB as u16 - 1);
            let below = vdupq_n_u32(L);
            let mut st: [uint32x4_t; 8] = std::array::from_fn(|k| vld1q_u32(x.as_ptr().add(4 * k)));
            let mut p = *pos;
            let w = words.as_ptr();
            for g in out.as_chunks_mut::<LANES>().0 {
                for h in 0..2 {
                    // (the slots of 16 lanes, and their symbols)
                    let sl: [uint16x8_t; 2] = std::array::from_fn(|j| {
                        let (a, b) =
                            (vreinterpretq_u16_u32(st[4 * h + 2 * j]), vreinterpretq_u16_u32(st[4 * h + 2 * j + 1]));
                        vandq_u16(vuzp1q_u16(a, b), mask)
                    });
                    let sy: [uint16x8_t; 2] = std::array::from_fn(|j| {
                        let mut s = vdupq_n_u16(0);
                        for ck in &c {
                            // (all ones where slot >= cum)
                            s = vsubq_u16(s, vcgeq_u16(sl[j], *ck));
                        }
                        s
                    });
                    let s8 = vuzp1q_u8(vreinterpretq_u8_u16(sy[0]), vreinterpretq_u8_u16(sy[1]));
                    vst1q_u8(g.as_mut_ptr().add(16 * h), s8);
                    let (fa, fb) = look(f0, f1, s8);
                    let (sa, sb) = look(s0, s1, s8);
                    // (the slots less the starts, and the frequencies, of 8 lanes each)
                    let d = [vsubq_u16(sl[0], sa), vsubq_u16(sl[1], sb)];
                    let fr = [fa, fb];
                    for k in 0..4 {
                        let (j, high) = (k / 2, k % 2 == 1);
                        let (f, dd) = if high {
                            (vmovl_high_u16(fr[j]), vmovl_high_u16(d[j]))
                        } else {
                            (vmovl_u16(vget_low_u16(fr[j])), vmovl_u16(vget_low_u16(d[j])))
                        };
                        let i = 4 * h + k;
                        let v = vmlaq_u32(dd, f, vshrq_n_u32::<{ PROB_BITS as i32 }>(st[i]));
                        let need = vcltq_u32(v, below);
                        let m = bits(need);
                        let ws = vcombine_u8(vld1_u8(w.add(p)), vdup_n_u8(0));
                        let ws = vreinterpretq_u32_u8(vqtbl1q_u8(ws, vld1q_u8(TAKE[m].as_ptr())));
                        st[i] = vbslq_u32(need, vorrq_u32(vshlq_n_u32::<16>(v), ws), v);
                        p += 2 * m.count_ones() as usize;
                    }
                }
            }
            for k in 0..8 {
                vst1q_u32(x.as_mut_ptr().add(4 * k), st[k]);
            }
            *pos = p;
        }
    }
}

#[cfg(target_family = "wasm")]
mod simd128 {
    use super::{L, LANES, PROB, PROB_BITS, Table};
    use core::arch::wasm32::*;

    /// Of each mask of 4 lanes, the bytes of the words (of 4, widened to 32 bits) each lane
    /// takes: the lanes below it that take one.
    static TAKE: [[u8; 16]; 16] = {
        let mut t = [[0u8; 16]; 16];
        let mut m = 0;
        while m < 16 {
            let mut n = 0;
            let mut i = 0;
            while i < 4 {
                let mut b = 0;
                while b < 4 {
                    // (the word's two bytes, then zeros: indices past 15 give 0)
                    t[m][4 * i + b] = if b < 2 { (2 * n + b) as u8 } else { 0x80 };
                    b += 1;
                }
                if m >> i & 1 == 1 {
                    n += 1;
                }
                i += 1;
            }
            m += 1;
        }
        t
    };

    #[target_feature(enable = "simd128")]
    pub(super) unsafe fn groups(x: &mut [u32; LANES], t: &Table, words: &[u8], pos: &mut usize, out: &mut [u8]) {
        unsafe {
            let c: [v128; 15] = core::array::from_fn(|k| u32x4_splat(t.cum[k + 1]));
            // (the frequencies and starts, 16 bits each, as tables of their low and high bytes)
            let bytes = |a: &[u32], sh: u32| -> v128 {
                let b: [u8; 16] = core::array::from_fn(|s| (a[s] >> sh) as u8);
                v128_load(b.as_ptr() as *const v128)
            };
            let (f0, f1, s0, s1) = (bytes(&t.freq, 0), bytes(&t.freq, 8), bytes(&t.cum, 0), bytes(&t.cum, 8));
            let mask = u32x4_splat(PROB - 1);
            let below = u32x4_splat(L);
            let zeros = u32x4_splat(0x8080_8000);
            let mut st: [v128; 8] = core::array::from_fn(|k| v128_load(x.as_ptr().add(4 * k) as *const v128));
            let mut p = *pos;
            let w = words.as_ptr();
            for g in out.chunks_exact_mut(LANES) {
                let mut syms = [u32x4_splat(0); 8];
                for k in 0..8 {
                    let v = st[k];
                    let slot = v128_and(v, mask);
                    let mut s = u32x4_splat(0);
                    for ck in &c {
                        s = i32x4_sub(s, u32x4_ge(slot, *ck));
                    }
                    let idx = v128_or(s, zeros);
                    let look =
                        |lo: v128, hi: v128| v128_or(i8x16_swizzle(lo, idx), i32x4_shl(i8x16_swizzle(hi, idx), 8));
                    let freq = look(f0, f1);
                    let start = look(s0, s1);
                    let v = i32x4_add(i32x4_mul(freq, u32x4_shr(v, PROB_BITS)), i32x4_sub(slot, start));
                    let need = u32x4_lt(v, below);
                    let m = i32x4_bitmask(need) as usize;
                    let ws = u32x4_extend_low_u16x8(v128_load64_zero(w.add(p) as *const u64));
                    // (the words as bytes, each lane's put in it)
                    let ws = i8x16_swizzle(u16x8_narrow_i32x4(ws, ws), v128_load(TAKE[m].as_ptr() as *const v128));
                    st[k] = v128_bitselect(v128_or(i32x4_shl(v, 16), ws), v, need);
                    p += 2 * m.count_ones() as usize;
                    syms[k] = s;
                }
                for h in 0..2 {
                    let a = u16x8_narrow_i32x4(syms[4 * h], syms[4 * h + 1]);
                    let b = u16x8_narrow_i32x4(syms[4 * h + 2], syms[4 * h + 3]);
                    v128_store(g.as_mut_ptr().add(16 * h) as *mut v128, u8x16_narrow_i16x8(a, b));
                }
            }
            for k in 0..8 {
                v128_store(x.as_mut_ptr().add(4 * k) as *mut v128, st[k]);
            }
            *pos = p;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        }
    }

    #[test]
    fn roundtrip_kernels() {
        let mut r = rng(7);
        let counts: Vec<[u64; 16]> = vec![
            std::array::from_fn(|s| if s < 3 { 1000 } else { 1 }),
            std::array::from_fn(|s| (s as u64 + 1) * 37),
            std::array::from_fn(|s| if s == 9 { 5 } else { 0 }),
            std::array::from_fn(|s| if s < 4 { 1 << s } else { 0 }),
        ];
        let tables: Vec<Table> = counts.iter().map(Table::of_counts).collect();
        let pick = |t: usize, r: &mut dyn FnMut() -> u64| loop {
            let s = (r() % 16) as usize;
            if tables[t].freq[s] > 0 && (r().is_multiple_of(4) || counts[t][s] > 10) {
                return s as u8;
            }
        };
        let mut enc = Encoder::new();
        let mut plan = Vec::new();
        for i in 0..3000 {
            let t = (r() % 4) as usize;
            let n = if i % 17 == 16 { (r() % 32 + 1) as usize } else { LANES };
            let syms: Vec<u8> = (0..n).map(|_| pick(t, &mut r)).collect();
            enc.group(t as u32, &syms);
            plan.push((t, syms));
        }
        let mut again = Encoder::new();
        for (t, syms) in &plan {
            again.group(*t as u32, syms);
        }
        let data = enc.finish_with(&tables, Kernel::Scalar);
        assert_eq!(again.finish(&tables), data, "the kernels' streams");
        let mut kernels = vec![Kernel::Scalar];
        if Kernel::best() != Kernel::Scalar {
            kernels.push(Kernel::best());
        }
        for k in kernels {
            let mut words = data[HEAD..].to_vec();
            words.resize(words.len() + 64, 0);
            let mut d = Decoder::new(&data, k).unwrap();
            let mut pos = 0;
            let mut out = [0u8; LANES];
            for (t, syms) in &plan {
                if syms.len() == LANES {
                    d.groups(&tables[*t], &words, &mut pos, &mut out);
                } else {
                    d.partial(&tables[*t], &words, &mut pos, &mut out[..syms.len()]);
                }
                assert_eq!(&out[..syms.len()], &syms[..], "{k:?}");
            }
            assert_eq!(pos, data.len() - HEAD);
            assert!(d.finish());
        }
    }

    #[test]
    fn normalised() {
        for counts in [[1u64; 16], std::array::from_fn(|s| if s == 0 { 1 << 40 } else { 1 }), {
            let mut c = [0; 16];
            c[3] = 1;
            c
        }] {
            let t = Table::of_counts(&counts);
            assert_eq!(t.cum[16], PROB);
            assert!((0..16).all(|s| (counts[s] > 0) == (t.freq[s] > 0)));
        }
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// The encoder's and decoder's speed on 64M symbols of a skewed table (`--ignored`, `-r`).
    #[test]
    #[ignore]
    fn speed() {
        let counts: [u64; 16] = std::array::from_fn(|s| 1 + 1000 / (1 + (s as i64 - 7).unsigned_abs()));
        let t = Table::of_counts(&counts);
        let mut s = 1u64;
        let syms: Vec<u8> = (0..64 << 20)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let mut r = (s >> 33) as u32 % PROB;
                let mut k = 0;
                while r >= t.freq[k] {
                    r -= t.freq[k];
                    k += 1;
                }
                k as u8
            })
            .collect();
        let m = syms.len() as f64 / 1e6;
        let mut kernels = vec![Kernel::Scalar];
        if Kernel::best() != Kernel::Scalar {
            kernels.push(Kernel::best());
        }
        for k in kernels {
            let at = std::time::Instant::now();
            let mut e = Encoder::new();
            e.groups(0, &syms);
            let gathered = at.elapsed();
            let data = e.finish_with(&[t], k);
            let enc = at.elapsed();
            let mut words = data[HEAD..].to_vec();
            words.resize(words.len() + 2 * 8192, 0);
            let mut out = vec![0u8; syms.len()];
            let at = std::time::Instant::now();
            let mut d = Decoder::new(&data, k).unwrap();
            let mut pos = 0;
            for c in out.chunks_mut(8192) {
                d.groups(&t, &words, &mut pos, c);
            }
            let dec = at.elapsed();
            assert_eq!(out, syms);
            println!(
                "{k:?}: {} bytes/sym; gather {:.0} Msym/s, encode {:.0} Msym/s, decode {:.0} Msym/s",
                data.len() as f64 / syms.len() as f64,
                m / gathered.as_secs_f64(),
                m / (enc - gathered).as_secs_f64(),
                m / dec.as_secs_f64(),
            );
        }
    }
}
