//! PackBits encoders reproducing those of known writers, and their identification.
//!
//! Photoshop cuts each row at the tile boundaries of its image cache (in document
//! coordinates, so at a per-layer `phase`, then every `period` bytes), cuts each piece into
//! 128-byte chunks from the piece's start, and encodes every chunk on its own: 2 equal bytes
//! make a run at the start of a chunk or after a run, 3 inside a literal. Other writers
//! (e.g. the one of the `tori29umai` set) do the same over the whole row at once with runs
//! of 3 or more.

use crate::pixels::unpackbits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    /// Tile pieces `[0, phase)`, then `period` bytes each, cut into 128-byte chunks.
    Photoshop { phase: usize, period: usize },
    /// One pass over the row: runs of at least `min_run`. With `split_rest`, what is left of
    /// a run longer than 128 is a run (even of 2) or a literal of its own; without, it may
    /// start a literal.
    Greedy { min_run: usize, split_rest: bool },
}

/// No tile boundary within any row.
pub const NO_TILES: Encoder = Encoder::Photoshop { phase: usize::MAX, period: usize::MAX };

/// Encodes one chunk (at most 128 bytes for Photoshop) the way Photoshop does.
fn encode_chunk(row: &[u8], out: &mut Vec<u8>) {
    let n = row.len();
    let run_at = |j: usize| {
        let mut k = j + 1;
        while k < n && row[k] == row[j] && k - j < 128 {
            k += 1;
        }
        k - j
    };
    let mut i = 0;
    while i < n {
        let r = run_at(i);
        if r >= 2 {
            out.push((1 - r as i32) as u8);
            out.push(row[i]);
            i += r;
        } else {
            let s = i;
            i += 1;
            while i < n && i - s < 128 && run_at(i) < 3 {
                i += 1;
            }
            out.push((i - s - 1) as u8);
            out.extend_from_slice(&row[s..i]);
        }
    }
}

fn encode_greedy(row: &[u8], min_run: usize, split_rest: bool, out: &mut Vec<u8>) {
    let n = row.len();
    let cap = if split_rest { usize::MAX } else { 128 };
    let run_at = |j: usize| row[j..].iter().take(cap).take_while(|&&b| b == row[j]).count();
    let mut i = 0;
    while i < n {
        let mut r = run_at(i);
        if r >= min_run {
            // A long run is cut into runs of 128; what is left is a run even of 2, or a
            // literal of its own.
            while r > 0 {
                let k = r.min(128);
                if k == 1 {
                    out.extend_from_slice(&[0, row[i]]);
                } else {
                    out.extend_from_slice(&[(1 - k as i32) as u8, row[i]]);
                }
                i += k;
                r -= k;
            }
        } else {
            let s = i;
            while i < n && i - s < 128 && (i == s || run_at(i) < min_run) {
                i += 1;
            }
            out.push((i - s - 1) as u8);
            out.extend_from_slice(&row[s..i]);
        }
    }
}

/// Appends the encoding of `row` to `out`.
pub fn encode(enc: Encoder, row: &[u8], out: &mut Vec<u8>) {
    match enc {
        Encoder::Photoshop { phase, period } => {
            let mut a = 0;
            let mut end = phase;
            while a < row.len() {
                let b = end.min(row.len());
                for chunk in row[a..b].chunks(128) {
                    encode_chunk(chunk, out);
                }
                a = b;
                end = end.saturating_add(period);
            }
        }
        Encoder::Greedy { min_run, split_rest } => encode_greedy(row, min_run, split_rest, out),
    }
}

/// Splits `data` (row byte counts, then rows) of `rows` rows into the encoded rows.
pub fn rows(data: &[u8], rows: usize, count_bytes: usize) -> Option<Vec<&[u8]>> {
    let mut p = rows * count_bytes;
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let c = data.get(r * count_bytes..(r + 1) * count_bytes)?;
        let n = c.iter().fold(0usize, |n, &b| n << 8 | b as usize);
        out.push(data.get(p..p + n)?);
        p += n;
    }
    Some(out)
}

/// Positions where an encoded row breaks a token although an encoder working over the whole
/// row would not have: a run followed by more of the same byte, or a literal shorter than
/// 128 followed by a literal or a 2-byte run.
pub fn forced_splits(src: &[u8], out: &mut Vec<usize>) {
    let (mut t, mut pos) = (0, 0usize);
    let mut prev: Option<(bool, u8, usize)> = None;
    while t + 1 < src.len() {
        let h = src[t] as i8;
        let (lit, len) = if h >= 0 { (true, h as usize + 1) } else { (false, (1 - h as i32) as usize) };
        if let Some((plit, pv, pl)) = prev {
            if pl < 128 && (!plit && pv == src[t + 1] || plit && (lit || len == 2)) {
                out.push(pos);
            }
        }
        prev = Some((lit, src[t + 1], len));
        pos += len;
        t += if lit { 1 + len } else { 2 };
    }
}

/// The Photoshop tile cut best explaining the token breaks of `encoded` rows.
fn infer_tiles(encoded: &[&[u8]]) -> Encoder {
    let mut splits = Vec::new();
    for r in encoded {
        forced_splits(r, &mut splits);
    }
    infer_tiles_from(splits)
}

