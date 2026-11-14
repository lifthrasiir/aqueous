//! LZNT1 made by Windows (`tests/data`: NTFS's compression units of a text file and of a mixed
//! one, `RtlCompressBuffer`'s buffers) and by a small encoder of this test, read and written
//! back, and split into data and record and rebuilt.

use aqr_common::rng::Rng;
use aqr_lznt1::lznt1::{self, CHUNK, Tok, Unit};
use aqr_lznt1::recons;

/// A fixture: its units (their bytes and their data's length) and the data they hold.
struct Fixture {
    units: Vec<(Vec<u8>, usize)>,
    raw: Vec<u8>,
}

fn fixture(name: &str) -> Fixture {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::decode_all(&z[..]).unwrap();
    let u32_at = |p: usize| u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
    let (n, mut at) = (u32_at(0), 4);
    let mut units = Vec::new();
    for _ in 0..n {
        let (dl, cl) = (u32_at(at), u32_at(at + 4));
        units.push((d[at + 8..at + 8 + cl].to_vec(), dl));
        at += 8 + cl;
    }
    let rl = u32_at(at);
    Fixture { units, raw: d[at + 4..at + 4 + rl].to_vec() }
}

fn refs(units: &[(Vec<u8>, usize)]) -> Vec<(&[u8], usize)> {
    units.iter().map(|(c, n)| (&c[..], *n)).collect()
}

/// The fixtures: Windows' output, whose records are small.
const FIXTURES: [&str; 6] = ["ntfs-text", "ntfs-mixed", "buf-text", "buf-gen", "buf-noise", "buf-mixed"];

#[test]
fn windows_units_read_and_written_back() {
    for name in FIXTURES {
        let f = fixture(name);
        let (mut at, mut dec, mut u) = (0, Vec::new(), Unit::default());
        for (b, n) in &f.units {
            lznt1::read_unit(b, &mut dec, &mut u).unwrap_or_else(|| panic!("{name}: not read"));
            // (a unit holds the data and zeros where Windows compressed past the end of a file)
            assert_eq!(dec[..*n.min(&dec.len())], f.raw[at..at + n.min(&dec.len())], "{name}: data");
            assert!(dec[*n.min(&dec.len())..].iter().all(|&x| x == 0), "{name}: zeros");
            at += n;
            // written back, chunk by chunk
            let (mut out, mut base) = (Vec::new(), 0);
            for c in &u.chunks {
                let d = &dec[base..base + c.len];
                if c.stored {
                    lznt1::write_stored(d, &mut out).unwrap();
                } else {
                    lznt1::write_chunk(&u.toks[c.toks.clone()], c.slack, d, &mut out).unwrap();
                }
                base += c.len;
            }
            assert_eq!(out[..], b[..u.used], "{name}: written back");
            assert_eq!(b[u.used..u.used + 2 * u.end as usize], vec![0; 2 * u.end as usize][..]);
        }
        assert_eq!(at, f.raw.len(), "{name}");
    }
}

#[test]
fn windows_units_split_and_joined() {
    for name in FIXTURES {
        let f = fixture(name);
        let us = refs(&f.units);
        let sp = recons::split(&us).unwrap();
        assert_eq!(sp.data, f.raw, "{name}");
        let comp: usize = us.iter().map(|c| c.0.len()).sum();
        let want: Vec<u8> = us.iter().flat_map(|c| c.0.iter().copied()).collect();
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), want, "{name}");
        // (the record is a fraction of the stream: Windows' parse is the one the model expects)
        assert!(sp.record.len() * 50 < comp || comp < 20000, "{name}: record {} of {comp}", sp.record.len());
        let mut data = vec![0; sp.data.len()];
        assert_eq!(recons::split_into(&us, &mut data).unwrap(), sp.record, "{name}");
        assert_eq!(data, f.raw);
        let mut again = vec![0; sp.data.len()];
        recons::data_into(&us, &mut again).unwrap();
        assert_eq!(again, f.raw);
    }
}

/// Windows' units of a text file: the last one of 57,535 bytes of data holds 16 chunks, the zeros
/// past the end compressed, the end header and zero padding up to the cluster.
#[test]
fn windows_last_unit_is_padded_with_zeros() {
    let f = fixture("ntfs-text");
    assert_eq!(f.units.last().unwrap().1, 57535);
    let (mut dec, mut u) = (Vec::new(), Unit::default());
    lznt1::read_unit(&f.units[1].0, &mut dec, &mut u).unwrap();
    assert_eq!((u.chunks.len(), dec.len(), u.end), (16, 65536, true));
    let b = &f.units[1].0;
    assert!(b.len().is_multiple_of(4096) && b[u.used + 2..].iter().all(|&x| x == 0));
    assert!(u.chunks.iter().all(|c| !c.stored && c.len == CHUNK));
}

