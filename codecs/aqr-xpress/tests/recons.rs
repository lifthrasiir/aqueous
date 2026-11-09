//! Streams of XPRESS Huffman made by Windows (`tests/data`: the buffers of `RtlCompressBuffer`'s
//! standard and maximum engines, WOF's chunks of 4, 8 and 16 KiB) and by a small encoder of this
//! test, read and written back, and split into data and record and rebuilt.

use aqr_common::rng::Rng;
use aqr_xpress::recons::{self, huffman};
use aqr_xpress::xpress::{self, BLOCK, BlockInfo, END, Parse, SYMS, Tok};

/// A fixture: its chunks (their bytes and their data's length) and the data they hold.
struct Fixture {
    chunks: Vec<(Vec<u8>, usize)>,
    raw: Vec<u8>,
}

fn fixture(name: &str) -> Fixture {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::decode_all(&z[..]).unwrap();
    let u32_at = |p: usize| u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
    let (n, mut at) = (u32_at(0), 4);
    let mut chunks = Vec::new();
    for _ in 0..n {
        let (dl, cl) = (u32_at(at), u32_at(at + 4));
        chunks.push((d[at + 8..at + 8 + cl].to_vec(), dl));
        at += 8 + cl;
    }
    let rl = u32_at(at);
    Fixture { chunks, raw: d[at + 4..at + 4 + rl].to_vec() }
}

fn refs(chunks: &[(Vec<u8>, usize)]) -> Vec<(&[u8], usize)> {
    chunks.iter().map(|(c, n)| (&c[..], *n)).collect()
}

/// The fixtures: Windows' output, whose records are small.
const FIXTURES: [&str; 7] = ["xph-text", "xph-max", "xph-gen", "wof4k", "wof8k", "wof16k", "wof4k-tail"];

#[test]
fn windows_chunks_read_and_written_back() {
    for name in FIXTURES {
        let f = fixture(name);
        let mut at = 0;
        for (c, n) in &f.chunks {
            if c.len() == *n {
                assert_eq!(c[..], f.raw[at..at + n], "{name}: stored");
            } else {
                let (data, parse) = xpress::read(c, *n).unwrap_or_else(|| panic!("{name}: not read"));
                assert_eq!(data[..], f.raw[at..at + n], "{name}: data");
                let mut out = Vec::new();
                xpress::write(&parse, &data, &mut out).unwrap();
                assert_eq!(&out, c, "{name}: written back");
            }
            at += n;
        }
        assert_eq!(at, f.raw.len());
    }
}

#[test]
fn windows_chunks_split_and_joined() {
    for name in FIXTURES {
        let f = fixture(name);
        let cs = refs(&f.chunks);
        let sp = recons::split(&cs).unwrap();
        assert_eq!(sp.data, f.raw, "{name}");
        let comp: usize = cs.iter().map(|c| c.0.len()).sum();
        let want: Vec<u8> = cs.iter().flat_map(|c| c.0.iter().copied()).collect();
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), want, "{name}");
        // (the record is a fraction of the stream: Windows' parse is the one the model expects)
        assert!(sp.record.len() * 8 < comp || comp < 1000, "{name}: record {} of {comp}", sp.record.len());
        let mut data = vec![0; sp.data.len()];
        assert_eq!(recons::split_into(&cs, &mut data).unwrap(), sp.record, "{name}");
        assert_eq!(data, f.raw);
        let mut again = vec![0; sp.data.len()];
        recons::data_into(&cs, &mut again).unwrap();
        assert_eq!(again, f.raw);
    }
}

#[test]
fn windows_stored_chunk() {
    let f = fixture("wof4k-tail");
    assert!(f.chunks.iter().any(|(c, n)| c.len() == *n), "a stored chunk in the fixture");
}

