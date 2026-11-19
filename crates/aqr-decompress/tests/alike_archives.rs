//! Native and WebAssembly decoding alike on damaged inputs (common::fail_alike), for the modules
//! of archive formats (ALZ, EGG, AZO, RAR, LHA, 7z, asar) and of Windows' compressions (XPRESS,
//! LZNT1, LZX), and for lengths declared far past the inputs.

#![cfg(feature = "wasm")]

use std::time::Duration;

use aqr_archive::{Encoder, Level};
use aqr_decompress::{Native, Wasm, wasm_of};
use aqr_formula::{Formula, Node};
use sha2::{Digest, Sha256};

const LIMIT: Duration = Duration::from_secs(30);

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The chunks of a `.chunks` or `.units` fixture of the Windows codecs: bytes and data's length.
fn chunks(path: &str) -> Vec<(Vec<u8>, usize)> {
    let d = fixture(path);
    let (mut out, mut at) = (Vec::new(), 0);
    while at < d.len() {
        let n = u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
        out.push((d[at + 8..at + 8 + len].to_vec(), n));
        at += 8 + len;
    }
    out
}

/// The formula of one stream of `$krate`'s chunks (bytes kept before and after it).
macro_rules! windows_formula {
    ($krate:ident, $chunks:expr) => {{
        let stream: Vec<(&[u8], usize)> = $chunks.iter().map(|c: &(Vec<u8>, usize)| (&c.0[..], c.1)).collect();
        let streams = vec![stream];
        let sp = $krate::formula::split(&streams, 1).remove(0).unwrap();
        let mut f = Formula::default();
        f.push("raw", sp.data.clone());
        f.push("kept", b"<before><after>".to_vec());
        let part = $krate::formula::Stream {
            kept: Node::slice(1, 0, 8),
            data: Node::slice(0, 0, sp.data.len() as u64),
            record: sp.record,
        };
        let len = 8 + streams[0].iter().map(|c| c.0.len() as u64).sum::<u64>() + 7;
        f.root = $krate::formula::apply(&mut f, vec![part], Node::slice(1, 8, 7), len);
        f
    }};
}

fn xpress() -> Formula {
    let c = chunks("codecs/aqr-xpress/tests/data/wof4k.chunks");
    windows_formula!(aqr_xpress, &c[..2])
}

fn lznt1() -> Formula {
    let c = chunks("codecs/aqr-lznt1/tests/data/ntfs.units");
    windows_formula!(aqr_lznt1, &c[..1])
}

fn lzx() -> Formula {
    let c = chunks("codecs/aqr-lzx/tests/data/greedy.chunks");
    windows_formula!(aqr_lzx, &c[..])
}

fn azo() -> Formula {
    let d = fixture("codecs/aqr-egg/tests/data/azoshort_txt.egg");
    let at = d.windows(4).position(|w| w == 0x02B5_0C13u32.to_le_bytes()).unwrap() + 4;
    let len = u32::from_le_bytes(d[at + 6..at + 10].try_into().unwrap()) as usize;
    let s = &d[at + 18..at + 18 + len];
    let sp = aqr_azo::recons::split(s, usize::MAX, 2).unwrap();
    let mut f = Formula::default();
    let data = f.push("raw", sp.data);
    let record = f.push("azo/record", sp.record);
    let index = f.push("azo/index", Vec::new());
    f.root = Node::Apply(Box::new(aqr_azo::formula::apply(data, record, index, s.len() as u64)));
    f
}

#[test]
fn azo_alike() {
    fail_alike("azo", &azo(), 24, 16, LIMIT);
}

#[test]
fn xpress_alike() {
    fail_alike("xpress", &xpress(), 8, 6, LIMIT);
}

#[test]
fn lznt1_alike() {
    fail_alike("lznt1", &lznt1(), 8, 6, LIMIT);
}

#[test]
fn lzx_alike() {
    fail_alike("lzx", &lzx(), 8, 6, LIMIT);
}

