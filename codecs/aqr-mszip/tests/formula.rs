//! CAB files of `scripts/gen-fixtures.py` (MSZIP folders by Python's zlib, stored folders,
//! reserved fields) and of aqr-lzx's CAB stream fixture taken apart and given back.

use aqr_decompress::Native;
use aqr_formula::{Formula, Node};
use aqr_mszip::{cab, formula};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.cab", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&Native::default()).unwrap()
}

/// The bytes of the folders of `d` (their data).
fn folder_data(d: &[u8]) -> usize {
    cab::parse(d).unwrap().folders.iter().map(|f| f.size()).sum()
}

#[test]
fn taken_apart_and_given_back() {
    for name in ["mszip", "mixed", "fixed", "small", "lzx"] {
        let d = if name == "lzx" { lzx_cab().0 } else { fixture(name) };
        let f = formula::encode(&d, 2).expect(name);
        assert!(eval(&f) == d, "{name}");
        // the files' contents are all there, apart from the rest
        let contents: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
        assert_eq!(contents, folder_data(&d), "{name}");
        // what is kept: the header, folders and files, not the folders' spans
        let c = cab::parse(&d).unwrap();
        let spans: usize = c.folders.iter().map(|f| f.span().unwrap().len()).sum();
        assert_eq!(f.streams[0].data.len(), d.len() - spans, "{name}");
    }
}

/// Folders of several spans: one application, split into them, in place among the bytes kept
/// (none after the last).
#[test]
fn folders_in_place() {
    let d = fixture("mixed");
    let f = formula::encode(&d, 1).unwrap();
    let Node::Concat(ns) = &f.root else { panic!("not a concatenation") };
    let kinds: Vec<&str> = ns
        .iter()
        .map(|n| match n {
            Node::Output { .. } => "folder",
            Node::Slice { stream: 0, .. } => "kept",
            n => panic!("{n:?}"),
        })
        .collect();
    assert_eq!(kinds, ["kept", "folder", "folder", "folder"]);
    assert_eq!(f.shared.len(), 1);
}

/// Contents by the kinds of their names, as `Options::kind` tells them.
#[test]
fn contents_by_kind() {
    let d = fixture("mszip");
    let kind = |n: &[u8]| if n.ends_with(b".txt") { aqr_formula::Kind::Text } else { aqr_formula::Kind::Unknown };
    let e = formula::encode_with(&d, &formula::Options { kind, ..Default::default() }).unwrap();
    assert!(eval(&e.formula) == d);
    let text: usize = e.formula.streams.iter().filter(|s| s.class == "cab/text").map(|s| s.data.len()).sum();
    assert_eq!(text, 105_000);
    assert_eq!(e.inflated, 145_000);
}

/// MSZIP folders past the limit are kept as they are; stored folders still taken.
#[test]
fn limit() {
    let d = fixture("mixed");
    let e = formula::encode_with(&d, &formula::Options { limit: 80_000, ..Default::default() }).unwrap();
    assert!(eval(&e.formula) == d);
    assert_eq!(e.inflated, 70_000);
    let e = formula::encode_with(&d, &formula::Options { limit: 0, ..Default::default() }).unwrap();
    assert!(eval(&e.formula) == d);
    assert_eq!(e.inflated, 0);
    assert!(formula::encode_with(&fixture("mszip"), &formula::Options { limit: 0, ..Default::default() }).is_none());
}

#[test]
fn not_a_cab() {
    assert!(formula::encode(b"MSCF\0\0\0\0", 1).is_none());
    assert!(formula::encode(&[0; 300], 1).is_none());
    // a folder of LZX is kept
    let mut d = fixture("small");
    let c = cab::parse(&d).unwrap();
    assert_eq!(c.folders[0].method(), cab::MSZIP);
    d[36 + 6] = cab::LZX as u8;
    assert!(formula::encode(&d, 1).is_none());
}

/// Files cut short or with bytes changed: no panic, and a formula, where there is one, gives
/// the input back.
#[test]
fn damaged() {
    let d = fixture("mixed");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for cut in [10, 36, 50, 200, d.len() / 3, d.len() / 2, d.len() - 10] {
        if let Some(f) = formula::encode(&d[..cut], 1) {
            assert!(eval(&f) == d[..cut], "cut at {cut}");
        }
    }
    let first = cab::parse(&d).unwrap().folders[0].blocks[0].start;
    for _ in 0..200 {
        let mut e = d.clone();
        for _ in 0..1 + next() % 3 {
            // the header, folders and files, or the data blocks
            let at = match next() % 2 {
                0 => (next() % first as u64) as usize,
                _ => first + (next() % (d.len() - first) as u64) as usize,
            };
            e[at] = next() as u8;
        }
        if let Some(f) = formula::encode(&e, 1) {
            assert!(eval(&f) == e);
        }
    }
}

