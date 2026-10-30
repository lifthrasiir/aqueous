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

/// Files whose formulas' streams are larger than what is copied rather than waited for once
/// evaluated (made beforehand as files of the batch): archived, not waited for forever.
#[test]
fn large_formulas() {
    let pgm = |seed: u64| {
        let (w, h) = (8192, 8500);
        [format!("P5\n{w} {h}\n255\n").into_bytes(), aqr_common::rng::Rng::new(seed).bytes(w * h)].concat()
    };
    let (_, r) = archived(&[("a.pgm", pgm(1)), ("b.pgm", pgm(2))], &Options { smaller: false, ..Options::default() });
    assert!(r.files.iter().all(|f| !f.modules.is_empty()), "{r:?}");
}
