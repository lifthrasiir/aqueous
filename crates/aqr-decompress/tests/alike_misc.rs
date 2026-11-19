//! Native and WebAssembly decoding alike on damaged inputs (common::fail_alike), the modules
//! that tests/alike.rs does not take: of each, the streams of a small application damaged in
//! turn decode to the same bytes or fail in both.

#![cfg(feature = "wasm")]

use std::time::Duration;

use aqr_archive::{Encoder, Level};
use aqr_decompress::{Native, Wasm, wasm_of};

use aqr_formula::{Formula, Node};

const LIMIT: Duration = Duration::from_secs(30);

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Formula `f` with each of its streams damaged in every way of aqr-testkit's `damages` (at about
/// `per` places each, and every place of its first `head` bytes), each archived with the modules
/// aqr-modules built: natively and as WebAssembly, each decodes to the same bytes or fails in
/// both (tests/common's `fail_alike`, but the texts of two failures need not agree: a trap
/// is told otherwise than a native error), within `LIMIT`. The cases that do not are told all
/// together.
fn alike_with(name: &str, f: &Formula, per: usize, head: usize) {
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
            match aqr_testkit::within(LIMIT, both) {
                Err(e) => bad.push(format!("{what}: {e}")),
                Ok((n, w)) if n.is_ok() != w.is_ok() => {
                    bad.push(format!("{what}: {:?} natively, {:?} as Wasm", n.err(), w.err()))
                }
                Ok((Ok(n), Ok(w))) if n != w => bad.push(format!("{what}: other bytes natively and as Wasm")),
                Ok(_) => {}
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} cases not alike:
{}",
        bad.len(),
        bad.join(
            "
"
        )
    );
}

/// [`alike_with`] of `f`, which applies the module `module` (else the test checks nothing).
fn alike(module: &str, name: &str, f: &Formula) {
    assert!(f.applies().iter().any(|a| a.module.name == module), "{name}: no application of {module}");
    alike_with(name, f, 5, 3);
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
    let deep = "[".repeat(MAX_DEPTH) + &"1" + &"]".repeat(MAX_DEPTH);
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

const ORIGIN: u32 = 0x2000;

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// A section of metadata (root at `meta`, after `head` bytes of method bodies): tables of the
/// `heap_sizes` flags (bit 0, 1, 2: 4-byte indices to #Strings, #GUID, #Blob), a MethodDef
/// table of `rows` rows (each RVA that of the body when `bodies`), the stream `#~` named
/// `tables_name` ("#~", or "#-" for an uncompressed one), and a fat method body with `eh_fat`
/// exception clauses where >0 (the fat form 0x41 of a section's flags).
fn section(heap_sizes: u8, rows: u32, bodies: bool, tables_name: &[u8], eh_fat: usize) -> (Vec<u8>, usize) {
    let idx = |bit: u8| if heap_sizes & bit != 0 { 4 } else { 2 };
    let (s, g, b) = (idx(1), idx(2), idx(4));
    let mut head = vec![0u8; 0x40];
    // a tiny body at 0x10 (ldstr, call, ret), a fat one at 0x20 with its exception table
    let code_a = [0x72, 1, 0, 0, 0x70, 0x28, 2, 0, 0, 0x0a, 0x2a];
    head[0x10] = (code_a.len() << 2 | 2) as u8;
    head[0x11..0x11 + code_a.len()].copy_from_slice(&code_a);
    let mut fat = vec![0u8; 12];
    let code_b = [0x16u8, 0x2a];
    fat[0..2].copy_from_slice(&(0x3003u16 | if eh_fat > 0 { 8 } else { 0 }).to_le_bytes());
    fat[2..4].copy_from_slice(&8u16.to_le_bytes());
    fat[4..8].copy_from_slice(&le32(code_b.len() as u32));
    fat[8..12].copy_from_slice(&le32(0x1100_0001));
    fat.extend(code_b);
    if eh_fat > 0 {
        while !fat.len().is_multiple_of(4) {
            fat.push(0);
        }
        let size = 4 + 24 * eh_fat;
        fat.extend([0x41, size as u8, (size >> 8) as u8, (size >> 16) as u8]);
        for i in 0..eh_fat {
            let v = [i as u32 % 2, 0, 1, 1, 1, 0x0100_0001];
            fat.extend(v.iter().flat_map(|x| x.to_le_bytes()));
        }
    }
    head.splice(0x20..0x20, fat.iter().copied());
    let head_len = head.len().next_multiple_of(4);
    head.resize(head_len, 0);

    let mut tables = vec![0, 0, 0, 0, 2, 0, heap_sizes, 1];
    tables.extend(((1u64 << 0) | (1 << 6)).to_le_bytes());
    tables.extend(0u64.to_le_bytes());
    tables.extend(le32(1).into_iter().chain(le32(rows)));
    // Module: generation, name, mvid, encid, encbaseid
    tables.extend([0, 0]);
    tables.extend(&le32(1)[..s]);
    tables.extend(&le32(1)[..g]);
    tables.extend(&le32(0)[..g]);
    tables.extend(&le32(0)[..g]);
    // MethodDef: RVA, impl flags, flags, name, signature, param list
    for i in 0..rows {
        let rva = if bodies { ORIGIN + if i % 2 == 0 { 0x10 } else { 0x20 } } else { 0 };
        tables.extend(le32(rva));
        tables.extend([0u8, 0, 0x86, 0]);
        tables.extend(&le32(1 + i % 3)[..s]);
        tables.extend(&le32(1)[..b]);
        tables.extend(1u16.to_le_bytes());
    }
    while tables.len() % 4 != 0 {
        tables.push(0);
    }
    let names: [&[u8]; 5] = [tables_name, b"#Strings", b"#US", b"#GUID", b"#Blob"];
    let heaps: [&[u8]; 5] = [&tables, b"\0Foo\0Bar\0Baz\0", b"\0\x05h\0i\0\0\0", &[0x5a; 16], b"\0\x03abc\0\0\0"];
    let mut m = Vec::new();
    m.extend(b"BSJB");
    m.extend([1, 0, 1, 0, 0, 0, 0, 0]);
    m.extend(le32(12));
    m.extend(b"v4.0.30319\0\0");
    m.extend([0, 0, 5, 0]);
    let header_len = m.len() + names.iter().map(|n| 8 + ((n.len() + 4) & !3)).sum::<usize>();
    let mut at = header_len;
    for (name, h) in names.iter().zip(heaps) {
        m.extend(le32(at as u32));
        m.extend(le32(h.len() as u32));
        m.extend(*name);
        m.extend(vec![0; ((name.len() + 4) & !3) - name.len()]);
        at += h.len();
    }
    for h in heaps {
        m.extend(h);
    }
    let meta = head.len();
    let mut d = head;
    d.extend(m);
    (d, meta)
}

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
