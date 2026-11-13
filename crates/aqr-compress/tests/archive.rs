//! Archives of several files: every file given back, those alike solid or in sets within the
//! bounds, the same contents once.

use std::io::Write;

use aqr_compress::{Input, Options, compress_tree};
use aqr_decompress::decompress_value;
use aqr_formula::aqr::{Entry, EntryNode, Extra, Time, Tree, Value};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
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
        out.extend_from_slice(&aqr_common::crc32::hash(&out[start..]).to_be_bytes());
    }
    out
}

/// At level 1, which leaves the archive enough to pay for its record.
fn gzip(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().filename("a").write(Vec::new(), flate2::Compression::new(1));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

/// A tree of `files` (path, data) under directories as their paths tell, with a link, the first
/// file with a fork and an extended attribute; and the contents, by file (the fork's last).
fn tree_of(files: &[(&str, Vec<u8>)]) -> (Tree, Vec<(String, Vec<u8>)>) {
    let mut t = Tree { root: Extra { mode: Some(0o755), ..Extra::default() }, entries: Vec::new() };
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
        let mtime = Some(Time::new(1_700_000_000 + contents.len() as i64, 1));
        let extra = Extra { mtime, mode: Some(0o644), ..Extra::default() };
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
    if let Some(e) = t.entries.iter_mut().find(|e| matches!(e.node, EntryNode::File(_))) {
        e.extra.forks.push((b"AFP_Resource".to_vec(), contents.len()));
        e.extra.xattrs.push((b"user.x"[..].into(), b"y"[..].into()));
        contents.push(("fork".into(), b"a fork of a file".repeat(10)));
    }
    (t, contents)
}

/// The archive of `files`, checked to give them back, with its report.
fn archived(files: &[(&str, Vec<u8>)], opt: &Options) -> (Vec<u8>, aqr_compress::Report) {
    let (tree, contents) = tree_of(files);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let a = compress_tree(&tree, &inputs, opt);
    let Ok(Value::Tree(t, got)) = decompress_value(&a.archive, 1) else { panic!("not decoded") };
    // (the forks apart, by their contents)
    type Files = Vec<(Vec<u8>, Extra, usize, Vec<(Vec<u8>, usize)>)>;
    let files_of = |t: &Tree| -> Files {
        t.entries
            .iter()
            .filter_map(|e| match e.node {
                EntryNode::File(c) => {
                    let forks = e.extra.forks.clone();
                    Some((e.name.clone(), Extra { forks: Vec::new(), ..e.extra.clone() }, c, forks))
                }
                _ => None,
            })
            .collect()
    };
    // (by name: the entries of several segments come in their segments' order)
    let (mut want, mut have) = (files_of(&tree), files_of(&t));
    want.sort_by(|a, b| a.0.cmp(&b.0));
    have.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(want.len(), have.len());
    for (w, h) in want.iter().zip(&have) {
        assert_eq!((&w.0, &w.1), (&h.0, &h.1));
        assert!(contents[w.2].1 == got[h.2], "{} not given back", contents[w.2].0);
        assert_eq!(w.3.len(), h.3.len());
        for (wf, hf) in w.3.iter().zip(&h.3) {
            assert!(wf.0 == hf.0 && contents[wf.1].1 == got[hf.1], "fork {} not given back", contents[wf.1].0);
        }
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
        ("g.jpg", fixture("codecs/aqr-jpeg/tests/data/gradient.jpg")),
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
    // (the archive's own bytes apart, the modules of the formulas among them)
    assert!(c.len() - r.overhead < total / 3, "{} of {total}", c.len() - r.overhead);
    // the text files solid, the same content once, the PNG files a set, the others recompressed
    // (the first of the same contents in the order of the files: same.txt)
    assert!(r.solid.iter().any(|s| [0, 2, 3].iter().all(|c| s.contents.contains(c))), "{:?}", r.solid);
    assert_eq!(r.files[1].same_as, Some(3));
    assert_eq!(sets(&r), [vec![5, 6, 7]]);
    let names = |c: usize| r.files[c].modules.iter().map(|m| m.name).collect::<Vec<_>>();
    // (its text coded by aqr-wrt)
    assert_eq!(names(8), ["flate-set", "wrt-recons"]);
    assert_eq!(names(9), ["jpeg-recons"]);
    let attributed: f64 = r.files.iter().map(|f| f.archived).sum();
    assert!(
        (attributed + r.overhead as f64 - c.len() as f64).abs() < 1.0,
        "{attributed} + {} of {}: {r:?}",
        r.overhead,
        c.len()
    );
}

/// The modules named by their hashes alone (shorthand module blocks, as a self-extracting
/// archive has them, `aqr --format sfx`): every file verified (the hashes resolved) and given
/// back as by default, by the same modules, and the archive has no module block in full.
#[test]
fn registered_modules() {
    use aqr_formula::aqr::{self, Wasm};
    fn registered(m: aqr_formula::Module) -> Option<Wasm> {
        let k = aqr_decompress::built(m)?;
        let classes = aqr::task_classes(&aqr::module_of_block_data(k.block)?);
        Some(Wasm::Registered { hash: k.sha512_256, classes })
    }
    let files = sample();
    let (c, r) = archived(&files, &Options { modules: registered, ..Options::default() });
    let (d, by_default) = archived(&files, &Options::default());
    let modules = |r: &aqr_compress::Report| r.files.iter().map(|f| f.modules.clone()).collect::<Vec<_>>();
    assert_eq!(modules(&r), modules(&by_default));
    let types = |c: &[u8]| {
        let i = aqr::inspect::inspect(c, false, &|_| None, None).unwrap();
        i.blocks.iter().map(|b| b.ty).filter(|t| (0x60..=0x63).contains(t)).collect::<Vec<_>>()
    };
    let (shorthand, full) = (types(&c), types(&d));
    assert!(!shorthand.is_empty() && shorthand.iter().all(|&t| t == 0x60), "{shorthand:x?}");
    assert_eq!(shorthand.len(), full.len());
    assert!(c.len() < d.len());
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

/// Files in batches of 30 KB: each batch apart, the same content once across them; the same
/// archive however many threads make them (batches made at once).
#[test]
fn batches() {
    let files = sample();
    let (one, _) = archived(&files, &Options::default());
    let (c, r) = archived(&files, &Options { batch: 30_000, ..Options::default() });
    assert_eq!(r.files[1].same_as, Some(3));
    let (threaded, _) = archived(&files, &Options { batch: 30_000, threads: 4, ..Options::default() });
    assert!(threaded == c, "{} bytes on 4 threads, {} on one", threaded.len(), c.len());
    // (the text files no longer solid all together)
    assert!(c.len() > one.len() && r.solid.iter().all(|s| s.len <= 2 * 30_000), "{} {}", c.len(), one.len());
    let attributed: f64 = r.files.iter().map(|f| f.archived).sum();
    assert!((attributed + r.overhead as f64 - c.len() as f64).abs() < 1.0);
}

/// The metadata blocks of archive `c`, as its blocks are read.
fn metadata_blocks(c: &[u8]) -> usize {
    let check = aqr_formula::aqr::mirror::check(c);
    assert!(check.walked);
    check.targets.iter().filter(|t| (0x10..=0x17).contains(&t.1)).count()
}

/// Files in segments of a batch each (batches of 30 KB): every file given back (a file and its
/// fork in one segment), a file of the same content as one of a segment before made again; with
/// mirror blocks, those of each segment before the next one's metadata block, as far from their
/// blocks as the spec asks at least.
#[test]
fn segments() {
    let files = sample();
    let (one, _) = archived(&files, &Options { batch: 30_000, ..Options::default() });
    assert_eq!(metadata_blocks(&one), 1);
    let opt = Options { batch: 30_000, segment: 1, ..Options::default() };
    let (c, r) = archived(&files, &opt);
    assert!(metadata_blocks(&c) > 3, "{}", metadata_blocks(&c));
    // (same.txt, then t1.txt of its content in a later batch: made again)
    assert_eq!(r.files[1].same_as, None);
    assert!(c.len() > one.len());
    let attributed: f64 = r.files.iter().map(|f| f.archived).sum();
    assert!((attributed + r.overhead as f64 - c.len() as f64).abs() < 1.0);
    let (threaded, _) = archived(&files, &Options { threads: 4, ..opt.clone() });
    assert!(threaded == c);
    let (m, r) = archived(&files, &Options { mirrors: true, ..opt });
    assert_eq!(r.len, m.len());
    let check = aqr_formula::aqr::mirror::check(&m);
    assert!(check.walked && check.mirrored(&m, false), "{check:?}");
    assert_eq!(metadata_blocks(&m), metadata_blocks(&c));
}

/// An archive written as a log of what was done: bytes appended, synced, written over.
#[derive(Default)]
struct Logged {
    bytes: Vec<u8>,
    ops: Vec<Op>,
    /// Committed once, as it ends ([`aqr_compress::Output::commits_once`]).
    once: bool,
}

enum Op {
    Append(usize),
    Sync,
    Patch(usize, Vec<u8>),
}

impl Write for Logged {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        self.ops.push(Op::Append(buf.len()));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl aqr_compress::Output for Logged {
    fn commits(&self) -> bool {
        true
    }

    fn commits_once(&self) -> bool {
        self.once
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.ops.push(Op::Sync);
        Ok(())
    }

    fn patch(&mut self, at: u64, bytes: &[u8]) -> std::io::Result<()> {
        let at = at as usize;
        self.ops.push(Op::Patch(at, self.bytes[at..at + bytes.len()].to_vec()));
        self.bytes[at..at + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

/// The files of the archive in `file` (located), by name, and what its last segment tells.
fn located(file: &[u8]) -> (Vec<Vec<u8>>, Option<aqr_formula::aqr::Status>) {
    let at = aqr_formula::aqr::locate(file).expect("an archive located");
    let a = &file[at];
    let Ok(Value::Tree(t, _)) = decompress_value(a, 1) else { panic!("not decoded") };
    let status = aqr_formula::aqr::open(a, &aqr_formula::aqr::NoModules).unwrap().status();
    let mut names: Vec<Vec<u8>> =
        t.entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).map(|e| e.name.clone()).collect();
    names.sort();
    (names, status)
}

/// An archive committed as it is written, a segment each batch: what it writes, wherever it is
/// stopped (a write cut short too), is the archive of a commit, files only added as it goes;
/// its last commit tells of the writing complete, the others of what is left.
#[test]
fn committed() {
    use aqr_formula::aqr::Status;
    let files = sample();
    let (tree, contents) = tree_of(&files);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let opt = Options { batch: 30_000, segment: 1, ..Options::default() };
    let mut out = Logged::default();
    let r = aqr_compress::compress_tree_into(&tree, &inputs[..], &opt, &mut out, None, None, &|_| {}).unwrap();
    assert_eq!(r.len, out.bytes.len());
    let (all, status) = located(&out.bytes);
    assert_eq!(status, Some(Status::Complete));
    let (plain, _) = archived(&files, &opt);
    assert_eq!(located(&plain).0, all);
    // the states the file goes through, undoing the operations from the last
    let mut file = out.bytes.clone();
    let mut seen = all.clone();
    let (mut syncs, mut states, mut none) = (0, 0, false);
    for op in out.ops.iter().rev() {
        match op {
            Op::Append(n) => {
                for cut in [*n / 2, *n] {
                    let state = &file[..file.len() - cut];
                    if aqr_formula::aqr::locate(state).is_none() {
                        // (nothing committed yet, nor before)
                        none = true;
                        continue;
                    }
                    assert!(!none, "an archive before its first commit");
                    let (names, status) = located(state);
                    assert!(names.iter().all(|n| seen.contains(n)), "files appear as the file shrinks");
                    assert_ne!(status, Some(Status::None));
                    seen = names;
                    states += 1;
                }
                file.truncate(file.len() - n);
            }
            Op::Sync => syncs += 1,
            Op::Patch(at, was) => file[*at..*at + was.len()].copy_from_slice(was),
        }
    }
    assert!(none && syncs > 6 && states > 10, "{syncs} {states}");
}

/// Files of the same content far apart by their names (of other extensions, others between
/// them), in segments of a batch each: put in one batch, so stored once rather than again in a
/// later segment.
#[test]
fn segments_same_contents_together() {
    let same = text(100_000, 9);
    let mut files: Vec<(&str, Vec<u8>)> = vec![("a.dat", same.clone())];
    let names = ["b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt"];
    files.extend(names.iter().enumerate().map(|(i, n)| (*n, text(30_000, 20 + i))));
    files.push(("z.bin", same.clone()));
    let opt = Options { batch: 30_000, segment: 1, ..Options::default() };
    let (c, r) = archived(&files, &opt);
    assert!(metadata_blocks(&c) > 3, "{}", metadata_blocks(&c));
    let (a, z) = (0, files.len() - 1);
    assert!(r.files[a].same_as == Some(z) || r.files[z].same_as == Some(a), "{:?} {:?}", r.files[a], r.files[z]);
}

/// Files linked together in two segments: of one link group after them, however they were
/// numbered (archive-wide).
#[test]
fn segments_link_groups() {
    let files = vec![("a.txt", text(20_000, 0)), ("b.txt", text(20_000, 1)), ("c.txt", text(20_000, 2))];
    let (mut tree, contents) = tree_of(&files);
    let mut groups = [Some(7), Some(3), Some(7)].into_iter();
    for e in tree.entries.iter_mut().filter(|e| matches!(e.node, EntryNode::File(_))) {
        e.extra.link_group = groups.next().flatten();
    }
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let a = compress_tree(&tree, &inputs, &Options { batch: 20_000, segment: 1, ..Options::default() });
    assert!(metadata_blocks(&a.archive) >= 3);
    let Ok(Value::Tree(t, _)) = decompress_value(&a.archive, 1) else { panic!("not decoded") };
    let group = |name: &[u8]| t.entries.iter().find(|e| e.name == name).unwrap().extra.link_group;
    let (ga, gb, gc) = (group(b"a.txt"), group(b"b.txt"), group(b"c.txt"));
    assert!(ga.is_some() && ga == gc && gb.is_some() && gb != ga, "{ga:?} {gb:?} {gc:?}");
}

/// A file, and the same in a gzip file and in a ZIP archive: their streams rebuilt and taken
/// from the file, so that its data is stored once, however far apart (in streams longer than
/// a block, which are not solid, and in batches smaller than each).
#[test]
fn held_once() {
    held_once_of(["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dogs", "and", "cats"], "wrt-recons");
    // (text of Hangul by its own module)
    held_once_of(
        ["사람은", "나는", "한국어로", "글을", "쓴다", "그리고", "읽는다", "말이다", "또", "개는"],
        "wrt-recons-hangul",
    );
}

fn held_once_of(words: [&str; 10], module: &str) {
    let mut x = 1u64;
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
    assert!(r.files[0].modules.iter().any(|m| m.name == module), "{r:?}");
    // (their records not small: rules fit miniz's streams less well than zlib's)
    assert!(c.len() < alone.len() * 3 / 2, "{} of {} alone", c.len(), alone.len());
    // in batches of less than each, kept together
    let (c, _) = archived(&files, &Options { batch: 1 << 20, ..opt });
    assert!(c.len() < alone.len() * 3 / 2, "{} of {} alone, in batches", c.len(), alone.len());
}

/// Below level 4, a file whose data a ZIP archive's entry, a gzip file or a zlib stream archived
/// along with it holds is made of their DEFLATE stream (flate-inflate), which is kept as it is
/// (not rebuilt) and stored once: the file costs about nothing on top of the container.
#[test]
fn inflated_below_level_4() {
    let mut x = 1u64;
    let mut data = Vec::new();
    while data.len() < 1 << 20 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        write!(data, "{} {} ", ["the", "quick", "brown", "fox", "jumps"][(x >> 60) as usize % 5], (x >> 40) % 1000)
            .unwrap();
    }
    let mut e = flate2::GzBuilder::new().filename("words").write(Vec::new(), flate2::Compression::new(6));
    e.write_all(&data).unwrap();
    let gz = e.finish().unwrap();
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(&data).unwrap();
    let zlib = e.finish().unwrap();
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(&data).unwrap();
    let zip = zip_of("words", &data, &e.finish().unwrap());
    let opt = Options { level: aqr_formula::aqr::Level::LOW, ..Options::default() };
    for (name, container) in [("w/words.gz", gz), ("w/words.zlib", zlib), ("w/words.zip", zip)] {
        let (alone, _) = archived(&[(name, container.clone())], &opt);
        let (c, r) = archived(&[("w/words", data.clone()), (name, container)], &opt);
        let modules = |k: usize| r.files[k].modules.iter().map(|m| m.name).collect::<Vec<_>>();
        assert_eq!(modules(0), ["flate-inflate"], "{name} {r:?}");
        assert!(!modules(1).contains(&"flate-set"), "{name} {r:?}");
        assert!(c.len() < alone.len() + (16 << 10), "{name}: {} of {} alone", c.len(), alone.len());
    }
}

/// `n` bytes at random from `seed`, which do not compress.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect()
}

/// A file that does not compress, and another that starts with it, alone in an archive (no
/// file recompressed): the first is stored once.
#[test]
fn raw_held_once() {
    let a = noise(3 << 20, 1);
    let b = [&a[..], &noise(1 << 20, 2)[..]].concat();
    let (c, _) = archived(&[("a.bin", a), ("b.bin", b)], &Options::default());
    assert!(c.len() < (4 << 20) + (64 << 10), "{} bytes", c.len());
}

/// A PPMd var. J file of `data` as file `name` of time `time` (order 10, 256 MB, restore
/// method 1), its data as it is.
fn ppmd(name: &str, time: u32, data: &[u8]) -> Vec<u8> {
    let mut d = vec![0x8f, 0xaf, 0xac, 0x84, 0x20, 0, 0, 0, 0xf9, 0x9f];
    d.extend_from_slice(&(name.len() as u16 | 1 << 14).to_le_bytes());
    d.extend_from_slice(&time.to_le_bytes());
    d.extend_from_slice(name.as_bytes());
    d.extend_from_slice(data);
    d
}

/// Two PPMd files of the same text, the one's a part of the other's (but for the end of its
/// data), of other names and times: the shorter's data is stored once, in the longer's.
#[test]
fn ppmd_held_once() {
    let body = noise(3 << 20, 3);
    let short = [&body[..1 << 20], &noise(8, 4)[..]].concat();
    let files =
        [("dc/enwik8.pmd", ppmd("enwik8", 0x356f_3c17, &short)), ("dc/enwik9.pmd", ppmd("enwik9", 0x3566_b6e4, &body))];
    let (c, r) = archived(&files, &Options::default());
    assert!(c.len() < (3 << 20) + (64 << 10), "{} bytes, {r:?}", c.len());
}

/// A WebM file of a video track 1 and an audio track 2, and of a cluster for each
/// frame of `video` (a keyframe where `key`), with 2 KiB of audio of `seed` after it.
fn webm(video: &[(bool, &[u8])], seed: u64) -> Vec<u8> {
    fn element(id: &[u8], data: &[u8]) -> Vec<u8> {
        [id, &(data.len() as u64 | 1 << 56).to_be_bytes(), data].concat()
    }
    let entry = |n: u8, kind: u8| element(&[0xae], &[element(&[0xd7], &[n]), element(&[0x83], &[kind])].concat());
    let mut seg = element(&[0x16, 0x54, 0xae, 0x6b], &[entry(1, 1), entry(2, 2)].concat());
    for (i, &(key, frame)) in video.iter().enumerate() {
        let video = [&[0x81, 0, 0, if key { 0x80 } else { 0 }][..], frame].concat();
        let audio = [&[0x82, 0, 0, 0x80][..], &noise(2 << 10, seed + i as u64)].concat();
        let body = [element(&[0xe7], &[i as u8]), element(&[0xa3], &video), element(&[0xa3], &audio)].concat();
        seg.extend(element(&[0x1f, 0x43, 0xb6, 0x75], &body));
    }
    let head = element(&[0x1a, 0x45, 0xdf, 0xa3], &element(&[0x42, 0x82], b"webm"));
    [head, element(&[0x18, 0x53, 0x80, 0x67], &seg)].concat()
}

/// A WebM file of a still picture, its keyframe again every 4 frames: the keyframe is held once;
/// another of no frame repeated is stored as it is.
#[test]
fn mkv_keyframes_once() {
    let key = noise(256 << 10, 5);
    let still = vec![0u8; 300];
    let video: Vec<(bool, &[u8])> =
        (0..40).map(|i| if i % 4 == 0 { (true, &key[..]) } else { (false, &still[..]) }).collect();
    let d = webm(&video, 6);
    let (c, r) = archived(&[("still.webm", d.clone())], &Options::default());
    let least = key.len() + 40 * (2 << 10);
    assert!(c.len() < least + (16 << 10) && d.len() > least * 3, "{} bytes of {}, {r:?}", c.len(), d.len());
    // (zstd's window holds this file's keyframes too: what tells is the formula)
    let (_, f) = aqr_compress::compress_formula_as(&d, aqr_formula::Kind::Format("mkv"), &Options::default());
    let keys: Vec<usize> = f.streams.iter().filter(|s| s.class == "mkv/keys").map(|s| s.data.len()).collect();
    assert_eq!(keys, [key.len()]);

    let frames: Vec<Vec<u8>> = (0..40).map(|i| noise(8 << 10, 100 + i)).collect();
    let video: Vec<(bool, &[u8])> = frames.iter().enumerate().map(|(i, f)| (i % 4 == 0, &f[..])).collect();
    let d = webm(&video, 7);
    let (c, _) = archived(&[("moving.mkv", d.clone())], &Options::default());
    assert!(c.len() >= d.len() && c.len() < d.len() + (4 << 10), "{} bytes of {}", c.len(), d.len());
}

/// Two files alike but for their starts (another header, a few bytes longer), with a file
/// apart between them: in a batch together however small the batches, the archive about as
/// small as in one.
#[test]
fn alike_put_together() {
    let data = words(600_000, 3);
    let edited = [&b"a header of its own, longer\n"[..], &data[40..]].concat();
    let files = [("a.txt", data.clone()), ("b.txt", words(1 << 20, 4)), ("c.txt", edited)];
    let (alone, _) = archived(&[("a.txt", data)], &Options::default());
    let (c, _) = archived(&files, &Options::default());
    let (apart, _) = archived(&files, &Options { batch: 1 << 20, ..Options::default() });
    assert!(
        apart.len() < c.len() + alone.len() / 4,
        "{} in batches, {} in one, {} alone",
        apart.len(),
        c.len(),
        alone.len()
    );
}

/// Contents read as bytes shared (files mapped into memory, as `aqr` reads them): the same
/// archive as of contents copied, the shared bytes cut, compacted and merged as those are.
#[test]
fn shared_contents() {
    struct Shared(Vec<(String, std::sync::Arc<Vec<u8>>)>);
    impl aqr_compress::Source for Shared {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn name(&self, c: usize) -> &[u8] {
            self.0[c].0.as_bytes()
        }
        fn size(&self, c: usize) -> u64 {
            self.0[c].1.len() as u64
        }
        fn read(&self, c: usize) -> std::io::Result<aqr_formula::Bytes> {
            Ok(aqr_formula::Bytes::shared(self.0[c].1.clone()))
        }
    }
    let data = words(3 << 20, 1);
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    e.write_all(&data).unwrap();
    let zip = zip_of("words", &data, &e.finish().unwrap());
    let mut files = sample();
    files.extend([("w/words", data.clone()), ("w/words.gz", gzip(&data)), ("w/words.zip", zip)]);
    for opt in [Options::default(), Options { batch: 1 << 20, block: 1 << 20, ..Options::default() }] {
        let (copied, _) = archived(&files, &opt);
        let (tree, contents) = tree_of(&files);
        let src = Shared(contents.into_iter().map(|(n, d)| (n, std::sync::Arc::new(d))).collect());
        let mut shared = Vec::new();
        aqr_compress::compress_tree_to(&tree, &src, &opt, &mut shared, &|_| {}).unwrap();
        assert!(shared == copied, "{} bytes, not {}", shared.len(), copied.len());
    }
}

/// A content not read as it is (a file cut short while mapped): the archive fails, whether its
/// batch is written or, of the same content as another, not.
#[test]
fn not_intact() {
    struct Cut(Vec<(String, Vec<u8>)>, usize);
    impl aqr_compress::Source for Cut {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn name(&self, c: usize) -> &[u8] {
            self.0[c].0.as_bytes()
        }
        fn size(&self, c: usize) -> u64 {
            self.0[c].1.len() as u64
        }
        fn read(&self, c: usize) -> std::io::Result<aqr_formula::Bytes> {
            Ok(self.0[c].1.clone().into())
        }
        fn intact(&self, c: usize) -> std::io::Result<()> {
            match c == self.1 {
                true => Err(std::io::Error::other("cut")),
                false => Ok(()),
            }
        }
    }
    let files = sample();
    let (tree, contents) = tree_of(&files);
    let src = Cut(contents, 0);
    let mut out = Vec::new();
    assert!(aqr_compress::compress_tree_to(&tree, &src, &Options::default(), &mut out, &|_| {}).is_err());
    // (t1.txt, the same as same.txt, in a batch of its own)
    let opt = Options { batch: 1, ..Options::default() };
    let src = Cut(src.0, 1);
    assert!(aqr_compress::compress_tree_to(&tree, &src, &opt, &mut out, &|_| {}).is_err());
}

/// Words at random, `n` bytes of them from `seed`.
fn words(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let words = ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dogs", "and", "cats"];
    let mut data = Vec::new();
    while data.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        write!(data, "{} {} ", words[(x >> 60) as usize % 10], (x >> 40) % 1000).unwrap();
    }
    data
}

/// A ustar header of `name` for a body of `n` bytes.
fn tar_header(name: &str, n: usize) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[124..136].copy_from_slice(format!("{n:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"14123456701\0");
    h[156] = b'0';
    h[257..265].copy_from_slice(b"ustar\x0000");
    let sum: u32 = h.iter().map(|&b| b as u32).sum::<u32>() + 8 * b' ' as u32;
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut d = Vec::new();
    for (name, body) in files {
        d.extend(tar_header(name, body.len()));
        d.extend_from_slice(body);
        d.resize(d.len().next_multiple_of(512), 0);
    }
    d.resize(d.len() + 10240 - d.len() % 10240, 0);
    d
}

/// Files, a `.tar.lzma` of them and a `.lzma` of one: the streams rebuilt from data taken from
/// the files (the archive's members, the file), so that the files are stored once, in a batch
/// together however small the batches.
#[test]
fn lzma_held_once() {
    let members: Vec<(String, Vec<u8>)> = (0..3).map(|k| (format!("m/f{k}.txt"), words(1_000_000, k))).collect();
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let tlz = aqr_lzma::greedy::alone(&tar(&refs));
    let lz = aqr_lzma::greedy::alone(&members[0].1);
    assert!(tlz.len() > 256 << 10 && lz.len() > 256 << 10);
    let opt = Options { block: 1 << 20, ..Options::default() };
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let (alone, r) = archived(&files, &opt);
    // (of the files, without the modules' blocks)
    let alone = alone.len() - r.overhead;
    let all = [&files[..], &[("p/m.tar.lzma", tlz.clone()), ("p/f0.txt.lzma", lz.clone())]].concat();
    for opt in [opt.clone(), Options { batch: 1 << 20, ..opt.clone() }] {
        let (c, r) = archived(&all, &opt);
        // (the report's files in the order of the contents)
        for k in [3, 4] {
            assert!(r.files[k].modules.iter().any(|m| m.name == "lzma-recons"), "{}: {:?}", all[k].0, r.files[k]);
        }
        // (records of a greedy parse, and the tar archive's headers: small)
        let c = c.len() - r.overhead;
        assert!(c < alone + alone / 100, "{c} of {alone} alone");
    }
}

/// Files, a `.tar.zst` of them, a file of two frames (a skippable frame between them) of
/// another, and a `.zst` of the same as the tar archive: the files taken from the frames
/// (decompressed by Patch instructions), which are kept as they are, so that the files cost
/// next to nothing; the tar archive taken from the frames of its own. In a batch together
/// however small the batches.
#[test]
fn zstd_held_once() {
    let mut members: Vec<(String, Vec<u8>)> = (0..3).map(|k| (format!("m/f{k}.txt"), words(1_000_000, k))).collect();
    // (raw blocks)
    members.push(("m/noise.bin".into(), noise(300_000, 1)));
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let tarred = tar(&refs);
    let tzst = zstd::bulk::compress(&tarred, 3).unwrap();
    let g = words(400_000, 9);
    let mut gzst = zstd::bulk::compress(&g[..150_000], 1).unwrap();
    gzst.extend_from_slice(&[0x5e, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4]);
    gzst.extend(zstd::bulk::compress(&g[150_000..], 5).unwrap());
    let opt = Options { block: 1 << 20, ..Options::default() };
    let zsts = [("p/m.tar.zst", tzst.clone()), ("p/m2.tar.zst", zstd::bulk::compress(&tarred, 1).unwrap())];
    let zsts = [&zsts[..], &[("q/g.txt.zst", gzst.clone())]].concat();
    let (only, r) = archived(&zsts, &opt);
    let only = only.len() - r.overhead;
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let all = [&files[..], &[("q/g.txt", g.clone()), ("p/m.tar", tarred.clone())], &zsts[..]].concat();
    for opt in [opt.clone(), Options { batch: 1 << 20, ..opt.clone() }] {
        let (c, r) = archived(&all, &opt);
        let c = c.len() - r.overhead;
        assert!(c < only + only / 50, "{c} of {only} of the frames alone");
    }
    // (frames of more content than an application's bound: the tar archives' not taken)
    let (c, r) = archived(&all, &Options { application_bound: Some(1 << 20), ..opt });
    let c = c.len() - r.overhead;
    assert!(c > only + only / 2, "{c} of {only} of the frames alone");
}

/// Files, a `.tar.xz` of them and an `.xz` of one: the LZMA2 streams rebuilt from data taken
/// from the files, so that the files are stored once, in a batch together however small the
/// batches.
#[test]
fn xz_held_once() {
    let members: Vec<(String, Vec<u8>)> = (0..3).map(|k| (format!("m/f{k}.txt"), words(1_000_000, k))).collect();
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let txz = aqr_lzma::greedy::xz(&tar(&refs));
    let xz = aqr_lzma::greedy::xz(&members[0].1);
    assert!(txz.len() > 256 << 10 && xz.len() > 256 << 10);
    let opt = Options { block: 1 << 20, ..Options::default() };
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let (alone, r) = archived(&files, &opt);
    let alone = alone.len() - r.overhead;
    let all = [&files[..], &[("p/m.tar.xz", txz.clone()), ("p/f0.txt.xz", xz.clone())]].concat();
    for opt in [opt.clone(), Options { batch: 1 << 20, ..opt.clone() }] {
        let (c, r) = archived(&all, &opt);
        for k in [3, 4] {
            assert!(r.files[k].modules.iter().any(|m| m.name == "lzma2-recons"), "{}: {:?}", all[k].0, r.files[k]);
        }
        let c = c.len() - r.overhead;
        assert!(c < alone + alone / 100, "{c} of {alone} alone");
    }
}

fn bzip2(d: &[u8]) -> Vec<u8> {
    aqr_bz2::compress::stream(9, &aqr_bz2::compress::blocks(d, 9, 1))
}

/// Files, a `.tar.bz2` of them and a `.bz2` of one: their data is text, which archives larger
/// than bzip2 makes it, but the files hold it, so that the streams are rebuilt and the files
/// stored once; a `.bz2` of other text alone is kept as it is. In one batch: what the files of
/// other batches hold is not told ([`aqr_formula::probe::Held`]).
#[test]
fn bz2_held_once() {
    let members: Vec<(String, Vec<u8>)> = (0..3).map(|k| (format!("m/f{k}.txt"), words(1_000_000, k))).collect();
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let tbz = bzip2(&tar(&refs));
    let bz = bzip2(&members[0].1);
    let other = bzip2(&words(1_000_000, 9));
    let opt = Options { block: 1 << 20, ..Options::default() };
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let (alone, r) = archived(&files, &opt);
    let alone = alone.len() - r.overhead;
    let all = [&files[..], &[("p/m.tar.bz2", tbz), ("p/f0.txt.bz2", bz), ("q/other.txt.bz2", other.clone())]].concat();
    {
        let (c, r) = archived(&all, &opt);
        for k in [3, 4] {
            assert!(r.files[k].modules.iter().any(|m| m.name == "bz2-recons"), "{}: {:?}", all[k].0, r.files[k]);
        }
        assert!(r.files[5].modules.is_empty(), "{:?}", r.files[5]);
        let c = c.len() - r.overhead - other.len();
        assert!(c < alone + alone / 100, "{c} of {alone} alone");
    }
}

/// Files and a 7z archive of them: the folder rebuilt from the files' contents, stored once, in a
/// batch together however small the batches.
#[test]
fn sevenz_held_once() {
    let members: Vec<(String, Vec<u8>)> = (0..3).map(|k| (format!("m/f{k}.txt"), words(1_000_000, k))).collect();
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let sz = aqr_7z::write::solid(&refs);
    assert!(sz.len() > 256 << 10);
    let opt = Options { block: 1 << 20, ..Options::default() };
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let (alone, r) = archived(&files, &opt);
    let alone = alone.len() - r.overhead;
    let all = [&files[..], &[("p/m.7z", sz.clone())]].concat();
    for opt in [opt.clone(), Options { batch: 1 << 20, ..opt.clone() }] {
        let (c, r) = archived(&all, &opt);
        assert!(r.files[3].modules.iter().any(|m| m.name == "lzma2-recons"), "{:?}", r.files[3]);
        let c = c.len() - r.overhead;
        assert!(c < alone + alone / 100, "{c} of {alone} alone");
    }
}

fn deflated(d: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(d).unwrap();
    z.finish().unwrap()
}

/// An ALZ archive of `files`, deflated, as ALZip lays them out (sizes of 4 bytes).
fn alz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut a = b"ALZ\x01\x0a\0\0\0".to_vec();
    for &(name, d) in files {
        let body = deflated(d);
        a.extend_from_slice(b"BLZ\x01");
        a.extend_from_slice(&(name.len() as u16).to_le_bytes());
        a.extend_from_slice(&[0x20, 0, 0, 0x21, 0x5a, 0x40, 0]);
        a.extend_from_slice(&[aqr_alz::alz::DEFLATE, 0]);
        a.extend_from_slice(&aqr_common::crc32::hash(d).to_le_bytes());
        a.extend_from_slice(&(body.len() as u32).to_le_bytes());
        a.extend_from_slice(&(d.len() as u32).to_le_bytes());
        a.extend_from_slice(name.as_bytes());
        a.extend_from_slice(&body);
    }
    a.extend_from_slice(b"CLZ\x01\0\0\0\0\0\0\0\0CLZ\x02");
    a
}

/// An EGG archive of `files`, each a deflated block, as ALZip lays them out.
fn egg(files: &[(&str, &[u8])]) -> Vec<u8> {
    let put = |a: &mut Vec<u8>, s: u32| a.extend_from_slice(&s.to_le_bytes());
    let mut a = b"EGGA\0\x01\x01\x02\x03\x04\0\0\0\0".to_vec();
    put(&mut a, 0x08E2_8222);
    for (i, &(name, d)) in files.iter().enumerate() {
        let body = deflated(d);
        put(&mut a, 0x0A85_90E3);
        put(&mut a, i as u32);
        a.extend_from_slice(&(d.len() as u64).to_le_bytes());
        put(&mut a, 0x0A85_91AC);
        a.push(0);
        a.extend_from_slice(&(name.len() as u16).to_le_bytes());
        a.extend_from_slice(name.as_bytes());
        put(&mut a, 0x08E2_8222);
        put(&mut a, 0x02B5_0C13);
        a.extend_from_slice(&[aqr_egg::egg::DEFLATE, 0]);
        put(&mut a, d.len() as u32);
        put(&mut a, body.len() as u32);
        put(&mut a, aqr_common::crc32::hash(d));
        put(&mut a, 0x08E2_8222);
        a.extend_from_slice(&body);
    }
    put(&mut a, 0x08E2_8222);
    a
}

/// Files and an ALZ and an EGG archive of them: the archives' deflated bodies rebuilt from the
/// files' contents, stored once, in a batch together however small the batches (the entries'
/// CRC-32 and sizes tell what the archives hold).
#[test]
fn alz_egg_held_once() {
    let members: Vec<(String, Vec<u8>)> = (0..3).map(|k| (format!("m/f{k}.txt"), words(1_000_000, k))).collect();
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let opt = Options { block: 1 << 20, ..Options::default() };
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let (alone, r) = archived(&files, &opt);
    let alone = alone.len() - r.overhead;
    for (name, a) in [("p/m.alz", alz(&refs)), ("p/m.egg", egg(&refs))] {
        assert!(a.len() > 256 << 10);
        let all = [&files[..], &[(name, a)]].concat();
        for opt in [opt.clone(), Options { batch: 1 << 20, ..opt.clone() }] {
            let (c, r) = archived(&all, &opt);
            assert!(r.files[3].modules.iter().any(|m| m.name == "flate-set"), "{name}: {:?}", r.files[3]);
            // (and the streams' records: flate2's zlib-rs does not write zlib's own streams)
            let c = c.len() - r.overhead;
            assert!(c < alone * 5 / 4, "{name}, batch {}: {c} of {alone} alone", opt.batch);
        }
    }
}

/// A file and the git loose object of it (a zlib stream of `blob <size>\0` and the file): the
/// object's data is the file after its header, stored once, in a batch together however small
/// the batches.
#[test]
fn git_loose_object_held_once() {
    let data = words(1_000_000, 7);
    let mut object = format!("blob {}\0", data.len()).into_bytes();
    object.extend_from_slice(&data);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&object).unwrap();
    let loose = z.finish().unwrap();
    let opt = Options { block: 1 << 20, ..Options::default() };
    let (alone, r) = archived(&[("w/words.txt", data.clone())], &opt);
    let alone = alone.len() - r.overhead;
    let files = [("w/words.txt", data), (".git/objects/12/3456", loose)];
    for opt in [opt.clone(), Options { batch: 1 << 20, ..opt.clone() }] {
        let (c, r) = archived(&files, &opt);
        assert!(r.files[1].modules.iter().any(|m| m.name == "flate-set"), "{:?}", r.files[1]);
        // (and the stream's record: flate2's zlib-rs does not write zlib's own streams)
        let c = c.len() - r.overhead;
        assert!(c < alone * 5 / 4, "{c} of {alone} alone");
    }
}

/// A git repository's one pack with its index and reverse index, of a history of files that
/// change a little each commit (made with the git CLI).
fn git_pack_files() -> [Vec<u8>; 3] {
    let dir = std::env::temp_dir().join(format!("aqr-compress-test-{}-pack", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    for c in 0..12 {
        std::fs::write(dir.join("a.txt"), text(20_000 + c * 100, c)).unwrap();
        std::fs::write(dir.join(format!("b{}.txt", c % 3)), text(5_000, c * 3)).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", &format!("commit {c}")]);
    }
    git(&["-c", "pack.writeReverseIndex=true", "repack", "-q", "-a", "-d"]);
    let packs = dir.join(".git/objects/pack");
    let file = |ext: &str| {
        let mut e = std::fs::read_dir(&packs).unwrap().map(|e| e.unwrap().path());
        std::fs::read(e.find(|p| p.extension().is_some_and(|x| x == ext)).unwrap()).unwrap()
    };
    let files = [file("pack"), file("idx"), file("rev")];
    std::fs::remove_dir_all(&dir).unwrap();
    files
}

/// A git pack with its index and reverse index: a set, of which only the ids of the indexes are
/// stored, in a batch together however small the batches.
#[test]
fn git_pack_with_indexes() {
    let [pack, idx, rev] = git_pack_files();
    let files = [
        ("a.txt", text(30_000, 1)),
        ("objects/pack/pack-1.idx", idx.clone()),
        ("objects/pack/pack-1.pack", pack.clone()),
        ("objects/pack/pack-1.rev", rev.clone()),
    ];
    let n = u32::from_be_bytes(pack[8..12].try_into().unwrap()) as f64;
    let alone = |f: &(&str, Vec<u8>)| archived(std::slice::from_ref(f), &Options::default()).1.files[0].archived;
    let alone: f64 = files[1..].iter().map(alone).sum();
    for opt in [Options::default(), Options { batch: 1, ..Options::default() }] {
        let (_, r) = archived(&files, &opt);
        assert_eq!(sets(&r), [vec![1, 2, 3]]);
        assert!(r.files[2].modules.iter().any(|m| m.name == "git-pack-index-recons"), "{:?}", r.files[2]);
        // (of the indexes, the ids: 20 bytes an entry, against a compressed 37 alone)
        let set: f64 = r.files[1..].iter().map(|f| f.archived).sum();
        assert!(set + 10.0 * n < alone, "{set} of {alone} alone, {n} entries");
    }
    // an index not of the pack is archived as it is
    let mut other = idx.clone();
    other[8 + 1024] ^= 1;
    let (_, r) = archived(&[("p.idx", other), ("p.pack", pack.clone())], &Options::default());
    assert!(sets(&r).is_empty());
    // without the reverse index
    let (_, r) = archived(&[("p.idx", idx), ("p.pack", pack)], &Options::default());
    assert_eq!(sets(&r), [vec![0, 1]]);
}

/// A PE32+ image of x86-64 code: calls to a few functions and loads, of constants at random
/// from `seed`, `n` of each.
fn pe(n: u32, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut code = Vec::new();
    for i in 0..n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let at = code.len() as i32;
        // mov [rsp+8], rbx; call f(i % 5); mov rax, [rip+d]; add rsp, k; ret; int3
        code.extend_from_slice(&[0x48, 0x89, 0x5c, 0x24, 0x08, 0xe8]);
        code.extend_from_slice(&((i % 5) as i32 * 0x40 - at - 10).to_le_bytes());
        code.extend_from_slice(&[0x48, 0x8b, 0x05]);
        code.extend_from_slice(&((x >> 40) as i32 % 0x10000 - at - 17).to_le_bytes());
        code.extend_from_slice(&[0x48, 0x83, 0xc4, (x >> 56) as u8, 0xc3, 0xcc]);
    }
    let mut d = vec![0u8; 0x200];
    d[..2].copy_from_slice(b"MZ");
    d[0x3c] = 0x40;
    d[0x40..0x44].copy_from_slice(b"PE\0\0");
    d[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
    d[0x46] = 1; // sections
    d[0x54] = 0x70; // optional header size
    let opt = 0x58;
    d[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    d[opt + 0x18..opt + 0x20].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
    let sec = opt + 0x70;
    d[sec..sec + 5].copy_from_slice(b".text");
    d[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes());
    d[sec + 16..sec + 20].copy_from_slice(&(code.len() as u32).to_le_bytes());
    d[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
    d[sec + 36..sec + 40].copy_from_slice(&0x6000_0020u32.to_le_bytes());
    d.extend_from_slice(&code);
    d
}

/// PE files and a `.tar.lzma` of them: the tar archive taken as aqr-bundle's, its members' streams
/// stored once with the files', though as they are not the files' bytes the archive would not
/// seem to pay for itself (it does not, alone).
#[test]
fn lzma_of_pe_files_held_once() {
    // (versions of a program, a byte apart: the tar archive raw compresses as well as its
    // members apart, and costs less)
    let base = pe(60_000, 7);
    let members: Vec<(String, Vec<u8>)> = (0..4)
        .map(|k| {
            let mut d = base.clone();
            d[0x1000 + 997 * k] ^= 0x55;
            (format!("b/p{k}.exe"), d)
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = members.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    let tlz = aqr_lzma::greedy::alone(&tar(&refs));
    assert!(tlz.len() > 256 << 10, "{}", tlz.len());
    let opt = Options { block: 1 << 20, ..Options::default() };
    let files: Vec<(&str, Vec<u8>)> = members.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let (alone, r) = archived(&files, &opt);
    let alone = alone.len() - r.overhead;
    let all = [&files[..], &[("p/b.tar.lzma", tlz.clone())]].concat();
    let (c, r) = archived(&all, &opt);
    let names: Vec<&str> = r.files[4].modules.iter().map(|m| m.name).collect();
    assert_eq!(names, ["lzma-recons", "bundle-recons", "exe-recons/x86"], "{:?}", r.files[4]);
    let c = c.len() - r.overhead;
    assert!(c < alone + alone / 50, "{c} of {alone} alone");
}

/// ARM64 code of `n` functions of 8 to 14 words, after `pad` words of padding: each of distinct
/// words, calling another and taking a page of its own.
fn arm64_functions(n: usize, pad: usize) -> Vec<u8> {
    let len = |f: usize| 8 + f % 7;
    let mut at = Vec::with_capacity(n);
    let mut k = pad;
    for f in 0..n {
        at.push(k);
        k += len(f);
    }
    let mut w: Vec<u32> = vec![0xd503_201f; pad];
    for f in 0..n {
        let callee = (f * 7919 + 13) % n;
        w.push(0xa9bf_7bfd); // stp x29, x30, [sp, #-16]!
        w.push(0x9400_0000 | (at[callee] as u32).wrapping_sub(w.len() as u32) & 0x3ff_ffff);
        let page = (0x100 + f as u32 / 64).wrapping_sub((w.len() >> 10) as u32);
        w.push(0x9000_0008 | (page & 3) << 29 | (page >> 2 & 0x7_ffff) << 5); // adrp x8
        w.push(0xd280_0000 | (f as u32 & 0xffff) << 5 | 9); // movz x9, #f
        w.push(0xf2a0_0000 | (f as u32 >> 16) << 5 | 9); // movk x9, #f >> 16, lsl 16
        w.extend((5..len(f) - 1).map(|j| 0x9100_0000 | (j as u32) << 10 | 9 << 5 | 9)); // add x9, x9, #j
        w.push(0xd65f_03c0); // ret
    }
    w.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// x86-64 code at `origin` of `n` functions of 32 bytes, after `pad` bytes of padding: each of
/// distinct bytes, calling another, taking an address of its own and jumping to the next.
fn x86_functions(n: usize, pad: usize, origin: u64) -> Vec<u8> {
    let at = |f: usize| pad + 32 * f;
    let mut c = vec![0xcc; pad];
    for f in 0..n {
        let rel = |c: &Vec<u8>, to: usize| ((to as i64 - (c.len() + 4) as i64) as i32).to_le_bytes();
        c.extend_from_slice(&[0x55, 0x48, 0x89, 0xe5, 0xe8]); // push rbp; mov rbp, rsp; call
        let r = rel(&c, at((f * 7919 + 13) % n));
        c.extend_from_slice(&r);
        c.extend_from_slice(&[0x48, 0x8d, 0x05]); // lea rax, [rip + data f]
        let r = (0x1000_0000 + 64 * f as i64) - (origin as i64 + c.len() as i64 + 4);
        c.extend_from_slice(&(r as i32).to_le_bytes());
        c.push(0xb9); // mov ecx, f
        c.extend_from_slice(&(f as u32).to_le_bytes());
        c.push(0xe9); // jmp the next one
        let r = rel(&c, at((f + 1) % n));
        c.extend_from_slice(&r);
        c.extend_from_slice(&[0x5d, 0xc3]); // pop rbp; ret
        c.resize(at(f + 1), 0xcc);
    }
    c
}

/// A thin ARM64 Mach-O image of `code` in a section of instructions of its one segment.
fn macho_arm64(code: &[u8]) -> Vec<u8> {
    macho(0x0100_000c, code)
}

/// A thin 64-bit Mach-O image of `cputype`, of `code` in a section of instructions of its one
/// segment, at 0x1_0000_0200.
fn macho(cputype: u32, code: &[u8]) -> Vec<u8> {
    let (cmds, at) = (72 + 80, 0x200);
    let mut d = vec![0u8; at];
    let put32 = |d: &mut Vec<u8>, p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, p: usize, v: u64| d[p..p + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut d, 0, 0xfeed_facf);
    put32(&mut d, 4, cputype);
    put32(&mut d, 12, 2); // MH_EXECUTE
    put32(&mut d, 16, 1);
    put32(&mut d, 20, cmds as u32);
    let size = (at + code.len()) as u64;
    put32(&mut d, 32, 0x19); // LC_SEGMENT_64
    put32(&mut d, 36, cmds as u32);
    d[40..46].copy_from_slice(b"__TEXT");
    put64(&mut d, 32 + 24, 0x1_0000_0000);
    put64(&mut d, 32 + 32, size);
    put64(&mut d, 32 + 48, size);
    put32(&mut d, 32 + 64, 1);
    let s = 32 + 72;
    d[s..s + 6].copy_from_slice(b"__text");
    d[s + 16..s + 22].copy_from_slice(b"__TEXT");
    put64(&mut d, s + 32, 0x1_0000_0000 + at as u64);
    put64(&mut d, s + 40, code.len() as u64);
    put32(&mut d, s + 48, at as u32);
    put32(&mut d, s + 64, 0x8000_0400);
    d.extend_from_slice(code);
    d
}

/// Versions of a program whose code moved: one's targets written in the other's address space
/// (`exe-recons/arm64-remap`), its code stream then coded against the other's
/// (`exe-recons/arm64-delta`, the other's stored once), so that its code costs little.
#[test]
fn arm64_versions_remapped() {
    // (code of over half a block: of streams cut into pieces, put with those alike)
    let (a, b) = (macho_arm64(&arm64_functions(26_000, 0)), macho_arm64(&arm64_functions(26_000, 37)));
    assert!(a.len() > 1 << 20);
    let opt = Options { block: 2 << 20, ..Options::default() };
    let (alone, r) = archived(&[("a", a.clone())], &opt);
    let alone = alone.len() - r.overhead;
    let (both, r) = archived(&[("a", a), ("b", b)], &opt);
    let names = |k: usize| -> Vec<&str> { r.files[k].modules.iter().map(|m| m.name).collect() };
    // (either the root of the two)
    let mut all = [names(0), names(1)];
    all.sort();
    assert_eq!(
        all,
        [&["exe-recons/arm64"][..], &["exe-recons/arm64-remap", "exe-recons/arm64-delta"]],
        "{:?}",
        r.files
    );
    let both = both.len() - r.overhead;
    assert!(both < alone + alone / 20, "{both} of {alone} alone");
}

/// A thin ARM64 Mach-O image of `code` in a `__TEXT` segment and `data` in a `__DATA_CONST`
/// segment's `__const` section after it.
fn macho_arm64_data(code: &[u8], data: &[u8]) -> Vec<u8> {
    let (cmds, at) = (2 * (72 + 80), 0x400);
    let mut d = vec![0u8; at];
    let put32 = |d: &mut Vec<u8>, p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, p: usize, v: u64| d[p..p + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut d, 0, 0xfeed_facf);
    put32(&mut d, 4, 0x0100_000c);
    put32(&mut d, 12, 2); // MH_EXECUTE
    put32(&mut d, 16, 2);
    put32(&mut d, 20, cmds as u32);
    let data_at = at + code.len();
    for (k, (seg, sect, off, len, flags)) in [
        (&b"__TEXT"[..], &b"__text"[..], at, code.len(), 0x8000_0400u32),
        (b"__DATA_CONST", b"__const", data_at, data.len(), 0),
    ]
    .into_iter()
    .enumerate()
    {
        let c = 32 + k * (72 + 80);
        put32(&mut d, c, 0x19); // LC_SEGMENT_64
        put32(&mut d, c + 4, 72 + 80);
        d[c + 8..c + 8 + seg.len()].copy_from_slice(seg);
        let (va, fileoff, size) = (0x1_0000_0000 + off as u64, if k == 0 { 0 } else { off }, len as u64);
        put64(&mut d, c + 24, if k == 0 { 0x1_0000_0000 } else { va });
        put64(&mut d, c + 32, size + if k == 0 { at as u64 } else { 0 });
        put64(&mut d, c + 40, fileoff as u64);
        put64(&mut d, c + 48, size + if k == 0 { at as u64 } else { 0 });
        put32(&mut d, c + 64, 1);
        let s = c + 72;
        d[s..s + sect.len()].copy_from_slice(sect);
        d[s + 16..s + 16 + seg.len()].copy_from_slice(seg);
        put64(&mut d, s + 32, va);
        put64(&mut d, s + 40, size);
        put32(&mut d, s + 48, off as u32);
        put32(&mut d, s + 64, flags);
    }
    d.extend_from_slice(code);
    d.extend_from_slice(data);
    d
}

/// The tables of pointers of versions of a program (`__DATA_CONST,__const`), what they point to
/// moved: the other's coded against the root's (`exe-recons/arm64-delta`), as their code is.
#[test]
fn macho_pointers_against_root() {
    // pointers into the code of each version, those to a third of the functions 0x40 on in
    // the second
    let table = |shift: u64| -> Vec<u8> {
        (0..40_000u64)
            .flat_map(|i| {
                let f = i * 7919 % 26_000;
                let target = 0x1_0000_0400 + f * 44 + if f % 3 == 0 { shift } else { 0 };
                (target | 1 << 62).to_le_bytes()
            })
            .collect()
    };
    let a = macho_arm64_data(&arm64_functions(26_000, 0), &table(0));
    let b = macho_arm64_data(&arm64_functions(26_000, 37), &table(0x40));
    let opt = Options { block: 2 << 20, ..Options::default() };
    let (alone, r) = archived(&[("a", a.clone())], &opt);
    let alone = alone.len() - r.overhead;
    let (both, r) = archived(&[("a", a), ("b", b)], &opt);
    let both = both.len() - r.overhead;
    assert!(r.files.iter().any(|f| f.modules.iter().any(|m| m.name == "exe-recons/arm64-delta")), "{:?}", r.files);
    // (as they are, 7.5% more)
    assert!(both < alone + alone / 40, "{both} of {alone} alone");
}

/// A 64-bit ELF shared object of ARM64 `code`: its header, the code at 0x1000 (of that
/// address), then `.shstrtab` and the section headers.
fn elf_arm64(code: &[u8]) -> Vec<u8> {
    let names = b"\0.text\0.shstrtab\0";
    let strtab = 0x1000 + code.len();
    let shoff = (strtab + names.len()).next_multiple_of(8);
    let mut d = vec![0u8; 0x1000];
    d[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    d[16] = 3; // ET_DYN
    d[18] = 183;
    d[20] = 1;
    d[40..48].copy_from_slice(&(shoff as u64).to_le_bytes());
    d[52] = 64;
    d[58] = 64;
    d[60] = 3;
    d[62] = 2;
    d.extend_from_slice(code);
    d.extend_from_slice(names);
    d.resize(shoff + 64, 0);
    for (name, kind, flags, off, size) in [(1u32, 1u32, 6u64, 0x1000, code.len()), (7, 3, 0, strtab, names.len())] {
        let p = d.len();
        d.resize(p + 64, 0);
        d[p..p + 4].copy_from_slice(&name.to_le_bytes());
        d[p + 4..p + 8].copy_from_slice(&kind.to_le_bytes());
        d[p + 8..p + 16].copy_from_slice(&flags.to_le_bytes());
        d[p + 16..p + 24].copy_from_slice(&(if flags != 0 { off as u64 } else { 0 }).to_le_bytes());
        d[p + 24..p + 32].copy_from_slice(&(off as u64).to_le_bytes());
        d[p + 32..p + 40].copy_from_slice(&(size as u64).to_le_bytes());
    }
    d
}

/// The same of ELF images.
#[test]
fn elf_versions_remapped() {
    let (a, b) = (elf_arm64(&arm64_functions(26_000, 0)), elf_arm64(&arm64_functions(26_000, 37)));
    let opt = Options { block: 2 << 20, ..Options::default() };
    let (both, r) = archived(&[("a", a), ("b", b)], &opt);
    let names = |k: usize| -> Vec<&str> { r.files[k].modules.iter().map(|m| m.name).collect() };
    let mut all = [names(0), names(1)];
    all.sort();
    assert_eq!(
        all,
        [&["exe-recons/arm64"][..], &["exe-recons/arm64-remap", "exe-recons/arm64-delta"]],
        "{:?}",
        r.files
    );
    assert!(!both.is_empty());
}

/// aqr-exe's filters against other versions (`exe-recons/x86-remap`, `exe-recons/arm64-remap`,
/// `exe-recons/arm64-delta`) take their plans, to the page, decoded in WebAssembly.
#[test]
fn exe_remap_plans_hold() {
    let modules = [aqr_decompress::X86_REMAP, aqr_decompress::ARM64_REMAP, aqr_decompress::ARM64_DELTA];
    let hashes = modules.map(|m| aqr_decompress::built(m).expect("built").sha512_256);
    let x86 = |pad| macho(0x0100_0007, &x86_functions(40_000, pad, 0x1_0000_0200));
    let arm64 = |shift| macho_arm64(&arm64_functions(26_000, shift));
    let opt = Options { block: 2 << 20, ..Options::default() };
    let mut seen = [0; 3];
    for (what, a, b) in [("x86", x86(0), x86(48)), ("arm64", arm64(0), arm64(37))] {
        let (tree, contents) = tree_of(&[("a", a), ("b", b)]);
        let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
        let c = compress_tree(&tree, &inputs, &opt).archive;
        aqr_decompress::record_applications(true);
        let out = aqr_formula::aqr::decode_value_with(&c, &aqr_decompress::Wasm, 1);
        let applied = aqr_decompress::record_applications(false);
        assert!(out.is_ok(), "{what}: {:?}", out.err());
        for a in &applied {
            let Some(k) = hashes.iter().position(|&h| h == a.hash) else { continue };
            let plan = a.limit - a.imported;
            assert!(a.working <= plan && plan - a.working < 1 << 16, "{what}: plan {plan}, working {}", a.working);
            seen[k] += 1;
        }
    }
    assert!(seen.iter().all(|&n| n > 0), "{seen:?}");
}

/// The same of x86-64 code (`exe-recons/x86-remap`).
#[test]
fn x86_versions_remapped() {
    let code = |pad| macho(0x0100_0007, &x86_functions(40_000, pad, 0x1_0000_0200));
    let (a, b) = (code(0), code(48));
    assert!(a.len() > 1 << 20);
    let opt = Options { block: 2 << 20, ..Options::default() };
    let (alone, r) = archived(&[("a", a.clone())], &opt);
    let alone = alone.len() - r.overhead;
    let (both, r) = archived(&[("a", a), ("b", b)], &opt);
    let names = |k: usize| -> Vec<&str> { r.files[k].modules.iter().map(|m| m.name).collect() };
    let mut all = [names(0), names(1)];
    all.sort();
    assert_eq!(all, [["exe-recons/x86"], ["exe-recons/x86-remap"]], "{:?}", r.files);
    let both = both.len() - r.overhead;
    assert!(both < alone + alone / 10, "{both} of {alone} alone");
}

/// Binary data of no format: a header, `n` records of 20 bytes (a counter from `seed`, a time
/// rising by about 1000, a byte of four values), and a trailer.
fn table(n: usize, seed: u32) -> Vec<u8> {
    let mut d = b"TABL".to_vec();
    d.extend((0..300).map(|i| (i * 131 % 251) as u8));
    let mut t = 1_700_000_000_000u64 + seed as u64;
    for i in 0..n {
        t += 900 + (i * 7919 % 200) as u64;
        d.extend((seed + i as u32).to_le_bytes());
        d.extend(t.to_le_bytes());
        d.extend([[1, 2, 7, 30][i * 31 % 4], 0, 0, 0, 0, 0, 0, 0]);
    }
    d.extend(b"END!");
    d
}

/// PE files, a table of records and a `.tar.lzma` of them with a member of its own, too large
/// for the files to be taken as holding the tar archive: still rebuilt, as the members' streams
/// are those of the files' formulas (not their bytes), stored once.
#[test]
fn lzma_of_files_mostly_held_once() {
    let base = pe(60_000, 7);
    let mut files: Vec<(String, Vec<u8>)> = (0..3)
        .map(|k| {
            let mut d = base.clone();
            d[0x1000 + 997 * k] ^= 0x55;
            (format!("b/p{k}.exe"), d)
        })
        .collect();
    files.push(("b/t.bin".into(), table(20_000, 1000)));
    let own = text(40_000, 3);
    let mut refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (&n[..], &d[..])).collect();
    refs.push(("b/own.txt", &own));
    let tlz = aqr_lzma::greedy::alone(&tar(&refs));
    let files: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (&n[..], d.clone())).collect();
    let opt = Options { block: 1 << 20, ..Options::default() };
    let (alone, r) = archived(&files, &opt);
    let alone = alone.len() - r.overhead;
    let all = [&files[..], &[("p/b.tar.lzma", tlz.clone())]].concat();
    let (c, r) = archived(&all, &opt);
    let names: Vec<&str> = r.files[4].modules.iter().map(|m| m.name).collect();
    assert!(names.starts_with(&["lzma-recons", "bundle-recons"]), "{:?}", r.files[4]);
    // (and the member of its own)
    let c = c.len() - r.overhead;
    assert!(c < alone + alone / 20, "{c} of {alone} alone");
}

/// A ZIP archive of a single entry `name` of `data`, its body `deflated`.
fn zip_of(name: &str, data: &[u8], deflated: &[u8]) -> Vec<u8> {
    let mut common = Vec::new();
    for v in [20u16, 0, 8, 0, 0] {
        common.extend_from_slice(&v.to_le_bytes());
    }
    for v in [aqr_common::crc32::hash(data), deflated.len() as u32, data.len() as u32] {
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

/// Files whose formulas' streams are larger than what is copied rather than waited for once
/// evaluated (made beforehand as files of the batch): archived, not waited for forever. (Of 70 MB each, whose raster
/// module needs 267 MiB: over the default bound.)
#[test]
fn large_formulas() {
    let pgm = |seed: u64| {
        let (w, h) = (8192, 8500);
        [format!("P5\n{w} {h}\n255\n").into_bytes(), aqr_common::rng::Rng::new(seed).bytes(w * h)].concat()
    };
    let (_, r) = archived(
        &[("a.pgm", pgm(1)), ("b.pgm", pgm(2))],
        &Options { smaller: false, application_bound: Some(512 << 20), ..Options::default() },
    );
    // (the fork of the first file last)
    assert!(r.files[..2].iter().all(|f| !f.modules.is_empty()), "{r:?}");
}

/// The archive of the sample stopped once `after` batches are written (`None`: before any), with
/// `opt`, into `out`: the report, and what the stop was told of.
fn stopped_after(
    after: Option<usize>,
    opt: &Options,
    out: &mut dyn aqr_compress::Output,
) -> (aqr_compress::Report, Vec<aqr_compress::Report>) {
    stopped_when(
        after.is_none(),
        &|e, written| matches!(e, aqr_compress::Event::Written { .. }) && Some(written) == after,
        opt,
        out,
    )
}

/// The archive of the sample stopped as `when` tells, of each event and the batches written
/// (or at once where `now`): as [`stopped_after`].
fn stopped_when(
    now: bool,
    when: &(dyn Fn(&aqr_compress::Event, usize) -> bool + Sync),
    opt: &Options,
    out: &mut dyn aqr_compress::Output,
) -> (aqr_compress::Report, Vec<aqr_compress::Report>) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let files = sample();
    let (tree, contents) = tree_of(&files);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let requested = AtomicBool::new(now);
    let written = AtomicUsize::new(0);
    let told = std::sync::Mutex::new(Vec::new());
    let ended = |r: &aqr_compress::Report| told.lock().unwrap().push(r.clone());
    let stop = aqr_compress::Stop { requested: &requested, ended: &ended };
    let tell = |e: aqr_compress::Event| {
        if let aqr_compress::Event::Written { .. } = e {
            written.fetch_add(1, Ordering::Relaxed);
        }
        if when(&e, written.load(Ordering::Relaxed)) {
            requested.store(true, Ordering::Relaxed);
        }
    };
    let r = aqr_compress::compress_tree_into(&tree, &inputs[..], opt, out, None, Some(&stop), &tell).unwrap();
    (r, told.into_inner().unwrap())
}

/// An archive stopped as it is written: ended as it is, of the files of the batches written (in
/// the segment being written too), telling what it left out, whether committed as it is written
/// or not (with mirror blocks); stopped before anything, nothing kept.
#[test]
fn stopped() {
    use aqr_formula::aqr::Status;
    let all = located(&archived(&sample(), &Options::default()).0).0;
    let opt = Options { batch: 30_000, segment: 1 << 20, ..Options::default() };
    for (mirrors, mut out) in [(false, Logged::default()), (true, Logged::default())] {
        let opt = Options { mirrors, ..opt.clone() };
        let (r, told) = if mirrors {
            let mut v = Vec::new();
            let got = stopped_after(Some(2), &opt, &mut aqr_compress::Plain(&mut v));
            out.bytes = v;
            got
        } else {
            stopped_after(Some(2), &opt, &mut out)
        };
        assert_eq!(told.len(), 1);
        let s = r.stopped.expect("stopped");
        assert!(s.kept && s.files > 0 && s.bytes > 0, "{s:?}");
        assert_eq!(r.len, out.bytes.len());
        let (names, status) = located(&out.bytes);
        assert!(!names.is_empty() && names.len() < all.len() && names.iter().all(|n| all.contains(n)), "{names:?}");
        assert_eq!(status, Some(Status::Incomplete { files: Some(s.files), bytes: Some(s.bytes) }));
        if mirrors {
            let check = aqr_formula::aqr::mirror::check(&out.bytes);
            assert!(check.walked && check.mirrored(&out.bytes, false), "{check:?}");
        }
    }
    // (while a batch is made, where the events come before the archive is done: of the batches
    // written before it)
    let mut out = Logged::default();
    let during = |e: &aqr_compress::Event, written| matches!(e, aqr_compress::Event::Started(_)) && written >= 3;
    let (r, told) = stopped_when(false, &during, &opt, &mut out);
    assert_eq!(told.len(), r.stopped.is_some() as usize);
    if let Some(s) = r.stopped {
        assert!(s.kept);
        let (names, status) = located(&out.bytes);
        assert!(names.len() < all.len() && matches!(status, Some(Status::Incomplete { .. })), "{names:?}");
    }
    // (while a batch is read: the stop not waiting for it)
    let files = sample();
    let (tree, contents) = tree_of(&files);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let requested = std::sync::atomic::AtomicBool::new(false);
    let slow = Slow { inputs: &inputs, at: inputs.len() / 2, requested: &requested };
    let at = std::sync::Mutex::new(None);
    let ended = |_: &aqr_compress::Report| *at.lock().unwrap() = Some(std::time::Instant::now());
    let stop = aqr_compress::Stop { requested: &requested, ended: &ended };
    let mut out = Logged::default();
    let r = aqr_compress::compress_tree_into(&tree, &slow, &opt, &mut out, None, Some(&stop), &|_| {}).unwrap();
    let ended = at.into_inner().unwrap().expect("ended");
    assert!(r.stopped.unwrap().kept);
    assert!(ended.elapsed() > std::time::Duration::from_millis(300), "ended without waiting for the read");
    let mut out = Logged::default();
    let (r, told) = stopped_after(None, &opt, &mut out);
    assert_eq!(told.len(), 1);
    assert!(!r.stopped.unwrap().kept);
}

/// Contents of which content `at` is read slowly, a stop requested as it is.
struct Slow<'a> {
    inputs: &'a [Input<'a>],
    at: usize,
    requested: &'a std::sync::atomic::AtomicBool,
}

impl aqr_compress::Source for Slow<'_> {
    fn len(&self) -> usize {
        self.inputs.len()
    }

    fn name(&self, c: usize) -> &[u8] {
        self.inputs[c].name
    }

    fn size(&self, c: usize) -> u64 {
        self.inputs[c].data.len() as u64
    }

    fn read(&self, c: usize) -> std::io::Result<aqr_formula::Bytes> {
        if c == self.at {
            self.requested.store(true, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Ok(self.inputs[c].data.to_vec().into())
    }
}

/// The files of `files` archived into `out` (appended to the archive it holds where `from`),
/// committed as it is written.
fn committed_into(files: &[(&str, Vec<u8>)], opt: &Options, out: &mut Vec<u8>, from: Option<&aqr_compress::Appending>) {
    let (tree, contents) = tree_of(files);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let r = aqr_compress::compress_tree_into(&tree, &inputs[..], opt, out, from, None, &|_| {}).unwrap();
    assert!(r.stopped.is_none());
}

/// Segments appended to an archive committed (spec, "Appending to an archive"): its files
/// kept, those of the segments added or replacing theirs, in a segment or several; not with
/// mirror blocks.
#[test]
fn appended() {
    use aqr_formula::aqr::{Status, footer_of};
    let all = sample();
    let (first, second) = all.split_at(5);
    for (segment, kill) in [(0, true), (1, true), (1, false)] {
        let opt = Options { batch: 30_000, segment, ..Options::default() };
        let mut out = Vec::new();
        committed_into(first, &opt, &mut out, None);
        let (f, m) = footer_of(&out).unwrap();
        if !kill {
            // (written over: cut there)
            out.truncate(f.start);
        }
        let from = aqr_compress::Appending { footer: f, metadata: m.unwrap(), link_groups: -10, kill };
        // the rest, and a file of the first again, changed
        let mut more = second.to_vec();
        more.push(("a/t0.txt", text(5_000, 9)));
        committed_into(&more, &opt, &mut out, Some(&from));
        assert_eq!(aqr_formula::aqr::locate(&out), Some(0..out.len()));
        let Ok(Value::Tree(t, got)) = decompress_value(&out, 1) else { panic!("not decoded") };
        let path = |mut k: usize| {
            let mut p = t.entries[k].name.clone();
            while let Some(q) = t.entries[k].parent {
                p = [&t.entries[q].name[..], b"/", &p].concat();
                k = q;
            }
            String::from_utf8(p).unwrap()
        };
        let mut have: Vec<(String, Vec<u8>)> = (0..t.entries.len())
            .filter_map(|k| match t.entries[k].node {
                EntryNode::File(c) => Some((path(k), got[c].clone())),
                _ => None,
            })
            .collect();
        have.sort();
        let mut want: Vec<(String, Vec<u8>)> =
            first.iter().chain(&more).map(|(p, d)| (p.to_string(), d.clone())).collect();
        want.sort();
        want.dedup_by(|b, a| {
            // (the later one of a path kept)
            if a.0 == b.0 {
                a.1 = b.1.clone();
                true
            } else {
                false
            }
        });
        // (the first's a/t0.txt replaced)
        let t0 = want.iter().position(|w| w.0 == "a/t0.txt").unwrap();
        want[t0].1 = text(5_000, 9);
        assert_eq!(have, want, "segment {segment}");
        let status = aqr_formula::aqr::open(&out, &aqr_formula::aqr::NoModules).unwrap().status();
        assert_eq!(status, Some(Status::Complete));
    }
    let opt = Options { mirrors: true, ..Options::default() };
    let mut out = Vec::new();
    committed_into(first, &Options::default(), &mut out, None);
    let (f, m) = footer_of(&out).unwrap();
    let from = aqr_compress::Appending { footer: f, metadata: m.unwrap(), link_groups: 0, kill: true };
    let (tree, contents) = tree_of(second);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    assert!(aqr_compress::compress_tree_into(&tree, &inputs[..], &opt, &mut out, Some(&from), None, &|_| {}).is_err());
}

/// Segments appended to an archive committed once (as `--atomic` has it): wherever it is
/// stopped before it ends, the archive as it was; then the segments with it.
#[test]
fn appended_once() {
    use aqr_formula::aqr::footer_of;
    let all = sample();
    let (first, second) = all.split_at(5);
    let opt = Options { batch: 30_000, segment: 1, ..Options::default() };
    let mut before = Vec::new();
    committed_into(first, &opt, &mut before, None);
    let (f, m) = footer_of(&before).unwrap();
    let from = aqr_compress::Appending { footer: f, metadata: m.unwrap(), link_groups: 0, kill: true };
    let (tree, contents) = tree_of(second);
    let inputs: Vec<Input> = contents.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let mut out = Logged { bytes: before.clone(), once: true, ..Logged::default() };
    aqr_compress::compress_tree_into(&tree, &inputs[..], &opt, &mut out, Some(&from), None, &|_| {}).unwrap();
    let (names, _) = located(&out.bytes);
    assert_eq!(names.len(), located(&before).0.len() + second.len());
    // (one commit: synced once before the old footer is killed, once after)
    assert_eq!(out.ops.iter().filter(|o| matches!(o, Op::Sync)).count(), 2);
    let kill = out.ops.iter().position(|o| matches!(o, Op::Patch(..))).unwrap();
    let mut file = out.bytes.clone();
    for op in out.ops[kill..].iter().rev() {
        if let Op::Patch(at, was) = op {
            file[*at..*at + was.len()].copy_from_slice(was);
        }
    }
    for op in out.ops[..kill].iter().rev() {
        if let Op::Append(n) = op {
            assert_eq!(located(&file).0, located(&before).0);
            file.truncate(file.len() - n);
        }
    }
    assert_eq!(file, before);
}

/// Below level 2, where nothing is recompressed: every file given back as it is, of no module,
/// the text files solid (but at level 0, each a block of its own but where they share), the same content once and a
/// file starting with another's content made of it; the same archive however many threads make
/// it (batches made at once), in segments and with mirror blocks.
#[test]
fn plain_levels() {
    let mut files = sample();
    let longer = [&files[1].1[..], &text(10_000, 5)].concat();
    files.push(("a/longer.txt", longer));
    for level in [aqr_formula::aqr::Level::Of(0), aqr_formula::aqr::Level::Of(1)] {
        let opt = Options { level, ..Options::default() };
        let (c, r) = archived(&files, &opt);
        assert!(r.files.iter().all(|f| f.modules.is_empty()), "{level:?}: {r:?}");
        assert_eq!(r.files[1].same_as, Some(3), "{level:?}");
        let texts = r.solid.iter().any(|s| [0, 2, 3].iter().all(|c| s.contents.contains(c)));
        assert_eq!(texts, level != aqr_formula::aqr::Level::STORED, "{level:?}: {:?}", r.solid);
        // (longer.txt of t1.txt's bytes and its own: 60 KB not stored again, even at level 0)
        let attributed: f64 = r.files.iter().map(|f| f.archived).sum();
        let total: usize = files.iter().map(|f| f.1.len()).sum();
        assert!(attributed < (total - 59_000) as f64, "{level:?}: {attributed} of {total}");
        assert!((attributed + r.overhead as f64 - c.len() as f64).abs() < 1.0, "{level:?}");
        let (threaded, _) = archived(&files, &Options { threads: 4, ..opt.clone() });
        assert!(threaded == c, "{level:?}: {} bytes on 4 threads, {} on one", threaded.len(), c.len());
        let opt = Options { batch: 30_000, segment: 1, ..opt };
        let (s, _) = archived(&files, &opt);
        assert!(metadata_blocks(&s) > 3, "{level:?}: {}", metadata_blocks(&s));
        let (threaded, _) = archived(&files, &Options { threads: 4, ..opt.clone() });
        assert!(threaded == s, "{level:?}: in segments, {} bytes on 4 threads, {} on one", threaded.len(), s.len());
        let (m, r) = archived(&files, &Options { mirrors: true, ..opt });
        assert_eq!(r.len, m.len());
        let check = aqr_formula::aqr::mirror::check(&m);
        assert!(check.walked && check.mirrored(&m, false), "{level:?}: {check:?}");
    }
}

/// A VPK directory file (version 1) of `files` (path, data, in the directory file or in
/// archive 0), and archive 0.
fn vpk(files: &[(&str, &[u8], bool)]) -> (Vec<u8>, Vec<u8>) {
    let (mut tree, mut here, mut archive) = (Vec::new(), Vec::<u8>::new(), Vec::<u8>::new());
    for &(name, b, in_dir) in files {
        let (path, ext) = name.rsplit_once('.').unwrap();
        let (dir, base) = path.rsplit_once('/').unwrap_or((" ", path));
        for s in [ext, dir, base] {
            tree.extend(s.as_bytes());
            tree.push(0);
        }
        let (index, at) = if in_dir { (0x7fffu16, here.len()) } else { (0, archive.len()) };
        tree.extend(aqr_common::crc32::hash(b).to_le_bytes());
        tree.extend([0, 0]);
        tree.extend(index.to_le_bytes());
        tree.extend((at as u32).to_le_bytes());
        tree.extend((b.len() as u32).to_le_bytes());
        tree.extend([0xff, 0xff, 0, 0]);
        if in_dir { here.extend(b) } else { archive.extend(b) }
    }
    tree.push(0);
    let mut d = vec![0x34, 0x12, 0xAA, 0x55, 1, 0, 0, 0];
    d.extend((tree.len() as u32).to_le_bytes());
    d.extend(tree);
    d.extend(here);
    (d, archive)
}

/// A BMP file of 24 bits of `w` x `h` (rows of a multiple of 4 bytes), of a gradient and
/// noise.
fn bmp(w: usize, h: usize) -> Vec<u8> {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let row = (3 * w).next_multiple_of(4);
    let mut d = b"BM".to_vec();
    d.extend(((54 + row * h) as u32).to_le_bytes());
    d.extend([0; 4]);
    d.extend(54u32.to_le_bytes());
    d.extend(40u32.to_le_bytes());
    d.extend((w as i32).to_le_bytes());
    d.extend((h as i32).to_le_bytes());
    d.extend(1u16.to_le_bytes());
    d.extend(24u16.to_le_bytes());
    d.extend([0; 24]);
    for y in 0..h {
        for x in 0..row {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            d.push(if x < 3 * w { ((x * 3 + y * 5) / 4 + (seed >> 61) as usize) as u8 } else { 0 });
        }
    }
    d
}

/// A VPK's archive (of no header, starting with a PNG file) taken apart by its directory file
/// archived along: its PNG files recompressed by aqr-png, its BMP file by aqr-raw (which a scan
/// of the archive alone does not find: only at the start of data); a Godot pack's alike (as
/// each of aqr-bundle's packs, by its own header).
#[test]
fn game_packs() {
    let (a, b, t) = (variant(64, 48, None), variant(80, 40, Some((3, 4, 20, 10))), bmp(120, 90));
    let (dir, archive) = vpk(&[("materials/a.png", &a, false), ("sky.bmp", &t, false), ("materials/b.png", &b, false)]);
    let mut pck = b"GDPC             ".to_vec();
    pck.extend([0; 64]);
    pck.extend(1u32.to_le_bytes());
    let path = b"res://a.png ";
    pck.extend((path.len() as u32).to_le_bytes());
    pck.extend(path);
    pck.extend(((pck.len() + 32) as u64).to_le_bytes());
    pck.extend((b.len() as u64).to_le_bytes());
    pck.extend([0x77; 16]);
    pck.extend(&b);
    let files = [("game/pak01_dir.vpk", dir), ("game/pak01_000.vpk", archive.clone()), ("game/game.pck", pck)];
    let (_, r) = archived(&files, &Options::default());
    let modules = |r: &aqr_compress::Report, c: usize| -> Vec<String> {
        r.files[c].modules.iter().map(|m| m.name.to_string()).collect()
    };
    let m = modules(&r, 1);
    assert!(m.iter().any(|m| m == "png-recons") && m.iter().any(|m| m == "raster-recons"), "{m:?}");
    assert!(modules(&r, 2).iter().any(|m| m == "png-recons"), "{:?}", modules(&r, 2));
    // (alone, of no directory file, the archive is not taken apart)
    let (_, r) = archived(&[("game/pak01_000.vpk", archive)], &Options::default());
    assert!(!modules(&r, 0).iter().any(|m| m == "raster-recons"), "{:?}", modules(&r, 0));
}

/// An Unreal 3 texture cache (`.tfc`) of LZO1X chunks: its blocks rebuilt by aqr-lzo, their
/// data a content.
#[test]
fn unreal_texture_cache() {
    // (liblzo2's streams of aqr-lzo's tests, their data's sizes by aqr-lzo)
    let streams: Vec<Vec<u8>> = ["mixed.1.lzo1x", "mixed.999-9.lzo1x", "mixed.1_12.lzo1x"]
        .iter()
        .map(|n| fixture(&format!("codecs/aqr-lzo/tests/data/{n}")))
        .collect();
    let raw = |s: &[u8]| aqr_lzo::recons::split_as(s, 1 << 24, None).unwrap().data.len();
    let mut tfc = Vec::new();
    // (a block a chunk: each chunk's blocks are of its block size of data, but the last)
    for c in 0..9 {
        let blocks: Vec<&Vec<u8>> = vec![&streams[c % 3]];
        let size: usize = blocks.iter().map(|s| s.len()).sum();
        let total: usize = blocks.iter().map(|s| raw(s)).sum();
        tfc.extend_from_slice(&[0xC1, 0x83, 0x2A, 0x9E]);
        for v in [0x20000, size, total] {
            tfc.extend_from_slice(&(v as u32).to_le_bytes());
        }
        for s in &blocks {
            tfc.extend_from_slice(&(s.len() as u32).to_le_bytes());
            tfc.extend_from_slice(&(raw(s) as u32).to_le_bytes());
        }
        blocks.iter().for_each(|s| tfc.extend_from_slice(s));
    }
    let (_, r) = archived(&[("CookedPC/Textures.tfc", tfc)], &Options::default());
    let m: Vec<&str> = r.files[0].modules.iter().map(|m| m.name).collect();
    assert!(m.contains(&"lzo-recons"), "{m:?}");
}

/// A Unity serialized file (format 17, of Unity 2019.4, no type trees) of one `Texture2D` whose
/// BC1 blocks (`n` bytes, of `w` x `h` and its mipmaps) stream from `path` at `offset`.
fn unity_assets(w: u32, h: u32, mips: u32, n: u32, offset: u32, path: &str) -> Vec<u8> {
    let mut o = Vec::new();
    let str4 = |o: &mut Vec<u8>, s: &[u8]| {
        o.extend((s.len() as u32).to_le_bytes());
        o.extend(s);
        o.resize(o.len().next_multiple_of(4), 0);
    };
    str4(&mut o, b"rock");
    o.extend([4, 0, 0, 0, 0, 0, 0, 0]);
    for v in [w, h, n, 10, mips] {
        o.extend(v.to_le_bytes());
    }
    o.extend([0; 4]);
    for v in [0u32, 1, 2, 1, 1, 0, 0, 0, 0, 0, 1, 0, offset, n] {
        o.extend(v.to_le_bytes());
    }
    str4(&mut o, path.as_bytes());
    // the metadata: the version, the platform, no trees, one type (Texture2D), one object
    let mut m = b"2019.4.1f1\0".to_vec();
    m.extend(19u32.to_le_bytes());
    m.push(0);
    m.extend(1u32.to_le_bytes());
    m.extend(28u32.to_le_bytes());
    m.push(0);
    m.extend(0xffffu16.to_le_bytes());
    m.extend([0xa5; 16]);
    m.extend(1u32.to_le_bytes());
    m.resize((20 + m.len()).next_multiple_of(4) - 20, 0);
    m.extend(1u64.to_le_bytes());
    for v in [0u32, o.len() as u32, 0] {
        m.extend(v.to_le_bytes());
    }
    m.extend([0; 8]);
    let data = (20 + m.len()).next_multiple_of(16);
    let mut d = Vec::new();
    for v in [m.len(), data + o.len(), 17, data] {
        d.extend((v as u32).to_be_bytes());
    }
    d.extend([0; 4]);
    d.extend(m);
    d.resize(data, 0);
    d.extend(o);
    d
}

/// A Unity resource file (`.resS`, of no header) taken apart by the serialized file beside it:
/// the BC1 blocks its texture streams from it rebuilt by aqr-tex's `bcn-recons`; alone, it is
/// not.
#[test]
fn unity_resource_file() {
    let l =
        aqr_tex::layout::Layout { format: aqr_tex::layout::Format::Bc1, width: 256, height: 256, mips: 9, layers: 1 };
    let mut x = 0x1234_5678u32;
    let blocks: Vec<u8> = (0..l.len())
        .map(|i| {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            if i % 8 < 4 { (i / 512) as u8 } else { (x >> 24) as u8 & 0x0f }
        })
        .collect();
    let mut ress = vec![0u8; 4096];
    ress.extend(&blocks);
    let assets = unity_assets(256, 256, 9, blocks.len() as u32, 4096, "sharedassets0.assets.resS");
    let files = [("Game_Data/sharedassets0.assets", assets), ("Game_Data/sharedassets0.assets.resS", ress.clone())];
    let (_, r) = archived(&files, &Options::default());
    let m: Vec<&str> = r.files[1].modules.iter().map(|m| m.name).collect();
    assert!(m.contains(&"bcn-recons"), "{m:?}");
    let (_, r) = archived(&files[1..], &Options::default());
    assert!(!r.files[0].modules.iter().any(|m| m.name == "bcn-recons"));
}
