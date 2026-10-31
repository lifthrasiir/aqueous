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

/// The suffix array of `s`, whose symbols are below `k`, into `sa` (`s.len()` entries). A
/// suffix that is a prefix of another sorts first, as if a sentinel smaller than every symbol
/// ended the text.
pub fn suffix_array<T: Sym>(s: &[T], k: usize, sa: &mut [u32]) {
    let n = s.len();
    assert!(sa.len() == n && n < EMPTY as usize);
    match n {
        0 => return,
        1 => {
            sa[0] = 0;
            return;
        }
        _ => {}
    }
    // S-type (true) or L-type; the last is L, as the sentinel after it is smaller
    let mut stype = vec![false; n];
    for i in (0..n - 1).rev() {
        stype[i] = s[i] < s[i + 1] || (s[i] == s[i + 1] && stype[i + 1]);
    }
    let lms = |i: usize| i > 0 && stype[i] && !stype[i - 1];
    let mut count = vec![0u32; k];
    for &c in s {
        count[c.idx()] += 1;
    }
    let mut bkt = vec![0u32; k];

    // the LMS suffixes at the ends of their buckets, in any order, then the rest induced from
    // them: the LMS substrings come out sorted
    sa.fill(EMPTY);
    tails(&count, &mut bkt);
    for (i, c) in s.iter().enumerate().skip(1) {
        if lms(i) {
            let c = c.idx();
            bkt[c] -= 1;
            sa[bkt[c] as usize] = i as u32;
        }
    }
    induce(s, sa, &stype, &count, &mut bkt);

    // the sorted LMS substrings named, the names in text order a text of their own
    let mut sorted = Vec::new();
    sorted.extend(sa.iter().copied().filter(|&j| lms(j as usize)));
    let n1 = sorted.len();
    let name_at = sa;
    name_at.fill(EMPTY);
    let mut names = 0u32;
    let mut prev = None;
    for &p in &sorted {
        let p = p as usize;
        if prev.is_none_or(|q| !same_lms(s, &stype, q, p)) {
            names += 1;
            prev = Some(p);
        }
        name_at[p / 2] = names - 1;
    }
    let positions: Vec<u32> = (1..n).filter(|&i| lms(i)).map(|i| i as u32).collect();
    let s1: Vec<u32> = positions.iter().map(|&p| name_at[p as usize / 2]).collect();
    debug_assert_eq!(s1.len(), n1);
    drop(sorted);

    // the LMS suffixes sorted: directly where every name differs, else by recursion
    let mut sa1 = vec![0u32; n1];
    if (names as usize) < n1 {
        suffix_array(&s1, names as usize, &mut sa1);
    } else {
        for (i, &c) in s1.iter().enumerate() {
            sa1[c as usize] = i as u32;
        }
    }
    drop(s1);

    // and the rest induced from them, placed in that order at the ends of their buckets
    let sa = name_at;
    sa.fill(EMPTY);
    tails(&count, &mut bkt);
    for &r in sa1.iter().rev() {
        let p = positions[r as usize];
        let c = s[p as usize].idx();
        bkt[c] -= 1;
        sa[bkt[c] as usize] = p;
    }
    drop((sa1, positions));
    induce(s, sa, &stype, &count, &mut bkt);
}

fn heads(count: &[u32], bkt: &mut [u32]) {
    let mut sum = 0;
    for (b, &c) in bkt.iter_mut().zip(count) {
        *b = sum;
        sum += c;
    }
}

fn tails(count: &[u32], bkt: &mut [u32]) {
    let mut sum = 0;
    for (b, &c) in bkt.iter_mut().zip(count) {
        sum += c;
        *b = sum;
    }
}

/// The L-type suffixes induced from the LMS ones in `sa`, left to right, then the S-type ones
/// from those, right to left.
fn induce<T: Sym>(s: &[T], sa: &mut [u32], stype: &[bool], count: &[u32], bkt: &mut [u32]) {
    let n = s.len();
    heads(count, bkt);
    // (the suffix before the sentinel)
    let c = s[n - 1].idx();
    sa[bkt[c] as usize] = (n - 1) as u32;
    bkt[c] += 1;
    for i in 0..n {
        let j = sa[i];
        if j != EMPTY && j > 0 && !stype[j as usize - 1] {
            let c = s[j as usize - 1].idx();
            sa[bkt[c] as usize] = j - 1;
            bkt[c] += 1;
        }
    }
    tails(count, bkt);
    for i in (0..n).rev() {
        let j = sa[i];
        if j != EMPTY && j > 0 && stype[j as usize - 1] {
            let c = s[j as usize - 1].idx();
            bkt[c] -= 1;
            sa[bkt[c] as usize] = j - 1;
        }
    }
}

/// Whether the LMS substrings at `a` and `b` (up to the next LMS position, with it) are equal,
/// in symbols and types. The one reaching the sentinel equals no other.
fn same_lms<T: Sym>(s: &[T], stype: &[bool], a: usize, b: usize) -> bool {
    let n = s.len();
    for d in 0.. {
        let (x, y) = (a + d, b + d);
        if x == n || y == n || s[x] != s[y] || stype[x] != stype[y] {
            return false;
        }
        if d > 0 && stype[x] && !stype[x - 1] {
            // (y is LMS too: their types agree here and before)
            return true;
        }
    }
    unreachable!()
}

/// The Burrows-Wheeler transform of `block`'s rotations: the last symbols of the rotations
/// sorted, and the rank of the block itself among them. Equal rotations (of a periodic block)
/// sort by descending start, as suffixes of the block written twice do; libbzip2 may order
/// them otherwise, which changes only the rank.
pub fn bwt(block: &[u8]) -> (Vec<u8>, u32) {
    let n = block.len();
    if n == 0 {
        return (Vec::new(), 0);
    }
    let twice = [block, block].concat();
    let mut sa = vec![0u32; 2 * n];
    suffix_array(&twice, 256, &mut sa);
    drop(twice);
    let mut last = Vec::with_capacity(n);
    let mut orig = 0;
    for &j in &sa {
        let j = j as usize;
        if j < n {
            if j == 0 {
                orig = last.len() as u32;
            }
            last.push(block[(j + n - 1) % n]);
        }
    }
    (last, orig)
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
                suffix_array(&s, 256, &mut sa);
                assert_eq!(sa, naive_sa(&s), "{s:?}");
            }
        }
        for s in [&b"mississippi"[..], b"abracadabra", b"aaaaaaaaaaaa", b"abababababab", b"ba", b"aab"] {
            let mut sa = vec![0; s.len()];
            suffix_array(s, 256, &mut sa);
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
