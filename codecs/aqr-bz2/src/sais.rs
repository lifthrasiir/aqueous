//! Suffix arrays by induced sorting (SA-IS: Nong, Zhang and Chan, "Two Efficient Algorithms
//! for Linear Time Suffix Array Construction", 2011), and the Burrows-Wheeler transform of a
//! block's rotations from them.

const EMPTY: u32 = u32::MAX;

/// A symbol of a text: bytes at the top level, names of substrings below it.
pub trait Sym: Copy + Ord {
    fn idx(self) -> usize;
}

impl Sym for u8 {
    #[inline(always)]
    fn idx(self) -> usize {
        self as usize
    }
}

impl Sym for u32 {
    #[inline(always)]
    fn idx(self) -> usize {
        self as usize
    }
}

/// Words of the buckets [`suffix_array`] takes for a text of `n` symbols below `k` (besides a
/// byte of type for each symbol): those of the level below (a text of at most `n / 2` symbols,
/// of fewer names than symbols) taking the same words in turn.
pub const fn work(n: usize, k: usize) -> usize {
    if k > n / 2 { k } else { n / 2 }
}

/// The suffix array of `s`, whose symbols are below `k`, into `sa` (`s.len()` entries), with
/// `ws` (at least [`work`] words) and `st` (a byte for each symbol) to work in. A suffix that is a prefix of another sorts first,
/// as if a sentinel smaller than every symbol ended the text.
///
/// The reduced text of a level and its suffix array are in `sa` (the text at its end, the array
/// at its start: the LMS positions are at most half the text); the types and the buckets are
/// made again after the level below, which takes the workspace from its start.
pub fn suffix_array<T: Sym>(s: &[T], k: usize, sa: &mut [u32], ws: &mut [u32], st: &mut [u8]) {
    let n = s.len();
    assert!(sa.len() == n && n < EMPTY as usize && ws.len() >= work(n, k) && st.len() >= n);
    match n {
        0 => return,
        1 => {
            sa[0] = 0;
            return;
        }
        _ => {}
    }
    let st = &mut st[..n];
    let bkt = &mut ws[..k];
    types(s, st);

    // the LMS suffixes at the ends of their buckets, in any order, then the rest induced from
    // them: the LMS substrings come out sorted
    sa.fill(EMPTY);
    buckets(s, bkt, true);
    for (i, c) in s.iter().enumerate().skip(1) {
        if lms(st, i) {
            let c = c.idx();
            bkt[c] -= 1;
            sa[bkt[c] as usize] = i as u32;
        }
    }
    induce(s, sa, st, bkt);

    // the sorted LMS substrings at the start, named after them (at half their positions), the
    // names in text order a text of their own at the end
    let mut n1 = 0;
    for i in 0..n {
        let j = sa[i];
        if lms(st, j as usize) {
            sa[n1] = j;
            n1 += 1;
        }
    }
    sa[n1..].fill(EMPTY);
    let mut names = 0u32;
    let mut prev = None;
    for x in 0..n1 {
        let p = sa[x] as usize;
        if prev.is_none_or(|q| !same_lms(s, st, q, p)) {
            names += 1;
            prev = Some(p);
        }
        sa[n1 + p / 2] = names - 1;
    }
    let mut j = n;
    for i in (n1..n).rev() {
        if sa[i] != EMPTY {
            j -= 1;
            sa[j] = sa[i];
        }
    }

    // the LMS suffixes sorted: directly where every name differs, else by recursion
    let (sa1, s1) = sa.split_at_mut(n - n1);
    let sa1 = &mut sa1[..n1];
    if (names as usize) < n1 {
        suffix_array(&*s1, names as usize, sa1, ws, &mut st[..n1]);
    } else {
        for (i, &c) in s1.iter().enumerate() {
            sa1[c as usize] = i as u32;
        }
    }

    // their positions in place of the reduced text, and those in the order sorted
    let bkt = &mut ws[..k];
    types(s, st);
    let mut j = 0;
    for i in 1..n {
        if lms(st, i) {
            s1[j] = i as u32;
            j += 1;
        }
    }
    for r in sa1.iter_mut() {
        *r = s1[*r as usize];
    }

    // and the rest induced from them, placed in that order at the ends of their buckets (each
    // at or past where it is, those past it moved already)
    sa[n1..].fill(EMPTY);
    buckets(s, bkt, true);
    for i in (0..n1).rev() {
        let p = std::mem::replace(&mut sa[i], EMPTY);
        let c = s[p as usize].idx();
        bkt[c] -= 1;
        sa[bkt[c] as usize] = p;
    }
    induce(s, sa, st, bkt);
}

/// The types of the suffixes of `s` into `st`: S-type (1) or L-type (0); the last is L, as the
/// sentinel after it is smaller.
fn types<T: Sym>(s: &[T], st: &mut [u8]) {
    let n = s.len();
    st[n - 1] = 0;
    let mut next = false;
    for i in (0..n - 1).rev() {
        next = s[i] < s[i + 1] || (s[i] == s[i + 1] && next);
        st[i] = next as u8;
    }
}

#[inline(always)]
fn is_s(st: &[u8], i: usize) -> bool {
    st[i] != 0
}

#[inline(always)]
fn lms(st: &[u8], i: usize) -> bool {
    i > 0 && is_s(st, i) && !is_s(st, i - 1)
}

/// The buckets of the symbols of `s`: where each ends (`ends`) or starts.
fn buckets<T: Sym>(s: &[T], bkt: &mut [u32], ends: bool) {
    bkt.fill(0);
    for &c in s {
        bkt[c.idx()] += 1;
    }
    let mut sum = 0;
    for b in bkt.iter_mut() {
        let c = *b;
        *b = if ends { sum + c } else { sum };
        sum += c;
    }
}

