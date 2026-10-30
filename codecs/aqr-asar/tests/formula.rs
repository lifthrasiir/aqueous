use aqr_asar::formula::{self, Options};
use aqr_asar::layout;
use aqr_common::rng::{Rng, noise};
use aqr_formula::{Formula, Kind, Node};
use sha2::{Digest, Sha256};

fn eval(n: &Node, f: &Formula) -> Vec<u8> {
    match n {
        &Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| eval(n, f)).collect(),
        _ => panic!("{n:?}"),
    }
}

/// The module's inputs of formula `f`: the data, the header and the index.
fn inputs(f: &Formula) -> Vec<Vec<u8>> {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    a.inputs.iter().map(|n| eval(n, f)).collect()
}

/// Encodes `d`, checking that the module gives it back; the module's inputs.
fn roundtrip_with(d: &[u8], opt: &Options) -> (Formula, Vec<Vec<u8>>) {
    let f = formula::encode_with(d, opt).expect("encoded");
    let ins = inputs(&f);
    let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
    assert!(formula::decode(&[], &refs, d.len() as u64, 1).unwrap() == d, "not rebuilt");
    (f, ins)
}

fn roundtrip(d: &[u8]) -> Vec<Vec<u8>> {
    roundtrip_with(d, &Options::default()).1
}

fn holes(header: &[u8], what: u8) -> usize {
    header.windows(2).filter(|w| w == &[layout::HOLE, what]).count()
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The archive of JSON `json` and data `data`, its pickles as Chromium writes them.
fn archive(json: &str, data: &[u8]) -> Vec<u8> {
    let mut d = layout::prefix(json.len()).to_vec();
    d.extend_from_slice(json.as_bytes());
    d.resize(d.len() + layout::pad_to(json.len(), 4), 0);
    d.extend_from_slice(data);
    d
}

/// A file of [`pack`]: its path, contents, and whether it is left unpacked.
struct File<'a>(&'a str, Vec<u8>, bool);

/// `files` packed as @electron/asar does (in order, a directory where a path has one, the
/// integrity of blocks of `block` bytes where it is given), as an archive and its data.
fn pack(files: &[File], block: Option<usize>) -> Vec<u8> {
    #[derive(Default)]
    struct Dir(Vec<(String, Option<Dir>, String)>);
    fn write(d: &Dir, out: &mut String) {
        out.push_str("{\"files\":{");
        for (k, (name, sub, entry)) in d.0.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{name}\":"));
            match sub {
                Some(s) => write(s, out),
                None => out.push_str(entry),
            }
        }
        out.push_str("}}");
    }
    let (mut root, mut data) = (Dir::default(), Vec::new());
    for File(path, body, unpacked) in files {
        let mut entry = format!("{{\"size\":{}", body.len());
        match unpacked {
            true => entry.push_str(",\"unpacked\":true"),
            false => entry.push_str(&format!(",\"offset\":\"{}\"", data.len())),
        }
        if let Some(b) = block {
            let blocks: Vec<String> = body.chunks(b).map(|c| format!("\"{}\"", hex(&Sha256::digest(c)))).collect();
            entry.push_str(&format!(
                ",\"integrity\":{{\"algorithm\":\"SHA256\",\"hash\":\"{}\",\"blockSize\":{b},\"blocks\":[{}]}}",
                hex(&Sha256::digest(body)),
                blocks.join(",")
            ));
        }
        entry.push('}');
        if !unpacked {
            data.extend_from_slice(body);
        }
        let mut dir = &mut root;
        let mut parts: Vec<&str> = path.split('/').collect();
        let name = parts.pop().unwrap();
        for p in parts {
            let at = match dir.0.iter().position(|e| e.0 == p) {
                Some(at) => at,
                None => {
                    dir.0.push((p.into(), Some(Dir::default()), String::new()));
                    dir.0.len() - 1
                }
            };
            dir = dir.0[at].1.as_mut().unwrap();
        }
        dir.0.push((name.into(), None, entry));
    }
    let mut json = String::new();
    write(&root, &mut json);
    archive(&json, &data)
}

