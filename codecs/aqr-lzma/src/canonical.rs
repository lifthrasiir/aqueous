//! The encoder of `aqr -c --lossy`'s LZMA streams (docs/lossy-design.md §6.2): a parse whose
//! choices are those [`crate::recons`]'s contexts predict, so that its record is a few percent of
//! an optimal parser's (the tar of this repository's sources: 334 KB against XZ Utils `-6`'s 1.06
//! MB), its streams about XZ Utils `-3`'s (15.51 MB against `-3`'s 15.58 and `-6`'s 14.64).
//!
//! At each position: the longest repeated match where it is at least the longest match but one
//! (its length all of it), else the longest match at the nearest distance it is found at (the
//! one the record does not code), but where it is short and far back (2 bytes from 128 back, 3
//! from 16 KiB, 4 from 1 MiB: costing more than its literals), else a literal. The search is
//! the record's walk's (its depth and lengths, [`crate::mf::DEPTH`], [`NICE`], [`LIMIT`]), and a
//! match is put off by a literal where the next position has one at least [`LAZY`] longer.

use crate::lzma::{self, Kind, MAX_LEN, Packet, Props};
use crate::lzma2::{self, Chunk};
use crate::mf::{DEPTH, Mf, View};

/// A match this long is taken without looking further (the record's walk's).
const NICE: usize = 64;
/// Candidates looked at for the nearest match of a length (the record's walk's).
const LIMIT: usize = 64;
/// A match is put off where the next position's is this much longer (and one more after a
/// repeated match).
const LAZY: usize = 2;
/// Bytes of data of the LZMA2 chunks (but the last): coded in less than 64 KiB, whatever it is.
const CHUNK: usize = 32 << 10;

/// What a position offers to take.
#[derive(Clone, Copy)]
struct Choice {
    len: usize,
    dist: usize,
    rep: Option<usize>,
}

/// Whether a match of `len` bytes `dist` back costs more than its literals.
fn too_far(len: usize, dist: usize) -> bool {
    match len {
        2 => dist >= 128,
        3 => dist >= 1 << 14,
        4 => dist >= 1 << 20,
        _ => false,
    }
}

/// What position `pos` of the `n` bytes of `v` offers, with the last distances `reps`.
fn choose(mf: &mut Mf, v: &View, pos: usize, n: usize, reps: &[u32; 4], reach: usize) -> Option<Choice> {
    let cap = (MAX_LEN as usize).min(n - pos);
    let mut rl = [0usize; 4];
    for k in 0..4 {
        if (reps[k] as usize) < pos.min(reach) {
            rl[k] = v.lcp(pos - reps[k] as usize - 1, pos, cap);
        }
    }
    let (k, lr) = (0..4).map(|k| (k, rl[k])).max_by_key(|&(k, l)| (l, std::cmp::Reverse(k))).expect("four");
    let m = mf.longest(v, pos, DEPTH, NICE, cap);
    let matched = match m >= 2 {
        true => mf.nearest(v, pos, m, LIMIT).map(|c| Choice { len: m, dist: pos - c - 1, rep: None }),
        false => None,
    }
    .filter(|c| !too_far(c.len, c.dist));
    let repeated = (lr >= 2).then_some(Choice { len: lr, dist: reps[k] as usize, rep: Some(k) });
    match (repeated, matched) {
        (Some(r), Some(m)) if r.len + 1 >= m.len => Some(r),
        (_, Some(m)) => Some(m),
        (r, None) => r,
    }
}

