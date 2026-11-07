//! A match finder over the data: the nearest earlier position matching for a length, and the
//! longest match among the nearest few. Positions are added in order, so that a query at `pos`
//! sees only what lies before it, as when the stream was made, and no farther back than the
//! stream's dictionary allows (the chains are rings of that size).

pub use aqr_common::lz::lcp_up;

const NONE: u32 = u32::MAX;
const HASH_BITS: u32 = 20;
/// Positions by a hash of their next 3 bytes: a bucket's newest position, where its bytes are
/// those, is their newest occurrence (as with LZMA's encoders, which look no further).
const HASH3_BITS: u32 = 16;
/// The key of the chain of matches of medium length.
pub const MID: usize = 8;
/// The key of the chain of long matches.
pub const LONG: usize = 16;
/// Candidates looked at for the longest match on each of two chains: [`Mf::longest`]'s `depth`
/// at most, which the chains' lists of matches found are reserved for.
pub const DEPTH: usize = 16;

/// Positions by a hash of their next `K` bytes, the newest first.
struct Chain<const K: usize> {
    /// The newest `w` positions of each hash (1 or [`RECENT`]), the newest first, from
    /// `recent[base]`: a walk's first 16 candidates in one cache line, rather than one cache miss
    /// after another along `prev`. The candidates are the same whatever `w` is.
    recent: Vec<u32>,
    base: usize,
    w: usize,
    /// The position before each in its chain, by position modulo the ring's size: as many as
    /// there are positions where the data is shorter than the ring.
    prev: Vec<u32>,
    mask: usize,
    /// How far back a candidate may be.
    reach: usize,
    /// The matches [`Mf::longest`] went through on this chain, each longer than those nearer
    /// (its length and position).
    found: Vec<(u32, u32)>,
}

impl<const K: usize> Chain<K> {
    /// A chain whose list of matches found has room for `found` of them.
    fn new(ring: usize, len: usize, reach: usize, w: usize, found: usize) -> Self {
        let prev = vec![NONE; ring.min(len.max(1))];
        // The buckets on cache lines of 64 bytes (an allocation this large is often not).
        let recent = vec![NONE; (w << HASH_BITS) + 15];
        let base = recent.as_ptr().align_offset(64).min(15);
        Chain { recent, base, w, prev, mask: ring - 1, reach, found: Vec::with_capacity(found) }
    }

    #[inline(always)]
    fn bucket(&self, h: usize) -> &[u32] {
        &self.recent[self.base + h * self.w..][..self.w]
    }

    #[inline]
    fn hash(d: &[u8], p: usize) -> usize {
        let h = if K == 4 {
            u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as u64
        } else if K == 8 {
            u64::from_le_bytes(d[p..p + 8].try_into().unwrap())
        } else {
            let a = u64::from_le_bytes(d[p..p + 8].try_into().unwrap());
            let b = u64::from_le_bytes(d[p + K - 8..p + K].try_into().unwrap());
            a ^ b.rotate_left(29).wrapping_mul(0xff51_afd7_ed55_8ccd)
        };
        (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - HASH_BITS)) as usize
    }

    /// Asks for the bucket of position `p`, to be inserted soon.
    #[inline(always)]
    fn ahead(&self, d: &[u8], p: usize) {
        if p + K <= d.len() {
            prefetch_w(&self.recent[self.base + Self::hash(d, p) * self.w]);
        }
    }

    #[inline]
    fn insert(&mut self, d: &[u8], p: usize) {
        if p + K <= d.len() {
            let at = self.base + Self::hash(d, p) * self.w;
            self.prev[p & self.mask] = self.recent[at];
            if self.w == RECENT {
                let r: &mut [u32; RECENT] = (&mut self.recent[at..at + RECENT]).try_into().unwrap();
                r.copy_within(0..RECENT - 1, 1);
            }
            self.recent[at] = p as u32;
        }
    }

    /// The candidates for `pos`, nearest first, within reach.
    #[inline]
    fn walk(&self, d: &[u8], pos: usize) -> impl Iterator<Item = usize> {
        let r = self.bucket(Self::hash(d, pos));
        let (mut i, mut c) = (0, r[0]);
        std::iter::from_fn(move || {
            if c == NONE || pos - c as usize > self.reach {
                return None;
            }
            let cu = c as usize;
            i += 1;
            c = if i < r.len() { r[i] } else { self.prev[cu & self.mask] };
            Some(cu)
        })
    }

    /// The nearest candidate matching `n` bytes, among `limit`.
    fn nearest(&self, d: &[u8], pos: usize, n: usize, limit: usize) -> Option<usize> {
        self.walk(d, pos).take(limit).find(|&c| d[c + n - 1] == d[pos + n - 1] && lcp_up(d, c, pos, n) >= n)
    }

    /// Goes through `depth` candidates for matches longer than `best`, up to `nice`, keeping
    /// them in `found`. Returns the longest, or 0 for none.
    fn longest(&mut self, d: &[u8], pos: usize, depth: usize, nice: usize, cap: usize, mut best: usize) -> usize {
        debug_assert!(depth <= self.found.capacity());
        let mut found = std::mem::take(&mut self.found);
        found.clear();
        if pos + K <= d.len() {
            for c in self.walk(d, pos).take(depth) {
                // A longer match must agree at `best` first.
                if best < cap && d[c + best] == d[pos + best] {
                    let n = lcp_up(d, c, pos, cap);
                    if n > best {
                        best = n;
                        found.push((n as u32, c as u32));
                        if n >= nice {
                            break;
                        }
                    }
                }
            }
        }
        let r = if found.is_empty() { 0 } else { best };
        self.found = found;
        r
    }
}

