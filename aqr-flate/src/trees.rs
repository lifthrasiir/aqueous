//! Huffman code builders of known encoders, as candidates for predicting dynamic headers
//! (FORMAT.md, "Header candidates"): zopfli's length-limited package-merge, zopfli's and
//! brotli's (as ECT uses it) smoothing of counts, zopfli's / ECT's run-length coding of code
//! lengths, and zlib's code lengths.

use crate::huffman::RleOp;

/// zopfli's `ZopfliLengthLimitedCodeLengths` (boundary package-merge, after Katajainen,
/// Moffat and Turpin, with leaves sorted by weight then symbol), computed as the classic
/// package-merge over arrays, which gives the same lengths several times faster: at every
/// level the leaves merge with the packages of the level below, a package going first on
/// equal weights (as zopfli takes a leaf only when it is
/// strictly lighter); the lengths count the levels at which each leaf is among the items
/// needed.
pub fn package_merge(freqs: &[u64], maxbits: usize) -> Vec<u8> {
    // Without the limit biting, the plain Huffman code with the same tie breaking (much
    // cheaper) is the same; the tests check that against the package-merge below.
    if let Some(l) = huffman_lens(freqs, maxbits) {
        return l;
    }
    let n = freqs.len();
    let mut out = vec![0u8; n];
    let mut leaves: Vec<u64> = (0..n)
        .filter(|&i| freqs[i] > 0)
        .map(|i| freqs[i] << 9 | i as u64)
        .collect();
    let ns = leaves.len();
    if ns == 0 || (1usize << maxbits) < ns {
        return out;
    }
    if ns <= 2 {
        for &l in &leaves {
            out[(l & 511) as usize] = 1;
        }
        return out;
    }
    leaves.sort_unstable();
    let w: Vec<u64> = leaves.iter().map(|&l| l >> 9).collect();
    let levels = maxbits.min(ns - 1);
    // lists[j]: the merged list of level j (0: leaves only, the deepest). Only weights and
    // whether an item is a leaf are needed.
    // Items are weight << 1 | (is a leaf).
    let mut lists: Vec<Vec<u64>> = Vec::with_capacity(levels);
    lists.push(w.iter().map(|&x| x << 1 | 1).collect());
    for j in 1..levels {
        let prev = &lists[j - 1];
        let mut cur = Vec::with_capacity(ns + prev.len() / 2);
        let (mut li, mut pi) = (0, 0);
        let np = prev.len() / 2;
        while li < ns || pi < np {
            let pw = if pi < np {
                (prev[2 * pi] >> 1) + (prev[2 * pi + 1] >> 1)
            } else {
                u64::MAX
            };
            if li < ns && w[li] < pw {
                cur.push(w[li] << 1 | 1);
                li += 1;
            } else {
                cur.push(pw << 1);
                pi += 1;
            }
        }
        lists.push(cur);
    }
    // From the top level down: the first `need` items are used; their packages need twice as
    // many items from the level below. A leaf's length is the number of levels using it.
    let mut need = 2 * ns - 2;
    let mut count = vec![0usize; levels];
    for j in (0..levels).rev() {
        let items = &lists[j][..need.min(lists[j].len())];
        let leaves_used = items.iter().map(|&x| (x & 1) as usize).sum::<usize>();
        count[j] = leaves_used;
        need = 2 * (items.len() - leaves_used);
    }
    for (i, &l) in leaves.iter().enumerate() {
        out[(l & 511) as usize] = count.iter().filter(|&&c| i < c).count() as u8;
    }
    out
}

/// Optimal code lengths by Huffman's two-queue algorithm, leaves sorted by (weight,
/// symbol) and a package taken before a leaf of equal weight, as [`package_merge`] does; or
/// `None` if they exceed `maxbits`.
pub fn huffman_lens(freqs: &[u64], maxbits: usize) -> Option<Vec<u8>> {
    let n = freqs.len();
    let mut out = vec![0u8; n];
    let mut leaves: Vec<u64> = (0..n)
        .filter(|&i| freqs[i] > 0)
        .map(|i| freqs[i] << 9 | i as u64)
        .collect();
    let ns = leaves.len();
    if ns <= 2 {
        for &l in &leaves {
            out[(l & 511) as usize] = 1;
        }
        return Some(out);
    }
    leaves.sort_unstable();
    // Nodes 0..ns are the leaves, then the packages as made (in order of weight).
    let mut weight: Vec<u64> = leaves.iter().map(|&l| l >> 9).collect();
    let mut parent = vec![0u32; 2 * ns - 1];
    let (mut li, mut pi) = (0, ns);
    // The lightest leaf or package left (a package on equal weights).
    let take = |weight: &[u64], li: &mut usize, pi: &mut usize| -> usize {
        if *li < ns && (*pi >= weight.len() || weight[*li] < weight[*pi]) {
            *li += 1;
            *li - 1
        } else {
            *pi += 1;
            *pi - 1
        }
    };
    for k in ns..2 * ns - 1 {
        let a = take(&weight, &mut li, &mut pi);
        let b = take(&weight, &mut li, &mut pi);
        parent[a] = k as u32;
        parent[b] = k as u32;
        weight.push(weight[a] + weight[b]);
    }
    // Depths from the root down (packages are made after their children).
    let mut depth = vec![0u8; 2 * ns - 1];
    for k in (0..2 * ns - 2).rev() {
        depth[k] = depth[parent[k] as usize] + 1;
        if depth[k] as usize > maxbits {
            return None;
        }
    }
    for (i, &l) in leaves.iter().enumerate() {
        out[(l & 511) as usize] = depth[i];
    }
    Some(out)
}

