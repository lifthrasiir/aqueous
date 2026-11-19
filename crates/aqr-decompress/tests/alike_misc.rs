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

/// DEX files written here (as codecs/aqr-dex/tests/dex.rs').
mod dexw {
    use aqr_common::rng::Rng;

    /// How a file is written apart from its contents.
    #[derive(Default, Clone, Copy)]
    pub struct Shape {
        /// Code items of neighbouring classes swapped.
        pub code_order: bool,
        /// The data of two classes swapped in its section.
        pub data_order: bool,
        /// A LEB128 of a class's data in two bytes where one would do.
        pub long_leb: bool,
        /// The checksum and the signature wrong.
        pub bad_sums: bool,
        /// A section of a type the format does not have, and bytes not zero between sections.
        pub unknown: bool,
    }

    fn uleb(out: &mut Vec<u8>, mut v: u32) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn sleb(out: &mut Vec<u8>, mut v: i32) {
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

    fn pad4(d: &mut Vec<u8>, fill: u8) {
        while !d.len().is_multiple_of(4) {
            d.push(fill);
        }
    }

    fn shorty(desc: &str) -> char {
        match desc.as_bytes()[0] {
            b'[' => 'L',
            c => c as char,
        }
    }

    /// An instruction array: instructions of any opcode with any fields (a `nop` with a zero high
    /// byte), and now and then a payload of each kind at the end.
    fn instructions(rng: &mut Rng) -> Vec<u16> {
        let mut u = Vec::new();
        for _ in 0..rng.below(12) {
            let op = 1 + rng.below(255) as u16;
            let (n, _, _) = aqr_dex::code::format(op as u8);
            u.push(op | (rng.u64() as u16) << 8);
            for _ in 1..n {
                // (small values mostly, as indices and registers are)
                u.push(if rng.below(2) == 0 { rng.below(8) as u16 } else { rng.u64() as u16 });
            }
        }
        if rng.below(2) == 0 {
            u.push(0);
        }
        match rng.below(4) {
            0 => {
                let n = rng.below(4) as u16;
                u.extend([0x0100, n, 7, 0]);
                u.extend((0..2 * n).map(|_| rng.u64() as u16));
            }
            1 => {
                let n = rng.below(4) as u16;
                u.extend([0x0200, n]);
                u.extend((0..4 * n).map(|_| rng.u64() as u16));
            }
            2 => {
                let (w, n) = (1 + rng.below(4) as u16, rng.below(5) as u16);
                u.extend([0x0300, w, n, 0]);
                u.extend((0..(w * n).div_ceil(2)).map(|_| rng.u64() as u16));
            }
            _ => {}
        }
        if u.is_empty() {
            u.push(0x000e);
        }
        u
    }

    /// A DEX file of classes made from `seed`.
    pub fn write(seed: u64, shape: Shape) -> Vec<u8> {
        let mut rng = Rng::new(seed);
        let classes = 3 + rng.below(12) as usize;
        let mut descs: Vec<String> =
            ["V", "I", "J", "Z", "Ljava/lang/Object;", "[I"].iter().map(|s| s.to_string()).collect();
        for k in 0..classes {
            descs.push(format!("La/C{k};"));
        }
        descs.push("[La/C0;".into());
        // Prototypes: a return type and up to three parameters, as descriptors.
        let mut protos: Vec<(usize, Vec<usize>)> = (0..4 + rng.below(10))
            .map(|_| {
                let ret = rng.below(descs.len() as u64) as usize;
                let params = (0..rng.below(4)).map(|_| 1 + rng.below(descs.len() as u64 - 1) as usize).collect();
                (ret, params)
            })
            .collect();
        let names = ["<init>", "run", "x", "\u{e9}t\u{e9}", "\u{d55c}\u{ae00}", "\u{1f600}", "value", "A.java"];
        let mut strings: Vec<Vec<u8>> = descs.iter().map(|d| d.as_bytes().to_vec()).collect();
        for (r, p) in &protos {
            strings.push(
                std::iter::once(&descs[*r]).chain(p.iter().map(|&t| &descs[t])).map(|d| shorty(d) as u8).collect(),
            );
        }
        let mut name_bytes = Vec::new();
        for n in names {
            // (MUTF-8: characters past U+FFFF as their surrogates, three bytes each)
            let mut b = Vec::new();
            for c in n.encode_utf16() {
                match c {
                    0..0x80 => b.push(c as u8),
                    0x80..0x800 => b.extend([0xc0 | (c >> 6) as u8, 0x80 | (c & 0x3f) as u8]),
                    _ => b.extend([0xe0 | (c >> 12) as u8, 0x80 | (c >> 6 & 0x3f) as u8, 0x80 | (c & 0x3f) as u8]),
                }
            }
            name_bytes.push(b.clone());
            strings.push(b);
        }
        strings.sort();
        strings.dedup();
        let sidx = |s: &[u8]| strings.iter().position(|t| t == s).unwrap() as u32;
        let mut types: Vec<u32> = descs.iter().map(|d| sidx(d.as_bytes())).collect();
        types.sort();
        let tidx = |d: &str| types.iter().position(|&t| t == sidx(d.as_bytes())).unwrap() as u16;
        let mut proto_rows: Vec<(u32, u32, Vec<u16>)> = protos
            .drain(..)
            .map(|(r, p)| {
                let sh: Vec<u8> =
                    std::iter::once(&descs[r]).chain(p.iter().map(|&t| &descs[t])).map(|d| shorty(d) as u8).collect();
                (sidx(&sh), tidx(&descs[r]) as u32, p.iter().map(|&t| tidx(&descs[t])).collect())
            })
            .collect();
        proto_rows.sort_by(|a, b| (a.1, &a.2).cmp(&(b.1, &b.2)));
        proto_rows.dedup_by(|a, b| (a.1, &a.2) == (b.1, &b.2));
        let class_t: Vec<u16> = (0..classes).map(|k| tidx(&format!("La/C{k};"))).collect();
        let name_ids: Vec<u32> = name_bytes.iter().map(|b| sidx(b)).collect();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        for &c in &class_t {
            for _ in 0..rng.below(4) {
                fields.push((
                    c,
                    1 + rng.below(types.len() as u64 - 1) as u16,
                    name_ids[rng.below(name_ids.len() as u64) as usize],
                ));
            }
            for _ in 0..rng.below(6) {
                methods.push((
                    c,
                    rng.below(proto_rows.len() as u64) as u16,
                    name_ids[rng.below(name_ids.len() as u64) as usize],
                ));
            }
        }
        for t in [&mut fields, &mut methods] {
            t.sort_by_key(|&(c, x, n)| (c, n, x));
            t.dedup();
        }

        // Type lists: the prototypes' parameters and the classes' interfaces.
        let ifaces: Vec<Vec<u16>> = (0..classes)
            .map(|_| (0..rng.below(3)).map(|_| class_t[rng.below(classes as u64) as usize]).collect())
            .collect();
        let mut lists: Vec<Vec<u16>> =
            proto_rows.iter().map(|p| p.2.clone()).chain(ifaces.iter().cloned()).filter(|l| !l.is_empty()).collect();
        lists.sort();
        lists.dedup();

        // The data section: code items, type lists, string data, classes' data; then the map.
        let header_len = 0x70;
        let ids_len = 4 * strings.len()
            + 4 * types.len()
            + 12 * proto_rows.len()
            + 8 * fields.len()
            + 8 * methods.len()
            + 32 * classes;
        let mut d = vec![0u8; header_len + ids_len];
        let data_off = d.len();
        // Each class's methods, direct then virtual, with code or none.
        let mut class_methods: Vec<Vec<(u32, u32, Option<usize>)>> = vec![Vec::new(); classes];
        let mut code_items = Vec::new();
        for (k, &c) in class_t.iter().enumerate() {
            for (i, m) in methods.iter().enumerate().filter(|(_, m)| m.0 == c) {
                let _ = m;
                let has_code = rng.below(5) != 0;
                class_methods[k].push((
                    i as u32,
                    if rng.below(2) == 0 { 1 } else { 0x10001 },
                    has_code.then(|| {
                        code_items.push(k);
                        code_items.len() - 1
                    }),
                ));
            }
        }
        let mut order: Vec<usize> = (0..code_items.len()).collect();
        if shape.code_order {
            for i in (1..order.len()).step_by(3) {
                order.swap(i - 1, i);
            }
        }
        let mut code_off = vec![0u32; code_items.len()];
        let code_start = d.len();
        for &j in &order {
            pad4(&mut d, 0);
            code_off[j] = d.len() as u32;
            let units = instructions(&mut rng);
            let insns = units.len() as u32;
            let tries = (rng.below(3) == 0) as u16;
            for v in [1 + rng.below(8) as u16, rng.below(3) as u16, rng.below(3) as u16, tries] {
                d.extend_from_slice(&v.to_le_bytes());
            }
            d.extend_from_slice(&0u32.to_le_bytes());
            d.extend_from_slice(&insns.to_le_bytes());
            for u in &units {
                d.extend_from_slice(&u.to_le_bytes());
            }
            if tries > 0 {
                if insns % 2 == 1 {
                    d.extend_from_slice(&[0, 0]);
                }
                d.extend_from_slice(&0u32.to_le_bytes());
                d.extend_from_slice(&(insns as u16).to_le_bytes());
                d.extend_from_slice(&1u16.to_le_bytes());
                uleb(&mut d, 1);
                let size = rng.below(3) as i32 - 1;
                sleb(&mut d, size);
                for _ in 0..size.unsigned_abs() {
                    uleb(&mut d, class_t[0] as u32);
                    uleb(&mut d, rng.below(insns as u64) as u32);
                }
                if size <= 0 {
                    uleb(&mut d, 0);
                }
            }
        }
        let code_end = d.len();
        if shape.unknown {
            d.extend_from_slice(&[0xee; 3]);
        }
        pad4(&mut d, 0);
        let lists_start = d.len();
        let mut list_off = Vec::new();
        for l in &lists {
            pad4(&mut d, 0);
            list_off.push(d.len() as u32);
            d.extend_from_slice(&(l.len() as u32).to_le_bytes());
            for t in l {
                d.extend_from_slice(&t.to_le_bytes());
            }
        }
        let list_of = |l: &[u16]| if l.is_empty() { 0 } else { list_off[lists.iter().position(|x| x == l).unwrap()] };
        let strings_start = d.len();
        let mut string_off = Vec::new();
        for s in &strings {
            string_off.push(d.len() as u32);
            uleb(&mut d, s.iter().filter(|&&b| b & 0xc0 != 0x80).count() as u32);
            d.extend_from_slice(s);
            d.push(0);
        }
        let unknown_start = d.len();
        if shape.unknown {
            d.extend_from_slice(b"\x01\x02unknown section");
        }
        let data_start = d.len();
        let mut data_order: Vec<usize> = (0..classes).filter(|&k| !class_methods[k].is_empty() || k % 2 == 0).collect();
        let with_data = data_order.clone();
        if shape.data_order && data_order.len() >= 3 {
            data_order.swap(0, 2);
        }
        let mut class_data_off = vec![0u32; classes];
        for (n, &k) in data_order.iter().enumerate() {
            class_data_off[k] = d.len() as u32;
            let cf: Vec<usize> = (0..fields.len()).filter(|&i| fields[i].0 == class_t[k]).collect();
            let (st, inst) = cf.split_at(cf.len() / 2);
            let ms = &class_methods[k];
            let (direct, virt) = ms.split_at(ms.len() / 2);
            if shape.long_leb && n == 0 {
                d.extend_from_slice(&[st.len() as u8 | 0x80, 0]);
            } else {
                uleb(&mut d, st.len() as u32);
            }
            for v in [inst.len(), direct.len(), virt.len()] {
                uleb(&mut d, v as u32);
            }
            for list in [st, inst] {
                let mut prev = 0;
                for &i in list {
                    uleb(&mut d, i as u32 - prev);
                    uleb(&mut d, 8);
                    prev = i as u32;
                }
            }
            for list in [direct, virt] {
                let mut prev = 0;
                for &(i, flags, code) in list {
                    uleb(&mut d, i - prev);
                    uleb(&mut d, flags);
                    uleb(&mut d, code.map_or(0, |j| code_off[j]));
                    prev = i;
                }
            }
        }
        let data_end = d.len();
        pad4(&mut d, 0);
        let map_off = d.len();

        // The ids.
        let mut p = header_len;
        let mut put = |d: &mut Vec<u8>, b: &[u8]| {
            d[p..p + b.len()].copy_from_slice(b);
            p += b.len();
        };
        let string_ids = header_len;
        for o in &string_off {
            put(&mut d, &o.to_le_bytes());
        }
        let type_ids = string_ids + 4 * strings.len();
        for t in &types {
            put(&mut d, &t.to_le_bytes());
        }
        let proto_ids = type_ids + 4 * types.len();
        for (sh, r, params) in &proto_rows {
            for v in [*sh, *r, list_of(params)] {
                put(&mut d, &v.to_le_bytes());
            }
        }
        let field_ids = proto_ids + 12 * proto_rows.len();
        let method_ids = field_ids + 8 * fields.len();
        for &(c, x, n) in fields.iter().chain(&methods) {
            put(&mut d, &c.to_le_bytes());
            put(&mut d, &x.to_le_bytes());
            put(&mut d, &n.to_le_bytes());
        }
        let class_defs = method_ids + 8 * methods.len();
        let source = sidx(b"A.java");
        for k in 0..classes {
            let sup = if k == 0 { u32::MAX } else { tidx("Ljava/lang/Object;") as u32 };
            let src = if k % 3 == 2 { u32::MAX } else { source };
            for v in [class_t[k] as u32, 1, sup, list_of(&ifaces[k]), src, 0, class_data_off[k], 0] {
                put(&mut d, &v.to_le_bytes());
            }
        }
        assert!(with_data.iter().all(|&k| class_data_off[k] != 0));

        // The map.
        let mut map: Vec<(u16, u32, u32)> = vec![
            (0x0000, 1, 0),
            (0x0001, strings.len() as u32, string_ids as u32),
            (0x0002, types.len() as u32, type_ids as u32),
            (0x0003, proto_rows.len() as u32, proto_ids as u32),
            (0x0004, fields.len() as u32, field_ids as u32),
            (0x0005, methods.len() as u32, method_ids as u32),
            (0x0006, classes as u32, class_defs as u32),
            (0x2001, code_items.len() as u32, code_start as u32),
            (0x1001, lists.len() as u32, lists_start as u32),
            (0x2002, strings.len() as u32, strings_start as u32),
            (0x2000, data_order.len() as u32, data_start as u32),
            (0x1000, 1, map_off as u32),
        ];
        if shape.unknown {
            map.push((0x7777, 1, unknown_start as u32));
        }
        map.retain(|e| e.1 > 0);
        map.sort_by_key(|e| e.2);
        let _ = (code_end, data_end);
        d.extend_from_slice(&(map.len() as u32).to_le_bytes());
        for (t, n, o) in &map {
            d.extend_from_slice(&t.to_le_bytes());
            d.extend_from_slice(&[0, 0]);
            d.extend_from_slice(&n.to_le_bytes());
            d.extend_from_slice(&o.to_le_bytes());
        }

        // The header.
        let len = d.len() as u32;
        d[..8].copy_from_slice(b"dex\n035\0");
        let fields_h = [
            len,
            0x70,
            0x12345678,
            0,
            0,
            map_off as u32,
            strings.len() as u32,
            string_ids as u32,
            types.len() as u32,
            type_ids as u32,
            proto_rows.len() as u32,
            proto_ids as u32,
            fields.len() as u32,
            if fields.is_empty() { 0 } else { field_ids as u32 },
            methods.len() as u32,
            if methods.is_empty() { 0 } else { method_ids as u32 },
            classes as u32,
            class_defs as u32,
            len - data_off as u32,
            data_off as u32,
        ];
        for (i, v) in fields_h.iter().enumerate() {
            d[0x20 + 4 * i..0x24 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        let sig = aqr_git::pack::sha1(&d[32..]);
        d[12..32].copy_from_slice(&sig);
        let sum = aqr_common::adler32::hash(&d[12..]);
        d[8..12].copy_from_slice(&sum.to_le_bytes());
        if shape.bad_sums {
            d[8] ^= 1;
            d[20] ^= 1;
        }
        d
    }
}

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