/// The Photoshop tile cut best explaining token breaks at `splits` (see [`forced_splits`]).
pub fn infer_tiles_from(mut splits: Vec<usize>) -> Encoder {
    splits.sort_unstable();
    splits.dedup();
    let explained = |phase: usize, period: usize| {
        splits.iter().filter(|&&s| if s < phase { s % 128 == 0 } else { (s - phase) % period % 128 == 0 }).count()
    };
    let Some(&s0) = splits.iter().find(|&&s| s % 128 != 0) else { return NO_TILES };
    let (mut phase, mut period) = (usize::MAX, usize::MAX);
    let mut best = explained(phase, period);
    for c in (0..=s0 / 128).map(|j| s0 - 128 * j) {
        // The next break off the 128 grid from c bounds the period (from below: it is at most
        // that far, less whole chunks).
        let s1 = splits.iter().copied().find(|&s| s > c && (s - c) % 128 != 0);
        let periods: Vec<usize> = match s1 {
            Some(s1) => (0..=(s1 - c) / 128).map(|j| s1 - c - 128 * j).filter(|&p| p >= 128).collect(),
            None => vec![usize::MAX],
        };
        for p in periods {
            let n = explained(c, p);
            if n > best || n == best && c == phase && p > period {
                (best, phase, period) = (n, c, p);
            }
        }
    }
    Encoder::Photoshop { phase, period }
}

/// Candidate encoders, most likely first, after the tile cut inferred for a channel.
pub fn candidates_after(tiles: Encoder) -> Vec<Encoder> {
    let mut v = vec![tiles];
    for min_run in [3, 2] {
        for split_rest in [false, true] {
            v.push(Encoder::Greedy { min_run, split_rest });
        }
    }
    v
}

/// The decoded length of PackBits `src`, or None if it is malformed.
pub fn decoded_len(mut src: &[u8]) -> Option<usize> {
    let mut n = 0;
    while let [h, rest @ ..] = src {
        let h = *h as i8;
        if h >= 0 {
            let k = h as usize + 1;
            src = rest.get(k..)?;
            n += k;
        } else if h != -128 {
            rest.first()?;
            n += (1 - h as isize) as usize;
            src = &rest[1..];
        } else {
            src = rest;
        }
    }
    Some(n)
}

/// Candidate encoders for a channel, most likely first.
pub fn candidates(encoded: &[&[u8]]) -> Vec<Encoder> {
    candidates_after(infer_tiles(encoded))
}

/// The encoder reproducing most of `encoded` rows, and how many it reproduces. Rows that do
/// not decode count as not reproduced.
pub fn identify(encoded: &[&[u8]]) -> (Encoder, usize) {
    let mut data = Vec::new();
    let mut lens = Vec::with_capacity(encoded.len());
    for r in encoded {
        let before = data.len();
        if !unpackbits(r, &mut data) {
            data.truncate(before);
        }
        lens.push(data.len() - before);
    }
    let mut starts = Vec::with_capacity(lens.len() + 1);
    starts.push(0);
    for l in &lens {
        starts.push(starts.last().unwrap() + l);
    }
    let rows: Vec<&[u8]> = (0..encoded.len()).map(|i| &data[starts[i]..starts[i + 1]]).collect();
    let (enc, bad) = identify_rows(encoded, &rows);
    (enc, encoded.len() - bad.len())
}

/// The encoder reproducing most of `encoded` rows from their decoded `rows`, and the rows
/// it does not reproduce.
pub fn identify_rows(encoded: &[&[u8]], rows: &[&[u8]]) -> (Encoder, Vec<usize>) {
    let mut best: Option<(Encoder, Vec<usize>)> = None;
    let mut buf = Vec::new();
    for enc in candidates(encoded) {
        let limit = best.as_ref().map_or(usize::MAX, |b| b.1.len());
        let mut bad = Vec::new();
        for (i, (r, d)) in encoded.iter().zip(rows).enumerate() {
            buf.clear();
            encode(enc, d, &mut buf);
            if buf != *r {
                bad.push(i);
                if bad.len() >= limit {
                    break;
                }
            }
        }
        if bad.len() < limit {
            let done = bad.is_empty();
            best = Some((enc, bad));
            if done {
                break;
            }
        }
    }
    best.unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(enc: Encoder, row: &[u8]) {
        let mut e = Vec::new();
        encode(enc, row, &mut e);
        let mut d = Vec::new();
        assert!(unpackbits(&e, &mut d));
        assert_eq!(d, row);
    }

    #[test]
    fn encoders_roundtrip() {
        let mut row: Vec<u8> = (0..1000u32).map(|i| (i * i / 7 % 5) as u8).collect();
        row.extend(std::iter::repeat_n(9, 300));
        row.extend((0..300u32).map(|i| (i % 251) as u8));
        for enc in [NO_TILES, Encoder::Photoshop { phase: 37, period: 368 }, Encoder::Greedy { min_run: 3, split_rest: true }] {
            roundtrip(enc, &row);
            roundtrip(enc, &[]);
            roundtrip(enc, &[1]);
        }
    }

    #[test]
    fn photoshop_runs() {
        // A pair is a run at the start, but part of a literal after one.
        let mut e = Vec::new();
        encode(NO_TILES, &[5, 5, 1, 2, 2, 3], &mut e);
        assert_eq!(e, [0xff, 5, 3, 1, 2, 2, 3]);
        // Runs stop at chunk and tile boundaries.
        e.clear();
        encode(Encoder::Photoshop { phase: 100, period: 368 }, &[7; 300], &mut e);
        assert_eq!(e, [(1 - 100i32) as u8, 7, 0x81, 7, (1 - 72i32) as u8, 7]);
    }
}
