//! Edge cases of `.clip` files: tiles of noise, streams zlib would not make (other levels,
//! trailing bytes, a wrong Adler-32, another inflated length), an embedded SQLite database,
//! the container's and the tables' numbers at their extremes, and damaged files and streams
//! (no panic, no hang, no allocation the data cannot hold).

mod common;

use aqr_archive::Evaluate;
use aqr_clip::formula;
use aqr_clip::tile::{AREA, Layout};
use aqr_flate::zlib::{Params, Strategy, compress};
use aqr_formula::{Apply, Formula};
use common::*;
use std::time::Duration;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn modules(threads: usize) -> impl Fn(&Apply, &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    move |a, inputs| formula::apply(a, inputs, threads).map_err(aqr_formula::Error::new)
}

fn check(file: &[u8]) -> Formula {
    let f = formula::encode(file, 1).expect("a formula");
    for t in [1, 3] {
        assert!(f.eval(&modules(t)).unwrap() == file, "evaluated back on {t} threads");
    }
    f
}

fn stream<'a>(f: &'a Formula, class: &str) -> &'a [u8] {
    &f.streams.iter().find(|s| s.class == class).unwrap().data
}

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 11) as u8
        })
        .collect()
}

fn file_of(blocks: &[Block]) -> Vec<u8> {
    clip_file(&[external(1, &block_data(blocks))], None)
}

/// Tiles of noise in every layout (all pixels distinct, the fourth byte arbitrary: the classes
/// that escape) are modelled, and come back byte for byte.
#[test]
fn noise_tiles_are_modelled() {
    for l in [Layout::Color, Layout::One, Layout::Two] {
        let t = noise(l.bytes(), 7 + l.samples() as u32);
        let z = aqr_clip::zlib::deflate(&t).unwrap();
        let file = file_of(&[Block::Tile(l, z.clone())]);
        let f = check(&file);
        assert!(stream(&f, "clip/skeleton").len() < file.len() - z.len() / 2, "{l:?} is modelled");
    }
    // a colour tile of the fourth byte arbitrary but the pixels in few colours
    let mut t = tile(Layout::Color, 3);
    let r = noise(AREA, 5);
    for i in 0..AREA {
        t[AREA + 4 * i + 3] = r[i];
    }
    check(&file_of(&[Block::Tile(Layout::Color, aqr_clip::zlib::deflate(&t).unwrap())]));
}

/// Streams that zlib at level 1 would not make stay in the skeleton, whole.
#[test]
fn other_streams_are_kept() {
    let t = tile(Layout::Color, 11);
    let good = aqr_clip::zlib::deflate(&t).unwrap();
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    for level in [2u8, 6, 9] {
        let p = Params { level, strategy: Strategy::Default, wbits: 15, mem: 8 };
        let mut z = vec![0x78, 0x9c];
        z.extend(compress(&t, p).unwrap().get(2..).unwrap().to_vec());
        cases.push(("level", z));
    }
    // trailing bytes inside the stream's size
    cases.push(("trailing", [&good[..], &[0, 0, 0][..]].concat()));
    // a wrong Adler-32
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    cases.push(("adler", bad));
    // the stream of another length than the block says
    cases.push(("shorter", aqr_clip::zlib::deflate(&t[..t.len() - 1]).unwrap()));
    cases.push(("longer", aqr_clip::zlib::deflate(&[&t[..], &[1]].concat()).unwrap()));
    cases.push(("one plane's", aqr_clip::zlib::deflate(&t[..Layout::One.bytes()]).unwrap()));
    cases.push(("stored", stored_zlib(&t)));
    cases.push(("cut", good[..good.len() / 2].to_vec()));
    for (name, z) in cases {
        let file = file_of(&[Block::Tile(Layout::Color, z.clone())]);
        let f = check(&file);
        let skeleton = stream(&f, "clip/skeleton");
        assert_eq!(skeleton.len(), file.len(), "{name}: kept whole");
    }
}

