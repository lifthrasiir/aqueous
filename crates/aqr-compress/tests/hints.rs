//! Hints by attributes (docs/hints-design.md): the files of a tree made as their attributes tell,
//! a format told that turns a file down leaving it to detection, and the archive giving the files
//! back whatever the hints.

use std::io::Write;
use std::sync::Arc;

use aqr_archive::{Entry, EntryNode, Extra, Tree, Value};
use aqr_compress::attributes::{Attributes, Origin};
use aqr_compress::{Hints, Options, Report, Source, kind_by_name};
use aqr_decompress::decompress_value;
use aqr_formula::{Bytes, Kind};

/// Files at the root of a tree, of the kinds `hints` gives them.
struct Hinted {
    files: Vec<(String, Vec<u8>)>,
    hints: Option<Arc<Hints>>,
}

impl Source for Hinted {
    fn len(&self) -> usize {
        self.files.len()
    }
    fn name(&self, c: usize) -> &[u8] {
        self.files[c].0.as_bytes()
    }
    fn kind(&self, c: usize) -> Kind {
        let by_name = kind_by_name(self.name(c));
        match &self.hints {
            Some(h) => h.kind(self.name(c), by_name),
            None => by_name,
        }
    }
    fn size(&self, c: usize) -> u64 {
        self.files[c].1.len() as u64
    }
    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        Ok(self.files[c].1.clone().into())
    }
}

pub fn hints_of(origin: Origin, text: &str) -> Arc<Hints> {
    let mut a = Attributes::new();
    let w = a.add(origin, b"", text, "attributes", None, true);
    assert!(w.is_empty(), "{w:?}");
    Hints::new(a)
}

/// The archive of `files` with `hints`, checked to give them back, and its report.
pub fn archived(files: &[(&str, Vec<u8>)], hints: Option<Arc<Hints>>) -> (Vec<u8>, Report) {
    let entries = (0..files.len())
        .map(|c| Entry { parent: None, name: files[c].0.into(), node: EntryNode::File(c), extra: Extra::default() })
        .collect();
    let tree = Tree { root: Extra::default(), entries };
    let src = Hinted { files: files.iter().map(|(n, d)| (n.to_string(), d.clone())).collect(), hints };
    let mut out = Vec::new();
    let r = aqr_compress::compress_tree_to(&tree, &src, &Options::default(), &mut out, &|_| {}).unwrap();
    let Ok(Value::Tree(_, got)) = decompress_value(&out, 1) else { panic!("not decoded") };
    for (c, (n, d)) in files.iter().enumerate() {
        assert!(got[c] == *d, "{n} not given back");
    }
    (out, r)
}

fn modules(r: &Report, c: usize) -> Vec<String> {
    r.files[c].modules.iter().map(|m| m.to_string()).collect()
}

pub fn gzip(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().filename("a").write(Vec::new(), flate2::Compression::new(1));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

pub fn words(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let words = ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dogs", "and", "cats"];
    let mut data = Vec::new();
    while data.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        write!(data, "{} {} ", words[(x >> 60) as usize % 10], (x >> 40) % 1000).unwrap();
    }
    data
}

#[test]
fn tree_files() {
    // (named a PNG file: as a PNG file alone where named so, detected where a hint told it)
    let names = ["plain.gz", "off.gz", "not.gz", "compressed.dat", "wrong.png", "told.png"];
    let files: Vec<(&str, Vec<u8>)> =
        names.iter().enumerate().map(|(i, &n)| (n, gzip(&words(100_000, i as u64 + 1)))).collect();
    let hints = hints_of(
        Origin::Aqr,
        "off.gz -aqr-format\n\
         not.gz aqr-not=gzip,png\n\
         *.dat aqr-kind=compressed\n\
         told.png aqr-format=png\n\
         nothing.* aqr-kind=text\n",
    );
    let (_, plain) = archived(&files, None);
    let (_, r) = archived(&files, Some(hints.clone()));
    let flate = modules(&plain, 0);
    assert!(!flate.is_empty());
    assert_eq!(modules(&r, 0), flate, "no hint");
    assert!(modules(&r, 1).is_empty(), "-aqr-format");
    assert!(modules(&r, 2).is_empty(), "aqr-not");
    assert!(modules(&r, 3).is_empty(), "aqr-kind=compressed");
    assert!(modules(&plain, 4).is_empty(), "named a PNG file");
    assert_eq!(modules(&r, 5), flate, "detected once its format told turned it down");
    assert_eq!(hints.report(), ["told.png: aqr-format=png turned it down", "attributes:5: nothing.*: matched no file"]);
}

/// Lines of a log, which aqr-log takes.
fn log(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut data = Vec::new();
    for i in 0..n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let (h, m, s) = (i / 3600 % 24, i / 60 % 60, i % 60);
        writeln!(data, "2026-11-24 {h:02}:{m:02}:{s:02} INFO request {} took {} ms", x >> 44, (x >> 20) % 900).unwrap();
    }
    data
}