/// zopfli's `PatchDistanceCodesForBuggyDecoders`: at least two distance codes.
pub fn patch_distances(d: &mut [u8]) {
    let used = d[..30].iter().filter(|&&l| l > 0).count();
    if used == 0 {
        d[0] = 1;
        d[1] = 1;
    } else if used == 1 {
        let i = if d[0] > 0 { 1 } else { 0 };
        d[i] = 1;
    }
}

/// The positions below `c.len()` that lie in a long enough run of equal counts (5 or more
/// zeros, 7 or more of anything else), as the smoothings treat them.
fn settled_positions(c: &[u64]) -> Vec<bool> {
    let mut settled = vec![false; c.len()];
    let mut start = 0;
    for run in c.chunk_by(|a, b| a == b) {
        if run.len() >= if run[0] == 0 { 5 } else { 7 } {
            settled[start..start + run.len()].fill(true);
        }
        start += run.len();
    }
    settled
}

/// zopfli's smoothing of `counts` (FORMAT.md, "Code lengths").
pub fn smooth_zopfli(counts: &mut [u64]) {
    let Some(len) = counts.iter().rposition(|&x| x != 0).map(|i| i + 1) else {
        return;
    };
    let settled = settled_positions(&counts[..len]);
    let (mut size, mut sum, mut reference) = (0usize, 0u64, counts[0]);
    for i in 0..=len {
        let here = counts.get(i).copied().filter(|_| i < len).unwrap_or(0);
        if i == len || settled[i] || here.abs_diff(reference) >= 4 {
            if size >= 4 || (size >= 3 && sum == 0) {
                let v = if sum == 0 {
                    0
                } else {
                    ((sum + size as u64 / 2) / size as u64).max(1)
                };
                counts[i - size..i].fill(v);
            }
            size = 0;
            sum = 0;
            reference = if i + 3 < len {
                (counts[i..i + 4].iter().sum::<u64>() + 2) / 4
            } else {
                here
            };
        }
        size += 1;
        sum += here;
    }
}

/// brotli's smoothing of `counts`, as ECT uses it (FORMAT.md, "Code lengths").
pub fn smooth_brotli(counts: &mut [u64]) {
    let Some(len) = counts.iter().rposition(|&x| x != 0).map(|i| i + 1) else {
        return;
    };
    let settled = settled_positions(&counts[..len]);
    // Counts past the end read as 0; references are on a scale of 256.
    let at = |c: &[u64], i: usize| c.get(i).copied().unwrap_or(0) as i128;
    let (mut size, mut sum) = (0i128, 0i128);
    let mut reference = 256 * (at(counts, 0) + at(counts, 1) + at(counts, 2)) / 3 + 420;
    for i in 0..=len {
        let here = if i < len { at(counts, i) } else { 0 };
        if i == len
            || settled[i]
            || (i > 0 && settled[i - 1])
            || (256 * here - reference).abs() >= 1240
        {
            if size >= 4 {
                let mut v = (sum + size / 2) / size;
                if v < 1 && sum != 0 {
                    v = 1;
                }
                counts[i - size as usize..i].fill(v as u64);
            }
            size = 0;
            sum = 0;
            reference = if i + 2 < len {
                256 * (at(counts, i) + at(counts, i + 1) + at(counts, i + 2)) / 3 + 420
            } else {
                256 * at(counts, i)
            };
        }
        size += 1;
        if i < len {
            sum += here;
            if size >= 4 {
                reference = (256 * sum + size / 2) / size + if size == 4 { 120 } else { 0 };
            }
        }
    }
}

