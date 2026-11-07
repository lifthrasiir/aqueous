//! The modules as part modules ([`module::decode_streamed`]: the data through a window, the
//! record read in order, the stream put in pieces) against the whole decoding, on streams far past
//! the window, with dictionaries from 4 KiB, records in parts, chunks of every kind and the inputs
//! given a few bytes at a time.

use aqr_lzma::lzma::{self, Props};
use aqr_lzma::lzma2::{self, Chunk};
use aqr_lzma::recons::{self, Split};
use aqr_lzma::stream::Pull;
use aqr_lzma::window::{self, LOOK, Window};
use aqr_lzma::{alone, greedy, module};

/// Data of `n` bytes of 20 letters and repeats of them (at any distance), from `seed`.
fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if d.len() > 64 && x >> 60 < 6 {
            let len = 2 + (x >> 8) as usize % 30;
            let back = 1 + (x >> 20) as usize % d.len().min(1 << (4 + (x >> 50) as usize % 18));
            let src = d.len() - back.max(len.min(d.len()));
            for j in 0..len {
                d.push(d[src + j]);
            }
        } else {
            d.push(b'a' + (x >> 40) as u8 % 20);
        }
    }
    d.truncate(n);
    d
}

/// An LZMA stream of `d` (greedy, matches within `dict`) and its split, in parts of `part`.
fn stream(d: &[u8], props: Props, dict: u32, marker: bool, part: usize) -> (Vec<u8>, Split) {
    let packets = greedy::packets(d, dict as usize);
    let s = lzma::encode(d, props, &packets, marker);
    let size = (!marker).then_some(d.len() as u64);
    let sp = recons::split_with(&s, props, dict, size, 1 << 28, part, 1).unwrap();
    (s, sp)
}

/// The inputs of the module for the splits, and the streams they make.
fn inputs(splits: &[(Vec<u8>, Split)]) -> ([Vec<u8>; 3], Vec<u8>) {
    let (mut data, mut records, mut lens, mut all) = (vec![], vec![], vec![], vec![]);
    for (s, sp) in splits {
        lens.push((sp.data.len(), sp.record.len()));
        data.extend_from_slice(&sp.data);
        records.extend_from_slice(&sp.record);
        all.extend_from_slice(s);
    }
    ([data, records, aqr_lzma::formula::index(&lens)], all)
}

/// The module whole (on one thread and on four: streams, and a stream's parts, side by side) and
/// through windows, `lzma2` or not, on `inputs`: the same, and `want`.
fn same(what: &str, i: &[Vec<u8>; 3], want: &[u8], lzma2: bool) {
    let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
    let streamed = module::decode_streamed(&[], &r, want.len() as u64, lzma2);
    assert!(streamed.as_deref().ok() == Some(want), "{what}: streamed");
    for threads in [1, 4] {
        let whole = if lzma2 {
            module::decode2(&[], &r, want.len() as u64, threads)
        } else {
            module::decode(&[], &r, want.len() as u64, threads)
        };
        assert!(whole.as_deref().ok() == Some(want), "{what}: whole on {threads} threads");
        assert!(whole == streamed, "{what}: whole on {threads} threads differs from streamed");
    }
}

