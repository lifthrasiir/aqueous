//! Edge cases of region files' chunks: NBT that is a candidate for holes but none, malformed NBT,
//! chunks inflating past the limit, gzip and custom chunks.

use std::time::Duration;

use aqr_flate::zlib::{Params, Strategy};
use aqr_formula::Node;
use aqr_minecraft::layout;
use aqr_minecraft::nbt::{self, There};
use aqr_minecraft::region::{self, CUSTOM, EXTERNAL, GZIP, HEADER, NONE, SECTOR, ZLIB};
use aqr_minecraft::{formula, module};

mod common;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

const L6: Params = Params { level: 6, strategy: Strategy::Default, wbits: 15, mem: 8 };

/// A region file of chunks (index, type, body), one after another, each in its own sectors.
fn region_of(chunks: &[(u16, u8, Vec<u8>)]) -> Vec<u8> {
    let mut f = vec![0; HEADER];
    for &(index, t, ref body) in chunks {
        let at = f.len();
        f.extend(((body.len() + 1) as u32).to_be_bytes());
        f.push(t);
        f.extend_from_slice(body);
        f.resize(f.len().next_multiple_of(SECTOR), 0);
        let (sector, count) = ((at / SECTOR) as u32, ((f.len() - at) / SECTOR) as u32);
        assert!(count < 256);
        f[4 * index as usize..][..4].copy_from_slice(&(sector << 8 | count).to_be_bytes());
    }
    f
}

fn zlib(d: &[u8]) -> Vec<u8> {
    layout::zlib_onto(d, L6, Vec::new()).unwrap()
}

/// The inputs of the formula's application: header, layout, data, kept, holes; its params, length.
fn inputs(f: &aqr_formula::Formula) -> (Vec<u8>, Vec<Vec<u8>>, u64) {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    let ins = a
        .inputs
        .iter()
        .map(|n| match *n {
            Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
            _ => panic!("an input not a slice"),
        })
        .collect();
    (a.params.clone(), ins, a.len)
}

fn decode_of(f: &aqr_formula::Formula) -> Result<Vec<u8>, aqr_minecraft::Error> {
    let (params, ins, len) = inputs(f);
    let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
    module::decode(&params, &refs, len, 2)
}

/// The formula of `file` (if taken), checked to make the file again.
fn round_trip(what: &str, file: &[u8]) -> Option<aqr_formula::Formula> {
    assert!(region::starts(file), "{what}: not a region file");
    let f = formula::encode(file, 1)?;
    assert!(decode_of(&f).unwrap() == file, "{what}: not made again");
    Some(f)
}

/// The sizes of the module's inputs: header, layout, data, kept, holes.
fn sizes(f: &aqr_formula::Formula) -> Vec<usize> {
    inputs(f).1.iter().map(|i| i.len()).collect()
}

// ---- NBT builders

fn name(d: &mut Vec<u8>, ty: u8, n: &str) {
    d.push(ty);
    d.extend((n.len() as u16).to_be_bytes());
    d.extend_from_slice(n.as_bytes());
}

fn palette_of(d: &mut Vec<u8>, n: usize) {
    name(d, 9, "palette");
    d.push(10);
    d.extend((n as i32).to_be_bytes());
    for i in 0..n {
        name(d, 8, "Name");
        let s = format!("minecraft:b{i}");
        d.extend((s.len() as u16).to_be_bytes());
        d.extend_from_slice(s.as_bytes());
        d.push(0);
    }
}

/// A section of a palette of `n` entries with each of `data` as a long array of its block
/// states (written in this order), and `light` byte arrays (name, length).
fn section(n: usize, data: &[Vec<u64>], light: &[(&str, usize)]) -> Vec<u8> {
    let mut d = Vec::new();
    name(&mut d, 1, "Y");
    d.push(0);
    name(&mut d, 10, "block_states");
    palette_of(&mut d, n);
    for longs in data {
        name(&mut d, 12, "data");
        d.extend((longs.len() as i32).to_be_bytes());
        for l in longs {
            d.extend(l.to_be_bytes());
        }
    }
    d.push(0);
    for &(kind, len) in light {
        name(&mut d, 7, kind);
        d.extend((len as i32).to_be_bytes());
        d.extend((0..len).map(|i| (i * 7) as u8));
    }
    d.push(0);
    d
}