/// The newest positions of each hash kept together, on the chains [`Mf::longest`] goes through
/// (its `depth`): as many as a cache line of 64 bytes holds.
const RECENT: usize = 16;

/// Data from which those chains keep [`RECENT`] positions of each hash rather than one: below,
/// filling their tables (64 MiB each) takes longer than it saves.
const RECENT_FROM: usize = 1 << 20;

/// The positions kept of each hash on the chains [`Mf::longest`] goes through, for data of `n`
/// bytes. In a module, one always: the tables of 16 would be 128 MiB of its plan, to rebuild
/// twice as fast, and the candidates (so the record) are the same. Natively the speed is wanted
/// ([`blocks`] is the module's).
fn recent(n: usize) -> usize {
    if n >= RECENT_FROM && cfg!(not(target_arch = "wasm32")) { RECENT } else { 1 }
}

/// Bytes of the tables of a match finder over data of `n` bytes besides its rings.
#[cfg(not(target_arch = "wasm32"))]
fn tables(n: usize) -> usize {
    4 * ((2 * recent(n) + 1) << HASH_BITS) + 4 * (1 << 16) + 4 * (1 << HASH3_BITS)
}

/// The sizes in bytes of the allocations of a match finder in a module (one position of each
/// hash, [`recent`]) over data of `n` bytes for matches at most `reach` back ([`Mf::new`]), in
/// the order it makes them: the two heads, then each chain's ring, buckets and list of matches
/// found. They are all it holds (besides the stack's alignment of each to 16 bytes).
pub fn blocks(n: usize, reach: usize) -> [u64; 11] {
    let ring = reach.clamp(1, n.max(1)).next_power_of_two().min(n.max(1)) as u64;
    let chain = |w: usize, found: usize| [4 * ring, 4 * ((w << HASH_BITS) as u64 + 15), 8 * found as u64];
    let (short, mid, long) = (chain(1, DEPTH), chain(1, 0), chain(1, DEPTH));
    let [a, b, c, d, e, f, g, h, i] = [short, mid, long].concat().try_into().unwrap();
    [4 << 16, 4 << HASH3_BITS, a, b, c, d, e, f, g, h, i]
}

/// How far ahead [`Mf::advance`] asks for the buckets it will insert into: the data is all at
/// hand, so their cache misses overlap (at 8 or 16, inserting took 1.4 or 1.2 times as long).
const AHEAD: usize = 32;