/// An SQLite database in the file is aqr-sqlite's formula, embedded.
#[test]
fn real_database_is_embedded() {
    let path = std::env::temp_dir().join(format!("clip-edge-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let sql = "create table t(a integer primary key, b text); \
               with recursive n(i) as (select 1 union all select i+1 from n where i < 300) \
               insert into t select i, 'row ' || i from n;";
    let out = std::process::Command::new("sqlite3")
        .arg(&path)
        .arg(sql)
        .output()
        .expect("the sqlite3 tool, which this test needs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let db = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let file = clip_file(&[external(1, &block_data(&[made(Layout::One, 2)]))], Some(&db));
    let f = check(&file);
    let names = format!("{:?}", f.root);
    assert!(names.contains("sqlite"), "the database's module is applied: {names}");
}

/// A file of nothing but its header, and chunks and tables of extreme numbers.
#[test]
fn header_and_number_edges() {
    let mut h = b"CSFCHUNK".to_vec();
    h.extend([0u8; 8]);
    h.extend(24u64.to_be_bytes());
    if let Some(f) = formula::encode(&h, 1) {
        assert!(f.eval(&modules(1)).unwrap() == h);
    }
    // the first chunk's offset: past the end, before the header, huge
    for first in [25u64, 23, 0, u32::MAX as u64, u32::MAX as u64 + 25, u64::MAX] {
        let mut g = h.clone();
        g[16..24].copy_from_slice(&first.to_be_bytes());
        assert!(formula::encode(&g, 1).is_none(), "{first}");
    }
    // the file: header 24, Head chunk 16 + 40, then the Exta chunk at 80 (size at 88), its body
    // at 96 (ID length, 40 bytes of ID, data size at 144), the data at 152: the first block's
    // record size, its string (42 bytes) and its five fields from 198, the zlib sizes
    let base = file_of(&[made(Layout::One, 1), Block::Empty]);
    assert_eq!(&base[80..84], b"CHNK");
    let table = base.windows(utf16("BlockStatus").len()).position(|w| w == utf16("BlockStatus")).unwrap();
    let table = table + utf16("BlockStatus").len();
    let mut patches: Vec<(usize, Vec<u8>)> = Vec::new();
    for v in [u64::MAX, 1 << 63, u32::MAX as u64 + 1] {
        for p in [8, 24 + 8, 88, 96, 144] {
            patches.push((p, v.to_be_bytes().to_vec()));
        }
    }
    for v in [u32::MAX, u32::MAX - 3, u32::MAX - 4, 0x8000_0000, 0] {
        for p in [152, 198, 202, 206, 210, 214, 218] {
            patches.push((p, v.to_be_bytes().to_vec()));
        }
        patches.push((222, v.to_le_bytes().to_vec()));
        for k in 0..3 {
            patches.push((table + 4 * k, v.to_be_bytes().to_vec()));
        }
    }
    for (p, v) in patches {
        let mut g = base.clone();
        g[p..p + v.len()].copy_from_slice(&v);
        aqr_testkit::case(&format!("patch at {p} to {v:?}"));
        let r = aqr_testkit::within(LIMIT, || formula::encode(&g, 1)).unwrap();
        if let Some(f) = r {
            assert!(f.eval(&modules(1)).unwrap() == g, "patch at {p} to {v:?}");
        }
    }
}

/// Damaged files: a formula, if any, evaluates back to the file.
#[test]
fn damaged_files() {
    let file = clip_file(
        &[external(1, &block_data(&[made(Layout::One, 1), Block::Empty, made(Layout::Two, 3)]))],
        Some(b"not a database"),
    );
    aqr_testkit::damaged(&file, 150, 160, LIMIT, |v| {
        if let Some(f) = formula::encode(v, 1) {
            assert!(f.eval(&modules(1)).unwrap() == v, "evaluated back");
        }
    });
}

/// Damaged streams of a formula: an error, or other bytes, within the time and the heap.
#[test]
fn damaged_streams() {
    let file = clip_file(
        &[external(1, &block_data(&[made(Layout::One, 1), made(Layout::One, 1), made(Layout::Two, 3)]))],
        Some(b"not a database"),
    );
    let f = formula::encode(&file, 1).unwrap();
    for (k, s) in f.streams.iter().enumerate() {
        if s.data.is_empty() {
            continue;
        }
        aqr_testkit::damaged(&s.data, 40, 48, LIMIT, |d| {
            let mut g = f.clone();
            g.streams[k].data = d.to_vec().into();
            let _ = g.eval(&modules(2));
        });
    }
}
