//! Native and WebAssembly decoding alike on damaged inputs (common::fail_alike), the modules
//! that tests/alike.rs does not take: of each, the streams of a small application damaged in
//! turn decode to the same bytes or fail in both.

#![cfg(feature = "wasm")]

use std::time::Duration;

mod common;

use aqr_formula::{Formula, Node};

const LIMIT: Duration = Duration::from_secs(30);

/// common::fail_alike of `f` within [`LIMIT`].
fn alike_with(name: &str, f: &Formula, per: usize, head: usize) {
    common::fail_alike(name, f, per, head, LIMIT);
}

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// [`alike_with`] of `f`, which applies the module `module` (else the test checks nothing).
fn alike(module: &str, name: &str, f: &Formula) {
    assert!(f.applies().iter().any(|a| a.module.name == module), "{name}: no application of {module}");
    alike_with(name, f, 12, 4);
}

/// The application `k` of module `name` in `f` as the root of a copy of it.
fn only(f: &Formula, name: &str, k: usize) -> Formula {
    let mut apps = Vec::new();
    applications(&f.root, name, &mut apps);
    let mut g = f.clone();
    g.root = apps.swap_remove(k);
    g
}

fn applications(n: &Node, name: &str, out: &mut Vec<Node>) {
    match n {
        Node::Apply(a) => {
            if a.module.name == name {
                out.push(n.clone());
            }
            a.inputs.iter().for_each(|i| applications(i, name, out));
        }
        Node::Concat(ns) => ns.iter().for_each(|n| applications(n, name, out)),
        _ => {}
    }
}

#[test]
fn flac_alike() {
    for name in ["u8_mono", "silence"] {
        let d = fixture(&format!("codecs/aqr-flac/tests/data/{name}.flac"));
        let f = aqr_flac::formula::encode(&d, 1).unwrap();
        alike("flac-recons", &format!("flac {name}"), &f);
    }
}

#[path = "../../../codecs/aqr-flac/tests/common/synth.rs"]
mod flac_synth;

/// FLAC's synthetic frames that the fixtures have none of: of a variable block size, verbatim
/// and constant subframes, the side channels (left/side, right/side, mid/side), escaped
/// partitions and LPC with wasted bits.
#[test]
fn flac_synthetic_alike() {
    use flac_synth::{Hdr, Sub, file};
    let mut variable = Hdr::new(8, 0, 4);
    variable.variable = true;
    variable.number = 1 << 30;
    let extremes = |top: i64| Sub::Verbatim((0..256).map(|i| if i % 2 == 0 { top } else { -top - 1 }).collect());
    let lpc = Sub::Pred { order: 8, lpc: Some((12, 9)), method: 1, porder: 2, escape: None, k: 4, wasted: 2 };
    let escaped = Sub::Pred { order: 2, lpc: None, method: 0, porder: 1, escape: Some(9), k: 0, wasted: 0 };
    let cases = [
        ("variable", file(&variable, 16, 1, &[Sub::fixed(2, 0)], 6)),
        ("verbatim", file(&Hdr::new(8, 1, 4), 16, 2, &[extremes(32767), Sub::Constant(-5)], 4)),
        ("left/side", file(&Hdr::new(8, 8, 4), 16, 2, &[Sub::fixed(1, 0), extremes(32768)], 4)),
        ("right/side", file(&Hdr::new(8, 9, 4), 16, 2, &[extremes(32768), Sub::fixed(2, 1)], 4)),
        ("mid/side", file(&Hdr::new(8, 10, 4), 16, 2, &[Sub::fixed(3, 2), extremes(32768)], 4)),
        ("lpc, escaped", file(&Hdr::new(9, 1, 6), 24, 2, &[lpc, escaped], 4)),
    ];
    for (name, d) in cases {
        let f = aqr_flac::formula::encode(&d, 1).unwrap_or_else(|| panic!("flac {name}: not coded"));
        alike("flac-recons", &format!("flac {name}"), &f);
    }
}

#[test]
fn ogg_alike() {
    for name in ["ffmpeg-stereo.ogg", "opus.opus"] {
        let d = fixture(&format!("codecs/aqr-ogg/tests/data/{name}"));
        let (f, _) = aqr_ogg::formula::encode_any(&d, 1).unwrap();
        alike("ogg-recons", &format!("ogg {name}"), &f);
    }
}

#[test]
fn vorbis_alike() {
    let d = fixture("codecs/aqr-ogg/tests/data/ffmpeg-stereo.ogg");
    let f = aqr_ogg::formula::encode(&d, 1).unwrap();
    alike("vorbis-recons", "vorbis", &only(&f, "vorbis-recons", 0));
}

// ---- wwise: Vorbis packets without their type and window bits (`Ident::stripped`)

/// Writes a packet of random values of `setup`, as the walk asks for them.
struct Random<'a> {
    setup: &'a aqr_vorbis::setup::Setup,
    rng: aqr_common::rng::Rng,
    w: aqr_vorbis::bits::Writer,
}

impl aqr_vorbis::packet::Source for Random<'_> {
    fn bits(&mut self, f: aqr_vorbis::packet::Field, n: u32) -> Option<u32> {
        use aqr_vorbis::packet::Field;
        let v = match f {
            Field::Type => 0,
            Field::Window => self.rng.below(2) as u32,
            Field::Mode => self.rng.below(self.setup.modes.len() as u64) as u32,
            Field::Nonzero { .. } => (self.rng.below(4) > 0) as u32,
            _ => self.rng.below(1 << n) as u32,
        };
        self.w.bits(v, n);
        Some(v)
    }

    fn entry(&mut self, _: aqr_vorbis::packet::Field, book: usize) -> Option<u32> {
        let b = &self.setup.codebooks[book];
        let used: Vec<u32> = (0..b.entries() as u32).filter(|&e| b.used(e)).collect();
        let e = used[self.rng.below(used.len() as u64) as usize];
        b.write(e, &mut self.w);
        Some(e)
    }
}

