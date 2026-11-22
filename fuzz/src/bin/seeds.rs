//! Writes the seed archives of the fuzz targets to the directories given (`corpus/decode`, say):
//! single files and trees, stored and compressed, of each instruction and of the toy modules.
//! Stored blocks keep their bytes in sight of the mutations; the checksums are skipped anyway.

use std::io::{Cursor, Write};

#[path = "../seedgen.rs"]
mod seedgen;
use std::path::Path;

use aqr_archive::{
    ContentAt, Encoder, Entry, EntryNode, Extra, Leaf, Level, SegmentWriter, Tree, checksum, encode, envelope, mirror,
};
use aqr_formula::{Apply, Formula, Kind, NO_LIMIT, Node, patch};
use aqr_fuzz::{COPY, CUT, REV};

fn noise(n: usize, seed: u32) -> Vec<u8> {
    (0..n as u32).map(|i| (i.wrapping_add(seed).wrapping_mul(2654435761) >> 24) as u8).collect()
}

fn cut(inputs: Vec<Node>) -> Apply {
    Apply {
        module: CUT,
        resources: vec![],
        params: vec![],
        inputs,
        len: 0,
        memory_limit: NO_LIMIT,
        task_limits: Vec::new(),
    }
}

/// Formulas of a single file: what each exercises.
fn formulas() -> Vec<(&'static str, Formula)> {
    let mut out = Vec::new();
    let text = b"hello, aqueous; hello, aqueous; hello, aqueous".to_vec();

    let mut f = Formula::default();
    f.root = f.push("a", text.clone());
    out.push(("literal", f));

    let mut f = Formula::default();
    let a = f.push("a", noise(300, 1));
    let b = f.push("b", text.clone());
    f.root = Node::concat([Node::slice(1, 7, 5), a, Node::slice(0, 10, 20), b]);
    out.push(("concat", f));

    let mut f = Formula::default();
    let a = f.push("a", text.clone());
    f.root = Node::Pack(vec![a, Node::slice(0, 0, 3), Node::slice(0, 3, 0)]);
    out.push(("pack", f));

    let mut f = Formula::default();
    let a = f.push("a", text.clone());
    let b = f.push("b", noise(100, 2));
    f.root = Node::apply(REV, vec![1, 2, 3], vec![a, b], text.len() as u64 + 100);
    out.push(("rev", f));

    let mut f = Formula::default();
    let a = f.push("a", noise(5000, 3));
    f.root = Node::apply(COPY, vec![], vec![a, Node::slice(0, 100, 50)], 5050);
    out.push(("copy-part", f));

    let mut f = Formula::default();
    let a = f.push("a", b"abcdefgh".to_vec());
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
    let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1, Node::slice(0, 0, 2)]), vec![1, 6])).unwrap();
    f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![], vec![p0], 1)]);
    out.push(("shared", f));

    let base = noise(3000, 4);
    let mut target = base[..2000].to_vec();
    target[100..150].fill(1);
    let mut f = Formula::default();
    let a = f.push("a", base.clone());
    let d = f.push_as("d", Kind::Compressed, patch::make(&base[..2000], &target, 3).unwrap());
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![2000, 1000])).unwrap();
    f.root = Node::concat([o1, Node::patch(o0, d, 2000)]);
    out.push(("patch", f));

    let mut f = Formula::default();
    let a = f.push("a", noise(64, 5));
    let parts = f.split_node(a, vec![10, 20, 34]);
    f.root = Node::concat(parts.into_iter().rev());
    out.push(("split", f));
    out
}

