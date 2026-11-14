//! Huffman coding of literals as libzstd's compressor does it: the code lengths it builds
//! (the order it sorts symbols in, the tree, the limit of the lengths), the table description
//! it writes (RFC 8878, "Huffman Tree Description") and the streams.

use super::bits::Bits;
use super::fse::{self, highbit};

pub(crate) const TABLELOG_MAX: u32 = 12;
/// `LitHufLog`.
pub(crate) const LIT_LOG: u32 = 11;

/// A Huffman table: (code, length) by symbol, and its largest length.
#[derive(Clone)]
pub(crate) struct HTable {
    pub codes: [(u16, u8); 256],
    /// The largest symbol it was built for.
    pub max_symbol: u32,
    pub max_bits: u32,
}

#[derive(Clone, Copy, Default)]
struct Node {
    count: u32,
    parent: u16,
    byte: u8,
    bits: u8,
}

const BUCKETS_BEGIN: u32 = 192 - 1 - 32 - 1; // 158
/// Counts below it have a bucket each, the larger ones one for each power of two.
const CUTOFF: u32 = BUCKETS_BEGIN + 7;

fn index(count: u32) -> u32 {
    if count < CUTOFF { count } else { highbit(count) + BUCKETS_BEGIN }
}

fn insertion_sort(a: &mut [Node]) {
    for i in 1..a.len() {
        let key = a[i];
        let mut j = i as isize - 1;
        while j >= 0 && a[j as usize].count < key.count {
            a[j as usize + 1] = a[j as usize];
            j -= 1;
        }
        a[(j + 1) as usize] = key;
    }
}

fn partition(a: &mut [Node], low: isize, high: isize) -> isize {
    let pivot = a[high as usize].count;
    let mut i = low - 1;
    for j in low..high {
        if a[j as usize].count > pivot {
            i += 1;
            a.swap(i as usize, j as usize);
        }
    }
    a.swap((i + 1) as usize, high as usize);
    i + 1
}

fn quick_sort(a: &mut [Node], mut low: isize, mut high: isize) {
    if high - low < 8 {
        insertion_sort(&mut a[low as usize..(high + 1) as usize]);
        return;
    }
    while low < high {
        let idx = partition(a, low, high);
        if idx - low < high - idx {
            quick_sort(a, low, idx - 1);
            low = idx + 1;
        } else {
            quick_sort(a, idx + 1, high);
            high = idx - 1;
        }
    }
}

/// Symbols `0..=max` by count, decreasing (`HUF_sort`).
fn sort(nodes: &mut [Node], count: &[u32], max: u32) {
    let n1 = max as usize + 1;
    let mut base = [0u16; 192];
    let mut curr = [0u16; 192];
    for &c in &count[..n1] {
        base[index(c) as usize] += 1;
    }
    for n in (1..192).rev() {
        base[n - 1] += base[n];
        curr[n - 1] = base[n - 1];
    }
    for (s, &c) in count[..n1].iter().enumerate() {
        let r = index(c) as usize + 1;
        let pos = curr[r] as usize;
        curr[r] += 1;
        nodes[pos] = Node { count: c, byte: s as u8, ..Default::default() };
    }
    for n in CUTOFF as usize..191 {
        let size = curr[n] as isize - base[n] as isize;
        let start = base[n] as usize;
        if size > 1 {
            quick_sort(&mut nodes[start..], 0, size - 1);
        }
    }
}