/// Asks for the cache line of `x`, to be written soon.
#[inline(always)]
fn prefetch_w<T>(x: &T) {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a hint, on a reference.
    unsafe {
        std::arch::asm!("prfm pstl1keep, [{0}]", in(reg) x as *const T, options(nostack, preserves_flags, readonly))
    };
    #[cfg(target_arch = "x86_64")]
    // SAFETY: as above.
    unsafe {
        std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(x as *const T as *const i8)
    };
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let _ = x;
}

#[inline]
fn k2(d: &[u8], p: usize) -> usize {
    d[p] as usize | (d[p + 1] as usize) << 8
}

#[inline]
fn k3(d: &[u8], p: usize) -> usize {
    k2(d, p) | (d[p + 2] as usize) << 16
}

#[inline]
fn h3(d: &[u8], p: usize) -> usize {
    ((k3(d, p) as u32).wrapping_mul(0x9e37_79b1) >> (32 - HASH3_BITS)) as usize
}

pub struct Mf<'a> {
    d: &'a [u8],
    /// How far back a match may be: `pos - c` at most this.
    reach: usize,
    head2: Vec<u32>,
    head3: Vec<u32>,
    short: Chain<4>,
    mid: Chain<MID>,
    long: Chain<LONG>,
    next: usize,
    /// The position the chains' `found` are for.
    found_at: usize,
    /// Its chains let in by [`gate`], until dropped.
    #[cfg(not(target_arch = "wasm32"))]
    _admitted: gate::Admitted,
}

impl<'a> Mf<'a> {
    /// Bytes of the chains of a match finder of [`Mf::new`].
    #[cfg(not(target_arch = "wasm32"))]
    fn chains(n: usize, reach: usize) -> usize {
        // A ring of `reach` positions at least: a candidate `c` is at least `pos - reach`, and its
        // slot is taken again only by `c + ring`, which is at least `pos` (not added yet). So a
        // dictionary of 2^n bytes takes chains of 2^n, not 2^(n+1).
        let ring = reach.clamp(1, n.max(1)).next_power_of_two();
        3 * 4 * ring.min(n.max(1)) + tables(n)
    }