fn chunk_of(sections: &[Vec<u8>]) -> Vec<u8> {
    let mut d = Vec::new();
    name(&mut d, 10, "");
    name(&mut d, 9, "sections");
    d.push(10);
    d.extend((sections.len() as i32).to_be_bytes());
    for s in sections {
        d.extend_from_slice(s);
    }
    d.push(0);
    d
}

/// The longs of 4096 indices of a palette of 5 (4 bits: 16 to a long, 256 longs), as `f` gives them.
fn longs(f: impl Fn(usize) -> u64) -> Vec<u64> {
    (0..256).map(|l| (0..16).fold(0, |w, j| w | f(l * 16 + j) << (4 * j))).collect()
}

fn valid_longs() -> Vec<u64> {
    longs(|i| (i / 3 % 5) as u64)
}

const LIGHT: [(&str, usize); 2] = [("BlockLight", 2048), ("SkyLight", 2048)];

/// Sections that are candidates for holes and none (each tells why).
fn not_holes() -> Vec<(&'static str, Vec<u8>)> {
    let mut v = vec![("index past the palette", section(5, &[longs(|i| (i % 7) as u64)], &[]))];
    v.push(("a long short", section(5, &[valid_longs()[1..].to_vec()], &[])));
    v.push(("a long over", section(5, &[[valid_longs(), vec![0]].concat()], &[])));
    v.push(("data twice", section(5, &[valid_longs(), valid_longs()], &[])));
    v.push(("palette of one with data", section(1, &[valid_longs()], &[])));
    v.push(("short light", section(5, &[], &[("BlockLight", 100), ("SkyLight", 2049)])));
    let mut twice = LIGHT.to_vec();
    twice.push(("SkyLight", 2048));
    v.push(("light twice", section(5, &[], &twice)));
    v.push(("no data", section(5, &[], &[])));
    v
}

#[test]
fn nbt_candidates_that_are_no_holes() {
    for (what, s) in not_holes() {
        let file = region_of(&[(0, ZLIB, zlib(&chunk_of(std::slice::from_ref(&s)))), (1, ZLIB, zlib(&chunk_of(&[s])))]);
        // every chunk's data kept whole: taken as plain streams, no holes
        let f = round_trip(what, &file).unwrap_or_else(|| panic!("{what}: not taken"));
        let light_twice = what == "light twice" || what == "short light";
        assert!(light_twice || sizes(&f)[4] == 0, "{what}: holes");
    }
}

#[test]
fn a_hole_beside_candidates_that_are_none() {
    // a section of a hole, and each of the others, in one chunk
    let good = section(5, &[valid_longs()], &LIGHT);
    for (what, s) in not_holes() {
        let file = region_of(&[(3, ZLIB, zlib(&chunk_of(&[good.clone(), s.clone(), good.clone()])))]);
        let f = round_trip(what, &file).unwrap_or_else(|| panic!("{what}: not taken"));
        assert!(sizes(&f)[4] > 0, "{what}: no holes");
    }
    // a palette of 33 entries: 6 bits, 10 to a long, 410 longs; bits past the indices set
    let mut l = vec![0u64; 410];
    l[409] = 1 << 63;
    let s = section(33, &[l], &[]);
    let f = round_trip("padding bits", &region_of(&[(0, ZLIB, zlib(&chunk_of(&[s])))])).unwrap();
    assert_eq!(sizes(&f)[4], 0);
    // 4096 entries (12 bits: 5 to a long, 820 longs), and 4097 (13 bits: 4 to a long, 1024 longs)
    for (n, longs) in [(4096, 820), (4097, 1024)] {
        let s = section(n, &[vec![0; longs]], &[]);
        round_trip("large palette", &region_of(&[(0, ZLIB, zlib(&chunk_of(&[s])))])).unwrap();
    }
}