/// The packets of `data` with matches at most `dict` back.
pub fn packets(data: &[u8], dict: usize) -> Vec<Packet> {
    let v = View::whole(data);
    let n = data.len();
    let reach = crate::recons::reach(dict as u32);
    let mut mf = Mf::new(n, reach);
    let (mut pos, mut reps) = (0usize, [0u32; 4]);
    let mut out = Vec::with_capacity(n / 4);
    // (the choice of the next position, made to put one off)
    let mut next: Option<(usize, Option<Choice>)> = None;
    while pos < n {
        mf.advance(&v, pos);
        let here = match next.take() {
            Some((p, c)) if p == pos => c,
            _ => choose(&mut mf, &v, pos, n, &reps, reach),
        };
        let mut take = here;
        if let Some(c) = here
            && c.len < NICE
            && pos + 1 < n
        {
            mf.advance(&v, pos + 1);
            let then = choose(&mut mf, &v, pos + 1, n, &reps, reach);
            next = Some((pos + 1, then));
            if then.is_some_and(|t| t.len >= c.len + LAZY + c.rep.is_some() as usize) {
                take = None;
            }
        }
        let pk = match take {
            Some(Choice { len, dist, rep: Some(k) }) => {
                lzma::push_rep(&mut reps, k, dist as u32);
                Packet { kind: [Kind::Rep0, Kind::Rep1, Kind::Rep2, Kind::Rep3][k], len: len as u16, dist: dist as u32 }
            }
            Some(Choice { len, dist, rep: None }) => {
                lzma::push_rep(&mut reps, 3, dist as u32);
                Packet { kind: Kind::Match, len: len as u16, dist: dist as u32 }
            }
            None => Packet::LIT,
        };
        out.push(pk);
        pos += pk.len as usize;
    }
    out
}

/// A raw LZMA stream of `data`, of properties `props` and a dictionary of `dict` bytes, with the
/// end marker where `marker`.
pub fn lzma(data: &[u8], props: Props, dict: u32, marker: bool) -> Vec<u8> {
    lzma::encode(data, props, &packets(data, dict as usize), marker)
}

/// A raw LZMA2 stream of `data` (its end byte too), of properties `props` and a dictionary of
/// `dict` bytes: LZMA chunks of [`CHUNK`] bytes of data (cut where a packet ends), the first
/// resetting the dictionary and the properties, the others nothing. Empty data is the end byte
/// alone.
pub fn lzma2(data: &[u8], props: Props, dict: u32) -> Vec<u8> {
    let packets = packets(data, dict as usize);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut len = 0;
    for (i, pk) in packets.iter().enumerate() {
        len += pk.len as usize;
        let next = packets.get(i + 1).map_or(usize::MAX, |p| p.len as usize);
        if len.saturating_add(next) > CHUNK || i + 1 == packets.len() {
            let control = if chunks.is_empty() { 0xe0 } else { 0x80 };
            chunks.push(Chunk { control, len, props: (control == 0xe0).then_some(props) });
            len = 0;
        }
    }
    let mut s = lzma2::encode(data, &chunks, &packets).expect("chunks of the packets, each coded in less than 64 KiB");
    s.push(0);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: usize) -> Vec<u8> {
        let words = ["alpha ", "beta ", "gamma ", "delta ", "epsilon ", "zeta ", "eta ", "theta "];
        let (mut t, mut x) = (Vec::new(), 9u64);
        while t.len() < n {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            t.extend_from_slice(words[(x >> 61) as usize].as_bytes());
            t.extend_from_slice(format!("{} ", (x >> 40) % 1000).as_bytes());
        }
        t.truncate(n);
        t
    }

    /// The streams decode to the data, and their records are a small part of the greedy
    /// encoder's streams'.
    #[test]
    fn decodes_and_records_little() {
        let props = Props { lc: 3, lp: 0, pb: 2 };
        for data in [text(300_000), Vec::new(), vec![7; 100_000], text(1)] {
            let s = lzma(&data, props, 1 << 20, true);
            let d = lzma::decode(&s, props, None, data.len() + 1).expect("decodes");
            assert_eq!(d.data, data);
            let s2 = lzma2(&data, props, 1 << 20);
            let d2 = lzma2::decode(&s2, data.len() + 1).expect("decodes");
            assert_eq!(d2.segments.iter().flat_map(|s| s.data.iter().copied()).collect::<Vec<u8>>(), data);
        }
        let data = text(300_000);
        let pk = packets(&data, 1 << 20);
        let (record, _) = crate::recons::deconstruct(&data, &pk, 1 << 20, 0, None).unwrap();
        let s = lzma::encode(&data, props, &pk, false);
        assert!(record.len() * 20 < s.len(), "record {} of a stream of {}", record.len(), s.len());
    }
}