/// Formula `f` whose application declares `len` bytes of output: it fails natively, in time,
/// with no allocation of that size; and as WebAssembly too where `wasm`.
fn lying_alike(name: &str, mut f: Formula, len: u64, wasm: bool) {
    let Node::Apply(a) = &mut f.root else { panic!("{name}: not an application") };
    a.len = len;
    let enc = Encoder::new(Level::Zstd(1)).with_modules(wasm_of);
    let Ok(c) = enc.encode(&f, None) else { return };
    let (n, w) = aqr_testkit::within(LIMIT, || {
        (aqr_archive::decode(&c, &Native::default()), wasm.then(|| aqr_archive::decode(&c, &Wasm)))
    })
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(n.is_err(), "{name} {len}: a length past the inputs decoded");
    if let Some(w) = w {
        assert!(w.is_err(), "{name} {len}: a length past the inputs decoded as Wasm");
    }
}

/// The lengths a lying application declares: past what memory holds (or the plan allows).
const LIES: [u64; 2] = [1 << 40, u64::MAX >> 1];

#[test]
fn azo_lying_length() {
    LIES.iter().for_each(|&len| lying_alike("azo", azo(), len, false));
}

#[test]
fn xpress_lying_length() {
    LIES.iter().for_each(|&len| lying_alike("xpress", xpress(), len, false));
}

#[test]
fn lznt1_lying_length() {
    LIES.iter().for_each(|&len| lying_alike("lznt1", lznt1(), len, false));
}

#[test]
fn lzx_lying_length() {
    LIES.iter().for_each(|&len| lying_alike("lzx", lzx(), len, false));
}

/// As WebAssembly, the host allocates the output of the length declared (aqr-wasmrt's
/// `apply_with`: `vec![0; s]`) and aborts where the system refuses it.
#[test]
#[ignore = "until aqr-wasmrt refuses an output it cannot allocate"]
fn lying_lengths_as_wasm() {
    for len in LIES {
        lying_alike("azo", azo(), len, true);
        lying_alike("xpress", xpress(), len, true);
        lying_alike("lznt1", lznt1(), len, true);
        lying_alike("lzx", lzx(), len, true);
    }
}
#[test]
fn alz_alike() {
    for t in ["normal", "bzip2"] {
        let d = fixture(&format!("codecs/aqr-alz/tests/data/{t}.alz"));
        let f = aqr_alz::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        fail_alike(&format!("alz {t}"), &f, 4, 4, LIMIT);
    }
}

#[test]
fn egg_alike() {
    for t in ["small_kor", "lzma_simple", "solid", "azoshort_txt"] {
        let d = fixture(&format!("codecs/aqr-egg/tests/data/{t}.egg"));
        let f = aqr_egg::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        fail_alike(&format!("egg {t}"), &f, 4, 4, LIMIT);
    }
}

#[test]
fn rar_alike() {
    for t in ["r4_solid", "r5_solid"] {
        let d = fixture(&format!("codecs/aqr-rar/tests/data/{t}.rar"));
        let f = aqr_rar::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        fail_alike(t, &f, 6, 6, LIMIT);
    }
}

#[test]
fn lha_alike() {
    let mut streams = Vec::new();
    for name in ["dos255e_lh5", "unix_h1_lh6"] {
        let d = fixture(&format!("codecs/aqr-lha/tests/data/{name}.lzh"));
        let e = aqr_lha::lha::parse(&d).unwrap().entries.remove(0);
        streams.push((d[e.data.clone()].to_vec(), e.size as usize, aqr_lha::lzh::Method::of(&e.method).unwrap()));
    }
    let (mut data, mut records, mut lens, mut whole) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (s, size, m) in &streams {
        let sp = aqr_lha::recons::split(s, *size, *m).unwrap();
        lens.push((sp.data.len(), sp.record.len()));
        data.extend(sp.data);
        records.extend(sp.record);
        whole.extend_from_slice(s);
    }
    let mut f = Formula::default();
    let data = f.push("raw", data);
    let record = f.push("lha/record", records);
    let index = f.push("lha/index", aqr_lha::formula::index(&lens));
    f.root = Node::Apply(Box::new(aqr_lha::formula::apply_recons(data, record, index, whole.len() as u64)));
    fail_alike("lha", &f, 8, 6, LIMIT);
}