/// A tree of a directory, files (one of a module's output, two of one leaf), a link and a
/// fifo, in two batches; `hinted`, a size hint block first (as an encrypted archive's payload).
fn tree(level: Level, hinted: bool) -> Vec<u8> {
    let enc = Encoder::new(level);
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    let b = f.push("b", noise(700, 6));
    let mut g = Formula::default();
    let c = g.push("c", noise(500, 7));
    let [o0, o1] = <[Node; 2]>::try_from(g.share(cut(vec![c]), vec![200, 300])).unwrap();
    let mut w = SegmentWriter::with_size_hint(hinted);
    let mut out = w.head().to_vec();
    let mut contents = Vec::new();
    for (f, leaves) in [
        (
            &f,
            vec![
                Leaf { outputs: vec![Node::apply(REV, vec![], vec![a.clone(), b], 705)], sums: vec![None] },
                Leaf { outputs: vec![a], sums: vec![Some(checksum(b"hello"))] },
            ],
        ),
        (&g, vec![Leaf { outputs: vec![o1, o0], sums: vec![None, None] }]),
    ] {
        let sizes: Vec<Vec<u64>> = leaves.iter().map(|l| l.outputs.iter().map(Node::len).collect()).collect();
        let mut batch = w.batch(&enc, f, &leaves).unwrap();
        w.commit(&batch);
        let (d, r) = batch.take_blocks();
        out.extend_from_slice(&d[r]);
        for (&leaf, sizes) in batch.leaf_at.iter().zip(&sizes) {
            for (output, &size) in sizes.iter().enumerate() {
                contents.push(ContentAt { leaf, output, size });
            }
        }
    }
    let entry =
        |parent, name: &str, node| Entry { parent, name: name.as_bytes().to_vec(), node, extra: Extra::default() };
    let mut t = Tree::default();
    t.entries.push(entry(None, "d", EntryNode::Dir));
    for i in 0..contents.len() {
        t.entries.push(entry(Some(0), &format!("f{i}"), EntryNode::File(i)));
    }
    t.entries.push(entry(None, "l", EntryNode::Link(b"d/f0".to_vec())));
    t.entries.push(entry(None, "p", EntryNode::Fifo));
    out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
    out
}

/// The fixtures of the codecs' tests of at most `FIXTURE_MAX` bytes, by path, and the seeds made
/// of the formats of the codecs of none (`src/seedgen.rs`).
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let mut out = test_files();
    out.extend(seedgen::generated(FIXTURE_MAX));
    out.extend(local());
    out
}

/// Files of the codecs' local corpora (`corpus/`, not committed: where they are here) of
/// licences known by where they come from, for codecs of no test files: other projects' test
/// files (psd-tools', Pillow's and webtoon/psd's PSDs, but psd-tools' third parties'), F-Droid's
/// free apps' binary XML, Ubuntu's debugging information, public SQLite databases; of the many,
/// `MOST` spread over them. Of at most `FIXTURE_MAX` bytes, as the fixtures.
fn local() -> Vec<(String, Vec<u8>)> {
    const MOST: usize = 30;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../codecs");
    let dirs =
        ["aqr-psd/corpus/fixtures", "aqr-arsc/corpus/xml", "aqr-dwarf/corpus/ubuntu-dbg", "aqr-sqlite/corpus/public"];
    let mut out = Vec::new();
    for dir in dirs {
        let mut files = Vec::new();
        let mut todo = vec![root.join(dir)];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                match p.is_dir() {
                    true => todo.push(p),
                    false if !p.to_string_lossy().contains("third-party") => files.push(p),
                    false => {}
                }
            }
        }
        files.retain(|p| std::fs::metadata(p).is_ok_and(|m| m.len() as usize <= FIXTURE_MAX));
        files.sort();
        let step = files.len().div_ceil(MOST).max(1);
        let codec = dir.split('/').next().unwrap();
        let picked = if dir.contains("psd") || dir.contains("sqlite") { 1 } else { step };
        for (k, f) in files.iter().step_by(picked).enumerate() {
            let ext = f.extension().map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
            out.push((format!("local-{codec}-{k}{ext}"), std::fs::read(f).unwrap()));
        }
    }
    out
}

/// The fixtures of the codecs' tests of at most `FIXTURE_MAX` bytes, by path.
fn test_files() -> Vec<(String, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../codecs");
    let mut out = Vec::new();
    let mut crates: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.path()).collect();
    crates.sort();
    for c in crates {
        let Ok(files) = std::fs::read_dir(c.join("tests/data")) else { continue };
        let mut files: Vec<_> = files.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for f in files {
            let d = std::fs::read(&f).unwrap();
            if d.len() <= FIXTURE_MAX {
                let name = format!(
                    "{}-{}",
                    c.file_name().unwrap().to_string_lossy(),
                    f.file_name().unwrap().to_string_lossy()
                );
                out.push((name, d));
            }
        }
    }
    out
}

/// The largest fixture taken as a seed: libFuzzer's inputs are as long as the longest seed.
const FIXTURE_MAX: usize = 256 << 10;

/// The largest archive of stored blocks taken as a seed of natives (run.sh's `-max_len`, which
/// would cut a longer one).
const ARCHIVE_MAX: usize = 512 << 10;

/// Payload `a` (its size hint to be filled in) sealed by the fuzzed passphrase, `mirrored` or not.
fn sealed(a: &[u8], mirrored: bool) -> Vec<u8> {
    let mut s =
        envelope::Sealer::new(Cursor::new(Vec::new()), aqr_fuzz::PASSPHRASE, aqr_fuzz::WORK_FACTOR, mirrored).unwrap();
    s.write_all(a).unwrap();
    s.into_inner().unwrap().into_inner()
}

