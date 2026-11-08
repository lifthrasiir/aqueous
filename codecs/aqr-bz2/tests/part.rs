//! The modules as part modules ([`module::run`] through `decode_streamed`: the data pulled in
//! pieces, the stream put in pieces) against the whole decoding, on streams past the pieces, of
//! every level, with runs across the blocks' ends, ranks given, and inputs given a few bytes
//! at a time; and truncated, extended and damaged inputs.

use aqr_api::stream::{Pull, Put};
use aqr_bz2::module::{self, StreamParams};
use aqr_bz2::stream;
use aqr_bz2::{Framing, compress};

/// Data of `n` bytes of letters, runs of one byte (4 to 300 long) and repeats, from `seed`.
fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        match x >> 61 {
            0 => d.extend(std::iter::repeat_n((x >> 8) as u8, 4 + (x >> 20) as usize % 297)),
            1 if d.len() > 64 => {
                let len = 2 + (x >> 8) as usize % 30;
                let src = (x >> 20) as usize % (d.len() - len.min(d.len() - 1));
                for j in 0..len {
                    d.push(d[src + j]);
                }
            }
            _ => d.push(b'a' + (x >> 40) as u8 % 20),
        }
    }
    d.truncate(n);
    d
}

/// The params of `streams` of `framing`.
fn params(framing: Framing, streams: &[StreamParams]) -> Vec<u8> {
    let mut p = Vec::new();
    module::put_framed(&mut p, framing, streams);
    p
}

/// A [`Pull`] giving at most `k` bytes at a time.
struct Few<'a>(&'a [u8], usize);

impl Pull for Few<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.len().min(buf.len()).min(self.1);
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

/// A [`Put`] taking `cap` bytes at most.
struct Capped(Vec<u8>, usize);

impl Put for Capped {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        (self.0.len() + d.len() <= self.1).then(|| self.0.extend_from_slice(d))
    }
}

/// The whole decoding and the part module's of the params and data: the same, whatever the
/// pieces the data comes in, and `want` where given.
fn same(what: &str, framing: Framing, p: &[u8], d: &[u8], want: Option<&[u8]>) -> Option<Vec<u8>> {
    let whole = module::compress_framed(d, framing, p, usize::MAX, 1);
    let len = whole.as_ref().map_or(0, |w| w.len() as u64);
    let part = module::decode_streamed(framing, p, &[d], len);
    assert_eq!(part.as_ref().ok(), whole.as_ref().ok(), "{what}: part against whole");
    if let Ok(w) = &whole {
        if let Some(want) = want {
            assert!(w == want, "{what}: not the stream wanted");
        }
        for k in [1, 7, 4093] {
            let mut out = Capped(Vec::new(), w.len());
            module::run(framing, p, &mut Few(d, k), &mut out).unwrap_or_else(|e| panic!("{what}: {k} at a time: {e}"));
            assert!(out.0 == *w, "{what}: {k} at a time");
        }
        // an output of another length is refused
        if !w.is_empty() {
            let mut out = Capped(Vec::new(), w.len() - 1);
            assert!(module::run(framing, p, &mut Few(d, 4093), &mut out).is_err(), "{what}: short output");
        }
    }
    whole.ok()
}

#[test]
fn streams_through_pieces() {
    // (3 MB at level 1 is 31 blocks; level 9 is 4; many pieces of 64 KiB either way)
    for (k, &(n, level)) in
        [(0, 1), (1, 1), (5, 9), (3000, 3), (200_000, 1), (3 << 20, 1), (3 << 20, 9)].iter().enumerate()
    {
        let d = data(n, k as u64);
        let sp = StreamParams { level, len: n as u64, orig_ptrs: vec![] };
        let p = params(Framing::Bzip2, &[sp]);
        let want = compress::stream(level, &compress::blocks(&d, level, 1));
        same(&format!("{n} at {level}"), Framing::Bzip2, &p, &d, Some(&want));
    }
}

#[test]
fn runs_across_the_ends_of_blocks() {
    // runs of 255 (five symbols each) fill a level 1 block after 19997 of them, which cuts
    // inside a long run of one byte too: the run under way goes whole to the next block
    for (name, d) in [
        ("255s", (0..20 * 20000 * 255usize / 20).map(|i| (i / 255 % 2) as u8).collect::<Vec<u8>>()),
        ("one run", vec![7; 1 << 20]),
        ("alternating", (0..300_000).map(|i| (i % 2) as u8).collect()),
        // (the last byte alone: no block starts for one byte)
        ("lonely last", {
            let mut d: Vec<u8> = (0..99_981).map(|i| (i % 2) as u8).collect();
            d.push(9);
            d
        }),
        ("lonely last but one", {
            let mut d: Vec<u8> = (0..99_981).map(|i| (i % 2) as u8).collect();
            d.extend([9, 9]);
            d
        }),
    ] {
        let sp = StreamParams { level: 1, len: d.len() as u64, orig_ptrs: vec![] };
        let p = params(Framing::Bzip2, &[sp]);
        let want = compress::stream(1, &compress::blocks(&d, 1, 1));
        same(name, Framing::Bzip2, &p, &d, Some(&want));
    }
}

