//! The block headers a compressor of LHA's lineage writes from its tokens' frequencies
//! (`FORMAT.md`, "Predicted headers"): Huffman codes built by a heap, limited to 16 bits by moving
//! leaves down, the longest codes to the symbols taken out of the heap first; and the tables
//! written in the shortest way their rules allow.

#[cfg(test)]
use crate::lzh::NC;
use crate::lzh::{self, CTable, Head, Item, Method, NT, PtTable, Token};

/// The code lengths of `freq` (`n` symbols). For fewer than two symbols used: `Err` of the one
/// symbol, or of 0.
pub fn lengths(freq: &[u32]) -> Result<Vec<u8>, u16> {
    let n = freq.len();
    // node frequencies, leaves then inner nodes; the heap of nodes (1-based)
    let mut f: Vec<u64> = freq.iter().map(|&x| x as u64).collect();
    f.resize(2 * n, 0);
    let mut heap: Vec<usize> = vec![0];
    heap.extend((0..n).filter(|&i| freq[i] != 0));
    let mut size = heap.len() - 1;
    if size < 2 {
        return Err(if size == 1 { heap[1] as u16 } else { 0 });
    }
    let (mut left, mut right) = (vec![0usize; 2 * n], vec![0usize; 2 * n]);
    let down = |heap: &mut Vec<usize>, size: usize, f: &[u64], mut i: usize| {
        let k = heap[i];
        loop {
            let mut j = 2 * i;
            if j > size {
                break;
            }
            if j < size && f[heap[j]] > f[heap[j + 1]] {
                j += 1;
            }
            if f[k] <= f[heap[j]] {
                break;
            }
            heap[i] = heap[j];
            i = j;
        }
        heap[i] = k;
    };
    for i in (1..=size / 2).rev() {
        down(&mut heap, size, &f, i);
    }
    // the leaves in the order taken out
    let mut order = Vec::with_capacity(n);
    let mut avail = n;
    let mut k;
    loop {
        let i = heap[1];
        if i < n {
            order.push(i);
        }
        heap[1] = heap[size];
        size -= 1;
        down(&mut heap, size, &f, 1);
        let j = heap[1];
        if j < n {
            order.push(j);
        }
        k = avail;
        avail += 1;
        f[k] = f[i] + f[j];
        heap[1] = k;
        down(&mut heap, size, &f, 1);
        left[k] = i;
        right[k] = j;
        if size <= 1 {
            break;
        }
    }
    // the count of leaves at each depth (16 for those deeper), moved until the code is full
    let mut cnt = [0u32; 17];
    let mut stack = vec![(k, 0usize)];
    while let Some((i, depth)) = stack.pop() {
        if i < n {
            cnt[depth.min(16)] += 1;
        } else {
            stack.push((right[i], depth + 1));
            stack.push((left[i], depth + 1));
        }
    }
    let mut cum: u32 = (1..=16).map(|i| cnt[i] << (16 - i)).sum::<u32>();
    while cum != 1 << 16 {
        cnt[16] -= 1;
        for i in (1..16).rev() {
            if cnt[i] != 0 {
                cnt[i] -= 1;
                cnt[i + 1] += 2;
                break;
            }
        }
        cum = cum.wrapping_sub(1);
    }
    let mut lens = vec![0u8; n];
    let mut it = order.into_iter();
    for i in (1..=16).rev() {
        for _ in 0..cnt[i] {
            lens[it.next().expect("a leaf for each length")] = i as u8;
        }
    }
    Ok(lens)
}

/// The table `read_pt_len` reads of `lens` (or of one symbol), written the shortest way: the
/// count up to the last length that is not 0, the zeros at 3-5 skipped where `special`.
pub fn pt_table(lens: Result<Vec<u8>, u16>, special: bool) -> PtTable {
    let lens = match lens {
        Err(s) => return PtTable { n: 0, single: s, lens: Vec::new(), skip: 0 },
        Ok(l) => l,
    };
    let n = lens.iter().rposition(|&l| l != 0).map_or(0, |i| i + 1);
    let mut t = PtTable { n: n as u16, single: 0, lens: lens[..n].to_vec(), skip: 0 };
    if special && n >= 3 {
        let skip = (3..6).take_while(|&i| lens.get(i).is_none_or(|&l| l == 0)).count();
        t.skip = skip as u8;
        // (the zeros skipped past the count, as the reader takes them)
        t.lens.resize(t.lens.len().max(3 + skip), 0);
    }
    t
}

/// The items of `NC`'s lengths, written the shortest way: runs of zeros of 1-2 as single zeros,
/// 3-18 by 4 bits, 19 as one and 18, longer by 9 bits.
pub fn c_items(lens: &[u8]) -> (u16, Vec<Item>) {
    let n = lens.iter().rposition(|&l| l != 0).map_or(0, |i| i + 1);
    let mut items = Vec::new();
    let mut i = 0;
    while i < n {
        let k = lens[i];
        i += 1;
        if k != 0 {
            items.push(Item { sym: k + 2, extra: 0 });
            continue;
        }
        let mut count = 1;
        while i < n && lens[i] == 0 {
            i += 1;
            count += 1;
        }
        match count {
            1 | 2 => items.extend((0..count).map(|_| Item { sym: 0, extra: 0 })),
            3..=18 => items.push(Item { sym: 1, extra: count as u16 - 3 }),
            19 => items.extend([Item { sym: 0, extra: 0 }, Item { sym: 1, extra: 15 }]),
            _ => items.push(Item { sym: 2, extra: count as u16 - 20 }),
        }
    }
    (n as u16, items)
}

/// The header such a compressor writes for a block of `tokens`.
pub fn head(tokens: &[Token], m: Method) -> Head {
    let (cf, pf) = lzh::freqs(tokens, m.np());
    let (t, c) = match lengths(&cf) {
        Err(s) => {
            (PtTable { n: 0, single: 0, lens: Vec::new(), skip: 0 }, CTable { n: 0, single: s, items: Vec::new() })
        }
        Ok(cl) => {
            let (n, items) = c_items(&cl);
            (pt_table(lengths(&t_freqs(&items)), true), CTable { n, single: 0, items })
        }
    };
    let p = pt_table(lengths(&pf), false);
    Head { size: tokens.len() as u16, t, c, p }
}

/// The frequencies of the symbols of `NT` that `items` use.
pub fn t_freqs(items: &[Item]) -> Vec<u32> {
    let mut tf = vec![0u32; NT];
    for it in items {
        tf[it.sym as usize] += 1;
    }
    tf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_full_codes() {
        let mut f = vec![0u32; NC];
        // (frequencies of Fibonacci numbers make a deep tree, limited to 16)
        let (mut a, mut b) = (1u32, 1u32);
        for x in f.iter_mut().take(30) {
            *x = a;
            (a, b) = (b, a.saturating_add(b));
        }
        let l = lengths(&f).unwrap();
        assert!(l.iter().all(|&x| x <= 16));
        let kraft: u64 = l.iter().filter(|&&x| x != 0).map(|&x| 1u64 << (16 - x)).sum();
        assert_eq!(kraft, 1 << 16);
        assert_eq!(lengths(&[0, 5, 0]), Err(1));
        assert_eq!(lengths(&[0, 0]), Err(0));
    }
}