/// The seeds of `envelope`: the single files of [`formulas`] and the trees as payloads, sealed
/// with mirrored headers and without, a tree's blocks mirrored too; a file of several chunks, and
/// it cut short after a chunk (its last authenticating only as not the last) and of its header
/// magic damaged (its header taken from a mirrored header).
fn envelopes() -> Vec<(String, Vec<u8>)> {
    let mut payloads = Vec::new();
    for (name, f) in formulas() {
        for (l, level) in [("stored", Level::STORED), ("zstd", Level::Zstd(3))] {
            payloads.push((format!("{name}-{l}"), Encoder::new(level).with_size_hint(true).encode(&f, None).unwrap()));
        }
    }
    payloads.push(("tree-stored".into(), tree(Level::STORED, true)));
    payloads.push(("tree-zstd".into(), tree(Level::Zstd(3), true)));
    let mut big = Formula::default();
    big.root = big.push("a", noise(200 << 10, 8));
    let big = Encoder::new(Level::STORED).with_size_hint(true).encode(&big, None).unwrap();
    let mirrored = |a: &[u8]| {
        let mut m = mirror::Mirroring::new(Vec::new(), true);
        m.write_all(a).unwrap();
        m.into_inner()
    };
    payloads.push(("tree-mirrored".into(), mirrored(&tree(Level::STORED, true))));
    payloads.push(("big".into(), big));
    let mut seeds = Vec::new();
    for (name, a) in &payloads {
        seeds.push((format!("{name}-sealed"), sealed(a, false)));
        seeds.push((format!("{name}-sealed-mirrored"), sealed(a, true)));
    }
    let big = sealed(&payloads.last().unwrap().1, true);
    let mut damaged = big.clone();
    damaged[..8].fill(0);
    seeds.push(("big-header-damaged".into(), damaged));
    // (after chunk 1: the header of 113 bytes, two chunks sealed of 64 KiB and 16 bytes, and the
    // mirrored header of 117 bytes before chunk 1)
    let mut cut = big;
    cut.truncate(113 + 2 * ((64 << 10) + 16) + 117);
    seeds.push(("big-cut".into(), cut));
    let runner = aqr_fuzz::toys();
    for (name, c) in &seeds[..seeds.len() - 2] {
        let p = envelope::open(c).and_then(|e| e.unlock(aqr_fuzz::PASSPHRASE)).and_then(|u| u.decrypt(1));
        if let Err(e) = p.and_then(|p| aqr_archive::open_payload(&p, &runner)?.value(1)) {
            panic!("seed {name} does not decode: {e}");
        }
    }
    let [(_, damaged), (_, cut)] = &seeds[seeds.len() - 2..] else { unreachable!() };
    let (_, opened) = envelope::open_damaged(damaged).unwrap();
    assert_eq!(opened.header_from, Some(1), "the damaged header taken from mirrored header 1");
    let u = envelope::open(cut).unwrap().unlock(aqr_fuzz::PASSPHRASE).unwrap();
    assert_eq!(u.decrypt_lossy(&mut vec![0; u.len()], 1), (vec![], true), "cut short after chunk 1");
    seeds
}

/// The seeds of `unpack`: aqr-unpack's test archives and the codecs' archives of the formats
/// it reads (of at most `FIXTURE_MAX` bytes), each after the byte of its format (by its name).
fn unpackable() -> Vec<(String, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let dirs =
        ["crates/aqr-unpack", "codecs/aqr-zip", "codecs/aqr-7z", "codecs/aqr-rar", "codecs/aqr-alz", "codecs/aqr-egg"];
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(files) = std::fs::read_dir(root.join(dir).join("tests/data")) else { continue };
        let mut files: Vec<_> = files.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            let Some((format, _)) = aqr_unpack::by_name(name.as_bytes()) else { continue };
            let d = std::fs::read(&f).unwrap();
            if d.len() <= FIXTURE_MAX {
                let label = format!("{}-{name}", Path::new(dir).file_name().unwrap().to_string_lossy());
                out.push((label, [&[aqr_fuzz::unpack::format_byte(format)][..], &d].concat()));
            }
        }
    }
    out
}

