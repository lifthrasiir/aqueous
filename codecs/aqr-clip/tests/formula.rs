//! The formula of `.clip` files evaluated back with the module, on synthetic files.

mod common;

use aqr_clip::formula;
use aqr_clip::tile::Layout;
use aqr_formula::{Apply, Formula, Node};
use common::*;

fn modules(threads: usize) -> impl Fn(&Apply, &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    move |a, inputs| formula::apply(a, inputs, threads).map_err(aqr_formula::Error::new)
}

fn applied(f: &Formula) -> Vec<&'static str> {
    fn walk(n: &Node, out: &mut Vec<&'static str>) {
        match n {
            Node::Apply(a) => {
                out.push(a.module.name);
                a.inputs.iter().for_each(|i| walk(i, out));
            }
            Node::Concat(v) => v.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&f.root, &mut out);
    out
}

/// The formula of `file`: the same on any number of threads, evaluated back on any number.
fn check(file: &[u8]) -> Formula {
    let f = formula::encode(file, 1).expect("a formula");
    assert!(formula::encode(file, 3).unwrap() == f, "the same formula on 3 threads");
    for t in [1, 3] {
        assert!(f.eval(&modules(t)).unwrap() == file, "evaluated back on {t} threads");
    }
    f
}

fn stream<'a>(f: &'a Formula, class: &str) -> &'a [u8] {
    &f.streams.iter().find(|s| s.class == class).unwrap().data
}

#[test]
fn every_kind_of_part() {
    let file = sample(0);
    let f = check(&file);
    assert_eq!(applied(&f), [formula::MODULE.name]);
    // the stored stream stays in the skeleton, so do the 3D data and the headers
    let skeleton = stream(&f, "clip/skeleton");
    assert!(skeleton.len() > 5000 + stored_zlib(&tile(Layout::Color, 3)).len());
    let modelled: usize =
        // (the repeats' streams leave the skeleton too)
        [(Layout::Color, 1), (Layout::Color, 2), (Layout::One, 4), (Layout::One, 5), (Layout::Two, 6)]
            .iter()
            .chain(&[(Layout::Color, 2), (Layout::One, 4), (Layout::Color, 1)])
            .map(|&(l, k)| aqr_clip::zlib::deflate(&tile(l, k)).unwrap().len())
            .sum();
    assert_eq!(skeleton.len(), file.len() - modelled - stream(&f, "clip/database").len());
    // the database, no SQLite database, is content of its format
    assert!(stream(&f, "clip/database").starts_with(b"not a database"));
}

/// A tile repeated is a reference to its first: the tiles' streams hardly grow.
#[test]
fn repeated_tiles_are_references() {
    let one = |n: usize| {
        let blocks: Vec<Block> = (0..n).map(|_| made(Layout::Color, 7)).collect();
        check(&clip_file(&[external(1, &block_data(&blocks))], None))
    };
    let (f1, f50) = (one(1), one(50));
    assert_eq!(stream(&f50, "clip/tiles"), stream(&f1, "clip/tiles"));
    assert!(stream(&f50, "clip/index").len() < 120);
}

#[test]
fn batches_of_many_tiles() {
    // 3 batches of colour tiles, one of each other layout
    let f = check(&sample(150));
    let tiles = stream(&f, "clip/tiles");
    assert!(tiles.len() < 150 * 20_000, "{}", tiles.len());
}

#[test]
fn without_database_or_block_data() {
    let blocks = block_data(&[made(Layout::Color, 9), Block::Empty]);
    check(&clip_file(&[external(1, &blocks)], None));
    check(&clip_file(&[external(1, b"no block data here")], Some(b"db")));
    check(&clip_file(&[], None));
}

#[test]
fn not_clip_files_give_none() {
    let file = sample(0);
    assert!(formula::encode(b"not a clip file at all, not a chunk in it", 1).is_none());
    assert!(formula::encode(&file[..file.len() - 1], 1).is_none());
    // two databases
    let two = clip_file(&[], Some(b"db"));
    let mut two = [&two[..two.len() - 16], &two[two.len() - 30..]].concat();
    let n = two.len() as u64;
    two[8..16].copy_from_slice(&n.to_be_bytes());
    assert!(formula::encode(&two, 1).is_none());
    // a block whose record does not add up
    let mut bad = block_data(&[made(Layout::Color, 9)]);
    bad[3] ^= 1;
    assert!(formula::encode(&clip_file(&[external(1, &bad)], None), 1).is_none());
}

/// Streams altered, cut or emptied make errors (or other files), never panics.
#[test]
fn corrupt_streams_are_errors_not_panics() {
    let file = sample(70);
    let f = formula::encode(&file, 1).unwrap();
    for (k, s) in f.streams.iter().enumerate() {
        let n = s.data.len();
        let mut cases: Vec<Vec<u8>> = vec![Vec::new(), s.data[..n / 2].to_vec(), [&s.data[..], &[1, 2, 3]].concat()];
        for at in [0, 1, n / 3, n / 2, n.saturating_sub(1)] {
            if at < n {
                let mut d = s.data.clone();
                d[at] ^= 0x5a;
                cases.push(d);
            }
        }
        for d in cases {
            let mut g = f.clone();
            g.streams[k].data = d;
            let r = std::panic::catch_unwind(|| g.eval(&modules(3)));
            assert!(r.is_ok(), "stream {} ({}) panicked", k, s.class);
        }
    }
}
