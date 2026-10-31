use aqr_bz2::formula::{self, MODULE};
use aqr_bz2::module::get_params;
use aqr_bz2::{Framing, compress, stream};
use aqr_formula::{Apply, Formula, Node};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, 2).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The fixtures of libbzip2 1.0.8 (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "bz2"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(v.len() >= 10);
    v
}

fn application(f: &Formula) -> &Apply {
    let mut found = None;
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            found = Some(&**a);
        }
    });
    found.expect("an application")
}

#[test]
fn streams_of_libbzip2() {
    for (name, d) in fixtures() {
        let level: u8 = name.rsplit_once('-').unwrap().1.parse().unwrap();
        let s = stream::decode(&d, true, usize::MAX).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((s.level, s.len), (level, d.len()), "{name}");
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let blocks = compress::blocks(&s.data, level, 2);
        assert_eq!(blocks.len(), s.orig_ptrs.len(), "{name}: blocks");
        let f = formula::encode(&d, 2).unwrap_or_else(|| panic!("{name}: no formula"));
        assert!(eval(&f).unwrap() == d, "{name}");
        let p = get_params(&application(&f).params).unwrap();
        // only a periodic block can need its rank noted
        eprintln!("{name}: {:?}", p[0].orig_ptrs);
        assert!(
            p[0].orig_ptrs.is_empty() || name.starts_with("zeros") || name.starts_with("periodic"),
            "{name}: {p:?}"
        );
    }
}

/// libbzip2's code lengths (`scripts/gen-fixtures.py`), Fibonacci frequencies cut down to 17
/// bits among them.
#[test]
fn code_lengths_of_libbzip2() {
    let t = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/code-lengths.txt")).unwrap();
    let mut cut = 0;
    for line in t.lines() {
        let (f, l) = line.split_once(';').unwrap();
        let f: Vec<u32> = f.split(' ').map(|x| x.parse().unwrap()).collect();
        let want: Vec<u8> = l.split(' ').map(|x| x.parse().unwrap()).collect();
        let mut got = vec![0; f.len()];
        aqr_bz2::huffman::code_lengths(&mut got, &f, aqr_bz2::huffman::MAKE_MAX_LEN);
        assert_eq!(got, want, "{f:?}");
        let mut unlimited = vec![0; f.len()];
        aqr_bz2::huffman::code_lengths(&mut unlimited, &f, 255);
        cut += (unlimited != got) as usize;
    }
    assert!(cut >= 5, "{cut} cut down");
}

/// A stream decoded as far as some data, then the rest on several threads, as it is decoded
/// in one go.
#[test]
fn decoded_in_part() {
    for (name, d) in fixtures() {
        let s = stream::decode(&d, true, usize::MAX).unwrap();
        for (want, threads) in [(0, 1), (1, 3), (150_000, 2), (usize::MAX, 4)] {
            let p = stream::Partial::start(&d, want, usize::MAX).unwrap();
            assert!(p.data.len() >= want.min(s.data.len()), "{name}");
            assert_eq!(p.ended.is_some(), p.data.len() == s.data.len() && want > s.data.len(), "{name}");
            let t = p.finish(threads, usize::MAX).unwrap();
            assert!(
                (t.level, &t.data, &t.sizes, &t.orig_ptrs, t.len) == (s.level, &s.data, &s.sizes, &s.orig_ptrs, s.len)
            );
        }
        // too much data, found at the start or as the rest is decoded
        if s.data.len() > 200_000 {
            let r = stream::Partial::start(&d, 1, 200_000).and_then(|p| p.finish(2, 200_000));
            assert!(r.is_err(), "{name}");
        }
    }
    let text = &fixtures().into_iter().find(|f| f.0 == "text-1").unwrap().1;
    let mut bad = text.clone();
    let n = bad.len() - 100;
    bad[n] ^= 1;
    assert!(stream::Partial::start(&bad, 1, usize::MAX).unwrap().finish(2, usize::MAX).is_err());
}

#[test]
fn several_streams_and_what_follows() {
    let fx = fixtures();
    let get = |n: &str| &fx.iter().find(|f| f.0 == n).unwrap().1;
    let d = [get("text-5"), get("empty-9"), get("zeros-1"), get("small-9"), &b"trailing garbage"[..]].concat();
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f).unwrap() == d);
    assert_eq!(get_params(&application(&f).params).unwrap().len(), 4);
    assert_eq!(f.streams[0].data.as_ref(), b"trailing garbage");
}

#[test]
fn turned_down() {
    let fx = fixtures();
    let text = &fx.iter().find(|f| f.0 == "text-5").unwrap().1;
    // another level in the header: the blocks would be cut otherwise
    let mut other = fx.iter().find(|f| f.0 == "text-1").unwrap().1.clone();
    other[3] = b'2';
    assert!(formula::encode(&other, 1).is_none());
    // damaged: a bit flipped in the data, the stream cut short
    let mut bad = text.clone();
    bad[200] ^= 0x10;
    assert!(formula::encode(&bad, 1).is_none());
    assert!(formula::encode(&text[..text.len() - 3], 1).is_none());
    assert!(formula::extent(&text[..text.len() - 3]).is_none());
    assert!(formula::encode(b"BZh9", 1).is_none());
    // a damaged second stream is sliced as it is
    let d = [&text[..], &bad].concat();
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f).unwrap() == d);
    assert_eq!(f.streams[0].data.as_ref(), &bad[..]);
}

