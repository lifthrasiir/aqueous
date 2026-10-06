//! Huffman code builders of known encoders, as candidates for predicting dynamic headers.
//!
//! Bit-exact ports of zopfli's length-limited package-merge, of zopfli's and brotli's (as
//! used by ECT) count smoothing for run-length coding, and of zopfli/ECT's run-length coder
//! of code lengths.

use crate::huffman::RleOp;

/// zopfli's `ZopfliLengthLimitedCodeLengths`: boundary package-merge (Katajainen, Moffat,
/// Turpin) over `freqs`, with leaves sorted by (weight, symbol). A bit-exact port, kept as the
/// reference of [`package_merge`].
#[cfg(test)]
fn package_merge_boundary(freqs: &[u64], maxbits: usize) -> Vec<u8> {
    #[derive(Clone, Copy, Default)]
    struct Node {
        weight: u64,
        tail: u32,
        count: u32,
    }
    const NIL: u32 = u32::MAX;
    thread_local! {
        static POOL: std::cell::RefCell<Vec<Node>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    let n = freqs.len();
    let mut out = vec![0u8; n];
    // Leaves by (weight, symbol), packed in one key.
    let mut leaves: Vec<u64> = (0..n)
        .filter(|&i| freqs[i] > 0)
        .map(|i| freqs[i] << 9 | i as u64)
        .collect();
    let numsymbols = leaves.len();
    if numsymbols == 0 || (1usize << maxbits) < numsymbols {
        return out;
    }
    if numsymbols <= 2 {
        for &l in &leaves {
            out[(l & 511) as usize] = 1;
        }
        return out;
    }
    leaves.sort_unstable();
    let weight = |i: usize| leaves[i] >> 9;
    let maxbits = maxbits.min(numsymbols - 1);
    POOL.with_borrow_mut(|pool| {
        pool.clear();
        pool.push(Node {
            weight: weight(0),
            tail: NIL,
            count: 1,
        });
        pool.push(Node {
            weight: weight(1),
            tail: NIL,
            count: 2,
        });
        let mut lists = [[0u32, 1u32]; 16];
        let ns = numsymbols as u32;

        // BoundaryPM. The original recurses twice into the previous list after packaging; a
        // LIFO stack keeps its depth-first order, since every call sets up its node before
        // recursing.
        let mut stack: Vec<usize> = Vec::with_capacity(2 * maxbits);
        for _ in 0..2 * numsymbols - 5 {
            stack.push(maxbits - 1);
            while let Some(index) = stack.pop() {
                let lastcount = pool[lists[index][1] as usize].count;
                if index == 0 && lastcount >= ns {
                    continue;
                }
                let oldchain = lists[index][1];
                let newchain = pool.len() as u32;
                lists[index] = [oldchain, newchain];
                if index == 0 {
                    pool.push(Node {
                        weight: weight(lastcount as usize),
                        tail: NIL,
                        count: lastcount + 1,
                    });
                    continue;
                }
                let sum = pool[lists[index - 1][0] as usize].weight
                    + pool[lists[index - 1][1] as usize].weight;
                if lastcount < ns && sum > weight(lastcount as usize) {
                    let tail = pool[oldchain as usize].tail;
                    pool.push(Node {
                        weight: weight(lastcount as usize),
                        tail,
                        count: lastcount + 1,
                    });
                } else {
                    pool.push(Node {
                        weight: sum,
                        tail: lists[index - 1][1],
                        count: lastcount,
                    });
                    stack.push(index - 1);
                    stack.push(index - 1);
                }
            }
        }
        // BoundaryPMFinal.
        let index = maxbits - 1;
        let lastcount = pool[lists[index][1] as usize].count;
        let sum =
            pool[lists[index - 1][0] as usize].weight + pool[lists[index - 1][1] as usize].weight;
        if lastcount < ns && sum > weight(lastcount as usize) {
            let tail = pool[lists[index][1] as usize].tail;
            pool.push(Node {
                weight: 0,
                tail,
                count: lastcount + 1,
            });
            lists[index][1] = pool.len() as u32 - 1;
        } else {
            let last = lists[index][1] as usize;
            pool[last].tail = lists[index - 1][1];
        }
        // ExtractBitLengths.
        let mut counts = [0usize; 16];
        let mut end = 16;
        let mut node = lists[maxbits - 1][1];
        while node != NIL {
            end -= 1;
            counts[end] = pool[node as usize].count as usize;
            node = pool[node as usize].tail;
        }
        let mut val = counts[15];
        let mut ptr = 15;
        let mut value = 1;
        while ptr >= end {
            while val > counts[ptr - 1] {
                out[(leaves[val - 1] & 511) as usize] = value;
                val -= 1;
            }
            ptr -= 1;
            value += 1;
        }
    });
    out
}

/// zopfli's `ZopfliLengthLimitedCodeLengths` (boundary package-merge, after Katajainen,
/// Moffat and Turpin, with leaves sorted by weight then symbol), computed as the classic
/// package-merge over arrays, which gives the same lengths several times faster: at every level the leaves merge with the packages of the level
/// below, a package going first on equal weights (as zopfli takes a leaf only when it is
/// strictly lighter); the lengths count the levels at which each leaf is among the items
/// needed.
pub fn package_merge(freqs: &[u64], maxbits: usize) -> Vec<u8> {
    // Without the limit biting, the plain Huffman code with the same tie breaking (much
    // cheaper) is the same; the tests check that against zopfli's own algorithm.
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
            let pw = if pi < np { (prev[2 * pi] >> 1) + (prev[2 * pi + 1] >> 1) } else { u64::MAX };
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

/// zopfli's `OptimizeHuffmanForRle`.
pub fn smooth_zopfli(counts: &mut [u64]) {
    let Some(length) = counts.iter().rposition(|&c| c != 0).map(|i| i + 1) else {
        return;
    };
    let mut good = vec![false; length];
    let mut symbol = counts[0];
    let mut stride = 0usize;
    for i in 0..=length {
        if i == length || counts[i] != symbol {
            if (symbol == 0 && stride >= 5) || (symbol != 0 && stride >= 7) {
                for k in 0..stride {
                    good[i - k - 1] = true;
                }
            }
            stride = 1;
            if i != length {
                symbol = counts[i];
            }
        } else {
            stride += 1;
        }
    }
    let mut stride = 0u64;
    let mut limit = counts[0];
    let mut sum = 0u64;
    for i in 0..=length {
        if i == length || good[i] || counts[i].abs_diff(limit) >= 4 {
            if stride >= 4 || (stride >= 3 && sum == 0) {
                let count = if sum == 0 {
                    0
                } else {
                    ((sum + stride / 2) / stride).max(1)
                };
                for k in 0..stride as usize {
                    counts[i - k - 1] = count;
                }
            }
            stride = 0;
            sum = 0;
            limit = if i + 3 < length {
                (counts[i] + counts[i + 1] + counts[i + 2] + counts[i + 3] + 2) / 4
            } else if i < length {
                counts[i]
            } else {
                0
            };
        }
        stride += 1;
        if i != length {
            sum += counts[i];
        }
    }
}

/// brotli's `OptimizeHuffmanCountsForRle`, as ECT uses it.
pub fn smooth_brotli(counts: &mut [u64]) {
    let Some(length) = counts.iter().rposition(|&c| c != 0).map(|i| i + 1) else {
        return;
    };
    let mut good = vec![false; length];
    let mut symbol = counts[0];
    let mut stride = 0usize;
    for i in 0..=length {
        if i == length || counts[i] != symbol {
            if (symbol == 0 && stride >= 5) || stride >= 7 {
                for k in 0..stride {
                    good[i - k - 1] = true;
                }
            }
            stride = 1;
            // The original reads counts[length] here; its value only matters as `symbol`
            // after the loop, which is not used.
            symbol = counts.get(i).copied().unwrap_or(0);
        } else {
            stride += 1;
        }
    }
    const STREAK_LIMIT: i64 = 1240;
    let c = |counts: &[u64], i: usize| counts.get(i).copied().unwrap_or(0) as i64;
    let mut stride = 0i64;
    let mut limit: i64 = 256 * (c(counts, 0) + c(counts, 1) + c(counts, 2)) / 3 + 420;
    let mut sum: i64 = 0;
    let mut i = 0;
    while i <= length {
        let ci = if i < length { counts[i] as i64 } else { 0 };
        if i == length
            || good[i]
            || (i > 0 && good[i - 1])
            || (256 * ci - limit).abs() >= STREAK_LIMIT
        {
            if stride >= 4 {
                let mut count = (sum + stride / 2) / stride;
                if count < 1 && sum != 0 {
                    count = 1;
                }
                for k in 0..stride as usize {
                    counts[i - k - 1] = count as u64;
                }
            }
            stride = 0;
            sum = 0;
            let ci = c(counts, i);
            limit = if (i as i64) < length as i64 - 2 {
                256 * (counts[i] + counts[i + 1] + counts[i + 2]) as i64 / 3 + 420
            } else {
                256 * ci
            };
        }
        stride += 1;
        if i != length {
            sum += counts[i] as i64;
            if stride >= 4 {
                limit = (256 * sum + stride / 2) / stride;
            }
            if stride == 4 {
                limit += 120;
            }
        }
        i += 1;
    }
}

/// zopfli/ECT's `EncodeTree` over the code lengths `lens` (literal/length then distance,
/// already cut to HLIT and HDIST), with its flags (`fuse_8` and `fuse_7` are ECT's): the
/// run-length coding.
pub fn encode_tree(lens: &[u8], [use_16, use_17, use_18, fuse_8, fuse_7]: [bool; 5]) -> Vec<RleOp> {
    let mut ops = vec![];
    let op = |sym: u8, extra: u8| RleOp { sym, extra };
    let total = lens.len();
    let mut i = 0;
    while i < total {
        let symbol = lens[i];
        let mut count = 1usize;
        if use_16 || (symbol == 0 && (use_17 || use_18)) {
            while i + count < total && lens[i + count] == symbol {
                count += 1;
            }
        }
        i += count;
        if symbol == 0 && count >= 3 {
            if use_18 {
                while count >= 11 {
                    let c2 = count.min(138);
                    ops.push(op(18, (c2 - 11) as u8));
                    count -= c2;
                }
            }
            if use_17 {
                while count >= 3 {
                    let c2 = count.min(10);
                    ops.push(op(17, (c2 - 3) as u8));
                    count -= c2;
                }
            }
        }
        if use_16 && count >= 4 {
            count -= 1;
            ops.push(op(symbol, 0));
            while count >= 3 {
                if fuse_8 && count == 8 {
                    ops.push(op(16, 1));
                    ops.push(op(16, 1));
                    count = 0;
                } else if fuse_7 && count == 7 {
                    ops.push(op(16, 1));
                    ops.push(op(16, 0));
                    count = 0;
                } else {
                    let c2 = count.min(6);
                    ops.push(op(16, (c2 - 3) as u8));
                    count -= c2;
                }
            }
        }
        for _ in 0..count {
            ops.push(op(symbol, 0));
        }
    }
    ops
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

/// [`crate::huffman::zlib_lens`] (zlib's `build_tree` and `gen_bitlen`), the same lengths
/// faster: 32-bit keys and a sentinel past the heap, so that sifting down picks a child
/// without branching.
pub fn zlib_lens(freq: &[u32], max_length: u8) -> Vec<u8> {
    const MAX_NODES: usize = 2 * 286 + 1;
    // Heap entries are key << 16 | node, keys frequency << 8 | depth (zlib's order); only
    // keys are compared. Past the heap, sentinels heavier than anything.
    const SENT: u64 = u64::MAX;
    let elems = freq.len();
    // Keys must fit in 32 bits: sums of frequencies in 24.
    if elems > 286 || freq.iter().map(|&f| f as u64).sum::<u64>() >= 1 << 23 {
        return crate::huffman::zlib_lens(freq, max_length);
    }
    let mut heap = [SENT; MAX_NODES + 2];
    let mut len = 0usize;
    let mut max_code: isize = -1;
    for (n, &f) in freq.iter().enumerate() {
        if f != 0 {
            len += 1;
            heap[len] = (f as u64) << 24 | n as u64;
            max_code = n as isize;
        }
    }
    while len < 2 {
        let node = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        len += 1;
        heap[len] = 1 << 24 | node as u64;
    }
    let max_code = max_code as usize;
    // Which symbols are coded (zlib's nonzero frequencies, forced ones included).
    let mut coded = [false; 286];
    for &e in &heap[1..=len] {
        coded[(e & 0xffff) as usize] = true;
    }

    /// zlib's sift-down, bottom-up (after Floyd): the smaller children move up all the way
    /// to a leaf, then `v` climbs back to where zlib's sift-down stops (the first child on the
    /// path not lighter than it; keys only grow along the path), with fewer unpredictable
    /// branches.
    #[inline(always)]
    fn down(heap: &mut [u64; MAX_NODES + 2], len: usize, start: usize) {
        let v = heap[start];
        let kv = v >> 16;
        let mut k = start;
        loop {
            let j = 2 * k;
            if j > len {
                break;
            }
            let (a, b) = (heap[j], heap[j + 1]);
            let right = b >> 16 <= a >> 16;
            heap[k] = if right { b } else { a };
            k = j + right as usize;
        }
        while k > start && heap[k / 2] >> 16 >= kv {
            heap[k] = heap[k / 2];
            k /= 2;
        }
        heap[k] = v;
    }

    for k in (1..=len / 2).rev() {
        down(&mut heap, len, k);
    }
    // Nodes in the order zlib stacks them at the top of its heap array, and parents.
    let mut order = [0u16; MAX_NODES];
    let mut no = 0;
    let mut dad = [0u16; MAX_NODES];
    let mut node = elems;
    loop {
        let n = heap[1];
        heap[1] = heap[len];
        heap[len] = SENT;
        len -= 1;
        down(&mut heap, len, 1);
        let m = heap[1];
        let (kn, km) = (n >> 16, m >> 16);
        let (n, m) = ((n & 0xffff) as u16, (m & 0xffff) as u16);
        order[no] = n;
        order[no + 1] = m;
        no += 2;
        let depth = (kn & 0xff).max(km & 0xff) + 1;
        let k = ((kn >> 8) + (km >> 8)) << 8 | depth;
        dad[n as usize] = node as u16;
        dad[m as usize] = node as u16;
        heap[1] = k << 16 | node as u64;
        node += 1;
        down(&mut heap, len, 1);
        if len < 2 {
            break;
        }
    }
    order[no] = (heap[1] & 0xffff) as u16;
    no += 1;

    // gen_bitlen: from the root down.
    let mut lens = [0u8; MAX_NODES];
    let mut bl_count = [0u32; 16];
    let mut overflow = 0i32;
    for &n in order[..no - 1].iter().rev() {
        let n = n as usize;
        let mut bits = lens[dad[n] as usize] + 1;
        if bits > max_length {
            bits = max_length;
            overflow += 1;
        }
        lens[n] = bits;
        if n <= max_code {
            bl_count[bits as usize] += 1;
        }
    }
    if overflow > 0 {
        loop {
            let mut bits = max_length as usize - 1;
            while bl_count[bits] == 0 {
                bits -= 1;
            }
            bl_count[bits] -= 1;
            bl_count[bits + 1] += 2;
            bl_count[max_length as usize] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        let mut h = 0;
        for bits in (1..=max_length as usize).rev() {
            let mut k = bl_count[bits];
            while k != 0 {
                let m = order[h] as usize;
                h += 1;
                if m > max_code {
                    continue;
                }
                lens[m] = bits as u8;
                k -= 1;
            }
        }
    }
    (0..elems)
        .map(|n| if n <= max_code && coded[n] { lens[n] } else { 0 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Huffman's lengths are zopfli's package-merge's whenever they fit.
    #[test]
    fn huffman_as_package_merge() {
        let mut lcg = 7u64;
        let mut rnd = |n: u64| {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
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
                Some(h) if h == package_merge_boundary(&freq, maxbits) => same += 1,
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
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
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

    /// The array package-merge gives the boundary package-merge's lengths, ties included.
    #[test]
    fn package_merge_is_zopflis() {
        let mut lcg = 77u32;
        let mut rnd = |m: u32| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 8) % m
        };
        for round in 0..20000 {
            let n = [3, 5, 19, 30, 32, 286, 288][round % 7];
            // Many ties: few distinct weights, and skewed ones that hit the limit.
            let f: Vec<u64> = (0..n)
                .map(|_| match rnd(4) {
                    0 => 0,
                    1 => rnd(4) as u64 + 1,
                    2 => 1u64 << rnd(20),
                    _ => rnd(1000) as u64,
                })
                .collect();
            for maxbits in [7, 9, 12, 15] {
                assert_eq!(
                    package_merge(&f, maxbits),
                    package_merge_boundary(&f, maxbits),
                    "{f:?} limit {maxbits}"
                );
            }
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
                        if r.is_multiple_of(4) { 0 } else { (r % 1000) as u64 * (r % 7) as u64 }
                    })
                    .collect();
                let used = f.iter().filter(|&&x| x > 0).count();
                let l = package_merge(&f, 15);
                if used < 2 {
                    continue;
                }
                let kraft: f64 = l.iter().filter(|&&x| x > 0).map(|&x| 0.5f64.powi(x as i32)).sum();
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