/// The parse of `d` by a greedy encoder: the longest of the last candidates of a 3-byte key,
/// those of every position (`all`) or of the tokens' starts; `jitter` has it take literals and
/// shorter matches at random.
fn parse_of(d: &[u8], rng: &mut Rng, all: bool, jitter: bool) -> Vec<Tok> {
    type Occ = std::collections::HashMap<[u8; 3], Vec<usize>>;
    let mut occ: Occ = Default::default();
    let (mut toks, mut pos) = (Vec::new(), 0);
    let insert = |occ: &mut Occ, p: usize| {
        if p + 3 <= d.len() {
            occ.entry(d[p..p + 3].try_into().unwrap()).or_default().push(p);
        }
    };
    while pos < d.len() {
        let (mut best, mut off) = (0, 0);
        let cap = (d.len() - pos).min(lznt1::max_len(pos));
        if pos + 3 <= d.len() {
            for &q in occ.get(&d[pos..pos + 3]).map_or(&[][..], |v| &v[..]).iter().rev().take(64) {
                let n = (0..cap).take_while(|&i| d[q + i] == d[pos + i]).count();
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
            toks.push(Tok { len: best as u16, off: off as u16 });
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

/// A chunk of `d` (1 to 4096 bytes) encoded: stored where it would not shrink, the unused flag
/// bits of the last group set at random where `tweak`.
fn chunk(d: &[u8], rng: &mut Rng, all: bool, jitter: bool, tweak: bool) -> Vec<u8> {
    let toks = parse_of(d, rng, all, jitter);
    let k = toks.len() % 8;
    let slack = if tweak && k != 0 { rng.below(1 << (8 - k)) as u8 } else { 0 };
    let mut out = Vec::new();
    if lznt1::write_chunk(&toks, slack, d, &mut out).is_none() || out.len() >= d.len() + 2 {
        out.clear();
        lznt1::write_stored(d, &mut out).unwrap();
    }
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
            2 if d.len() > 5000 => {
                let a = d.len() - 4095 + rng.below(1000) as usize;
                (a..a + 400).for_each(|i| d.push(d[i]));
            }
            3 => d.extend(std::iter::repeat_n(rng.below(256) as u8, 1 + rng.below(300) as usize)),
            _ => d.extend(b"the quick brown fox jumps over the lazy dog "[..1 + rng.below(40) as usize].iter()),
        }
    }
    d.truncate(n);
    d
}

/// What a test makes a unit of: the bytes after the chunks.
#[derive(Clone, Copy)]
enum Tail {
    /// Nothing (a buffer).
    None,
    /// An end header, zeros to the cluster.
    Zeros,
    /// An end header or not, other bytes to the cluster.
    Junk,
}

/// A compressed unit of `d` (the data and zeros past it, in chunks of `cs`): its bytes.
fn unit_of(d: &[u8], cs: usize, cluster: usize, tail: Tail, rng: &mut Rng, k: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for part in d.chunks(cs) {
        out.extend(chunk(part, rng, k.is_multiple_of(2), k.is_multiple_of(3), k.is_multiple_of(4)));
    }
    let used = out.len();
    match tail {
        Tail::None => {}
        Tail::Zeros => {
            out.extend([0, 0]);
            out.resize(out.len().next_multiple_of(cluster), 0);
        }
        Tail::Junk => {
            if rng.below(2) == 0 {
                out.extend([0, 0]);
            }
            let to = (out.len() + 1).next_multiple_of(cluster);
            // (not a chunk header: a signature other than 3)
            out.extend((out.len()..to).map(|_| 0x80 | rng.below(0x10) as u8));
            assert!(used < out.len());
        }
    }
    out
}

#[test]
fn split_and_joined() {
    let mut rng = Rng::new(3);
    for k in 0..60 {
        let us = [4096, 5000, 16384, 20000, 65536][k % 5];
        let count = 1 + (k / 5) % 4;
        let mut units: Vec<(Vec<u8>, usize)> = Vec::new();
        let mut raw = Vec::new();
        let cluster = [1, 512, 4096][k % 3];
        for i in 0..count {
            let last = i + 1 == count;
            let n = if last { 1 + rng.below(us as u64) as usize } else { us };
            let d = if rng.below(6) == 0 { vec![0; n] } else { data(&mut rng, n) };
            let kind = rng.below(8);
            let bytes = if kind == 0 {
                Vec::new()
            } else if kind == 1 {
                d.clone()
            } else {
                // (the last unit may hold zeros past its data, to the end of its chunk or the unit)
                let mut ext = d.clone();
                if last && rng.below(2) == 0 {
                    ext.resize(if rng.below(2) == 0 { n.next_multiple_of(CHUNK).min(us) } else { us }, 0);
                }
                let cs = if k % 7 == 3 && rng.below(2) == 0 { 1 + rng.below(4096) as usize } else { CHUNK };
                let tail = [Tail::None, Tail::Zeros, Tail::Junk][rng.below(3) as usize];
                let mut b = unit_of(&ext, cs, cluster, tail, &mut rng, k);
                if b.len() == n {
                    b.push(0);
                }
                b
            };
            raw.extend(if kind == 0 { vec![0; n] } else { d });
            units.push((bytes, n));
        }
        // (a stream of a single unit knows its unit length from the chunks, not from the data)
        let us_ = refs(&units);
        let sp = match recons::split(&us_) {
            Ok(s) => s,
            Err(e) => panic!("{k}: {e}"),
        };
        assert_eq!(sp.data, raw, "{k}");
        let want: Vec<u8> = us_.iter().flat_map(|c| c.0.iter().copied()).collect();
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), want, "{k}");
        let mut data = vec![1; raw.len()];
        assert_eq!(recons::split_into(&us_, &mut data).unwrap(), sp.record, "{k}");
        assert_eq!(data, raw, "{k}");
    }
}