/// The parse of `d` by a greedy encoder: the longest of the last candidates of a 3-byte key,
/// those of every position (`all`) or of the tokens' starts; `jitter` has it take literals and
/// shorter matches at random.
fn parse_of(d: &[u8], rng: &mut Rng, all: bool, jitter: bool) -> Vec<Tok> {
    let mut occ: std::collections::HashMap<[u8; 3], Vec<usize>> = Default::default();
    let (mut toks, mut pos) = (Vec::new(), 0);
    let insert = |occ: &mut std::collections::HashMap<[u8; 3], Vec<usize>>, p: usize| {
        if p + 3 <= d.len() {
            occ.entry(d[p..p + 3].try_into().unwrap()).or_default().push(p);
        }
    };
    while pos < d.len() {
        let (mut best, mut off) = (0, 0);
        if pos + 3 <= d.len() {
            for &q in occ.get(&d[pos..pos + 3]).map_or(&[][..], |v| &v[..]).iter().rev().take(64) {
                if pos - q > xpress::MAX_OFF {
                    break;
                }
                let n = (0..d.len() - pos).take_while(|&i| d[q + i] == d[pos + i]).count();
                if n > best {
                    (best, off) = (n, pos - q);
                }
            }
        }
        if jitter && best >= 3 {
            match rng.below(8) {
                0 => best = 0,
                1 => best = 3 + rng.below(best as u64 - 2) as usize,
                _ => {}
            }
        }
        if best >= 3 {
            toks.push(Tok { len: best as u32, off: off as u32 });
            insert(&mut occ, pos);
            if all {
                (pos + 1..pos + best).for_each(|p| insert(&mut occ, p));
            }
            pos += best;
        } else {
            toks.push(Tok::LIT);
            insert(&mut occ, pos);
            pos += 1;
        }
    }
    toks
}

/// A chunk of `d` encoded: blocks cut as the format says, the end symbol where `end`, code
/// lengths of the block's frequencies (`tweak` has one of them off by one now and then, as long
/// as the code stays complete).
fn encode(d: &[u8], toks: Vec<Tok>, end: bool, rng: &mut Rng, tweak: bool) -> Vec<u8> {
    let mut parse = Parse { end, ..Parse::default() };
    let (mut at, mut t) = (0, 0);
    while at < d.len() {
        let (from, t0) = (at, t);
        let mut freqs = [0u32; SYMS];
        while at < d.len() && at < from + BLOCK {
            let tok = toks[t];
            match tok.len {
                0 => freqs[d[at] as usize] += 1,
                l => freqs[256 + (l as usize - 3).min(15) + 16 * tok.off.ilog2() as usize] += 1,
            }
            at += tok.size();
            t += 1;
        }
        let last = at >= d.len();
        freqs[END] += (last && end) as u32 + 1;
        // (two symbols at least, so that the code is complete)
        freqs[(d[0] as usize + 1) % 256] += 1;
        let mut lens = Box::new([0u8; SYMS]);
        huffman(&freqs, 15, &mut lens[..]);
        if tweak && rng.below(2) == 0 {
            // (a code of the same shape with two lengths swapped)
            let used: Vec<usize> = (0..SYMS).filter(|&s| lens[s] != 0).collect();
            let (a, b) = (used[rng.below(used.len() as u64) as usize], used[rng.below(used.len() as u64) as usize]);
            lens.swap(a, b);
        }
        parse.blocks.push(BlockInfo { lens, toks: t - t0 });
    }
    parse.toks = toks;
    let mut out = Vec::new();
    xpress::write(&parse, d, &mut out).unwrap();
    out
}

/// Data with repeats near and far, a long run, and bytes of every kind.
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut d: Vec<u8> = Vec::new();
    while d.len() < n {
        match rng.below(6) {
            0 => {
                let n = 1 + rng.below(40) as usize;
                d.extend(rng.bytes(n))
            }
            1 if d.len() > 10 => {
                let (a, l) = (rng.below(d.len() as u64) as usize, 3 + rng.below(100) as usize);
                (a..(a + l).min(d.len())).for_each(|i| d.push(d[i]));
            }
            2 if d.len() > 70000 => {
                let a = d.len() - 65535 + rng.below(1000) as usize;
                (a..a + 400).for_each(|i| d.push(d[i]));
            }
            3 => d.extend(std::iter::repeat_n(rng.below(256) as u8, 1 + rng.below(300) as usize)),
            _ => d.extend(b"the quick brown fox jumps over the lazy dog "[..1 + rng.below(40) as usize].iter()),
        }
    }
    d.truncate(n);
    d
}

#[test]
fn encoder_chunks_read_and_written_back() {
    let mut rng = Rng::new(1);
    for k in 0..24 {
        let n = [1, 2, 3, 100, 4096, 70000, 150000][k % 7];
        let d = data(&mut rng, n);
        let toks = parse_of(&d, &mut rng, k % 2 == 0, k % 3 == 0);
        let c = encode(&d, toks.clone(), k % 5 != 0, &mut rng, k % 4 == 0);
        let (back, parse) = xpress::read(&c, n).unwrap_or_else(|| panic!("{k}: not read"));
        assert_eq!(back, d, "{k}");
        assert_eq!(parse.toks, toks, "{k}");
    }
}