/// A CAB file of one LZX folder: aqr-lzx's CAB stream fixture (a window of 64 KiB, e8), a data
/// block for each frame, two files over its data.
fn lzx_cab() -> (Vec<u8>, usize) {
    let s = std::fs::read(format!("{}/../aqr-lzx/tests/data/greedy.cab-lzx", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let bits = s[0] as u16;
    let n = u32::from_le_bytes(s[1..5].try_into().unwrap()) as usize;
    let (mut frames, mut at) = (Vec::new(), 5);
    while at < s.len() {
        let len = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as usize;
        frames.push(&s[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    let files: [(&[u8], usize, usize); 2] = [(b"a.bin", 0, 40000), (b"b.dll", 40000, n - 40000)];
    let files_at = 36 + 8;
    let files_len: usize = files.iter().map(|f| 16 + f.0.len() + 1).sum();
    let data_at = files_at + files_len;
    let total = data_at + frames.iter().map(|f| 8 + f.len()).sum::<usize>();
    let mut d = b"MSCF\0\0\0\0".to_vec();
    for v in [total as u32, 0, files_at as u32, 0] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.extend_from_slice(&[3, 1, 1, 0, 2, 0, 0, 0, 0x34, 0x12, 0, 0]);
    d.extend_from_slice(&(data_at as u32).to_le_bytes());
    d.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    d.extend_from_slice(&(cab::LZX | bits << 8).to_le_bytes());
    for (name, off, size) in files {
        d.extend_from_slice(&(size as u32).to_le_bytes());
        d.extend_from_slice(&(off as u32).to_le_bytes());
        d.extend_from_slice(&[0, 0, 0x21, 0x50, 0, 0x60, 0x20, 0]);
        d.extend_from_slice(name);
        d.push(0);
    }
    for (i, f) in frames.iter().enumerate() {
        let uncomp = (n - i * 32768).min(32768);
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&(f.len() as u16).to_le_bytes());
        d.extend_from_slice(&(uncomp as u16).to_le_bytes());
        d.extend_from_slice(f);
    }
    assert_eq!(d.len(), total);
    (d, n)
}

/// An LZX folder rebuilt by aqr-lzx's module from its data (e8 undone: the files' contents).
#[test]
fn lzx_folder() {
    let (d, n) = lzx_cab();
    let f = formula::encode(&d, 2).expect("a formula");
    assert!(eval(&f) == d);
    assert!(f.applies().iter().any(|a| a.module == aqr_lzx::formula::MODULE && a.params == [16]));
    let contents: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
    assert_eq!(contents, n);
    // past the limit, or without LZX folders, kept
    assert!(formula::encode_with(&d, &formula::Options { limit: n as u64 - 1, ..Default::default() }).is_none());
    assert!(formula::encode_with(&d, &formula::Options { lzx: false, ..Default::default() }).is_none());
}

/// A number as CHM's directory writes it: 7-bit groups, the high first.
fn encint(mut v: u64, out: &mut Vec<u8>) {
    let mut g = vec![(v & 0x7f) as u8];
    v >>= 7;
    while v > 0 {
        g.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.extend(g.iter().rev());
}

/// A CHM file of aqr-lzx's CHM fixture (resets every 2 frames, a window of 64 KiB): two files in
/// the LZX section, its content, control data and reset table in section 0. The data's length.
fn chm_file() -> (Vec<u8>, usize) {
    let s = std::fs::read(format!("{}/../aqr-lzx/tests/data/greedy.chm-lzx", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let n = u32::from_le_bytes(s[0..4].try_into().unwrap()) as usize;
    let (mut content, mut frames, mut at) = (Vec::new(), Vec::new(), 4);
    while at < s.len() {
        let len = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as usize;
        frames.push(content.len() as u64);
        content.extend_from_slice(&s[at + 4..at + 4 + len]);
        at += 4 + len;
    }
    frames.truncate(n.div_ceil(32768));
    let mut control = Vec::new();
    for v in [6u32, u32::from_le_bytes(*b"LZXC"), 2, 2, 2, 2, 0] {
        control.extend_from_slice(&v.to_le_bytes());
    }
    let mut table = Vec::new();
    for v in [2u32, frames.len() as u32, 8, 0x28] {
        table.extend_from_slice(&v.to_le_bytes());
    }
    for v in [n as u64, content.len() as u64, 0x8000] {
        table.extend_from_slice(&v.to_le_bytes());
    }
    frames.iter().for_each(|f| table.extend_from_slice(&f.to_le_bytes()));
    let ms = "::DataSpace/Storage/MSCompressed/";
    let sec0 = [content.len(), control.len(), table.len()];
    let names0 = [
        format!("{ms}Content"),
        format!("{ms}ControlData"),
        format!("{ms}Transform/{{7FC28940-9D31-11D0-9B27-00A0C91E9C7C}}/InstanceData/ResetTable"),
    ];
    let mut entries: Vec<(String, u64, u64, u64)> = vec![("/".into(), 0, 0, 0)];
    entries.push(("/a.htm".into(), 1, 0, 70000));
    entries.push(("/img/b.bin".into(), 1, 70000, n as u64 - 70000));
    let mut off = 0;
    for (name, len) in names0.iter().zip(sec0) {
        entries.push((name.clone(), 0, off, len as u64));
        off += len as u64;
    }
    let chunk = 4096;
    let mut pmgl = b"PMGL".to_vec();
    pmgl.extend_from_slice(&[0; 16]);
    for (name, sec, off, len) in &entries {
        encint(name.len() as u64, &mut pmgl);
        pmgl.extend_from_slice(name.as_bytes());
        [*sec, *off, *len].iter().for_each(|&v| encint(v, &mut pmgl));
    }
    let free = (chunk - pmgl.len()) as u32;
    pmgl[4..8].copy_from_slice(&free.to_le_bytes());
    pmgl.resize(chunk, 0);
    let mut itsp = b"ITSP".to_vec();
    itsp.resize(0x54, 0);
    itsp[4..8].copy_from_slice(&1u32.to_le_bytes());
    itsp[8..12].copy_from_slice(&0x54u32.to_le_bytes());
    itsp[0x10..0x14].copy_from_slice(&(chunk as u32).to_le_bytes());
    itsp[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
    let dir = 0x78u64;
    let data = dir + (itsp.len() + chunk) as u64;
    let mut d = b"ITSF".to_vec();
    d.resize(0x78, 0);
    d[4..8].copy_from_slice(&3u32.to_le_bytes());
    d[8..12].copy_from_slice(&0x60u32.to_le_bytes());
    for (i, v) in [0x60u64, 0x18, dir, (itsp.len() + chunk) as u64, data].into_iter().enumerate() {
        d[0x38 + 8 * i..0x40 + 8 * i].copy_from_slice(&v.to_le_bytes());
    }
    d.extend_from_slice(&itsp);
    d.extend_from_slice(&pmgl);
    d.extend_from_slice(&content);
    d.extend_from_slice(&control);
    d.extend_from_slice(&table);
    (d, n)
}

/// A CHM file's LZX section rebuilt by aqr-lzx's module, each reset interval a stream, from the
/// section's files.
#[test]
fn chm_lzx_section() {
    let (d, n) = chm_file();
    let c = aqr_mszip::chm::parse(&d).unwrap();
    assert_eq!(c.lzx.as_ref().map(|z| (z.window_bits, z.reset_frames, z.len)), Some((16, 2, n)));
    let f = formula::encode_chm(&d, 2).expect("a formula");
    assert!(eval(&f) == d);
    let a = f.applies().into_iter().find(|a| a.module == aqr_lzx::formula::MODULE).unwrap();
    assert_eq!(a.params, [16]);
    // the data: the section's, and the zeros of the last interval's whole frames
    let contents: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
    assert_eq!(contents, 5 * 32768);
    // past the limit, or damaged: kept, or given back
    assert!(formula::encode_chm_with(&d, &formula::Options { limit: n as u64 - 1, ..Default::default() }).is_none());
    for cut in [100, 0x78 + 0x54 + 50, d.len() / 2, d.len() - 5] {
        if let Some(f) = formula::encode_chm(&d[..cut], 1) {
            assert!(eval(&f) == d[..cut]);
        }
    }
}