#[test]
fn lzma_through_windows() {
    let props = |lc, lp, pb| Props { lc, lp, pb };
    let mut cases = Vec::new();
    // (past the window of a small dictionary many times; a dictionary of the data's size or more)
    for (k, &(n, lc, lp, dict, marker, part)) in [
        (3 << 20, 3, 0, 1 << 12, false, usize::MAX),
        (3 << 20, 0, 4, 1 << 16, true, usize::MAX),
        (2 << 20, 8, 4, 1 << 20, false, usize::MAX),
        (300_000, 3, 0, 1 << 26, false, usize::MAX),
        // (in parts, cut anywhere)
        (1 << 20, 3, 0, 1 << 14, true, 100_000),
        (1 << 20, 1, 1, 1 << 18, false, 300_000),
        (50_000, 3, 0, 1 << 12, false, 70),
        (1, 3, 0, 1 << 12, false, usize::MAX),
    ]
    .iter()
    .enumerate()
    {
        let d = data(n, k as u64);
        cases.push((
            format!("n {n} lc{lc} lp{lp} dict {dict} part {part}"),
            vec![stream(&d, props(lc, lp, 2), dict, marker, part)],
        ));
    }
    // (several streams, after one another)
    let many: Vec<_> = (0..12u64)
        .map(|k| {
            stream(
                &data([1, 7, 300, 5000, 70_000][k as usize % 5], 50 + k),
                props(k as u32 % 9, k as u32 / 9 % 5, k as u32 % 5),
                1 << (12 + k % 6),
                k % 3 == 0,
                usize::MAX,
            )
        })
        .collect();
    cases.push(("several".into(), many));
    // (the fixtures of XZ Utils)
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|e| e == "lzma") {
            let f = std::fs::read(&p).unwrap();
            let h = alone::parse(&f).unwrap();
            let s = &f[alone::HEADER..];
            let sp = recons::split(s, h.props, h.dict, h.size, 1 << 26, 1).unwrap();
            cases.push((p.file_name().unwrap().to_str().unwrap().into(), vec![(s.to_vec(), sp)]));
        }
    }
    for (what, splits) in cases {
        let (i, all) = inputs(&splits);
        same(&what, &i, &all, false);
    }
}

/// The chunks of at most `max` bytes of data over `packets`, the first `first` and the others
/// these controls in turn.
fn chunks(packets: &[lzma::Packet], props: Props, max: usize, first: u8, controls: &[u8]) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut len = 0;
    let push = |out: &mut Vec<Chunk>, len| {
        let control = if out.is_empty() { first } else { controls[(out.len() - 1) % controls.len()] };
        out.push(Chunk { control, len, props: (control >= 0xC0).then_some(props) });
    };
    for pk in packets {
        if len + pk.len as usize > max {
            push(&mut out, len);
            len = 0;
        }
        len += pk.len as usize;
    }
    push(&mut out, len);
    out
}

/// An LZMA2 stream of one segment of `n` bytes: `stored` of them in a chunk of their own first,
/// the others in chunks of `max` bytes of data, with these controls.
fn segment(seed: u64, n: usize, props: Props, dict: usize, max: usize, controls: &[u8], stored: usize) -> Vec<u8> {
    let d = data(n, seed);
    let packets = greedy::packets(&d[stored..], dict);
    let mut cs = chunks(&packets, props, max, if stored > 0 { 0xC0 } else { 0xE0 }, controls);
    if stored > 0 {
        cs.insert(0, Chunk { control: 1, len: stored, props: None });
    }
    lzma2::encode(&d, &cs, &packets).expect("chunks of the packets")
}

#[test]
fn lzma2_through_windows() {
    let props = |lc, lp, pb| Props { lc, lp, pb };
    let end = |mut s: Vec<u8>| {
        s.push(0);
        s
    };
    // (past the window, chunks of 40 KB; stored chunks of 64 KB, which the window must hold with
    // the lookahead; the dictionary at its smallest)
    let mut cases: Vec<(String, Vec<u8>, u32)> =
        vec![("40k".into(), end(segment(1, 3 << 20, props(3, 0, 2), 1 << 12, 40_000, &[0x80, 0xA0], 0)), 1 << 12)];
    cases.push((
        "stored".into(),
        end(segment(2, 1 << 20, props(3, 0, 2), 1 << 14, 50_000, &[0x80, 0xC0], 65_536)),
        1 << 14,
    ));
    cases.push((
        "lc0 lp4".into(),
        end(segment(3, 1 << 20, props(0, 4, 4), 1 << 16, 3000, &[0xC0, 0xA0], 100)),
        1 << 16,
    ));
    cases.push((
        "big dict".into(),
        end(segment(4, 400_000, props(4, 0, 0), 1 << 26, 20_000, &[0x80, 0xC0], 0)),
        1 << 26,
    ));
    // (segments from dictionary resets)
    let mut segs = Vec::new();
    for k in 0..9usize {
        let n = [1, 50, 70_000, 900, 300_000][k % 5];
        segs.extend(segment(
            10 + k as u64,
            n,
            props(k as u32 % 5, (4 - k as u32 % 5).min(k as u32 / 5), k as u32 % 5),
            1 << 14,
            4000,
            &[0x80, 0xA0, 0xC0],
            if k % 3 == 1 && n >= 700 { 80 } else { 0 },
        ));
    }
    cases.push(("segments".into(), end(segs), 1 << 14));
    // (the fixtures of XZ Utils)
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|e| e == "lzma2") {
            cases.push((p.file_name().unwrap().to_str().unwrap().into(), std::fs::read(&p).unwrap(), 8 << 20));
        }
    }
    for (what, s, dict) in cases {
        let sp = lzma2::split(&s, dict, 1 << 28, 1).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        let (mut data, mut records, mut lens) = (vec![], vec![], vec![]);
        for x in &sp {
            lens.push((x.data.len(), x.record.len()));
            data.extend_from_slice(&x.data);
            records.extend_from_slice(&x.record);
        }
        same(&what, &[data, records, aqr_lzma::formula::index(&lens)], &s, true);
    }
}