/// Malformed NBT, as the data of chunks: made again, with no holes.
#[test]
fn malformed_nbt() {
    let base = common::chunk(5, 3);
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    // nested too deep: compounds, lists
    for n in [510, 511, 512, 513, 5000] {
        let mut d = Vec::new();
        name(&mut d, 10, "");
        for _ in 0..n {
            name(&mut d, 10, "x");
        }
        d.extend(std::iter::repeat_n(0, n + 1));
        cases.push((format!("{n} compounds"), d));
        let mut d = Vec::new();
        name(&mut d, 10, "");
        name(&mut d, 9, "x");
        for _ in 0..n {
            d.push(9);
            d.extend(1i32.to_be_bytes());
        }
        cases.push((format!("{n} lists"), d));
    }
    // counts that no data holds
    for count in [i32::MAX, i32::MAX / 8 + 1, -1, i32::MIN, 1 << 28] {
        for ty in [1u8, 3, 7, 8, 9, 10, 11, 12] {
            let mut d = Vec::new();
            name(&mut d, 10, "");
            name(&mut d, 9, "x");
            d.push(ty);
            d.extend(count.to_be_bytes());
            d.extend_from_slice(&[0; 16]);
            d.push(0);
            cases.push((format!("list of {count} of type {ty}"), d));
        }
        for ty in [7u8, 11, 12] {
            let mut d = Vec::new();
            name(&mut d, 10, "");
            name(&mut d, ty, "data");
            d.extend(count.to_be_bytes());
            d.push(0);
            cases.push((format!("array of {count} of type {ty}"), d));
        }
        // a long array named data in a block_states
        let mut d = Vec::new();
        name(&mut d, 10, "");
        name(&mut d, 9, "sections");
        d.push(10);
        d.extend(1i32.to_be_bytes());
        name(&mut d, 10, "block_states");
        palette_of(&mut d, 5);
        name(&mut d, 12, "data");
        d.extend(count.to_be_bytes());
        d.extend_from_slice(&[0; 64]);
        cases.push((format!("data of {count}"), d));
    }
    // unknown tag types, a root that is no compound, trailing bytes
    for ty in [13u8, 99, 255] {
        let mut d = Vec::new();
        name(&mut d, 10, "");
        name(&mut d, ty, "x");
        d.extend_from_slice(&[0; 8]);
        d.push(0);
        cases.push((format!("tag type {ty}"), d));
    }
    cases.push(("list element type 13".into(), vec![10, 0, 0, 9, 0, 1, b'x', 13, 0, 0, 0, 1, 0, 0, 0, 0, 0]));
    cases.push(("list of ends".into(), vec![10, 0, 0, 9, 0, 1, b'x', 0, 0, 0, 0, 5, 0]));
    cases.push(("root a list".into(), vec![9, 0, 0, 1, 0, 0, 0, 0]));
    cases.push(("root a long".into(), vec![4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    let mut t = base.clone();
    t.push(0);
    cases.push(("trailing byte".into(), t));
    let mut t = base.clone();
    t.extend_from_slice(&base);
    cases.push(("two chunks".into(), t));
    cases.push(("empty".into(), vec![]));
    cases.push(("one byte".into(), vec![10]));
    // prefixes of the chunk's NBT (a cut tag)
    for n in (0..base.len()).step_by(base.len() / 300 + 1).chain(base.len() - 10..base.len()) {
        cases.push((format!("cut at {n}"), base[..n].to_vec()));
    }
    for (what, d) in cases {
        aqr_testkit::case(&what);
        // the parser alone
        let mut z = d.clone();
        let (mut secs, mut frames) = (Vec::new(), nbt::frames());
        let _ = nbt::parse(&mut z, &mut There, &mut secs, &mut frames, &mut |_| Ok(false));
        let _ = nbt::skip(&d, 0, nbt::COMPOUND, 64);
        // as a chunk's data
        let file = region_of(&[(0, ZLIB, zlib(&d)), (1, NONE, d.clone())]);
        let Some(f) = formula::encode(&file, 1) else { panic!("{what}: not taken") };
        assert!(decode_of(&f).unwrap() == file, "{what}: not made again");
        let full = what.starts_with("cut at") && d.len() == base.len();
        assert!(full || sizes(&f)[4] == 0, "{what}: holes of malformed NBT");
    }
}

/// The NBT of a chunk damaged in every way, as the data of chunks: made again whatever it
/// parses as, in time, with no large allocation.
#[test]
fn damaged_nbt_made_again() {
    let base = common::chunk(9, 2);
    aqr_testkit::damaged(&base, 150, 120, Duration::from_secs(20), |d| {
        let file = region_of(&[(0, ZLIB, zlib(d)), (1, NONE, d.to_vec())]);
        let f = formula::encode(&file, 1).expect("not taken");
        assert!(decode_of(&f).unwrap() == file, "not made again");
        assert!(aqr_testkit::most() < 64 << 20, "an allocation of {}", aqr_testkit::most());
    });
}

/// The streams of a file with holes damaged in every way: an error or another file, never a
/// panic, a hang or an allocation that the data cannot hold.
#[test]
fn damaged_holes_and_skeleton() {
    let nbt = common::chunk(10, 2);
    let file = region_of(&[(0, ZLIB, zlib(&nbt)), (1, NONE, nbt)]);
    let f = formula::encode(&file, 1).unwrap();
    let (params, ins, len) = inputs(&f);
    assert!(!ins[4].is_empty());
    for k in [1, 2, 4] {
        aqr_testkit::damaged(&ins[k], 120, 60, Duration::from_secs(20), |d| {
            let mut ins = ins.clone();
            ins[k] = d.to_vec();
            let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
            let _ = module::decode(&params, &refs, len, 1);
            assert!(aqr_testkit::most() < 256 << 20, "input {k}: an allocation of {}", aqr_testkit::most());
        });
    }
}

/// A chunk inflating past the limit of its file is kept as it is; the others are taken.
#[test]
fn inflating_past_the_limit() {
    let small = common::chunk(3, 2);
    let big = vec![0u8; 1 << 20];
    let file = region_of(&[(0, ZLIB, zlib(&small)), (1, ZLIB, zlib(&big)), (2, ZLIB, zlib(&small))]);
    assert!(file.len() < 100_000);
    let _g = formula::LIMIT_BASE.set_here(0);
    // 64 times the file is over 1 MiB: taken
    let f = round_trip("within", &file).unwrap();
    assert!(sizes(&f)[2] > 1 << 20);
    // at a ratio under the big chunk's it is kept (whole, as it is) and the others taken
    let _h = formula::LIMIT_RATIO.set_here(8);
    let f = round_trip("past", &file).unwrap();
    let d = sizes(&f)[2];
    assert!(d > 0 && d < 1 << 19, "data {d}");
    // every chunk past it: nothing to make
    let file = region_of(&[(0, ZLIB, zlib(&big))]);
    assert!(formula::encode(&file, 1).is_none());
}

fn gzip(d: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

#[test]
fn gzip_and_custom_chunks() {
    let nbt = common::chunk(4, 2);
    let mut custom = Vec::new();
    let n = "mod:fancy";
    custom.extend((n.len() as u16).to_be_bytes());
    custom.extend_from_slice(n.as_bytes());
    custom.extend_from_slice(&[1, 2, 3, 4, 5]);
    let file = region_of(&[
        (0, GZIP, gzip(&nbt)),
        (1, CUSTOM, custom.clone()),
        (2, ZLIB, zlib(&nbt)),
        (3, GZIP | EXTERNAL, vec![]),
        (4, CUSTOM | EXTERNAL, vec![]),
    ]);
    let f = round_trip("gzip, custom, zlib", &file).unwrap();
    // (the zlib chunk's data is the only one made again)
    assert!(sizes(&f)[2] < nbt.len(), "data {} of {}", sizes(&f)[2], nbt.len());
    // none of them zlib: nothing to make again
    let file = region_of(&[(0, GZIP, gzip(&nbt)), (1, CUSTOM, custom.clone()), (2, GZIP | EXTERNAL, vec![])]);
    assert!(region::starts(&file));
    assert!(formula::encode(&file, 1).is_none());
    // an unknown type is no region file
    let file = region_of(&[(0, ZLIB, zlib(&nbt)), (1, 5, vec![1, 2, 3])]);
    assert!(!region::starts(&file));
    assert!(formula::encode(&file, 1).is_none() || region::parse(&file).is_some());
    // bodies of a custom chunk, short, cut or large
    for body in [vec![0], vec![0, 200, 1], vec![0xff; 70_000]] {
        let file = region_of(&[(0, ZLIB, zlib(&nbt)), (1, CUSTOM, body)]);
        round_trip("custom body", &file).unwrap();
    }
}