#[test]
fn bad_params() {
    let data = b"hello";
    for p in [&[][..], &[0], &[1, 0, 5, 0], &[1, 9, 4, 0], &[1, 9, 5, 0, 0], &[1, 9, 5, 1, 1, 0]] {
        assert!(formula::decode(p, &[data], 0, 1).is_err(), "{p:?}");
    }
    let ok = formula::decode(&[1, 9, 5, 0], &[data], 0, 1);
    assert!(ok.is_err());
    let out = compress::stream(9, &compress::blocks(data, 9, 1));
    assert_eq!(formula::decode(&[1, 9, 5, 0], &[data], out.len() as u64, 1).unwrap(), out);
}

/// ALZip's streams (`tests/data/alz`), by name.
fn alz_fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/alz");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "dlz"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert_eq!(v.len(), 3);
    v
}

/// The streams `take_alz` takes of `d` (one after another) rebuilt by `bz2-alz-recons`.
fn alz_rebuilt(ds: &[&[u8]]) -> Vec<u8> {
    let (mut streams, mut data) = (Vec::new(), Vec::new());
    for d in ds {
        let (sp, dd) = formula::take_alz(d, 2, usize::MAX, &mut |_, _, _| true).expect("taken");
        streams.push(sp);
        data.extend_from_slice(&dd);
    }
    let len = ds.iter().map(|d| d.len() as u64).sum();
    let a = formula::apply_alz(&streams, Node::Concat(Vec::new()), len);
    assert_eq!(a.module, formula::MODULE_ALZ);
    formula::decode_alz(&a.params, &[&data], len, 2).unwrap()
}

#[test]
fn streams_of_alzip() {
    for (name, d) in alz_fixtures() {
        let s = stream::decode_framed(&d, Framing::Alz, true, usize::MAX, &mut |_, _| {}).unwrap();
        assert_eq!((s.level, s.len, s.sizes.len()), (9, d.len(), 1), "{name}");
        assert!(alz_rebuilt(&[&d]) == d, "{name}");
    }
    let fx = alz_fixtures();
    let all: Vec<&[u8]> = fx.iter().map(|f| &f.1[..]).collect();
    assert!(alz_rebuilt(&all) == all.concat());
}

/// Streams of several blocks as the compressor makes them (ALZip's own are not at hand): cut as
/// libbzip2 cuts at level 9, the blocks unaligned; a periodic block's rank noted.
#[test]
fn alz_blocks() {
    let text: Vec<u8> =
        (0..2_500_000u32).map(|i| b"the quick brown fox "[(i % 20) as usize] ^ (i / 997 % 3) as u8).collect();
    // (no runs for RLE1: two blocks, both periodic)
    let periodic: Vec<u8> = b"ab".repeat(600_000);
    for d in [&text, &periodic] {
        let blocks = compress::blocks_framed(d, Framing::Alz, 9, 2);
        assert!(blocks.len() > 1);
        let s = compress::stream_alz(&blocks);
        assert!(alz_rebuilt(&[&s]) == s);
        let (sp, data) = formula::take_alz(&s, 2, usize::MAX, &mut |_, _, _| true).unwrap();
        assert!(data == *d);
        assert_eq!(sp.level, 9);
    }
}

#[test]
fn alz_turned_down() {
    let fx = alz_fixtures();
    let text = &fx.iter().find(|f| f.0 == "text").unwrap().1;
    let none = |d: &[u8]| formula::take_alz(d, 1, usize::MAX, &mut |_, _, _| true).is_none();
    // trailing bytes, cut short, a bit flipped, bzip2's own framing, too much data, not paying
    assert!(none(&[&text[..], b"x"].concat()));
    assert!(none(&text[..text.len() - 1]));
    let mut bad = text.clone();
    bad[40] ^= 4;
    assert!(none(&bad));
    let bz = &fixtures().into_iter().find(|f| f.0 == "small-9").unwrap().1;
    assert!(none(bz));
    assert!(formula::take_alz(text, 1, 100, &mut |_, _, _| true).is_none());
    assert!(formula::take_alz(text, 1, usize::MAX, &mut |_, _, _| false).is_none());
    // params of bz2-recons (with a level) are not those of bz2-alz-recons
    let data = b"hello";
    let out = compress::stream_alz(&compress::blocks_framed(data, Framing::Alz, 9, 1));
    assert_eq!(formula::decode_alz(&[1, 5, 0], &[data], out.len() as u64, 1).unwrap(), out);
    for p in [&[][..], &[1, 9, 5, 0], &[1, 5, 1, 0, 9], &[1, 4, 0], &[1, 5, 0, 0]] {
        assert!(formula::decode_alz(p, &[data], out.len() as u64, 1).is_err(), "{p:?}");
    }
}