#[test]
fn streams_one_after_another_and_alz() {
    // runs may not cross from one stream's data to the next's
    let (a, b, c) = (vec![5u8; 1000], vec![5u8; 70_000], data(250_000, 4));
    let mut all = [a.clone(), b.clone(), c.clone()].concat();
    let sp = |level, d: &[u8]| StreamParams { level, len: d.len() as u64, orig_ptrs: vec![] };
    let p = params(Framing::Bzip2, &[sp(2, &a), sp(1, &[]), sp(9, &b), sp(1, &c)]);
    let want: Vec<u8> = [(2, &a[..]), (1, &[][..]), (9, &b[..]), (1, &c[..])]
        .iter()
        .flat_map(|&(l, d)| compress::stream(l, &compress::blocks(d, l, 1)))
        .collect();
    same("four streams", Framing::Bzip2, &p, &all, Some(&want));
    let sp9 = |d: &[u8]| StreamParams { level: 9, len: d.len() as u64, orig_ptrs: vec![] };
    let p = params(Framing::Alz, &[sp9(&a), sp9(&b), sp9(&c)]);
    let want: Vec<u8> = [&a, &b, &c]
        .iter()
        .flat_map(|d| {
            let blocks = compress::blocks_framed(d, Framing::Alz, 9, 1);
            compress::stream_framed(Framing::Alz, 9, &blocks)
        })
        .collect();
    same("alz", Framing::Alz, &p, &all, Some(&want));
    // data of another length
    all.push(0);
    assert!(same("longer data", Framing::Alz, &p, &all, None).is_none());
    all.truncate(all.len() - 2);
    assert!(same("shorter data", Framing::Alz, &p, &all, None).is_none());
}

#[test]
fn ranks_given() {
    // the fixtures' periodic blocks order equal rotations otherwise: their ranks are given
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut found = 0;
    for name in ["periodic-1", "zeros-1", "runs-1", "text-1"] {
        let bz = std::fs::read(format!("{dir}/{name}.bz2")).unwrap();
        let f = aqr_bz2::formula::encode(&bz, 1).unwrap_or_else(|| panic!("{name}"));
        let mut app = None;
        f.root.visit(&mut |n| {
            if let aqr_formula::Node::Apply(a) = n {
                app = Some((a.params.clone(), a.len));
            }
        });
        let (p, len) = app.unwrap();
        let s = stream::decode(&bz, true, usize::MAX).unwrap();
        found += module::get_params(&p).unwrap().iter().map(|s| s.orig_ptrs.len()).sum::<usize>();
        let out = same(name, Framing::Bzip2, &p, &s.data, Some(&bz));
        assert_eq!(out.map(|o| o.len() as u64), Some(len));
        // a rank past the block's symbols, a rank for a block that is not there
        let mut sp = module::get_params(&p).unwrap();
        let next = sp[0].orig_ptrs.last().map_or(0, |l| l.0 + 1) + 40;
        sp[0].orig_ptrs.push((next, 3));
        assert!(same(name, Framing::Bzip2, &params(Framing::Bzip2, &sp), &s.data, None).is_none());
    }
    assert!(found > 0, "no rank given in the fixtures");
}

#[test]
fn damaged_params_and_data() {
    let d = data(400_000, 9);
    let sp = StreamParams { level: 1, len: d.len() as u64, orig_ptrs: vec![(1, 5), (3, 7)] };
    let p = params(Framing::Bzip2, &[sp]);
    // (a rank above its block's symbols fails the same way in both)
    same("ranks", Framing::Bzip2, &p, &d, None);
    for cut in 0..p.len() {
        same(&format!("params cut at {cut}"), Framing::Bzip2, &p[..cut], &d, None);
    }
    for i in 0..p.len() {
        let mut q = p.clone();
        q[i] ^= 0x55;
        same(&format!("params damaged at {i}"), Framing::Bzip2, &q, &d, None);
    }
    let mut q = p.clone();
    q.push(0);
    assert!(same("params extended", Framing::Bzip2, &q, &d, None).is_none());
}