/// The code lengths of `count` (symbols `0..=max`), at most `max_bits`: the table and its
/// largest length (`HUF_buildCTable_wksp`).
pub(crate) fn build(count: &[u32], max: u32, max_bits: u32) -> HTable {
    // huff[0] is the barrier (`huffNode0[0]`); the nodes are huff[1..].
    let mut huff = [Node::default(); 2 * 256 + 2];
    sort(&mut huff[1..], count, max);
    const START: usize = 256;
    let h = |i: isize| (i + 1) as usize;
    // build the tree
    let mut non_null = max as isize;
    while huff[h(non_null)].count == 0 {
        non_null -= 1;
    }
    let mut low_s = non_null;
    let node_root = START as isize + low_s - 1;
    let mut low_n = START as isize;
    let mut node_nb = START as isize;
    huff[h(node_nb)].count = huff[h(low_s)].count + huff[h(low_s - 1)].count;
    huff[h(low_s)].parent = node_nb as u16;
    huff[h(low_s - 1)].parent = node_nb as u16;
    node_nb += 1;
    low_s -= 2;
    for n in node_nb..=node_root {
        huff[h(n)].count = 1 << 30;
    }
    huff[0].count = 1 << 31;
    while node_nb <= node_root {
        let n1 = if huff[h(low_s)].count < huff[h(low_n)].count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        let n2 = if huff[h(low_s)].count < huff[h(low_n)].count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        huff[h(node_nb)].count = huff[h(n1)].count + huff[h(n2)].count;
        huff[h(n1)].parent = node_nb as u16;
        huff[h(n2)].parent = node_nb as u16;
        node_nb += 1;
    }
    huff[h(node_root)].bits = 0;
    for n in (START as isize..node_root).rev() {
        huff[h(n)].bits = huff[h(huff[h(n)].parent as isize)].bits + 1;
    }
    for n in 0..=non_null {
        huff[h(n)].bits = huff[h(huff[h(n)].parent as isize)].bits + 1;
    }
    let nodes = &mut huff[1..];
    let max_bits = set_max_height(nodes, non_null as usize, max_bits);
    // canonical codes
    let mut nb_per_rank = [0u16; TABLELOG_MAX as usize + 1];
    let mut val_per_rank = [0u16; TABLELOG_MAX as usize + 1];
    for n in &nodes[..=non_null as usize] {
        nb_per_rank[n.bits as usize] += 1;
    }
    let mut min = 0u16;
    for n in (1..=max_bits as usize).rev() {
        val_per_rank[n] = min;
        min += nb_per_rank[n];
        min >>= 1;
    }
    let mut codes = [(0u16, 0u8); 256];
    for n in &nodes[..=max as usize] {
        codes[n.byte as usize].1 = n.bits;
    }
    for c in &mut codes[..=max as usize] {
        c.0 = val_per_rank[c.1 as usize];
        val_per_rank[c.1 as usize] += 1;
    }
    HTable { codes, max_symbol: max, max_bits }
}

fn set_max_height(nodes: &mut [Node], last_non_null: usize, target: u32) -> u32 {
    let largest = nodes[last_non_null].bits as u32;
    if largest <= target {
        return largest;
    }
    let mut total_cost: i32 = 0;
    let base_cost = 1i32 << (largest - target);
    let mut n = last_non_null as isize;
    while nodes[n as usize].bits as u32 > target {
        total_cost += base_cost - (1 << (largest - nodes[n as usize].bits as u32));
        nodes[n as usize].bits = target as u8;
        n -= 1;
    }
    while nodes[n as usize].bits as u32 == target {
        n -= 1;
    }
    total_cost >>= largest - target;
    const NO: u32 = 0xF0F0_F0F0;
    let mut rank_last = [NO; TABLELOG_MAX as usize + 2];
    {
        let mut current = target;
        let mut pos = n;
        while pos >= 0 {
            let b = nodes[pos as usize].bits as u32;
            if b < current {
                current = b;
                rank_last[(target - current) as usize] = pos as u32;
            }
            pos -= 1;
        }
    }
    while total_cost > 0 {
        let mut dec = highbit(total_cost as u32) + 1;
        while dec > 1 {
            let high_pos = rank_last[dec as usize];
            let low_pos = rank_last[dec as usize - 1];
            if high_pos == NO {
                dec -= 1;
                continue;
            }
            if low_pos == NO {
                break;
            }
            let high_total = nodes[high_pos as usize].count;
            let low_total = 2 * nodes[low_pos as usize].count;
            if high_total <= low_total {
                break;
            }
            dec -= 1;
        }
        while dec <= TABLELOG_MAX && rank_last[dec as usize] == NO {
            dec += 1;
        }
        total_cost -= 1 << (dec - 1);
        let p = rank_last[dec as usize] as usize;
        nodes[p].bits += 1;
        if rank_last[dec as usize - 1] == NO {
            rank_last[dec as usize - 1] = rank_last[dec as usize];
        }
        if rank_last[dec as usize] == 0 {
            rank_last[dec as usize] = NO;
        } else {
            rank_last[dec as usize] -= 1;
            if nodes[rank_last[dec as usize] as usize].bits as u32 != target - dec {
                rank_last[dec as usize] = NO;
            }
        }
    }
    while total_cost < 0 {
        if rank_last[1] == NO {
            while nodes[n as usize].bits as u32 == target {
                n -= 1;
            }
            nodes[(n + 1) as usize].bits -= 1;
            rank_last[1] = (n + 1) as u32;
            total_cost += 1;
            continue;
        }
        nodes[rank_last[1] as usize + 1].bits -= 1;
        rank_last[1] += 1;
        total_cost += 1;
    }
    target
}