/// `.gitattributes`' `binary` (`-text`): no format of text looked for, though its bytes tell text;
/// its `aqr-*` taken as .aqr/attributes's are.
#[test]
fn git_attributes() {
    let files = [("a.log", log(20_000, 1)), ("b.dat", log(20_000, 2)), ("c.dat", log(20_000, 3))];
    let hints = hints_of(
        Origin::Git,
        "b.dat binary
c.dat aqr-kind=compressed
",
    );
    let (_, plain) = archived(&files, None);
    let (_, r) = archived(&files, Some(hints));
    assert!(!modules(&plain, 1).is_empty() && !modules(&plain, 2).is_empty(), "{plain:?}");
    assert!(!modules(&r, 0).is_empty());
    assert!(modules(&r, 1).is_empty(), "binary");
    assert!(modules(&r, 2).is_empty(), "aqr-kind");
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

fn tar(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut d = Vec::new();
    for (name, body) in files {
        d.extend(tar_header(name, body.len()));
        d.extend_from_slice(body);
        d.resize(d.len().next_multiple_of(512), 0);
    }
    d.resize(d.len() + 10240 - d.len() % 10240, 0);
    d
}

/// The modules of content `c`, by name, each once.
fn module_names(r: &Report, c: usize) -> Vec<String> {
    let mut m: Vec<String> = r.files[c].modules.iter().map(|m| m.name.to_string()).collect();
    m.sort();
    m.dedup();
    m
}

/// `>` into containers: a tar archive's members, a gzip file's one content, the members of a
/// tar archive in a gzip file; patterns of no `>` not matching members.
#[test]
fn containers() {
    let members =
        |seed: u64| tar(&[("in/a.gz", gzip(&words(60_000, seed))), ("b.gz", gzip(&words(60_000, seed + 10)))]);
    let files = [
        ("plain.tar", members(1)),
        ("told.tar", members(2)),
        ("whole.tar.gz", gzip(&tar(&[("a", words(600_000, 3)), ("b", words(600_000, 13))]))),
        ("deep.tar.gz", gzip(&members(4))),
    ];
    let hints = hints_of(
        Origin::Aqr,
        "told.tar>in/*.gz -aqr-format\n\
         told.tar>b.gz -aqr-format\n\
         whole.tar.gz> -aqr-format\n\
         deep.tar.gz>>*.gz -aqr-format\n\
         a.gz -aqr-format\n",
    );
    let (_, plain) = archived(&files, None);
    let (_, r) = archived(&files, Some(hints.clone()));
    let flate = module_names(&plain, 0);
    assert!(flate.len() == 2, "a tar archive's module and DEFLATE's: {flate:?}");
    let tar_only: Vec<String> = flate.iter().filter(|m| !m.contains("flate")).cloned().collect();
    // (`a.gz` matching no member: no `>`)
    assert_eq!(module_names(&r, 0), flate);
    assert_eq!(module_names(&r, 1), tar_only, "members told");
    // (the gzip file's DEFLATE stream rebuilt, what it holds not)
    let gz_only: Vec<String> = flate.iter().filter(|m| m.contains("flate")).cloned().collect();
    assert!(module_names(&plain, 2).contains(&tar_only[0]));
    assert_eq!(module_names(&r, 2), gz_only, "the one content told");
    // (the outer DEFLATE stream and the tar archive's module, not the members')
    assert_eq!(module_names(&r, 3), flate);
    // (the members kept as they are: as compressed as gzip made them)
    let gz_members = gzip(&words(60_000, 4)).len() + gzip(&words(60_000, 14)).len();
    assert!(r.files[3].archived > 1.2 * plain.files[3].archived, "{} {}", r.files[3].archived, plain.files[3].archived);
    assert!(r.files[3].archived >= 0.9 * gz_members as f64);
    assert_eq!(hints.report(), ["attributes:5: a.gz: matched no file"]);
}

/// F16 weights of no header, `n` of them: most zeros, the rest small values of a few scales.
fn f16_weights(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(2 * n);
    for i in 0..n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let v: u16 = match (x >> 60) % 4 {
            0 | 1 => 0,
            // (sign, an exponent of a few around 2^-6, the mantissa's top bits)
            _ => ((x >> 40) as u16 & 0x8000) | ((9 + (i / 4096) % 3) as u16) << 10 | (x >> 20) as u16 & 0x3ff,
        };
        d.extend_from_slice(&v.to_le_bytes());
    }
    d
}

fn module_of(r: &Report, c: usize, name: &str) -> bool {
    r.files[c].modules.iter().any(|m| m.name == name)
}