/// The seeds of `modules`: the modules aqr-modules builds of at most 128 KiB, as the bytes of a
/// module (after the byte choosing so and 64 of the application's), and bytes of noise for
/// wasm-smith to make modules of, of each kind of entry point.
fn modules() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/aqr-modules/wasm");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    files.retain(|p| p.extension().is_some_and(|e| e == "wasm"));
    files.sort();
    let head = noise(64, 11);
    let mut out: Vec<(String, Vec<u8>)> = files
        .iter()
        .filter_map(|f| {
            let d = std::fs::read(f).unwrap();
            (d.len() <= 128 << 10)
                .then(|| (f.file_name().unwrap().to_string_lossy().to_string(), [&[0][..], &head, &d].concat()))
        })
        .collect();
    for e in 1..=3u8 {
        for k in 0..4 {
            out.push((format!("made-{e}-{k}"), [&[e][..], &head, &noise(4096, 100 * e as u32 + k)].concat()));
        }
    }
    out
}

/// The seeds of target `t`.
fn seeds(t: &str) -> Vec<(String, Vec<u8>)> {
    match t {
        "decode" | "salvage" => {
            let runner = aqr_fuzz::toys();
            let mut seeds =
                vec![("quine".to_string(), include_bytes!("../../../crates/aqr-archive/testdata/quine.aqr").to_vec())];
            for (name, f) in formulas() {
                for (l, level) in [("stored", Level::STORED), ("zstd", Level::Zstd(3))] {
                    let c = encode(&f, None, level).unwrap();
                    let want = aqr_archive::decode(&c, &runner).unwrap_or_else(|e| panic!("seed {name}: {e}"));
                    seeds.push((format!("{name}-{l}"), c));
                    if !matches!(f.root, Node::Slice { .. }) {
                        seeds.push((format!("{name}-{l}-sum"), encode(&f, Some(checksum(&want)), level).unwrap()));
                    }
                }
            }
            seeds.push(("tree-stored".into(), tree(Level::STORED, false)));
            seeds.push(("tree-zstd".into(), tree(Level::Zstd(3), false)));
            for (name, c) in &seeds {
                if let Err(e) = aqr_archive::decode_value(c, &runner) {
                    panic!("seed {name} does not decode: {e}");
                }
            }
            seeds
        }
        "envelope" => envelopes(),
        "unpack" => unpackable(),
        "modules" => modules(),
        "sfx" => aqr_fuzz::sfx::seeds(),
        // (bytes to make trees of, of each format)
        "pack" => (0..32)
            .map(|i| (format!("noise-{i}"), [&[i as u8][..], &noise(2048 << (i % 4), i as u32)].concat()))
            .collect(),
        // (bytes that `src/formula.rs` makes formulas of)
        "roundtrip" => (0..16).map(|i| (format!("noise-{i}"), noise(64 << (i % 8), i as u32))).collect(),
        "compress" => fixtures(),
        // (the fixtures recompressed, their archives made again of stored blocks: those whose
        // formula applies a module)
        "natives" | "wasm" => {
            let enc = Encoder::new(Level::STORED).with_modules(aqr_fuzz::registered);
            fixtures()
                .into_iter()
                .filter_map(|(name, d)| {
                    let mut f = aqr_compress::recompress(&d, &aqr_fuzz::options());
                    if f.applies().is_empty() {
                        return None;
                    }
                    aqr_compress::declare_memory(&mut f);
                    let c = enc.encode(&f, Some(checksum(&d))).unwrap_or_else(|e| panic!("{name}: {e}"));
                    // (a formula not verified that does not give its file back is left out)
                    if c.len() > ARCHIVE_MAX {
                        return None;
                    }
                    match aqr_archive::decode(&c, &aqr_fuzz::Known) {
                        Ok(back) if back == d => Some((format!("{name}.aqr"), c)),
                        r => {
                            eprintln!("{name}: left out, {:?}", r.map(|b| b.len()));
                            None
                        }
                    }
                })
                .collect()
        }
        _ => panic!("no target {t}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // (`seeds report`: of each seed made, the modules its recompression applies)
    if args.first().map(String::as_str) == Some("report") {
        for (name, d) in seedgen::generated(FIXTURE_MAX) {
            let f = aqr_compress::recompress(&d, &aqr_fuzz::options());
            let mut ms: Vec<String> = f.applies().iter().map(|a| a.module.name.to_string()).collect();
            ms.sort();
            ms.dedup();
            println!("{name} {} {}", d.len(), ms.join(","));
        }
        return;
    }
    let [t, dir] = &args[..] else { panic!("usage: seeds TARGET DIR") };
    let seeds = seeds(t);
    std::fs::create_dir_all(dir).unwrap();
    for (name, c) in &seeds {
        let name = if name.ends_with(".aqr") || ["compress", "unpack", "modules", "pack", "sfx"].contains(&t.as_str()) {
            name.clone()
        } else {
            format!("{name}.aqr")
        };
        std::fs::write(Path::new(dir).join(name), c).unwrap();
    }
    eprintln!("{} seeds of {t}", seeds.len());
}