    /// How many match finders of [`Mf::new`] work at once (at least one; one in a module).
    pub fn at_once(n: usize, reach: usize) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        return (gate::AT_ONCE.load(std::sync::atomic::Ordering::Relaxed) / Self::chains(n, reach)).max(1);
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (n, reach);
            1
        }
    }

    /// A match finder for matches at most `reach` back (LZMA's dictionary size).
    pub fn new(d: &'a [u8], reach: usize) -> Self {
        Self::keeping(d, reach, recent(d.len()))
    }

    /// [`Mf::new`] keeping `w` positions of each hash together (1 or [`RECENT`]).
    fn keeping(d: &'a [u8], reach: usize, w: usize) -> Self {
        let ring = reach.clamp(1, d.len().max(1)).next_power_of_two();
        #[cfg(not(target_arch = "wasm32"))]
        let _admitted = gate::admit(Self::chains(d.len(), reach));
        Mf {
            d,
            reach,
            head2: vec![NONE; 1 << 16],
            head3: vec![NONE; 1 << HASH3_BITS],
            short: Chain::new(ring, d.len(), reach, w, DEPTH),
            mid: Chain::new(ring, d.len(), reach, 1, 0),
            long: Chain::new(ring, d.len(), reach, w, DEPTH),
            next: 0,
            found_at: usize::MAX,
            #[cfg(not(target_arch = "wasm32"))]
            _admitted,
        }
    }

    /// [`Mf::new`] holding no position before `from`: from `pos − reach` on, the same as one
    /// holding them all, as no query looks farther back.
    pub fn starting(d: &'a [u8], reach: usize, from: usize) -> Self {
        let mut mf = Mf::new(d, reach);
        mf.next = from;
        mf
    }

    /// Adds the positions before `pos`.
    pub fn advance(&mut self, pos: usize) {
        let d = self.d;
        for p in self.next..pos {
            self.short.ahead(d, p + AHEAD);
            self.long.ahead(d, p + AHEAD);
            self.mid.ahead(d, p + AHEAD);
            self.short.insert(d, p);
            self.mid.insert(d, p);
            self.long.insert(d, p);
            if p + 3 <= d.len() {
                self.head3[h3(d, p)] = p as u32;
            }
            if p + 2 <= d.len() {
                self.head2[k2(d, p)] = p as u32;
            }
        }
        self.next = self.next.max(pos);
    }

    /// The newest position with the same `k` (2 or 3) bytes as `pos`, within reach.
    #[inline]
    fn head(&self, k: usize, pos: usize) -> Option<usize> {
        let d = self.d;
        if pos + k > d.len() {
            return None;
        }
        let c = if k == 2 { self.head2[k2(d, pos)] } else { self.head3[h3(d, pos)] };
        (c != NONE && pos - c as usize <= self.reach && (k == 2 || k3(d, c as usize) == k3(d, pos)))
            .then_some(c as usize)
    }

    /// The nearest position before `pos` whose next `n` bytes are those at `pos`, looking at
    /// no more than `limit` candidates of a chain. Free up to what [`Mf::longest`] found at
    /// `pos`.
    pub fn nearest(&self, pos: usize, n: usize, limit: usize) -> Option<usize> {
        let d = self.d;
        if pos + n > d.len() || n < 2 {
            return None;
        }
        if n < 4 {
            return self.head(n, pos);
        }
        // Each chain is gone through nearest first, so the first match found as long is the
        // nearest.
        if self.found_at == pos {
            for found in [&self.short.found, &self.long.found] {
                if let Some(&(_, c)) = found.iter().find(|&&(l, _)| l as usize >= n) {
                    return Some(c as usize);
                }
            }
        }
        if n >= LONG {
            self.long.nearest(d, pos, n, limit)
        } else if n >= MID {
            self.mid.nearest(d, pos, n, limit)
        } else {
            self.short.nearest(d, pos, n, limit)
        }
    }

    /// After [`Mf::longest`] at `pos`: the matches there, each longer than all those nearer
    /// (their lengths and positions, at most `cap` long), as a binary-tree match finder gives
    /// them to an optimal parser. A length up to one's is nearest at its position.
    pub fn pairs(&self, pos: usize, cap: usize, out: &mut Vec<(u32, u32)>) {
        out.clear();
        out.extend(self.short.found.iter().chain(&self.long.found));
        for k in [2, 3] {
            if let Some(c) = self.head(k, pos) {
                out.push((lcp_up(self.d, c, pos, cap) as u32, c as u32));
            }
        }
        out.sort_unstable_by_key(|&(_, c)| std::cmp::Reverse(c));
        let mut top = 0;
        out.retain(|&(l, _)| {
            let keep = l > top;
            top = top.max(l);
            keep
        });
    }

    /// The length of the match at the newest candidate of 4 bytes (at most `cap`), or 2 or 3
    /// for the newest of 2 or 3 bytes: a cheap hint of what `pos` offers.
    pub fn first(&self, pos: usize, cap: usize) -> usize {
        let d = self.d;
        if pos + 4 <= d.len()
            && let Some(c) = self.short.walk(d, pos).next()
        {
            let n = lcp_up(d, c, pos, cap);
            if n >= 4 {
                return n;
            }
        }
        if self.head(3, pos).is_some() {
            3
        } else if self.head(2, pos).is_some() {
            2
        } else {
            0
        }
    }

    /// The longest match at `pos` (at most `cap`) among the nearest 2- and 3-byte matches,
    /// `depth` candidates of 4 bytes and `depth` of 16, stopping at one of `nice` bytes. A query
    /// of [`Mf::nearest`] at `pos` then costs nothing up to that length.
    pub fn longest(&mut self, pos: usize, depth: usize, nice: usize, cap: usize) -> usize {
        let d = self.d;
        self.found_at = pos;
        let mut best = if self.head(3, pos).is_some() {
            3
        } else if self.head(2, pos).is_some() {
            2
        } else {
            0
        };
        best = best.max(self.short.longest(d, pos, depth, nice, cap, best));
        if best < nice && best < cap {
            best = best.max(self.long.longest(d, pos, depth, nice, cap, LONG - 1));
        } else {
            self.long.found.clear();
        }
        best.min(cap)
    }
}