/// zopfli's / ECT's run-length coding of `lens` (FORMAT.md, "Run-length codings") with the
/// flags `use_16`, `use_17`, `use_18`, `fuse_8` and `fuse_7`.
pub fn encode_tree(lens: &[u8], [use_16, use_17, use_18, fuse_8, fuse_7]: [bool; 5]) -> Vec<RleOp> {
    let op = |sym: u8, extra: usize| RleOp {
        sym,
        extra: extra as u8,
    };
    let mut out = vec![];
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let whole = lens[i..].iter().take_while(|&&l| l == v).count();
        let mut r = if use_16 || (v == 0 && (use_17 || use_18)) {
            whole
        } else {
            1
        };
        i += r;
        if v == 0 && r >= 3 {
            if use_18 {
                while r >= 11 {
                    let k = r.min(138);
                    out.push(op(18, k - 11));
                    r -= k;
                }
            }
            if use_17 {
                while r >= 3 {
                    let k = r.min(10);
                    out.push(op(17, k - 3));
                    r -= k;
                }
            }
        }
        if use_16 && r >= 4 {
            out.push(op(v, 0));
            r -= 1;
            while r >= 3 {
                if fuse_8 && r == 8 {
                    out.extend([op(16, 1); 2]);
                    r = 0;
                } else if fuse_7 && r == 7 {
                    out.extend([op(16, 1), op(16, 0)]);
                    r = 0;
                } else {
                    let k = r.min(6);
                    out.push(op(16, k - 3));
                    r -= k;
                }
            }
        }
        out.extend(std::iter::repeat_n(op(v, 0), r));
    }
    out
}

/// Number of flag combinations of zopfli/ECT's run-length coder ([`zopfli_ops`]): ECT's
/// fusing only with code 16 in use.
pub const ZOPFLI_FLAGS: usize = 20;

/// The `k`-th run-length coding of `lens` (cut to HLIT and HDIST) by zopfli/ECT, by flag
/// combination.
pub fn zopfli_ops(lens: &[u8], k: usize) -> Option<Vec<RleOp>> {
    let i = (0..32u32).filter(|i| i & 1 != 0 || i & 24 == 0).nth(k)?;
    let flags = [i & 1 != 0, i & 2 != 0, i & 4 != 0, i & 8 != 0, i & 16 != 0];
    Some(encode_tree(lens, flags))
}