/// The table description of `t`, appended to `out` (`HUF_writeCTable`): None where it
/// cannot be written.
pub(crate) fn write_table(out: &mut Vec<u8>, t: &HTable) -> Option<usize> {
    let max = t.max_symbol as usize;
    let log = t.max_bits;
    let mut weights = [0u8; 256];
    for (w, &(_, b)) in weights.iter_mut().zip(&t.codes[..max]) {
        *w = if b == 0 { 0 } else { (log + 1 - b as u32) as u8 };
    }
    let start = out.len();
    out.push(0);
    let h = compress_weights(out, &weights[..max]);
    if h > 1 && h < max / 2 {
        out[start] = h as u8;
        return Some(h + 1);
    }
    out.truncate(start);
    if max > 128 {
        return None;
    }
    out.push((128 + (max - 1)) as u8);
    weights[max] = 0;
    for n in (0..max).step_by(2) {
        out.push((weights[n] << 4) + weights[n + 1]);
    }
    Some(max.div_ceil(2) + 1)
}

fn compress_weights(out: &mut Vec<u8>, w: &[u8]) -> usize {
    if w.len() <= 1 {
        return 0;
    }
    let mut count = [0u32; TABLELOG_MAX as usize + 1];
    let (max_symbol, max_count) = fse::histogram(&mut count, w);
    if max_count as usize == w.len() {
        return 1;
    }
    if max_count == 1 {
        return 0;
    }
    let log = fse::optimal_table_log(6, w.len(), max_symbol, 2);
    let Some(norm) = fse::normalize(&count[..=max_symbol as usize], w.len(), log, false) else { return 0 };
    let start = out.len();
    fse::write_ncount(out, &norm, log);
    let ct = fse::CTable::new(&norm, log);
    let c = fse::compress_using(out, w, &ct);
    if c == 0 {
        out.truncate(start);
        return 0;
    }
    out.len() - start
}

/// Whether `t` gives a code to every symbol counted (`HUF_validateCTable`).
pub(crate) fn valid(t: &HTable, count: &[u32], max: u32) -> bool {
    if t.max_symbol < max {
        return false;
    }
    (0..=max as usize).all(|s| count[s] == 0 || t.codes[s].1 != 0)
}

pub(crate) fn estimate(t: &HTable, count: &[u32], max: u32) -> usize {
    (0..=max as usize).map(|s| count[s] as usize * t.codes[s].1 as usize).sum::<usize>() >> 3
}

fn stream(out: &mut Vec<u8>, src: &[u8], t: &HTable) -> usize {
    let mut bits = Bits::new(out);
    for &b in src.iter().rev() {
        let (c, n) = t.codes[b as usize];
        bits.add(c as u32, n as u32);
    }
    bits.close()
}

/// The literals by `t`, in one stream or four (`HUF_compressCTable_internal`, from the
/// table's description already at `out[start..]`): 0 where it does not pay.
pub(crate) fn compress_with(out: &mut Vec<u8>, start: usize, src: &[u8], four: bool, t: &HTable) -> usize {
    let at = out.len();
    let c = if !four {
        stream(out, src, t)
    } else {
        if src.len() < 12 {
            return 0;
        }
        let seg = src.len().div_ceil(4);
        out.extend_from_slice(&[0; 6]);
        let mut ok = true;
        for k in 0..4 {
            let piece = &src[(k * seg).min(src.len())..((k + 1) * seg).min(src.len())];
            let piece = if k == 3 { &src[3 * seg..] } else { piece };
            let n = stream(out, piece, t);
            if n == 0 || n > 65535 {
                ok = false;
            }
            if k < 3 {
                out[at + 2 * k..at + 2 * k + 2].copy_from_slice(&(n as u16).to_le_bytes());
            }
        }
        if !ok {
            out.truncate(at);
            return 0;
        }
        out.len() - at
    };
    if c == 0 {
        out.truncate(at);
        return 0;
    }
    let total = out.len() - start;
    if total >= src.len() - 1 {
        out.truncate(at);
        return 0;
    }
    total
}