/// vorbis-recons of the stripped packets Wwise stores (the setup of a database row that parses
/// as a stereo or a mono stream, random packets of it, the last with bytes after its values).
#[test]
fn wwise_stripped_alike() {
    use aqr_vorbis::setup::{Ident, Setup};
    let db = aqr_vorbis::setups::database().unwrap();
    for channels in [1usize, 2] {
        let ident = Ident { channels, blocksizes: [256, 2048], stripped: true };
        let row = db
            .rows()
            .iter()
            .find(|r| Setup::parse(&db.assemble_row(r).unwrap(), ident).is_ok())
            .unwrap_or_else(|| panic!("no row of {channels} channels"));
        let full = db.assemble_row(row).unwrap();
        let s = Setup::parse(&full, ident).unwrap();
        let mut data = Vec::new();
        for seed in 0..12 {
            let mut r = Random { setup: &s, rng: aqr_common::rng::Rng::new(seed), w: aqr_vorbis::bits::Writer::new() };
            aqr_vorbis::packet::walk(&s, &mut r).unwrap();
            let mut p = r.w.finish();
            if seed == 11 {
                p.extend([0, 0, 0]);
            }
            data.extend_from_slice(&(p.len() as u16).to_le_bytes());
            data.extend_from_slice(&p);
        }
        let mut f = Formula::default();
        let (node, used) = aqr_vorbis::formula::packets(&mut f, &full, ident, &data).unwrap();
        assert_eq!(used, data.len());
        f.root = node;
        alike("vorbis-recons", &format!("wwise stripped, {channels} channels"), &f);
    }
}

// ---- log, xsv, jsonl

const EXTREMES: [&str; 21] = [
    "999999999999999999",
    "-999999999999999999",
    "0.000000000000000001",
    "-0.000000000000000001",
    "999999999999999.999",
    "-999999999999999.999",
    "0",
    "1.5",
    "999999999999999999",
    "0.999999999999999999",
    "-0.999999999999999999",
    "99999999999.9999999",
    "1e5",
    "123456789012345678e-17",
    "-9223372036854775808",
    "9223372036854775807",
    "9223372036854775808",
    "0.0000000000000000000000000001",
    "100000000000000000000000000000",
    "-0",
    "-0.0",
];

/// The parts of formula `f` of a segmented text (one application each), the first.
fn first_part(f: &Formula) -> Formula {
    let mut g = f.clone();
    if let Node::Concat(ns) = &f.root {
        g.root = ns[0].clone();
    }
    g
}

#[test]
fn log_alike() {
    // numbers of 18 digits and at the edges of i64, their scale changing from line to line
    let mut d = String::new();
    for i in 0..EXTREMES.len() {
        for (k, t) in EXTREMES[i..].iter().enumerate().take(6) {
            d += &format!("v {t}\nw {}\nu {t} {}\n", EXTREMES[(i + k * 7) % 21], EXTREMES[(i * 7 + 3) % 21]);
        }
    }
    let f = aqr_log::formula::encode_in(d.as_bytes(), usize::MAX, false, 1).unwrap();
    alike("log-recons", "log extremes", &first_part(&f));
    // times, addresses and the marks of the template
    let mut d = String::new();
    for i in 0..60u32 {
        d += &format!(
            "Dec 10 12:{:02}:{:02} host app[{}]: from 10.0.{}.{} \x04 1.2.3.4 \x05 {}\n",
            i % 60,
            i,
            i * 7,
            i % 3,
            i,
            -(i as i64) * 1_000_003
        );
        if i % 9 == 0 {
            d += "plain line \x01 with a mark\n";
        }
    }
    let f = aqr_log::formula::encode_in(d.as_bytes(), usize::MAX, false, 1).unwrap();
    alike("log-recons", "log syslog", &first_part(&f));
}

#[test]
fn xsv_alike() {
    use aqr_xsv::table::Dialect;
    let csv = Dialect { delim: b",".to_vec(), quote: Some(b'"'), term: vec![] };
    let mut d = String::from("a,b,c\n");
    for i in 0..EXTREMES.len() * 3 {
        d += &format!("{},{},{}\n", EXTREMES[i % 21], EXTREMES[(i * 5 + 1) % 21], EXTREMES[(i / 2) % 21]);
    }
    alike("xsv-recons", "xsv extremes", &aqr_xsv::formula::encode_as(d.as_bytes(), &csv));
    let mut d = String::new();
    for i in 0..120i64 {
        let v = if i % 2 == 0 { 999_999_999_999_999_999 - i } else { -999_999_999_999_999_999 + i };
        d += &format!("{v},{i}\n");
    }
    alike("xsv-recons", "xsv falling", &aqr_xsv::formula::encode_as(d.as_bytes(), &csv));
    // a byte order mark before a quoted field; separators of 4 bytes
    let mut d = String::from("\u{feff}\"id\",\"x\",\"y\"\n");
    for i in 0..80 {
        d += &format!("{i},{}.5,\"t{i}\"\n", i * 3);
    }
    alike("xsv-recons", "xsv bom", &aqr_xsv::formula::encode_as(d.as_bytes(), &csv));
    let sep = Dialect { delim: b"|;|;".to_vec(), quote: None, term: b"<EOL".to_vec() };
    let d: String = (0..80).map(|i| format!("{i}|;|;{}|;|;x{i}<EOL", i * 7)).collect();
    alike("xsv-recons", "xsv 4-byte separators", &aqr_xsv::formula::encode_as(d.as_bytes(), &sep));
}

#[test]
fn jsonl_alike() {
    use aqr_jsonl::tree::{MAX_DEPTH, Style};
    let style = Style { comma_space: false, colon_space: false };
    let deep = "[".repeat(MAX_DEPTH) + "1" + &"]".repeat(MAX_DEPTH);
    let deeper = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
    let mut d = String::new();
    for i in 0..30i64 {
        d += &format!("{deep}\n{deeper}\n{{\"a\":{{\"b\":{{\"c\":[[{i}],[2,[3]]]}}}},\"n\":{}}}\n", i64::MAX - i * 3);
        d += &format!("[{},{},-0,1.5e-7,9223372036854775808]\n", i64::MIN + i, i * 999_999_999_999_999);
        let long: Vec<String> = (0..70).map(|k| (k * i).to_string()).collect();
        d += &format!("{{\"x\":[{}]}}\n", long.join(","));
    }
    let (f, _) = aqr_jsonl::formula::encode_as(d.as_bytes(), style).unwrap();
    alike("jsonl-recons", "jsonl", &first_part(&f));
}

// ---- tensors