/// zlib's code lengths, as [`crate::huffman::zlib_lens`] gives them; this one is on the hot
/// path of header coding and should be fast.
pub fn zlib_lens(freq: &[u32], max_length: u8) -> Vec<u8> {
    const NODES: usize = 2 * 288;
    let n = freq.len();
    let max_len = max_length as usize;
    if !(2..=288).contains(&n) || !(1..=15).contains(&max_len) {
        return crate::huffman::zlib_lens(freq, max_length);
    }
    // A heap entry packs (frequency, depth, id) so that the weight, all that is compared,
    // is `entry >> 10`; frequencies stay below 2^41, depths below 2^9, ids below 2^10.
    let weight = |e: u64| e >> 10;
    let mut heap = [0u64; 290];
    let mut m = 0;
    let mut top: isize = -1;
    for (i, &f) in freq.iter().enumerate() {
        if f != 0 {
            m += 1;
            heap[m] = (f as u64) << 19 | i as u64;
            top = i as isize;
        }
    }
    while m < 2 {
        let sym = if top < 2 {
            top += 1;
            top as usize
        } else {
            0
        };
        m += 1;
        heap[m] = 1 << 19 | sym as u64;
    }
    let sink = |heap: &mut [u64; 290], m: usize, mut k: usize| {
        let node = heap[k];
        while 2 * k <= m {
            let mut c = 2 * k;
            if c < m && weight(heap[c + 1]) <= weight(heap[c]) {
                c += 1;
            }
            if weight(node) <= weight(heap[c]) {
                break;
            }
            heap[k] = heap[c];
            k = c;
        }
        heap[k] = node;
    };
    for k in (1..=m / 2).rev() {
        sink(&mut heap, m, k);
    }
    let mut parent = [0u16; NODES];
    let mut list = [0u16; NODES];
    let mut listed = 0;
    let mut next = n;
    while m >= 2 {
        let x = heap[1];
        heap[1] = heap[m];
        m -= 1;
        sink(&mut heap, m, 1);
        let y = heap[1];
        let (x_id, y_id) = ((x & 1023) as usize, (y & 1023) as usize);
        list[listed] = x_id as u16;
        list[listed + 1] = y_id as u16;
        listed += 2;
        parent[x_id] = next as u16;
        parent[y_id] = next as u16;
        let depth = ((x >> 10) & 511).max((y >> 10) & 511) + 1;
        heap[1] = ((x >> 19) + (y >> 19)) << 19 | depth << 10 | next as u64;
        sink(&mut heap, m, 1);
        next += 1;
    }
    list[listed] = (heap[1] & 1023) as u16;
    listed += 1;

    let mut len = [0u8; NODES];
    let mut count = [0u16; 16];
    let mut overflow = 0i32;
    for &node in list[..listed - 1].iter().rev() {
        let mut l = len[parent[node as usize] as usize] + 1;
        if l as usize > max_len {
            l = max_len as u8;
            overflow += 1;
        }
        len[node as usize] = l;
        if node as isize <= top {
            count[l as usize] += 1;
        }
    }
    if overflow > 0 {
        while overflow > 0 {
            let mut l = max_len - 1;
            while count[l] == 0 {
                l -= 1;
            }
            count[l] -= 1;
            count[l + 1] += 2;
            count[max_len] -= 1;
            overflow -= 2;
        }
        let mut bits = max_len;
        let mut left = count[bits];
        for &node in list[..listed].iter().filter(|&&x| x as isize <= top) {
            while left == 0 {
                bits -= 1;
                left = count[bits];
            }
            len[node as usize] = bits as u8;
            left -= 1;
        }
    }
    let mut out = vec![0u8; n];
    for &node in &list[..listed] {
        if node as isize <= top {
            out[node as usize] = len[node as usize];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Huffman's lengths are zopfli's package-merge's whenever they fit.
    #[test]
    fn huffman_as_package_merge() {
        let mut lcg = 7u64;
        let mut rnd = |n: u64| {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) % n
        };
        let (mut same, mut differ, mut deep) = (0, 0, 0);
        for case in 0..200_000 {
            let n = [286, 30, 19, 5][case % 4];
            let shape = rnd(5);
            let used = rnd(n as u64 + 1);
            let freq: Vec<u64> = (0..n)
                .map(|_| {
                    if rnd(n as u64 + 1) >= used {
                        return 0;
                    }
                    match shape {
                        0 => rnd(4) + 1,
                        1 => 1 << rnd(20),
                        2 => rnd(1000) + 1,
                        3 => [1, 1, 2, 3, 5, 8][rnd(6) as usize],
                        _ => (rnd(1 << 18) + 1) >> rnd(18),
                    }
                })
                .collect();
            let maxbits = if n == 19 { 7 } else { 15 };
            match huffman_lens(&freq, maxbits) {
                None => deep += 1,
                Some(h) if h == package_merge(&freq, maxbits) => same += 1,
                Some(_) => differ += 1,
            }
        }
        eprintln!("same {same}, differ {differ}, too deep {deep}");
        assert_eq!(differ, 0);
    }

    /// The fast zlib builder gives zlib's lengths, ties and overflows included.
    #[test]
    fn zlib_lens_as_zlib() {
        let mut lcg = 1u64;
        let mut rnd = |n: u64| {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) % n
        };
        for case in 0..40_000 {
            let elems = [286, 30, 19, 3][case % 4];
            let max_length = if elems == 19 { 7 } else { 15 };
            let shape = rnd(4);
            let used = rnd(elems as u64 + 1);
            let freq: Vec<u32> = (0..elems)
                .map(|_| {
                    if rnd(elems as u64 + 1) >= used {
                        return 0;
                    }
                    (match shape {
                        0 => rnd(4) + 1,
                        1 => 1 << rnd(20),
                        2 => rnd(1000) + 1,
                        _ => (rnd(1 << 18) + 1) >> rnd(18),
                    }) as u32
                })
                .collect();
            assert_eq!(
                zlib_lens(&freq, max_length),
                crate::huffman::zlib_lens(&freq, max_length),
                "{freq:?}"
            );
        }
    }

    /// Package-merge gives optimal length-limited codes: compare with the plain Huffman cost
    /// when the limit does not bind, and check the Kraft sum.
    #[test]
    fn package_merge_is_optimal() {
        let mut lcg = 11u32;
        for n in [3usize, 5, 19, 30, 288] {
            for _ in 0..20 {
                let f: Vec<u64> = (0..n)
                    .map(|_| {
                        lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        let r = lcg >> 16;
                        if r.is_multiple_of(4) {
                            0
                        } else {
                            (r % 1000) as u64 * (r % 7) as u64
                        }
                    })
                    .collect();
                let used = f.iter().filter(|&&x| x > 0).count();
                let l = package_merge(&f, 15);
                if used < 2 {
                    continue;
                }
                let kraft: f64 = l
                    .iter()
                    .filter(|&&x| x > 0)
                    .map(|&x| 0.5f64.powi(x as i32))
                    .sum();
                assert!((kraft - 1.0).abs() < 1e-9, "{kraft}");
                let cost: u64 = f.iter().zip(&l).map(|(&f, &l)| f * l as u64).sum();
                let f32: Vec<u32> = f.iter().map(|&x| x as u32).collect();
                if n <= 286 {
                    let z = crate::huffman::zlib_lens(&f32, 15);
                    let zc: u64 = f.iter().zip(&z).map(|(&f, &l)| f * l as u64).sum();
                    assert!(cost <= zc, "{cost} > {zc}");
                }
            }
        }
    }
}