#[test]
fn sevenz_alike() {
    for t in ["lzma2", "filter-bcj2", "filter-delta"] {
        let d = fixture(&format!("codecs/aqr-7z/tests/data/{t}.7z"));
        let f = aqr_7z::formula::encode(&d, 1).unwrap_or_else(|| panic!("{t}"));
        fail_alike(&format!("7z {t}"), &f, 3, 3, LIMIT);
    }
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// An asar archive of files with integrity (blocks of 1000 bytes), an unpacked one among them.
fn asar() -> Vec<u8> {
    let lines: Vec<u8> = (0..400).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let files: [(&[u8], bool); 3] = [(&lines, false), (b"hello\n", false), (b"unpacked", true)];
    let (mut data, mut entries) = (Vec::new(), Vec::new());
    for (body, unpacked) in files {
        let blocks: Vec<String> = body.chunks(1000).map(|c| format!("\"{}\"", hex(&Sha256::digest(c)))).collect();
        let place = if unpacked { ",\"unpacked\":true".to_string() } else { format!(",\"offset\":\"{}\"", data.len()) };
        entries.push(format!(
            "{{\"size\":{}{place},\"integrity\":{{\"algorithm\":\"SHA256\",\"hash\":\"{}\",\"blockSize\":1000,\"blocks\":[{}]}}}}",
            body.len(),
            hex(&Sha256::digest(body)),
            blocks.join(",")
        ));
        if !unpacked {
            data.extend_from_slice(body);
        }
    }
    let json = format!(
        "{{\"files\":{{\"a.js\":{},\"d\":{{\"files\":{{\"b.txt\":{}}}}},\"n.node\":{}}}}}",
        entries[0], entries[1], entries[2]
    );
    let mut d = aqr_asar::layout::prefix(json.len()).to_vec();
    d.extend_from_slice(json.as_bytes());
    d.resize(d.len() + aqr_asar::layout::pad_to(json.len(), 4), 0);
    d.extend_from_slice(&data);
    d
}

#[test]
fn asar_alike() {
    let f = aqr_asar::formula::encode(&asar(), 1).expect("asar formula");
    fail_alike("asar", &f, 8, 8, LIMIT);
}

/// [`common::fail_alike`]'s check, but with two failures alike whatever their messages (the
/// helper tells those apart: its `n != w` compares the errors too).
fn fail_alike(name: &str, f: &Formula, per: usize, head: usize, limit: Duration) {
    let enc = Encoder::new(Level::Zstd(1)).with_modules(wasm_of);
    let mut bad = Vec::new();
    for k in 0..f.streams.len() {
        let d = f.streams[k].data.to_vec();
        for x in aqr_testkit::damages(d.len(), per, head) {
            let mut g = f.clone();
            g.streams[k].data = x.apply(&d).into();
            let Ok(c) = enc.encode(&g, None) else { continue };
            let what = format!("{name}: stream {k} ({}), {x}", f.streams[k].class);
            let both = || (aqr_archive::decode(&c, &Native::default()), aqr_archive::decode(&c, &Wasm));
            match aqr_testkit::within(limit, both) {
                Err(e) => bad.push(format!("{what}: {e}")),
                Ok((n, w)) if n.is_ok() != w.is_ok() => {
                    bad.push(format!("{what}: {:?} natively, {:?} as Wasm", n.err(), w.err()))
                }
                Ok((Ok(n), Ok(w))) if n != w => bad.push(format!("{what}: other bytes natively and as Wasm")),
                Ok(_) => {}
            }
        }
    }
    assert!(bad.is_empty(), "{} cases not alike:\n{}", bad.len(), bad.join("\n"));
}

/// A length of u64::MAX: the plan of `azo-recons` takes it (its inputs, outputs and plan summed
/// by `needed_by` overflow: aqr-decompress's lib.rs).
#[test]
#[ignore = "until needed_by's sum of the memories saturates (aqr-decompress's lib.rs)"]
fn lying_length_max() {
    lying_alike("azo", azo(), u64::MAX, false);
    lying_alike("xpress", xpress(), u64::MAX, false);
    lying_alike("lznt1", lznt1(), u64::MAX, false);
    lying_alike("lzx", lzx(), u64::MAX, false);
}
