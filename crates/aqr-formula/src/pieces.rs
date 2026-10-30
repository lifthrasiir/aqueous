//! Pieces of long streams paired by what they have in common ([`crate::Formula::merge_classes_within`]):
//! the code of two versions of a program, each longer than a block, shares little stream by
//! stream (each compressed on its own, within a window), but much piece by piece, each piece
//! put together with the one of the other version most like it (docs/macos-app-todo.md §1).

use std::collections::HashMap;

/// Of a rolling hash of the 64 bytes up to each position, the top bits that are zero where the
/// position is an anchor: one in 4 KiB of bytes unlike each other.
const ANCHOR_BITS: u32 = 12;

/// Anchors in more pieces than this, of runs and padding, are not counted.
const COMMON: usize = 16;

/// Of the anchors of the larger of two pieces, the least share, as one in this many, that the
/// other has too for them to be paired.
const SHARE: u64 = 10;

/// The table of the gear hash: a value for each byte (SplitMix64).
const GEAR: [u64; 256] = {
    let (mut t, mut x, mut i) = ([0; 256], 0u64, 0);
    while i < 256 {
        x = x.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        t[i] = z ^ (z >> 31);
        i += 1;
    }
    t
};

/// The hashes of the anchors of `data`, sorted, each once.
fn anchors(data: &[u8]) -> Vec<u64> {
    let mut h = 0u64;
    let mut out = Vec::with_capacity(data.len() >> ANCHOR_BITS);
    for &b in data {
        h = (h << 1).wrapping_add(GEAR[b as usize]);
        if h >> (64 - ANCHOR_BITS) == 0 {
            out.push(h);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Of `pieces`, each the stream it is of and its bytes, the group each is put in, by the first
/// piece of it: groups of up to `most` pieces of different streams, joined by the pairs of
/// pieces with the largest share of anchors in common first. The same for the same pieces.
pub(crate) fn group(pieces: &[(usize, &[u8])], most: usize) -> Vec<usize> {
    let n = pieces.len();
    let anchors = aqr_common::pool::par_map(n, aqr_common::pool::size(), |i| anchors(pieces[i].1));
    let mut of: HashMap<u64, Vec<u32>> = HashMap::new();
    for (i, a) in anchors.iter().enumerate() {
        a.iter().for_each(|&h| of.entry(h).or_default().push(i as u32));
    }
    let mut common: HashMap<(u32, u32), u64> = HashMap::new();
    for ps in of.values().filter(|ps| ps.len() <= COMMON) {
        for (k, &x) in ps.iter().enumerate() {
            for &y in &ps[k + 1..] {
                if pieces[x as usize].0 != pieces[y as usize].0 {
                    *common.entry((x, y)).or_default() += 1;
                }
            }
        }
    }
    // (the share in 1/2^16, then the pieces: an order of its own)
    let mut pairs: Vec<(u64, u32, u32)> = common
        .into_iter()
        .filter_map(|((x, y), c)| {
            let larger = anchors[x as usize].len().max(anchors[y as usize].len()) as u64;
            (c * SHARE >= larger).then(|| ((c << 16) / larger, x, y))
        })
        .collect();
    pairs.sort_unstable_by(|a, b| b.0.cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
    // (of each group, by its first piece, the pieces in it)
    let (mut to, mut members): (Vec<usize>, Vec<Vec<usize>>) = ((0..n).collect(), (0..n).map(|i| vec![i]).collect());
    for (_, x, y) in pairs {
        let (gx, gy) = (to[x as usize], to[y as usize]);
        let (gx, gy) = (gx.min(gy), gx.max(gy));
        if gx == gy
            || members[gx].len() + members[gy].len() > most
            || members[gx].iter().any(|&a| members[gy].iter().any(|&b| pieces[a].0 == pieces[b].0))
        {
            continue;
        }
        let moved = std::mem::take(&mut members[gy]);
        moved.iter().for_each(|&p| to[p] = gx);
        members[gx].extend(moved);
    }
    to
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect()
    }

    /// Pieces grouped with those they share most with, of other streams only; one sharing too
    /// little left alone.
    #[test]
    fn pairs() {
        let (a, b, c, d) = (noise(1 << 20, 1), noise(1 << 20, 2), noise(1 << 20, 3), noise(1 << 20, 4));
        // b with a quarter changed, a with a tenth, both in stream 0 too
        let mut b2 = b.clone();
        b2[..1 << 18].copy_from_slice(&d[..1 << 18]);
        let mut a2 = a.clone();
        a2[1 << 19..][..1 << 17].copy_from_slice(&d[1 << 19..][..1 << 17]);
        let pieces = [(0, &a[..]), (0, &b[..]), (0, &a[..]), (1, &b2[..]), (1, &c[..]), (1, &a2[..])];
        assert_eq!(group(&pieces, 2), [0, 1, 2, 1, 4, 0]);
        // a third with a and a2
        assert_eq!(group(&[pieces[0], pieces[1], (2, &a[..]), pieces[5]], 3), [0, 1, 0, 0]);
    }
}
