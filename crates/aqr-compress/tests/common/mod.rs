//! What the tests of `-Z lzma` and `-Z together:always` share: files of one batch, archived and given back.

use std::io::Write;

use aqr_compress::{Input, Options, Report, compress_tree};
use aqr_decompress::decompress_value;
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree, Value};

fn text(n: usize, seed: usize) -> Vec<u8> {
    let mut t = Vec::new();
    while t.len() < n {
        let i = t.len();
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", (i + seed) * 7 % 13).unwrap();
    }
    t
}

/// Bytes of no pattern, which compress only as copies of each other.
fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (x >> 24) as u8
        })
        .collect()
}

fn zlib(d: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(d).unwrap();
    z.finish().unwrap()
}

/// Files of one batch: the same contents twice, files holding a piece of another (sliced from
/// both), compressed data (stored as it is, not taken), and a zlib stream (recompressed, its
/// record a stream of its own); archived (with the process's `-Z` as the test set it), checked to
/// be given back, the archive no larger than half of them; with the report.
pub fn files_given_back() -> Report {
    let shared = noise(300_000, 1);
    let mut holder = text(50_000, 3);
    holder.extend_from_slice(&shared);
    holder.extend_from_slice(&text(20_000, 4));
    let mut other = noise(10_000, 2);
    other.extend_from_slice(&shared);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("t0.txt", text(200_000, 0)),
        ("t1.txt", text(150_000, 1)),
        ("same.txt", text(200_000, 0)),
        ("holder.bin", holder),
        ("other.bin", other),
        ("shared.bin", shared),
        ("small", b"a few bytes".to_vec()),
        ("empty", Vec::new()),
        ("z.zlib", zlib(&text(100_000, 5))),
        ("n.bin", noise(70_000, 9)),
    ];
    let mut tree = Tree { root: Extra { mode: Some(0o755), ..Extra::default() }, entries: Vec::new() };
    for (k, (name, _)) in files.iter().enumerate() {
        let e =
            Entry { parent: None, name: name.as_bytes().to_vec(), node: EntryNode::File(k), extra: Extra::default() };
        tree.entries.push(e);
    }
    let inputs: Vec<Input> = files.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let a = compress_tree(&tree, &inputs, &Options::default());
    assert!(a.report.unverified.is_empty());
    let Ok(Value::Tree(t, got)) = decompress_value(&a.archive, 1) else { panic!("not decoded") };
    for (name, data) in &files {
        let e = t.entries.iter().find(|e| e.name == name.as_bytes()).unwrap();
        let EntryNode::File(c) = e.node else { panic!("{name}: not a file") };
        assert!(got[c] == *data, "{name} not given back");
    }
    let total: usize = files.iter().map(|f| f.1.len()).sum();
    assert!(a.archive.len() < total / 2, "{} of {total}", a.archive.len());
    a.report
}