/// A safetensors file of tensors `(dtype, row, data)`.
fn safetensors(ts: &[(&str, u64, Vec<u8>)]) -> Vec<u8> {
    let mut h = String::from("{");
    let (mut at, mut body) = (0usize, Vec::new());
    for (i, (ty, row, d)) in ts.iter().enumerate() {
        let n = d.len() as u64 / row;
        let sep = if i > 0 { "," } else { "" };
        h += &format!(r#"{sep}"t{i}":{{"dtype":"{ty}","shape":[{n},{row}],"data_offsets":[{at},{}]}}"#, at + d.len());
        at += d.len();
        body.extend_from_slice(d);
    }
    h.push('}');
    while h.len() % 8 != 0 {
        h.push(' ');
    }
    let mut out = (h.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(h.as_bytes());
    out.extend_from_slice(&body);
    out
}

/// tensors-recons of floats of every type: the patterns of zeros, infinities, NaNs, negative
/// zeros and denormals mixed (and a run of one value), two files (the one cut to segments too
/// small to hold a tensor), and the converted weights of a grid.
#[test]
fn tensors_alike() {
    let mut r = aqr_common::rng::Rng::new(3);
    let mut ts = Vec::new();
    for (ty, w) in [("F8_E4M3", 1usize), ("F16", 2), ("BF16", 2), ("F32", 4), ("F64", 8)] {
        let mut specials: Vec<Vec<u8>> = Vec::new();
        for hi in [0x00u8, 0x7f, 0x80, 0xff, 0x7e, 0xfe, 0x3f] {
            for lo in [0x00u8, 0x01, 0x80, 0xff, 0x55] {
                let mut b = vec![0xaau8; w];
                b[0] = lo;
                b[w - 1] = hi;
                specials.push(b);
            }
        }
        let mixed: Vec<u8> =
            (0..3000).flat_map(|_| specials[r.below(specials.len() as u64) as usize].clone()).collect();
        ts.push((ty, 64u64, mixed));
        ts.push((ty, 64, vec![0x80; 64 * w * 8]));
    }
    let d = safetensors(&ts);
    let f = aqr_tensors::formula::encode_in(&d, 1, 1 << 12).expect("taken");
    alike("tensors-recons", "tensors special floats", &f);
    let f = aqr_tensors::formula::encode_in(&aqr_tensors::synth::converted(5), 1, 1 << 14).expect("taken");
    alike("tensors-recons", "tensors grid", &only(&f, "tensors-recons", 0));
    let f = aqr_tensors::formula::encode_in(&aqr_tensors::synth::file(2, 1), 1, 1 << 14).expect("taken");
    alike("tensors-recons", "tensors gguf", &only(&f, "tensors-recons", 0));
}

// ---- exe: x86 (a part module), CIL

/// x86 code of `n` bytes, mostly branches (as codecs/aqr-exe/tests/edge_codecs.rs'): CALL and
/// JMP rel32, Jcc, short jumps, indirect jumps, RIP-relative addresses.
fn structured(n: usize, seed: u64) -> Vec<u8> {
    let mut r = aqr_common::rng::Rng::new(seed);
    let near =
        |r: &mut aqr_common::rng::Rng, range: u32| (r.below(range as u64) as i32 - range as i32 / 2).to_le_bytes();
    let mut d = Vec::with_capacity(n + 16);
    while d.len() < n {
        match r.below(12) {
            0 => d.extend([&[0xe8][..], &near(&mut r, 1 << 16)].concat()),
            1 => d.extend([&[0xe9][..], &near(&mut r, 1 << 12)].concat()),
            2 => d.extend([&[0x0f, 0x85][..], &near(&mut r, 1 << 12)].concat()),
            3 => d.extend([0x74, r.below(256) as u8]),
            4 => d.extend([0xeb, r.below(256) as u8]),
            5 => d.extend([&[0xff, 0x25][..], &near(&mut r, 1 << 20)].concat()),
            6 => d.extend([&[0x48, 0x8d, 0x05][..], &near(&mut r, 1 << 20)].concat()),
            7 => d.extend([0x55, 0x48, 0x89, 0xe5]),
            8 => d.extend([0x31, 0xc0, 0xc3]),
            9 => d.extend(std::iter::repeat_n(0xcc, r.below(8) as usize)),
            _ => d.extend((0..r.below(6)).map(|_| r.below(256) as u8)),
        }
    }
    d.truncate(n);
    d
}

fn x86_formula(code: &[u8], origin: u64, is64: bool, side: bool) -> Formula {
    use aqr_exe::arch::x86;
    let fields = x86::Fields { base: 0, lo: 0, hi: u32::MAX as u64 };
    let st = x86::encode_fields(code, origin, is64, fields).unwrap();
    let mut f = Formula::default();
    let inputs = std::iter::zip(st.streams(), x86::STREAM_NAMES)
        .map(|(s, name)| f.push(format!("x86/{}", name.trim_start_matches("ST_")), s.clone()))
        .collect();
    let mut params = st.origin().to_le_bytes().to_vec();
    params.extend([st.sentinel_size(), is64 as u8]);
    if side {
        params.extend(fields.base.to_le_bytes());
        params.extend([fields.lo as u32, fields.hi as u32].map(u32::to_le_bytes).concat());
    }
    let side = if side { st.side() } else { &[] };
    let a = aqr_formula::Apply {
        module: aqr_decompress::X86,
        resources: vec![aqr_formula::Resource::new(x86::Opcodes::BUILTIN.to_resource())],
        params,
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: vec![],
    };
    let outs = f.share(a, vec![code.len() as u64, side.len() as u64]);
    f.root = Node::concat(outs);
    f
}

#[test]
fn exe_x86_alike() {
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    for (what, code, origin, is64, side) in [
        ("structured 64", structured(3000, 1), 0x1_4000_1000u64, true, false),
        ("structured 32 with side output", structured(3000, 2), 0x40_1000, false, true),
        ("origin near 2^32", structured(1500, 3), 0xffff_f000, false, false),
        ("origin near 2^63", structured(1500, 4), 0x7fff_ffff_ffff_f000, true, false),
        ("real code", exe[4096..4096 + 4000].to_vec(), 0x1_4000_1000, true, true),
    ] {
        let f = x86_formula(&code, origin, is64, side);
        alike("exe-recons/x86", &format!("x86 {what}"), &f);
    }
}

#[path = "common/cil.rs"]
mod cil_gen;
use cil_gen::{ORIGIN, section};

/// exe-recons/cil of sections of metadata (heap indices of 2 and of 4 bytes, tables of many
/// rows, fat bodies with exception clauses, no bodies, the uncompressed `#-`).
#[test]
fn exe_cil_alike() {
    use aqr_exe::arch::cil;
    for (what, heap_sizes, rows, bodies, name, eh) in [
        ("small heaps", 0u8, 6u32, true, &b"#~"[..], 0usize),
        ("big heaps", 7, 6, true, b"#~", 2),
        ("no bodies", 1, 6, false, b"#~", 0),
        ("many rows", 0, 300, true, b"#~", 1),
        ("#-", 0, 6, true, b"#-", 0),
    ] {
        let (d, meta) = section(heap_sizes, rows, bodies, name, eh);
        let Some(st) = cil::encode(&d, ORIGIN, meta) else { continue };
        let mut f = Formula::default();
        let inputs: Vec<Node> = std::iter::zip(st.streams(), cil::STREAM_NAMES)
            .map(|(s, n)| f.push(format!("cil/{n}"), s.clone()))
            .collect();
        let params = [ORIGIN.to_le_bytes(), (meta as u32).to_le_bytes()].concat();
        f.root = Node::apply(aqr_decompress::CIL, params, inputs, d.len() as u64);
        alike("exe-recons/cil", &format!("cil {what}"), &f);
    }
}

// ---- bcj, wasm

/// The branch converters of every architecture, Delta and Swap (7z's Swap2 and Swap4) over code
/// and noise.
#[test]
fn bcj_filters_alike() {
    use aqr_bcj::bra::Arch;
    use aqr_bcj::formula::Filter;
    let code = structured(3000, 7);
    let noise: Vec<u8> = (0..3000).map(|i: u32| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    let mut filters: Vec<(String, Filter, &str)> =
        Arch::ALL.iter().map(|&a| (format!("{a:?}"), Filter::Bra(a, 0), "")).collect();
    filters.extend([
        ("Delta 1".into(), Filter::Delta(1), ""),
        ("Delta 7".into(), Filter::Delta(7), ""),
        ("Swap 2".into(), Filter::Swap(2), ""),
        ("Swap 4".into(), Filter::Swap(4), ""),
    ]);
    for (name, filter, _) in filters {
        for (what, data) in [("code", &code), ("noise", &noise)] {
            let mut f = Formula::default();
            let d = f.push("raw", data.clone());
            f.root = filter.apply(d, data.len() as u64);
            let module = f.applies()[0].module.name;
            alike(module, &format!("bcj {name} {what}"), &f);
        }
    }
}

/// bcj2 (7z's four-stream x86 filter), of the fixture archive.
#[test]
fn bcj2_alike() {
    let d = fixture("codecs/aqr-7z/tests/data/filter-bcj2.7z");
    let f = aqr_7z::formula::encode(&d, 1).unwrap();
    alike("bcj2", "bcj2", &f);
}

/// wasm-recons of the modules aqr-modules built (small ones, and one with a name section).
#[test]
fn wasm_recons_alike() {
    for m in ["bcj-delta", "bcj-swap2", "bcj-arm"] {
        let d = fixture(&format!("crates/aqr-modules/wasm/{m}.wasm"));
        let f = aqr_wasm::formula::encode(&d, 1).unwrap();
        alike("wasm-recons", &format!("wasm {m}"), &f);
    }
}

// ---- pdb

#[test]
fn pdb_msf_alike() {
    let streams: Vec<Option<Vec<u8>>> = (0..12)
        .map(|k| match k % 7 {
            3 => None,
            _ => Some((0..(k * 997) % 3000).map(|i| (i * 31 + k) as u8).collect()),
        })
        .collect();
    for (bs, shuffle) in [(512, false), (512, true), (1024, true)] {
        let d = msf_file(bs, &streams, shuffle, 0x5a);
        let inputs = aqr_pdb::msf::encode(&d).expect("taken");
        let mut f = Formula::default();
        let nodes = inputs.into_iter().map(|i| f.push("raw", i)).collect();
        f.root = Node::apply(aqr_decompress::PDB_MSF, Vec::new(), nodes, d.len() as u64);
        alike("pdb-msf", &format!("msf {bs} {shuffle}"), &f);
    }
}

#[test]
fn pdb_symbols_alike() {
    for (mods, n, depth, entries, ids) in [(3, 60, 5, 200, 50), (1, 30, 20, 0, 1), (5, 10, 3, 30, 30)] {
        let what = format!("symbols {mods} {n} {depth} {entries}");
        let whole: Vec<(Vec<u8>, usize, usize)> = (0..mods).map(|k| pdb_module(k, n, depth, entries, ids)).collect();
        let info =
            |&(_, symbols, c13): &(Vec<u8>, usize, usize)| aqr_pdb::dbi::Module { stream: 0, symbols, c11: 0, c13 };
        let parts: Vec<(&[u8], aqr_pdb::dbi::Module)> = whole.iter().map(|w| (&w.0[..], info(w))).collect();
        let (params, st) = aqr_pdb::symbols::encode(&parts).expect("taken");
        let mut f = Formula::default();
        let nodes = (aqr_pdb::symbols::INPUTS.iter())
            .map(|(_, which)| f.push("raw", aqr_pdb::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>())))
            .collect();
        f.root = Node::apply(aqr_decompress::PDB_SYMBOLS, params, nodes, whole.iter().map(|w| w.0.len() as u64).sum());
        alike("pdb-symbols", &what, &f);
    }
}

#[test]
fn pdb_types_alike() {
    fn rec(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut r = kind.to_le_bytes().to_vec();
        r.extend_from_slice(body);
        while !(r.len() + 2).is_multiple_of(4) {
            let left = 4 - (r.len() + 2) % 4;
            r.push(0xf0 | left as u8);
        }
        let mut out = (r.len() as u16).to_le_bytes().to_vec();
        out.extend(r);
        out
    }
    // the raw CRC-32 of the record, which is the hash of the kinds made here
    fn crc(d: &[u8]) -> u32 {
        let mut c = 0u32;
        for &b in d {
            c ^= b as u32;
            for _ in 0..8 {
                c = (c >> 1) ^ (0xedb8_8320 & (c & 1).wrapping_neg());
            }
        }
        c
    }
    fn stream(records: &[Vec<u8>], hash_stream: u16) -> (Vec<u8>, Vec<u8>) {
        let (mut hv, mut idx) = (Vec::new(), Vec::new());
        let (mut off, mut next) = (0usize, 0usize);
        for (k, r) in records.iter().enumerate() {
            hv.extend_from_slice(&(crc(r) % 262143).to_le_bytes());
            if off >= next {
                idx.extend_from_slice(&(0x1000 + k as u32).to_le_bytes());
                idx.extend_from_slice(&(off as u32).to_le_bytes());
                next = (off / 8192 + 1) * 8192;
            }
            off += r.len();
        }
        let body: Vec<u8> = records.concat();
        let mut h = Vec::new();
        let f = [
            20040203u32,
            56,
            0x1000,
            0x1000 + records.len() as u32,
            body.len() as u32,
            hash_stream as u32 | 0xffff << 16,
            4,
            262143,
            0,
            hv.len() as u32,
            hv.len() as u32,
            idx.len() as u32,
            (hv.len() + idx.len()) as u32,
            0,
        ];
        f.iter().for_each(|v| h.extend_from_slice(&v.to_le_bytes()));
        (h.into_iter().chain(body).collect(), hv.into_iter().chain(idx).collect())
    }
    for n in [3usize, 40] {
        let t = |v: u32| v.to_le_bytes().to_vec();
        let mut tpi = vec![rec(0x1201, &t(0))];
        for k in 0..n {
            let me = 0x1001 + k as u32;
            tpi.push(match k % 4 {
                0 => rec(0x1002, &[t(me - 1), t(0x1000c)].concat()),
                1 => rec(0x1201, &[t(2), t(0x74), t(me - 1)].concat()),
                2 => rec(0x1008, &[t(3), vec![0, 0, 1, 0], t(me - 1)].concat()),
                _ => {
                    let mut body = vec![0x0d, 0x15, 3, 0];
                    body.extend([t(0x74), vec![0, 0]].concat());
                    body.extend(format!("member_{k}\0").as_bytes());
                    while (body.len() + 6) % 4 != 0 {
                        let left = 4 - (body.len() + 6) % 4;
                        body.push(0xf0 | left as u8);
                    }
                    rec(0x1203, &body)
                }
            });
        }
        let (tpi_stream, tpi_hash) = stream(&tpi, 5);
        let ipi: Vec<Vec<u8>> = (0..n / 2 + 1)
            .map(|k| rec(0x1605, &[t(0), format!("C:\\src\\file{k}.cpp\0").into_bytes()].concat()))
            .collect();
        let (ipi_stream, ipi_hash) = stream(&ipi, 6);
        let streams: Vec<Option<Vec<u8>>> = vec![
            Some(vec![0; 60]),
            Some(b"pdb info".to_vec()),
            Some(tpi_stream.clone()),
            Some(vec![1; 8]),
            Some(ipi_stream.clone()),
            Some(tpi_hash.clone()),
            Some(ipi_hash.clone()),
        ];
        // (the streams one after another, each where it is)
        let mut all = Vec::new();
        let mut spans = Vec::new();
        for s in streams.iter().flatten() {
            spans.push(Some(all.len()..all.len() + s.len()));
            all.extend_from_slice(s);
        }
        let e = aqr_pdb::types::encode(&all, &spans).expect("taken");
        assert_eq!(e.places.len(), 4, "{n}");
        let mut f = Formula::default();
        let nodes = (aqr_pdb::types::STREAM_NAMES.iter().zip(&e.streams))
            .map(|(name, d)| f.push(format!("pdb-types/{name}"), d.clone()))
            .collect();
        let a = aqr_formula::Apply {
            module: aqr_decompress::PDB_TYPES,
            resources: vec![],
            params: e.params.clone(),
            inputs: nodes,
            len: 0,
            memory_limit: aqr_formula::NO_LIMIT,
            task_limits: vec![],
        };
        let outs = f.share(a, e.sizes.to_vec());
        f.root = Node::concat(outs);
        alike("pdb-types", &format!("pdb-types {n}"), &f);
    }
}

fn pdb_module(k: u32, n: u32, depth: u32, entries: u32, ids: u32) -> (Vec<u8>, usize, usize) {
    let record = |m: &mut Vec<u8>, kind: u16, payload: &[u8]| {
        let len = (payload.len() + 4).next_multiple_of(4);
        m.extend_from_slice(&((len - 2) as u16).to_le_bytes());
        m.extend_from_slice(&kind.to_le_bytes());
        m.extend_from_slice(payload);
        m.resize(m.len() + len - 4 - payload.len(), 0);
    };
    let mut m = 4u32.to_le_bytes().to_vec();
    let mut open: Vec<usize> = Vec::new();
    for i in 0..n {
        let parent = open.last().map_or(0, |&q| q as u32);
        let mut b: Vec<u8> = [parent, 0, 0x40 * i + k, 8 + i % 5].iter().flat_map(|v| v.to_le_bytes()).collect();
        b.extend_from_slice(&[1, 0, 0]);
        open.push(m.len());
        record(&mut m, 0x1103, &b);
        let mut local = (0x74 + i % 3).to_le_bytes().to_vec();
        local.extend_from_slice(&[1, 0]);
        local.extend_from_slice(format!("x{i}\0").as_bytes());
        record(&mut m, 0x113e, &local);
        if open.len() as u32 == depth || i + 1 == n {
            while let Some(q) = open.pop() {
                let end = m.len() as u32;
                m[q + 8..q + 12].copy_from_slice(&end.to_le_bytes());
                record(&mut m, 0x0006, &[]);
            }
        }
    }
    let symbols = m.len();
    if entries > 0 {
        m.extend_from_slice(&0xf6u32.to_le_bytes());
        m.extend_from_slice(&(4 + 12 * entries).to_le_bytes());
        m.extend_from_slice(&0u32.to_le_bytes());
        for e in 0..entries {
            let id = 0x1000 + (e * 7919 + k) % ids;
            for v in [id, 0x18 * (id % 9), 100 + e / 3 + id % 50] {
                m.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let c13 = m.len() - symbols;
    (m, symbols, c13)
}

fn msf_file(bs: usize, streams: &[Option<Vec<u8>>], shuffle: bool, stale: u8) -> Vec<u8> {
    let u32s = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let blocks_of = |n: usize| n.div_ceil(bs);
    // blocks: 0 superblock, 1-2 free page maps, 3 the directory's list, then the rest
    let mut next = 4u32;
    let mut lists = Vec::new();
    for s in streams {
        let n = s.as_ref().map_or(0, |s| blocks_of(s.len()));
        let mut l: Vec<u32> = (next..next + n as u32).collect();
        if shuffle {
            l.reverse();
        }
        next += n as u32 + 1;
        lists.push(l);
    }
    let mut dir = (streams.len() as u32).to_le_bytes().to_vec();
    for s in streams {
        dir.extend_from_slice(&s.as_ref().map_or(u32::MAX, |s| s.len() as u32).to_le_bytes());
    }
    for l in &lists {
        dir.extend(u32s(l));
    }
    let dir_blocks: Vec<u32> = (next..next + blocks_of(dir.len()) as u32).collect();
    let nb = next as usize + dir_blocks.len();
    let mut d = vec![stale; nb * bs];
    d[..bs].fill(0);
    d[..32].copy_from_slice(aqr_pdb::msf::MAGIC);
    for (k, v) in [bs as u32, 1, nb as u32, dir.len() as u32, 0, 3].into_iter().enumerate() {
        d[32 + 4 * k..36 + 4 * k].copy_from_slice(&v.to_le_bytes());
    }
    d[bs..3 * bs].fill(0xff);
    d[3 * bs..4 * bs].fill(0);
    d[3 * bs..3 * bs + 4 * dir_blocks.len()].copy_from_slice(&u32s(&dir_blocks));
    for (chunk, &b) in dir.chunks(bs).zip(&dir_blocks) {
        d[b as usize * bs..][..bs].fill(0);
        d[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
    }
    for (s, l) in streams.iter().zip(&lists) {
        let Some(s) = s else { continue };
        for (chunk, &b) in s.chunks(bs).zip(l) {
            d[b as usize * bs..][..bs].fill(0);
            d[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
        }
    }
    d
}

// ---- dex, arsc

/// DEX files written here (aqr-dex's tests' writer: no Android tools needed).
#[path = "../../../codecs/aqr-dex/tests/common/synth.rs"]
mod dexw;

/// Resource tables written here (as codecs/aqr-arsc/tests/arsc.rs').
mod arscw {
    use aqr_common::rng::Rng;

    /// How a table is written apart from its contents.
    #[derive(Default, Clone, Copy)]
    pub struct Shape {
        /// An entry of a type chunk compact.
        pub compact: bool,
        /// A string's length in two bytes where one would do.
        pub long_len: bool,
        /// A chunk of a type the format does not have, in the package.
        pub unknown: bool,
    }

    fn chunk(t: u16, header: &[u8], body: &[u8]) -> Vec<u8> {
        let hs = 8 + header.len();
        let mut c = Vec::new();
        c.extend_from_slice(&t.to_le_bytes());
        c.extend_from_slice(&(hs as u16).to_le_bytes());
        c.extend_from_slice(&((hs + body.len()) as u32).to_le_bytes());
        c.extend_from_slice(header);
        c.extend_from_slice(body);
        c
    }

    fn len8(out: &mut Vec<u8>, n: usize) {
        if n < 0x80 {
            out.push(n as u8);
        } else {
            out.extend_from_slice(&[0x80 | (n >> 8) as u8, n as u8]);
        }
    }

    /// A string pool of `strings`, UTF-8 or UTF-16, with `styles` styled ones first.
    fn pool(strings: &[String], utf8: bool, styles: usize, long_len: bool) -> Vec<u8> {
        let mut data = Vec::new();
        let mut offs = Vec::new();
        for (i, s) in strings.iter().enumerate() {
            offs.push(data.len() as u32);
            if utf8 {
                let u16n = s.encode_utf16().count();
                if long_len && i == 1 {
                    data.extend_from_slice(&[0x80, u16n as u8]);
                } else {
                    len8(&mut data, u16n);
                }
                len8(&mut data, s.len());
                data.extend_from_slice(s.as_bytes());
                data.push(0);
            } else {
                let u: Vec<u16> = s.encode_utf16().collect();
                data.extend_from_slice(&(u.len() as u16).to_le_bytes());
                for x in u {
                    data.extend_from_slice(&x.to_le_bytes());
                }
                data.extend_from_slice(&[0, 0]);
            }
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        // (a style: spans of a name, a start and an end, then 0xFFFFFFFF; then the list's end)
        let mut style = Vec::new();
        let mut style_offs = Vec::new();
        for _ in 0..styles {
            style_offs.push(style.len() as u32);
            for v in [0u32, 0, 1, u32::MAX] {
                style.extend_from_slice(&v.to_le_bytes());
            }
        }
        if styles > 0 {
            style.extend_from_slice(&[0xff; 8]);
        }
        let n = strings.len();
        let strings_start = 28 + 4 * (n + styles);
        let styles_start = if styles > 0 { strings_start + data.len() } else { 0 };
        let mut h = Vec::new();
        for v in [n as u32, styles as u32, if utf8 { 0x100 } else { 0 }, strings_start as u32, styles_start as u32] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        let mut body = Vec::new();
        for o in offs.iter().chain(&style_offs) {
            body.extend_from_slice(&o.to_le_bytes());
        }
        body.extend_from_slice(&data);
        body.extend_from_slice(&style);
        chunk(0x0001, &h, &body)
    }

    /// A table made from `seed`, and how many keys it has.
    pub fn write(seed: u64, shape: Shape) -> (Vec<u8>, usize) {
        let mut rng = Rng::new(seed);
        let words = ["OK", "Cancel", "Annuler", "Abbrechen", "확인", "取消", "\u{1f600} smile", "res/drawable/a.png"];
        let n = 3 + rng.below(30) as usize;
        let values: Vec<String> =
            (0..n).map(|i| format!("{} {i}", words[rng.below(words.len() as u64) as usize])).collect();
        let keys: Vec<String> = (0..1 + rng.below(20) as usize).map(|i| format!("key_{i}")).collect();
        let types = ["attr", "string", "style"].map(String::from);
        let mut pkg = Vec::new();
        pkg.extend(pool(&types, false, 0, false));
        pkg.extend(pool(&keys, true, 0, false));
        if shape.unknown {
            pkg.extend(chunk(0x0203, &[1, 0, 0, 0], b"library"));
        }
        for tid in 1..=3u8 {
            let count = keys.len();
            let mut spec = vec![tid, 0, 0, 0];
            spec.extend_from_slice(&(count as u32).to_le_bytes());
            let flags: Vec<u8> = (0..count).flat_map(|_| 0u32.to_le_bytes()).collect();
            pkg.extend(chunk(0x0202, &spec, &flags));
            for config in 0..1 + rng.below(4) {
                let mut offs = Vec::new();
                let mut entries = Vec::new();
                for i in 0..count {
                    if config > 0 && rng.below(3) == 0 {
                        offs.extend_from_slice(&u32::MAX.to_le_bytes());
                        continue;
                    }
                    offs.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                    let key = if rng.below(8) == 0 { rng.below(count as u64) as u32 } else { i as u32 };
                    if tid == 3 && shape.compact && i == 0 {
                        // (compact: the key and the value's type in the entry, its data after)
                        entries.extend_from_slice(&[8, 3, 9, 0]);
                        entries.extend_from_slice(&key.to_le_bytes());
                        continue;
                    }
                    if tid == 3 {
                        entries.extend_from_slice(&16u16.to_le_bytes());
                        entries.extend_from_slice(&1u16.to_le_bytes());
                        entries.extend_from_slice(&key.to_le_bytes());
                        entries.extend_from_slice(&0u32.to_le_bytes());
                        let maps = rng.below(3) as u32;
                        entries.extend_from_slice(&maps.to_le_bytes());
                        for m in 0..maps {
                            entries.extend_from_slice(&(0x0101_0000 + m).to_le_bytes());
                            entries.extend_from_slice(&[8, 0, 0, 0x10]);
                            entries.extend_from_slice(&(rng.u64() as u32).to_le_bytes());
                        }
                    } else {
                        entries.extend_from_slice(&8u16.to_le_bytes());
                        entries.extend_from_slice(&((rng.below(2) as u16) << 1).to_le_bytes());
                        entries.extend_from_slice(&key.to_le_bytes());
                        let (vt, data) = match tid {
                            2 => (0x03u8, rng.below(n as u64) as u32),
                            _ => [(0x10u8, rng.below(100) as u32), (0x12, u32::MAX), (0x01, 0x7f01_0000 + i as u32)]
                                [rng.below(3) as usize],
                        };
                        entries.extend_from_slice(&[8, 0, 0, vt]);
                        entries.extend_from_slice(&data.to_le_bytes());
                    }
                }
                let mut h = vec![tid, 0, 0, 0];
                h.extend_from_slice(&(count as u32).to_le_bytes());
                h.extend_from_slice(&((20 + 64 + 4 * count) as u32).to_le_bytes());
                // (the configuration: its size, then a language)
                let mut cfg = vec![0u8; 64];
                cfg[..4].copy_from_slice(&64u32.to_le_bytes());
                cfg[8] = b"-efk"[config as usize];
                h.extend_from_slice(&cfg);
                let mut body = offs;
                body.extend_from_slice(&entries);
                pkg.extend(chunk(0x0201, &h, &body));
            }
        }
        let mut ph = Vec::new();
        ph.extend_from_slice(&0x7fu32.to_le_bytes());
        let name: Vec<u8> =
            "app".encode_utf16().chain(std::iter::repeat(0)).take(128).flat_map(|u| u.to_le_bytes()).collect();
        ph.extend_from_slice(&name);
        for v in [288u32, 3, 288 + pool(&types, false, 0, false).len() as u32, keys.len() as u32, 0] {
            ph.extend_from_slice(&v.to_le_bytes());
        }
        let package = chunk(0x0200, &ph, &pkg);
        let mut body = pool(&values, true, 1 + rng.below(2) as usize, shape.long_len);
        body.extend(package);
        (chunk(0x0002, &1u32.to_le_bytes(), &body), keys.len())
    }
}

#[test]
fn dex_alike() {
    for (seed, shape) in [
        (3, dexw::Shape::default()),
        (11, dexw::Shape { code_order: true, data_order: true, ..Default::default() }),
        (5, dexw::Shape { long_leb: true, unknown: true, ..Default::default() }),
        (8, dexw::Shape { bad_sums: true, ..Default::default() }),
    ] {
        let d = dexw::write(seed, shape);
        let f = aqr_dex::formula::encode(&d, 1).expect("taken");
        alike("dex-recons", &format!("dex {seed}"), &f);
    }
}

#[test]
fn arsc_alike() {
    for (seed, shape) in [
        (3, arscw::Shape::default()),
        (4, arscw::Shape { compact: true, ..Default::default() }),
        (5, arscw::Shape { long_len: true, unknown: true, ..Default::default() }),
    ] {
        let (d, _) = arscw::write(seed, shape);
        let f = aqr_arsc::formula::encode(&d, 1).expect("taken");
        alike("arsc-recons", &format!("arsc {seed}"), &f);
    }
}

// ---- dwarf-info

/// `.debug_info` of units made here (as codecs/aqr-dwarf/tests/info.rs').
#[allow(dead_code)]
mod dwarfw {
    pub fn uleb(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    pub fn sleb(out: &mut Vec<u8>, mut v: i64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    // attributes and forms
    pub const SIBLING: u64 = 0x01;
    pub const NAME: u64 = 0x03;
    pub const BYTE_SIZE: u64 = 0x0b;
    pub const LOW_PC: u64 = 0x11;
    pub const HIGH_PC: u64 = 0x12;
    pub const CONST_VALUE: u64 = 0x1c;
    pub const DECL_FILE: u64 = 0x3a;
    pub const DECL_LINE: u64 = 0x3b;
    pub const TYPE: u64 = 0x49;
    pub const LOCATION: u64 = 0x02;
    pub const LINKAGE_NAME: u64 = 0x6e;
    pub const SPECIFICATION: u64 = 0x47;
    pub const RANGES: u64 = 0x55;
    pub const EXTERNAL: u64 = 0x3f;

    pub const ADDR: u64 = 0x01;
    pub const DATA1: u64 = 0x0b;
    pub const DATA2: u64 = 0x05;
    pub const DATA4: u64 = 0x06;
    pub const STRING: u64 = 0x08;
    pub const BLOCK1: u64 = 0x0a;
    pub const SDATA: u64 = 0x0d;
    pub const STRP: u64 = 0x0e;
    pub const UDATA: u64 = 0x0f;
    pub const REF_ADDR: u64 = 0x10;
    pub const REF4: u64 = 0x13;
    pub const INDIRECT: u64 = 0x16;
    pub const SEC_OFFSET: u64 = 0x17;
    pub const EXPRLOC: u64 = 0x18;
    pub const FLAG_PRESENT: u64 = 0x19;
    pub const STRX1: u64 = 0x25;
    pub const IMPLICIT_CONST: u64 = 0x21;

    /// `.debug_str` of `n` names, and the offset of each.
    pub fn strings(n: usize) -> (Vec<u8>, Vec<u32>) {
        let mut s = Vec::new();
        let mut at = Vec::new();
        for i in 0..n {
            at.push(s.len() as u32);
            s.extend_from_slice(format!("name_{i}").as_bytes());
            s.push(0);
        }
        (s, at)
    }

    /// An abbreviation table: (code, tag, children, attributes (attribute, form, implicit constant)).
    /// An abbreviation: its code, tag, whether it has children, and its attributes and forms.
    pub type Abbrev<'a> = (u64, u64, bool, &'a [(u64, u64)]);

    pub fn abbrev_table(t: &[Abbrev]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(code, tag, children, attrs) in t {
            uleb(&mut out, code);
            uleb(&mut out, tag);
            out.push(children as u8);
            for &(a, f) in attrs {
                uleb(&mut out, a);
                uleb(&mut out, f);
                if f == IMPLICIT_CONST {
                    sleb(&mut out, -3);
                }
            }
            out.extend_from_slice(&[0, 0]);
        }
        out.push(0);
        out
    }

    pub const CU: u64 = 1;
    pub const SUB: u64 = 2;
    pub const VAR: u64 = 3;
    pub const TY: u64 = 4;
    pub const MISC: u64 = 5;

    pub fn table() -> Vec<u8> {
        abbrev_table(&[
            (CU, 0x11, true, &[(NAME, STRP), (LOW_PC, ADDR), (HIGH_PC, ADDR), (0x10, SEC_OFFSET)]),
            (
                SUB,
                0x2e,
                true,
                &[
                    (SIBLING, REF4),
                    (NAME, STRP),
                    (LINKAGE_NAME, STRP),
                    (DECL_FILE, DATA1),
                    (DECL_LINE, DATA2),
                    (LOW_PC, ADDR),
                    (HIGH_PC, DATA4),
                    (EXTERNAL, FLAG_PRESENT),
                ],
            ),
            (
                VAR,
                0x34,
                false,
                &[(NAME, STRING), (TYPE, REF4), (LOCATION, EXPRLOC), (DECL_LINE, UDATA), (CONST_VALUE, SDATA)],
            ),
            (TY, 0x24, false, &[(NAME, STRP), (BYTE_SIZE, DATA1), (0x3e, IMPLICIT_CONST)]),
            (
                MISC,
                0x0d,
                false,
                &[(TYPE, REF_ADDR), (SPECIFICATION, INDIRECT), (CONST_VALUE, BLOCK1), (RANGES, SEC_OFFSET)],
            ),
        ])
    }

    /// A unit of version 4 at `start` of the section: a compile unit holding `n` subprograms, each of
    /// a few variables, types at the start; strings of `str_at`, the first at `first`.
    pub fn unit4(start: usize, n: usize, str_at: &[u32], first: usize, prev_type: Option<u32>) -> Vec<u8> {
        let mut u = vec![0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 8];
        let mut k = first;
        let mut name = || {
            k += 1;
            str_at[(k - 1) % str_at.len()]
        };
        uleb(&mut u, CU);
        u.extend_from_slice(&name().to_le_bytes());
        u.extend_from_slice(&0x1000u64.to_le_bytes());
        u.extend_from_slice(&0x9000u64.to_le_bytes());
        u.extend_from_slice(&(start as u32 * 3).to_le_bytes());
        // the types
        let types: Vec<u32> = (0..3)
            .map(|i| {
                let at = u.len() as u32;
                uleb(&mut u, TY);
                u.extend_from_slice(&name().to_le_bytes());
                u.push(1 << i);
                at
            })
            .collect();
        for s in 0..n {
            let sub = u.len();
            uleb(&mut u, SUB);
            let sib = u.len();
            u.extend_from_slice(&[0; 4]);
            u.extend_from_slice(&name().to_le_bytes());
            u.extend_from_slice(&name().to_le_bytes());
            u.push(1 + (s % 3) as u8);
            u.extend_from_slice(&(10 + 7 * s as u16).to_le_bytes());
            u.extend_from_slice(&(0x1000 + 0x40 * s as u64).to_le_bytes());
            u.extend_from_slice(&0x3cu32.to_le_bytes());
            for v in 0..s % 4 {
                uleb(&mut u, VAR);
                u.extend_from_slice(format!("v{v}\0").as_bytes());
                u.extend_from_slice(&types[v % 3].to_le_bytes());
                u.extend_from_slice(&[2, 0x91, 0x70 - v as u8]);
                uleb(&mut u, 11 + 7 * s as u64 + v as u64);
                sleb(&mut u, -(v as i64) * 1000);
            }
            if s % 5 == 0 {
                uleb(&mut u, MISC);
                let t = prev_type.unwrap_or(start as u32 + types[0]);
                u.extend_from_slice(&t.to_le_bytes());
                uleb(&mut u, DATA4);
                u.extend_from_slice(&(sub.wrapping_sub(start) as u32).wrapping_add(7).to_le_bytes());
                u.extend_from_slice(&[3, 1, 2, 3]);
                u.extend_from_slice(&(s as u32 * 16).to_le_bytes());
            }
            u.push(0);
            // the sibling: the end of the subtree, but for every seventh (as if written otherwise)
            let next = if s % 7 == 6 { types[1] } else { u.len() as u32 };
            u[sib..sib + 4].copy_from_slice(&next.to_le_bytes());
        }
        u.push(0);
        let len = (u.len() - 4) as u32;
        u[..4].copy_from_slice(&len.to_le_bytes());
        u
    }

    pub fn section(units: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (strs, at) = strings(300);
        let mut d = Vec::new();
        let mut prev_type = None;
        for k in 0..units {
            let start = d.len();
            let u = unit4(start, 12 + k, &at, 17 * k, prev_type);
            // (a reference to the first type of the unit before, of DW_FORM_ref_addr)
            prev_type = Some(start as u32 + 11 + 1 + 4 + 8 + 8 + 4);
            d.extend_from_slice(&u);
        }
        (d, table(), strs)
    }
}

fn dwarf_formula(d: &[u8], abbrev: &[u8], strs: &[u8]) -> Formula {
    let mut f = Formula::default();
    let node = aqr_dwarf::formula::info(&mut f, d, abbrev, strs, |f| {
        (f.push("dwarf/abbrev", abbrev.to_vec()), f.push("dwarf/str", strs.to_vec()))
    })
    .expect("taken");
    f.root = node;
    f
}

#[test]
fn dwarf_info_alike() {
    use dwarfw::*;
    let (d, abbrev, strs) = section(3);
    alike("dwarf-info", "dwarf 4", &dwarf_formula(&d, &abbrev, &strs));
    // a unit of version 5 (a skeleton, of a DWO id), strings by index
    let abbrev = abbrev_table(&[(1, 0x4a, true, &[(NAME, STRX1), (LOW_PC, ADDR)]), (2, 0x34, false, &[(NAME, STRX1)])]);
    let mut u = vec![0, 0, 0, 0, 5, 0, 4, 8, 0, 0, 0, 0];
    u.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    u.extend_from_slice(&[1, 0]);
    u.extend_from_slice(&0x4000u64.to_le_bytes());
    for i in 1..40 {
        u.extend_from_slice(&[2, i]);
    }
    u.push(0);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    alike("dwarf-info", "dwarf 5", &dwarf_formula(&u, &abbrev, &[]));
}