#[test]
fn long_matches() {
    // (a match past 65538 bytes has its length as a dword, one past 270 a word, one past 17 a byte)
    for run in [18, 20, 272, 300, 65538 + 20, 200000] {
        let d = vec![7u8; run + 1];
        let toks = vec![Tok::LIT, Tok { len: run as u32, off: 1 }];
        let c = encode(&d, toks, true, &mut Rng::new(0), false);
        let (back, _) = xpress::read(&c, d.len()).unwrap();
        assert_eq!(back, d);
        let sp = recons::split(&[(&c[..], d.len())]).unwrap();
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), c);
    }
}

#[test]
fn split_and_joined() {
    let mut rng = Rng::new(3);
    for k in 0..30 {
        let size = [1, 5, 4096, 8192, 16384, 40000][k % 6];
        let n = [1, 700, 5000, 33000, 70000, 140000][(k / 2) % 6];
        let d = data(&mut rng, n);
        // chunks of `size` bytes of data each (a buffer, where it is as long as the data), some stored
        let mut chunks: Vec<(Vec<u8>, usize)> = Vec::new();
        for part in d.chunks(size.min(n)) {
            let toks = parse_of(part, &mut rng, k % 2 == 0, k % 3 == 0);
            let c = if rng.below(8) == 0 {
                part.to_vec()
            } else {
                encode(part, toks, rng.below(3) != 0, &mut rng, k % 4 == 0)
            };
            chunks.push((c, part.len()));
        }
        let cs = refs(&chunks);
        let sp = match recons::split(&cs) {
            Ok(s) => s,
            Err(e) => panic!("{k}: {e}"),
        };
        assert_eq!(sp.data, d, "{k}");
        let want: Vec<u8> = cs.iter().flat_map(|c| c.0.iter().copied()).collect();
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), want, "{k}");
    }
}

#[test]
fn damaged_records() {
    let mut rng = Rng::new(4);
    let f = fixture("wof4k");
    let cs = refs(&f.chunks);
    let sp = recons::split(&cs).unwrap();
    let want = recons::join(&sp.data, &sp.record).unwrap();
    for _ in 0..300 {
        let mut r = sp.record.clone();
        match rng.below(3) {
            0 => {
                let i = rng.below(r.len() as u64) as usize;
                r[i] ^= 1 << rng.below(8);
            }
            1 => r.truncate(rng.below(r.len() as u64) as usize),
            _ => r.extend(rng.bytes(5)),
        }
        // (an error, or another stream: never a panic)
        if let Ok(out) = recons::join(&sp.data, &r) {
            assert!(out != want || r == sp.record || r.len() >= sp.record.len());
        }
    }
    // damaged data
    for _ in 0..100 {
        let mut d = sp.data.clone();
        let i = rng.below(d.len() as u64) as usize;
        d[i] ^= 0x55;
        let _ = recons::join(&d, &sp.record);
    }
    assert!(recons::join(&sp.data, &[]).is_err());
}

#[test]
fn damaged_streams() {
    let mut rng = Rng::new(5);
    for name in ["xph-text", "wof8k"] {
        let f = fixture(name);
        for (c, n) in f.chunks.iter().take(3) {
            if c.len() == *n {
                continue;
            }
            for _ in 0..200 {
                let mut c = c.clone();
                let i = rng.below(c.len() as u64) as usize;
                c[i] ^= 1 << rng.below(8);
                if rng.below(4) == 0 {
                    c.truncate(i);
                }
                if let Some((d, p)) = xpress::read(&c, *n) {
                    // (read only what writes back as it is)
                    let mut out = Vec::new();
                    xpress::write(&p, &d, &mut out).unwrap();
                    assert_eq!(out, c);
                }
                let _ = recons::split(&[(&c[..], *n)]);
            }
        }
    }
}

#[test]
fn chunks_of_other_sizes_do_not_split() {
    let d = [1u8; 100];
    assert!(recons::split(&[(&d[..], 100), (&d[..], 10), (&d[..], 100)]).is_err());
    assert!(recons::split(&[(&d[..], 10), (&d[..], 100)]).is_err());
    assert!(recons::split(&[(&d[..], 0)]).is_err());
    let sp = recons::split(&[]).unwrap();
    assert!(sp.data.is_empty() && sp.record.is_empty());
    assert!(recons::join(&[], &[]).unwrap().is_empty());
}