/// A source giving its bytes a few at a time.
struct Dribble<'a>(&'a [u8], u64);

impl Pull for Dribble<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        self.1 = self.1.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let n = ((self.1 >> 60) as usize + 1).min(self.0.len()).min(buf.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

/// A window on data that comes a few bytes at a time shows the same bytes as the data, from
/// `reach` before the position to the lookahead after it, whatever it dropped.
#[test]
fn window_shows_the_data() {
    let d = data(1 << 20, 99);
    for reach in [1 << 12, 5000, 1 << 16] {
        let mut feed = Dribble(&d, 5);
        let mut w = Window::fed(&mut feed, d.len(), reach);
        let mut pos = 0;
        let mut x = 3u64;
        while pos < d.len() {
            w.ensure(pos).unwrap();
            let v = w.view();
            let (lo, hi) = (pos.saturating_sub(reach), (pos + LOOK).min(d.len()));
            assert!(v.base <= lo && v.base + v.d.len() >= hi, "{reach} {pos}");
            assert_eq!(v.base % 4096, 0);
            assert!(v.get(lo, hi) == &d[lo..hi], "{reach} {pos}");
            assert!(v.d.len() <= window::room(d.len(), reach));
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            pos += 1 + (x >> 40) as usize % 300;
        }
        w.finish().unwrap();
    }
}

/// Inputs damaged or cut short fail, or give what the whole module gives: never a panic.
#[test]
fn damaged_inputs() {
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let d = data(200_000, 7);
    let splits = vec![stream(&d, props, 1 << 14, false, 60_000), stream(&d[..5000], props, 1 << 12, true, usize::MAX)];
    let (i, all) = inputs(&splits);
    let mut x = 11u64;
    for k in 0..3 {
        for _ in 0..40 {
            let mut j = i.clone();
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let at = (x >> 33) as usize % j[k].len();
            j[k][at] ^= 1 << (x >> 29 & 7);
            let r: Vec<&[u8]> = j.iter().map(|v| &v[..]).collect();
            let whole = module::decode(&[], &r, all.len() as u64, 1);
            let streamed = module::decode_streamed(&[], &r, all.len() as u64, false);
            // (what is given is the whole module's)
            if let Ok(s) = streamed {
                assert!(whole.as_ref().is_ok_and(|w| *w == s), "input {k} at {at}");
            }
        }
        let mut j = i.clone();
        j[k].truncate(j[k].len() - 1);
        let r: Vec<&[u8]> = j.iter().map(|v| &v[..]).collect();
        assert!(module::decode_streamed(&[], &r, all.len() as u64, false).is_err(), "input {k} cut");
    }
    let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
    assert!(module::decode_streamed(&[], &r, all.len() as u64 + 1, false).is_err());
    assert!(module::decode_streamed(&[], &r, all.len() as u64 - 1, false).is_err());
}