fn tree() -> Vec<File<'static>> {
    let lines: Vec<u8> = (0..3000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    vec![
        File("package.json", b"{\"name\":\"app\",\"main\":\"index.js\"}\n".to_vec(), false),
        File("index.js", b"require(\"./lib/a.js\");\n".to_vec(), false),
        File("lib/a.js", lines, false),
        File("lib/empty.txt", Vec::new(), false),
        File("lib/deep/er/x.txt", b"deep\n".to_vec(), false),
        File("\u{d55c}\u{ae00}/\u{d30c}\u{c77c}.txt", "안녕\n".into(), false),
        File("assets/esc \\\"q\\\".txt", b"quote\"back\\slash\n".to_vec(), false),
        File("assets/noise.bin", noise(20000), false),
        File("native.node", noise(5000), true),
    ]
}

/// An archive of @electron/asar: every packed file's size, offset and integrity derived, the
/// header with holes holding no hash of them.
#[test]
fn integrity() {
    let d = pack(&tree(), Some(4 << 20));
    let ins = roundtrip(&d);
    let h = &ins[1];
    assert_eq!(holes(h, layout::SIZE), 8);
    assert_eq!(holes(h, layout::OFFSET), 8);
    assert_eq!(holes(h, layout::HASH), 8);
    assert_eq!(holes(h, layout::BLOCKS), 8);
    // (the unpacked file's hashes are kept, the others' are not)
    let hashes = h.windows(9).filter(|w| w.starts_with(b"\"hash\":\"") && w[8] != layout::HOLE).count();
    assert_eq!(hashes, 1, "{}", String::from_utf8_lossy(h));
    assert_eq!(ins[0], d[d.len() - ins[0].len()..]);
}

/// Integrity of several blocks, the last short, and of blocks the size of the file.
#[test]
fn blocks() {
    for b in [1, 7, 1000, 2500, 2501] {
        let files = vec![File("a", noise(2500), false), File("b", Vec::new(), false), File("c", noise(1), false)];
        let ins = roundtrip(&pack(&files, Some(b)));
        assert_eq!(holes(&ins[1], layout::BLOCKS), 3, "{b}");
    }
}

/// The old `asar`'s archives, without integrity, and with a directory named as a number.
#[test]
fn no_integrity() {
    let ins = roundtrip(&pack(&tree(), None));
    assert_eq!(holes(&ins[1], layout::OFFSET), 8);
    assert_eq!(holes(&ins[1], layout::HASH), 0);
}

/// Data no file tells of (between files and after them), files out of order, and two files of
/// the same contents: all rebuilt, the data as it is.
#[test]
fn gaps_and_order() {
    let data = noise(3000);
    let json = r#"{"files":{"b":{"size":1000,"offset":"1500"},"a":{"size":1000,"offset":"200"},"c":{"size":1000,"offset":"200"},"z":{"size":0,"offset":"3000"}}}"#;
    let d = archive(json, &data);
    let ins = roundtrip(&d);
    assert_eq!(ins[0], data);
    assert_eq!(holes(&ins[1], layout::OFFSET), 4);
}

/// Files of the same contents (of at least 1 KiB) are stored once, the copies taking the first's
/// slices; shorter ones are not.
#[test]
fn copies() {
    let (a, b) = (noise(3000), noise(500));
    let files = vec![
        File("a", a.clone(), false),
        File("b", b.clone(), false),
        File("x/a", a.clone(), false),
        File("x/b", b.clone(), false),
        File("y/a", a, false),
    ];
    let (f, ins) = roundtrip_with(&pack(&files, Some(4 << 20)), &Options::default());
    assert_eq!(ins[0].len(), 3000 * 3 + 500 * 2);
    assert_eq!(f.streams[0].data.len(), 3000 + 500 * 2);
}