#[test]
fn holes_stored_and_compressed_units() {
    let f = fixture("ntfs-text");
    let mut rng = Rng::new(9);
    let noise = rng.bytes(65536);
    let units: Vec<(&[u8], usize)> = vec![
        (&f.units[0].0, 65536),
        (&[], 65536),
        (&noise, 65536),
        (&[], 65536),
        (&f.units[1].0, 65536),
        (&f.units[1].0, 57535),
    ];
    // (the last unit's data shorter than the chunks hold: its zeros past it)
    let sp = recons::split(&units).unwrap();
    assert_eq!(sp.data.len(), 5 * 65536 + 57535);
    assert!(sp.data[65536..2 * 65536].iter().all(|&x| x == 0));
    assert_eq!(sp.data[2 * 65536..3 * 65536], noise[..]);
    let want: Vec<u8> = units.iter().flat_map(|c| c.0.iter().copied()).collect();
    assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), want);
    // holes, stored and compressed units cost the record a few bytes
    let all = recons::split(&units[..2]).unwrap();
    assert!(all.record.len() < sp.record.len());
    let holes = recons::split(&[(&[][..], 65536), (&[][..], 65536), (&[][..], 100)]).unwrap();
    assert_eq!(holes.data, vec![0; 2 * 65536 + 100]);
    assert!(holes.record.len() <= 12, "{}", holes.record.len());
    assert!(recons::join(&holes.data, &holes.record).unwrap().is_empty());
}

#[test]
fn damaged_records() {
    let mut rng = Rng::new(4);
    let f = fixture("ntfs-mixed");
    let us = refs(&f.units);
    let sp = recons::split(&us).unwrap();
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
fn damaged_units() {
    let mut rng = Rng::new(5);
    for name in ["ntfs-text", "buf-mixed"] {
        let f = fixture(name);
        for (b, n) in f.units.iter().take(2) {
            for _ in 0..100 {
                let mut b = b.clone();
                let i = rng.below(b.len().min(9000) as u64) as usize;
                b[i] ^= 1 << rng.below(8);
                if rng.below(4) == 0 {
                    b.truncate(i);
                }
                // (it reads whole and rebuilds, or it does not split)
                if let Ok(sp) = recons::split(&[(&b[..], *n)]) {
                    assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), b);
                }
            }
        }
    }
}

#[test]
fn units_of_other_lengths_do_not_split() {
    let d = [1u8; 100];
    assert!(recons::split(&[(&d[..], 100), (&d[..], 10), (&d[..], 100)]).is_err());
    assert!(recons::split(&[(&d[..], 10), (&d[..], 100)]).is_err());
    assert!(recons::split(&[(&d[..], 0)]).is_err());
    let sp = recons::split(&[]).unwrap();
    assert!(sp.data.is_empty() && sp.record.is_empty());
    assert!(recons::join(&[], &[]).unwrap().is_empty());
    // (a compressed unit with data other than zeros past the data's end)
    let f = fixture("ntfs-text");
    assert!(recons::split(&[(&f.units[0].0, 1000)]).is_err());
}

#[test]
fn join_stops_at_its_limit() {
    let f = fixture(FIXTURES[0]);
    let cs = refs(&f.units);
    let sp = recons::split(&cs).unwrap();
    let want = recons::join(&sp.data, &sp.record).unwrap();
    let mut out = Vec::new();
    recons::join_onto(&sp.data, &sp.record, &mut out, want.len()).unwrap();
    assert_eq!(out, want);
    assert!(recons::join_onto(&sp.data, &sp.record, &mut Vec::new(), want.len() / 2).is_err());
}
