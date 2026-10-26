//! Archives of several files: every file given back, those alike solid or in sets within the
//! bounds, the same contents once.

use std::io::Write;

use aqr_compress::{Input, Options, compress_tree};
use aqr_decompress::decompress_value;
use aqr_formula::aqr::{Entry, EntryNode, Extra, Time, Tree, Value};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn text(n: usize, seed: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", (i + seed) * 7 % 13).unwrap();
    }
    t
}

/// A gray PNG file of a gradient of `w` x `h`, with the pixels of `rect` (x, y, width,
/// height) changed.
fn variant(w: usize, h: usize, rect: Option<(usize, usize, usize, usize)>) -> Vec<u8> {
    let mut data = Vec::new();
    for y in 0..h {
        data.push(0);
        for x in 0..w {
            let inside = rect.is_some_and(|(rx, ry, rw, rh)| (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y));
            data.push(if inside { ((x * 7) ^ (y * 3)) as u8 } else { (x + 2 * y) as u8 / 3 });
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&data).unwrap();
    let mut ihdr = (w as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, d) in [(b"IHDR", ihdr), (b"IDAT", z.finish().unwrap()), (b"IEND", vec![])] {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(&d);
        out.extend_from_slice(&crc32fast::hash(&out[start..]).to_be_bytes());
    }
    out
}

/// At level 1, which leaves the archive enough to pay for its record.
fn gzip(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().filename("a").write(Vec::new(), flate2::Compression::new(1));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

/// A tree of `files` (path, data) under directories as their paths tell, with a link; and the
/// contents, by file.
fn tree_of(files: &[(&str, Vec<u8>)]) -> (Tree, Vec<(String, Vec<u8>)>) {
    let mut t = Tree { root: Extra { mtime: None, mode: Some(0o755) }, entries: Vec::new() };
    let mut dirs: Vec<(String, usize)> = Vec::new();
    let mut contents = Vec::new();
    for (path, data) in files {
        let mut parent = None;
        let parts: Vec<&str> = path.split('/').collect();
        for i in 0..parts.len() - 1 {
            let dir = parts[..=i].join("/");
            parent = Some(match dirs.iter().find(|d| d.0 == dir) {
                Some(d) => d.1,
                None => {
                    let e = Entry { parent, name: parts[i].into(), node: EntryNode::Dir, extra: Extra::default() };
                    t.entries.push(e);
                    dirs.push((dir, t.entries.len() - 1));
                    t.entries.len() - 1
                }
            });
        }
        let extra =
            Extra { mtime: Some(Time { secs: 1_700_000_000 + contents.len() as i64, nanos: 1 }), mode: Some(0o644) };
        let name = parts[parts.len() - 1].into();
        t.entries.push(Entry { parent, name, node: EntryNode::File(contents.len()), extra });
        contents.push((path.to_string(), data.clone()));
    }
    t.entries.push(Entry {
        parent: None,
        name: b"link".to_vec(),
        node: EntryNode::Link(b"a/t0.txt".to_vec()),
        extra: Extra::default(),
    });
    (t, contents)
}

/// The archive of `files`, checked to give them back, with its report.
fn archived(files: &[(&str, Vec<u8>)], opt: &Options) -> (Vec<u8>, aqr_compress::Report) {
    let (tree, contents) = tree_of(files);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let a = compress_tree(&tree, &inputs, opt);
    let Ok(Value::Tree(t, got)) = decompress_value(&a.archive, 1) else { panic!("not decoded") };
    let files_of = |t: &Tree| -> Vec<(Vec<u8>, Extra, usize)> {
        t.entries
            .iter()
            .filter_map(|e| if let EntryNode::File(c) = e.node { Some((e.name.clone(), e.extra, c)) } else { None })
            .collect()
    };
    let (want, have) = (files_of(&tree), files_of(&t));
    assert_eq!(want.len(), have.len());
    for (w, h) in want.iter().zip(&have) {
        assert_eq!((&w.0, w.1), (&h.0, h.1));
        assert!(contents[w.2].1 == got[h.2], "{} not given back", contents[w.2].0);
    }
    assert_eq!(t.root, tree.root);
    assert!(t.entries.iter().any(|e| e.node == EntryNode::Link(b"a/t0.txt".to_vec())));
    assert!(a.report.unverified.is_empty());
    (a.archive, a.report)
}

fn sample() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a/t0.txt", text(20_000, 0)),
        ("a/t1.txt", text(30_000, 1)),
        ("a/b/t2.txt", text(25_000, 2)),
        ("a/b/same.txt", text(30_000, 1)),
        ("empty", Vec::new()),
        ("p/0.png", variant(200, 150, None)),
        ("p/1.png", variant(200, 150, Some((20, 30, 40, 50)))),
        ("p/2.png", variant(200, 150, Some((90, 10, 30, 90)))),
        (
            "z.gz",
            gzip(
                &(0..30000)
                    .flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes())
                    .collect::<Vec<_>>(),
            ),
        ),
        ("g.jpg", fixture("aqr-jpeg/tests/data/gradient.jpg")),
    ]
}

/// The sets, each its members in order.
fn sets(r: &aqr_compress::Report) -> Vec<Vec<usize>> {
    r.sets
        .iter()
        .map(|s| {
            let mut s = s.clone();
            s.sort_unstable();
            s
        })
        .collect()
}