/// What does not follow from the data is kept as it is: values not written as they would be,
/// hashes that do not match, whitespace, the pickles' padding and sizes.
#[test]
fn kept() {
    let a = noise(100);
    let h = hex(&Sha256::digest(&a));
    let cases = [
        format!(r#"{{"files":{{"a":{{"size":100,"offset":"0","integrity":{{"algorithm":"SHA256","hash":"{}","blockSize":4194304,"blocks":["{h}"]}}}}}}}}"#, h.to_uppercase()),
        format!(r#"{{"files":{{"a":{{"size":100,"offset":"0","integrity":{{"algorithm":"SHA256","hash":"{h}","blockSize":4194304,"blocks":[ "{h}" ]}}}}}}}}"#),
        format!(r#"{{"files":{{"a":{{"size":100,"offset":"0","integrity":{{"hash":"{}","blocks":["{h}"]}}}}}}}}"#, hex(&Sha256::digest(b"x"))),
        r#"{"files":{"a":{"size":100,"offset":"00"}}}"#.into(),
        r#"{"files":{"a":{"offset":"0","size":100}}}"#.into(),
        r#"{"files": { "a" : {"size": 100, "offset": "0"} } }"#.into(),
        r#"{"files":{"a":{"size":100,"offset":"0","executable":true},"l":{"link":"a"},"u":{"size":5,"unpacked":true}}}"#.into(),
    ];
    for json in &cases {
        roundtrip(&archive(json, &a));
    }
    // (the pickles' padding not zeros, and more of it)
    let json = r#"{"files":{"a":{"size":100,"offset":"0"}}}"#;
    let mut d = archive(json, &[]);
    d.truncate(16 + json.len());
    d.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    let b = (d.len() - 8) as u32;
    d[4..8].copy_from_slice(&b.to_le_bytes());
    d.extend_from_slice(&a);
    roundtrip(&d);
}

/// Archives not taken: not asar, a header of no JSON or past the data, or no file in the data.
#[test]
fn not_taken() {
    let a = noise(100);
    assert!(formula::encode(&a, 1).is_none());
    assert!(formula::encode(&archive(r#"{"files":{"a":{"size":100,"offset":"0"}}"#, &a), 1).is_none());
    assert!(formula::encode(&archive(r#"{"files":{"a":{"size":101,"offset":"0"}}}"#, &a), 1).is_none());
    assert!(formula::encode(&archive(r#"{"files":{}}"#, &a), 1).is_none());
    assert!(formula::encode(&archive(r#"{"files":{"a":{"size":100.0,"offset":"0"}}}"#, &a), 1).is_none());
    let mut d = archive(r#"{"files":{"a":{"size":100,"offset":"0"}}}"#, &a);
    d[4] = 0xff;
    assert!(formula::encode(&d, 1).is_none());
}

/// The files' contents are slices by their kinds, those of at least `own` bytes their own.
#[test]
fn kinds() {
    let kind = |n: &[u8]| if n.ends_with(b".js") || n.ends_with(b".txt") { Kind::Text } else { Kind::Unknown };
    let d = pack(&tree(), Some(4 << 20));
    let (f, _) = roundtrip_with(&d, &Options { kind, ..Default::default() });
    let classes: Vec<&str> = f.streams.iter().map(|s| &s.class[..]).collect();
    assert_eq!(classes, ["raw", formula::HEADER, formula::INDEX, "asar/text"]);
    // package.json and the noise are raw, the text all the rest
    assert_eq!(f.streams[0].data.len(), 20000 + 33);
    let paths: Vec<Vec<u8>> = formula::files(&d).into_iter().map(|f| f.0).collect();
    assert_eq!(paths.len(), 8);
    assert_eq!(paths[2], b"/lib/a.js");
}

/// The module fails on inputs broken anywhere, rather than panic.
#[test]
fn broken() {
    let d = pack(&tree(), Some(1000));
    let ins = roundtrip(&d);
    let mut rng = Rng::new(7);
    for _ in 0..3000 {
        let mut ins = ins.clone();
        let k = rng.below(3) as usize;
        if ins[k].is_empty() {
            continue;
        }
        let at = rng.below(ins[k].len() as u64) as usize;
        match rng.below(3) {
            0 => ins[k].truncate(at),
            1 => ins[k][at] ^= 1 << rng.below(8),
            _ => ins[k][at] = [0, 1, 2, 3, 4, 0xff][rng.below(6) as usize],
        }
        let refs: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
        let _ = formula::decode(&[], &refs, d.len() as u64, 1);
    }
    assert!(formula::decode(&[1], &[&ins[0], &ins[1], &ins[2]], d.len() as u64, 1).is_err());
    assert!(formula::decode(&[], &[&ins[0], &ins[1]], d.len() as u64, 1).is_err());
    assert!(formula::decode(&[], &[&ins[0], &ins[1], &ins[2]], d.len() as u64 - 1, 1).is_err());
}