/// `aqr-format=tensors`: data of no header as one tensor of `aqr-dtype`, or the tensors of the
/// file `aqr-layout` names (relative to the attributes' directory); a value not known leaves the
/// file to detection, and is told.
#[test]
fn tensors_told() {
    let dir = std::env::temp_dir().join(format!("aqr-hints-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut layout = String::from("dtype = \"f16\"\n");
    for (k, n) in [4096usize, 65536, 30000].iter().enumerate() {
        let shape = if k == 1 { format!("[256, {}]", n / 256) } else { format!("[{n}]") };
        layout += &format!("\n[[tensor]]\nname = \"t{k}\"\nshape = {shape}\n");
    }
    std::fs::write(dir.join("layout.toml"), layout).unwrap();
    let files = [
        ("one.bin", f16_weights(100_000, 1)),
        ("laid.bin", f16_weights(99_632, 2)),
        ("bad.bin", f16_weights(100_000, 3)),
        ("plain.bin", f16_weights(100_000, 4)),
    ];
    let mut a = Attributes::new();
    let text = "one.bin aqr-format=tensors aqr-dtype=f16\n\
                laid.bin aqr-format=tensors aqr-layout=layout.toml\n\
                bad.bin aqr-format=tensors aqr-dtype=f12\n";
    assert!(a.add(Origin::Aqr, b"", text, "attributes", Some(dir.clone()), true).is_empty());
    let hints = Hints::new(a);
    let (_, r) = archived(&files, Some(hints.clone()));
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(module_of(&r, 0, "tensors-recons"), "{:?}", r.files[0]);
    assert!(module_of(&r, 1, "tensors-recons"), "{:?}", r.files[1]);
    assert!(!module_of(&r, 2, "tensors-recons") && !module_of(&r, 3, "tensors-recons"));
    assert!(r.files[0].archived < 0.95 * r.files[3].archived, "{} {}", r.files[0].archived, r.files[3].archived);
    assert_eq!(
        hints.report(),
        ["bad.bin: aqr-dtype=f12: not a type of tensors", "bad.bin: aqr-format=tensors turned it down"]
    );
}

/// `aqr-offset`: a raw DEFLATE stream (`aqr-format=deflate`, never detected) after bytes of a
/// header of its own, the header and what follows as they are; an offset past the end told.
#[test]
fn deflate_at_offset() {
    let file = |seed: u8| {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(1));
        e.write_all(&words(300_000, seed as u64)).unwrap();
        [&[seed; 100][..], &e.finish().unwrap(), b"trailer"].concat()
    };
    let files = [("at.dat", file(1)), ("hex.dat", file(2)), ("none.dat", file(3)), ("far.dat", file(4))];
    let hints = hints_of(
        Origin::Aqr,
        "at.dat aqr-format=deflate aqr-offset=100\n\
         hex.dat aqr-format=deflate aqr-offset=0x64\n\
         far.dat aqr-format=deflate aqr-offset=99999999\n",
    );
    let (_, plain) = archived(&files, None);
    let (_, r) = archived(&files, Some(hints.clone()));
    assert!(module_names(&plain, 0).is_empty(), "{:?}", plain.files[0]);
    assert!(module_names(&r, 0).contains(&"flate-set".to_string()), "{:?}", r.files[0]);
    assert!(module_names(&r, 1).contains(&"flate-set".to_string()));
    assert!(module_names(&r, 2).is_empty() && module_names(&r, 3).is_empty());
    assert!(r.files[0].archived < 0.8 * plain.files[0].archived, "{} {}", r.files[0].archived, plain.files[0].archived);
    assert_eq!(
        hints.report(),
        ["far.dat: aqr-offset=99999999: not an offset within the file", "far.dat: aqr-format=deflate turned it down"]
    );
}

/// `aqr-format=raw` with a layout: PCM samples of no header (16-bit stereo here, noisy tones).
#[test]
fn pcm_told() {
    let pcm = |phase: f64| -> Vec<u8> {
        let mut d = Vec::new();
        let mut x = phase.to_bits();
        for i in 0..200_000 {
            let t = i as f64 / 44100.0;
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let noise = ((x >> 40) % 400) as f64 - 200.0;
            let l = (8000.0 * (t * 440.0 * std::f64::consts::TAU + phase).sin() + noise) as i16;
            let r = (6000.0 * (t * 663.0 * std::f64::consts::TAU).sin() + noise / 2.0) as i16;
            d.extend_from_slice(&l.to_le_bytes());
            d.extend_from_slice(&r.to_le_bytes());
        }
        d
    };
    let mut x = 5u64;
    let noise: Vec<u8> = (0..400_000)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect();
    let files = [("a.pcm", pcm(0.0)), ("b.pcm", pcm(1.0)), ("c.pcm", pcm(2.0)), ("d.pcm", noise)];
    let hints = hints_of(
        Origin::Aqr,
        "a.pcm aqr-format=raw aqr-bits=16 aqr-channels=2\n\
         c.pcm aqr-format=raw aqr-bits=12\n\
         d.pcm aqr-format=raw aqr-bits=16\n",
    );
    let (_, r) = archived(&files, Some(hints.clone()));
    assert_eq!(module_names(&r, 0), ["pcm-recons"], "{:?}", r.files[0]);
    assert!(!module_names(&r, 1).contains(&"pcm-recons".to_string()));
    assert!(r.files[0].archived < 0.9 * r.files[1].archived, "{} {}", r.files[0].archived, r.files[1].archived);
    assert_eq!(
        hints.report(),
        [
            "c.pcm: aqr-bits=12: not 8, 16, 24 or 32",
            "c.pcm: aqr-format=pcm turned it down",
            "d.pcm: aqr-format=pcm made it about as small as it is"
        ]
    );
}