/// Lets the match finders of the process take up to `bytes` of chains at once ([`gate`]),
/// where that is more than they may already.
#[cfg(not(target_arch = "wasm32"))]
pub fn allow_chains(bytes: usize) {
    gate::AT_ONCE.fetch_max(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// The match finders' chains of a process at once, natively: one that would take them past
/// [`gate::AT_ONCE`] bytes, or past the budget of the process beside what is in use
/// ([`aqr_common::budget::global`]), waits for others to be dropped (one alone is let in
/// whatever its size and the budget), so that streams of large dictionaries split or rebuilt on
/// several threads (the folders of a 7z archive, the parts of a stream: 0.8 GB of chains each
/// for 64 MiB) take turns: one at a time by default, more as [`allow_chains`] and the budget let
/// in. Only the order of the work changes. A module, on a thread of its own, has none.
#[cfg(not(target_arch = "wasm32"))]
mod gate {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    use aqr_common::budget::Taken;

    pub static AT_ONCE: AtomicUsize = AtomicUsize::new(1 << 30);

    /// The bytes of the chains let in (changed within the budget's lock, or as chains are
    /// dropped, which then tells the budget).
    static HELD: AtomicUsize = AtomicUsize::new(0);

    /// Chains let in, until dropped.
    pub struct Admitted(usize, Option<Taken<'static>>);

    /// Waits for `n` bytes of chains to be let in.
    pub fn admit(n: usize) -> Admitted {
        let (_, t) = aqr_common::budget::global().wait(|room| {
            let held = HELD.load(Relaxed);
            let fits = held + n <= AT_ONCE.load(Relaxed) && room.fits(n as u64);
            (held == 0 || fits).then(|| {
                HELD.fetch_add(n, Relaxed);
                ((), n as u64)
            })
        });
        Admitted(n, Some(t))
    }

    impl Drop for Admitted {
        fn drop(&mut self) {
            // (before the budget is given back, which tells those waiting)
            HELD.fetch_sub(self.0, Relaxed);
            drop(self.1.take());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same matches whether the newest positions of each hash are kept together or not:
    /// longest, pairs and nearest, on data of few symbols (long chains, past the buckets).
    #[test]
    fn buckets() {
        const REACH: usize = 1 << 16;
        let mut r = aqr_common::rng::Rng::new(5);
        let d: Vec<u8> = (0..RECENT_FROM).map(|_| b"abc"[r.below(3) as usize]).collect();
        let (mut a, mut b) = (Mf::keeping(&d, REACH, RECENT), Mf::keeping(&d, REACH, 1));
        let (mut pa, mut pb) = (vec![], vec![]);
        for pos in (0..d.len()).step_by(97) {
            let cap = 273.min(d.len() - pos);
            a.advance(pos);
            b.advance(pos);
            assert_eq!(a.first(pos, cap), b.first(pos, cap), "{pos}");
            assert_eq!(a.longest(pos, 16, 64, cap), b.longest(pos, 16, 64, cap), "{pos}");
            a.pairs(pos, cap, &mut pa);
            b.pairs(pos, cap, &mut pb);
            assert_eq!(pa, pb, "{pos}");
            for n in [4, 6, MID, 12, LONG, 24] {
                assert_eq!(a.nearest(pos, n, 64), b.nearest(pos, n, 64), "{pos} {n}");
            }
        }
    }

    /// Chains of as many positions as a dictionary of 2^n bytes, which still find the nearest
    /// match within it, at its very end too (data of a few bytes, longer than the dictionary).
    #[test]
    fn rings() {
        const REACH: usize = 4096;
        let mut r = aqr_common::rng::Rng::new(3);
        let d: Vec<u8> = (0..4 * REACH).map(|_| b"ab"[r.below(2) as usize]).collect();
        let mut mf = Mf::new(&d, REACH);
        assert_eq!([mf.short.prev.len(), mf.mid.prev.len(), mf.long.prev.len()], [REACH; 3]);
        for pos in (REACH..d.len() - LONG).step_by(7) {
            mf.advance(pos);
            for n in [4, MID, LONG] {
                let want = (pos - REACH..pos).rev().find(|&c| lcp_up(&d, c, pos, n) >= n);
                assert_eq!(mf.nearest(pos, n, usize::MAX), want, "{pos} {n}");
            }
        }
    }
}