#[test]
fn files_given_back() {
    let files = sample();
    let (c, r) = archived(&files, &Options::default());
    let total: usize = files.iter().map(|f| f.1.len()).sum();
    assert!(c.len() < total / 3, "{} of {total}", c.len());
    // the text files solid, the same content once, the PNG files a set, the others recompressed
    // (the first of the same contents in the order of the files: same.txt)
    assert!(r.solid.iter().any(|s| [0, 2, 3].iter().all(|c| s.contents.contains(c))), "{:?}", r.solid);
    assert_eq!(r.files[1].same_as, Some(3));
    assert_eq!(sets(&r), [vec![5, 6, 7]]);
    let names = |c: usize| r.files[c].modules.iter().map(|m| m.name).collect::<Vec<_>>();
    assert_eq!(names(8), ["flate-recons"]);
    assert_eq!(names(9), ["jpeg-recons"]);
    let attributed: f64 = r.files.iter().map(|f| f.archived).sum();
    assert!(
        (attributed + r.overhead as f64 - c.len() as f64).abs() < 1.0,
        "{attributed} + {} of {}: {r:?}",
        r.overhead,
        c.len()
    );
}

/// Sets and solid blocks within their bounds; a file alone in its set is coded alone.
#[test]
fn bounded() {
    let files = sample();
    // (in the order of the tree, 6 first: the one most like the others)
    let opt = Options { set_bytes: files[5].1.len() + files[6].1.len(), block: 40_000, ..Options::default() };
    let (_, r) = archived(&files, &opt);
    assert_eq!(sets(&r), [vec![5, 6]]);
    assert!(r.solid.iter().all(|s| s.len <= 40_000));
    assert!(!r.files[7].modules.is_empty());
    // nothing recompressed: all raw, solid still
    let (_, r) = archived(&files, &Options { depth: 0, ..Options::default() });
    assert!(r.files.iter().all(|f| f.modules.is_empty()) && !r.solid.is_empty());
}

/// Files in batches of 30 KB: each batch apart, the same content once across them.
#[test]
fn batches() {
    let files = sample();
    let (one, _) = archived(&files, &Options::default());
    let (c, r) = archived(&files, &Options { batch: 30_000, ..Options::default() });
    assert_eq!(r.files[1].same_as, Some(3));
    // (the text files no longer solid all together)
    assert!(c.len() > one.len() && r.solid.iter().all(|s| s.len <= 2 * 30_000), "{} {}", c.len(), one.len());
    let attributed: f64 = r.files.iter().map(|f| f.archived).sum();
    assert!((attributed + r.overhead as f64 - c.len() as f64).abs() < 1.0);
}

/// A file, and the same in a gzip file and in a ZIP archive: their streams rebuilt and taken
/// from the file, so that its data is stored once, however far apart (in streams longer than
/// a block, which are not solid, and in batches smaller than each).
#[test]
fn held_once() {
    let mut x = 1u64;
    let words = ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dogs", "and", "cats"];
    let mut data = Vec::new();
    while data.len() < 3 << 20 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        write!(data, "{} {} ", words[(x >> 60) as usize % 10], (x >> 40) % 1000).unwrap();
    }
    let mut e = flate2::GzBuilder::new().filename("words").write(Vec::new(), flate2::Compression::new(9));
    e.write_all(&data).unwrap();
    let gz = e.finish().unwrap();
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    e.write_all(&data).unwrap();
    let zip = zip_of("words", &data, &e.finish().unwrap());
    let opt = Options { block: 1 << 20, ..Options::default() };
    let (alone, _) = archived(&[("w/words", data.clone())], &opt);
    let files = [("w/words", data), ("w/words.gz", gz), ("w/words.zip", zip)];
    let (c, r) = archived(&files, &opt);
    assert!(!r.files[1].modules.is_empty() && !r.files[2].modules.is_empty(), "{r:?}");
    // (their records not small: rules fit miniz's streams less well than zlib's)
    assert!(c.len() < alone.len() * 3 / 2, "{} of {} alone", c.len(), alone.len());
    // in batches of less than each, kept together
    let (c, _) = archived(&files, &Options { batch: 1 << 20, ..opt });
    assert!(c.len() < alone.len() * 3 / 2, "{} of {} alone, in batches", c.len(), alone.len());
}

/// A ZIP archive of a single entry `name` of `data`, its body `deflated`.
fn zip_of(name: &str, data: &[u8], deflated: &[u8]) -> Vec<u8> {
    let mut common = Vec::new();
    for v in [20u16, 0, 8, 0, 0] {
        common.extend_from_slice(&v.to_le_bytes());
    }
    for v in [crc32fast::hash(data), deflated.len() as u32, data.len() as u32] {
        common.extend_from_slice(&v.to_le_bytes());
    }
    common.extend_from_slice(&(name.len() as u16).to_le_bytes());
    common.extend_from_slice(&0u16.to_le_bytes());
    let mut o = [&b"PK\x03\x04"[..], &common, name.as_bytes(), deflated].concat();
    let at = o.len() as u32;
    let cd = [&b"PK\x01\x02\x14\x03"[..], &common, &[0; 14], name.as_bytes()].concat();
    o.extend_from_slice(&cd);
    o.extend_from_slice(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
    o.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    o.extend_from_slice(&at.to_le_bytes());
    o.extend_from_slice(&[0, 0]);
    o
}