/// The L-type suffixes induced from the LMS ones in `sa`, left to right, then the S-type ones
/// from those, right to left.
fn induce<T: Sym>(s: &[T], sa: &mut [u32], st: &[u8], bkt: &mut [u32]) {
    let n = s.len();
    buckets(s, bkt, false);
    // (the suffix before the sentinel)
    let c = s[n - 1].idx();
    sa[bkt[c] as usize] = (n - 1) as u32;
    bkt[c] += 1;
    for i in 0..n {
        let j = sa[i];
        if j != EMPTY && j > 0 && !is_s(st, j as usize - 1) {
            let c = s[j as usize - 1].idx();
            sa[bkt[c] as usize] = j - 1;
            bkt[c] += 1;
        }
    }
    buckets(s, bkt, true);
    for i in (0..n).rev() {
        let j = sa[i];
        if j != EMPTY && j > 0 && is_s(st, j as usize - 1) {
            let c = s[j as usize - 1].idx();
            bkt[c] -= 1;
            sa[bkt[c] as usize] = j - 1;
        }
    }
}

/// Whether the LMS substrings at `a` and `b` (up to the next LMS position, with it) are equal,
/// in symbols and types. The one reaching the sentinel equals no other.
fn same_lms<T: Sym>(s: &[T], st: &[u8], a: usize, b: usize) -> bool {
    let n = s.len();
    for d in 0.. {
        let (x, y) = (a + d, b + d);
        if x == n || y == n || s[x] != s[y] || is_s(st, x) != is_s(st, y) {
            return false;
        }
        if d > 0 && lms(st, x) {
            // (y is LMS too: their types agree here and before)
            return true;
        }
    }
    unreachable!()
}

/// Words of [`bwt_in`]'s workspace for a block of `n` symbols.
pub const fn bwt_work(n: usize) -> usize {
    work(2 * n, 256)
}

/// The Burrows-Wheeler transform of a block's rotations, of `twice` (the block written twice,
/// `2n` symbols), in `sa` (`2n` words) with `ws` ([`bwt_work`] words) and `st` (`2n` bytes) to
/// work in: the last
/// symbols of the rotations sorted in `sa[..n]`, and the rank of the block itself among them.
/// Equal rotations (of a periodic block) sort by descending start, as suffixes of the block
/// written twice do; libbzip2 may order them otherwise, which changes only the rank.
pub fn bwt_in(twice: &[u8], sa: &mut [u32], ws: &mut [u32], st: &mut [u8]) -> u32 {
    let n = twice.len() / 2;
    suffix_array(twice, 256, sa, ws, st);
    let (mut k, mut orig) = (0, 0);
    for i in 0..2 * n {
        let j = sa[i] as usize;
        if j < n {
            if j == 0 {
                orig = k as u32;
            }
            sa[k] = twice[j + n - 1] as u32;
            k += 1;
        }
    }
    orig
}

/// [`bwt_in`] of `block`, the last symbols as bytes.
pub fn bwt(block: &[u8]) -> (Vec<u8>, u32) {
    let n = block.len();
    if n == 0 {
        return (Vec::new(), 0);
    }
    let twice = [block, block].concat();
    let mut sa = vec![0u32; 2 * n];
    let orig = bwt_in(&twice, &mut sa, &mut vec![0; bwt_work(n)], &mut vec![0; 2 * n]);
    (sa[..n].iter().map(|&c| c as u8).collect(), orig)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_sa(s: &[u8]) -> Vec<u32> {
        let mut v: Vec<u32> = (0..s.len() as u32).collect();
        v.sort_by(|&a, &b| s[a as usize..].cmp(&s[b as usize..]));
        v
    }

    #[test]
    fn against_sorting() {
        let mut x = 12345u32;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for n in 0..300 {
            for k in [1u32, 2, 3, 4, 26, 256] {
                let s: Vec<u8> = (0..n).map(|_| (rnd() % k) as u8).collect();
                let mut sa = vec![0; n];
                suffix_array(&s, 256, &mut sa, &mut vec![0; work(n, 256)], &mut vec![0; n]);
                assert_eq!(sa, naive_sa(&s), "{s:?}");
            }
        }
        for s in [&b"mississippi"[..], b"abracadabra", b"aaaaaaaaaaaa", b"abababababab", b"ba", b"aab"] {
            let mut sa = vec![0; s.len()];
            suffix_array(s, 256, &mut sa, &mut vec![0; work(s.len(), 256)], &mut vec![0; s.len()]);
            assert_eq!(sa, naive_sa(s));
        }
    }

    #[test]
    fn rotations() {
        let (l, o) = bwt(b"banana");
        assert_eq!((&l[..], o), (&b"nnbaaa"[..], 3));
        let mut x = 99u32;
        for n in 1..200usize {
            let b: Vec<u8> = (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1103515245).wrapping_add(12345);
                    b"abc"[(x >> 16) as usize % 3]
                })
                .collect();
            let mut rot: Vec<usize> = (0..n).collect();
            let r = |i: usize| [&b[i..], &b[..i]].concat();
            rot.sort_by(|&p, &q| r(p).cmp(&r(q)).then(q.cmp(&p)));
            let want: Vec<u8> = rot.iter().map(|&i| b[(i + n - 1) % n]).collect();
            let o = rot.iter().position(|&i| i == 0).unwrap() as u32;
            assert_eq!(bwt(&b), (want, o), "{b:?}");
        }
    }
}
