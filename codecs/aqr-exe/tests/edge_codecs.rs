//! Edge cases of the codecs: long structured x86 code (dense branches, origins near 2^32 and
//! 2^63, side output) through the whole and the part decodings, and the decoders of every
//! codec of the crate on damaged streams: no panic, no hang, no allocation the data cannot
//! hold, and what comes out of an encoder comes back as it was.

use aqr_exe::arch::x86::{self, Opcodes};
use aqr_exe::arch::{arm64, cil};
use aqr_exe::{codesign, module, reloc, symtab};
use aqr_testkit::{damaged, Refusing};
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(30);

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 32) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// x86 code of `n` bytes, mostly branches: CALL and JMP rel32, Jcc rel32 and rel8, indirect
/// jumps through memory, RIP-relative LEAs, switch tables' loads, in a small range of
/// targets so that many of them land on instructions; with a few noise bytes between.
fn structured(n: usize, seed: u64, dense: bool) -> Vec<u8> {
    let mut r = Rng(seed);
    let mut d = Vec::with_capacity(n + 16);
    while d.len() < n {
        let near = |r: &mut Rng, range: u32| (r.below(range) as i32 - range as i32 / 2).to_le_bytes();
        match r.below(if dense { 9 } else { 14 }) {
            0 => d.extend([&[0xe8][..], &near(&mut r, 1 << 16)].concat()),
            1 => d.extend([&[0xe9][..], &near(&mut r, 1 << 12)].concat()),
            2 => d.extend([&[0x0f, 0x80 + r.below(16) as u8][..], &near(&mut r, 1 << 12)].concat()),
            3 => d.extend([0x70 + r.below(16) as u8, r.next() as u8]),
            4 => d.extend([0xeb, r.next() as u8]),
            5 => d.extend([&[0xff, 0x25][..], &near(&mut r, 1 << 20)].concat()),
            6 => d.extend([&[0x48, 0x8d, 0x05][..], &near(&mut r, 1 << 20)].concat()),
            7 => d.extend([&[0xe8][..], &near(&mut r, 64)].concat()),
            8 => d.extend([0xe3, r.next() as u8]),
            9 => d.extend([0x55, 0x48, 0x89, 0xe5]),
            10 => d.extend([0x31, 0xc0, 0xc3]),
            11 => d.extend([&[0xc7, 0x05][..], &near(&mut r, 1 << 20), &r.next().to_le_bytes()].concat()),
            12 => d.extend(std::iter::repeat_n(0xcc, r.below(8) as usize)),
            _ => d.extend((0..r.below(6)).map(|_| r.next() as u8)),
        }
    }
    d.truncate(n);
    d
}

fn params(origin: u64, sentinel: u8, is64: bool) -> Vec<u8> {
    let mut p = origin.to_le_bytes().to_vec();
    p.extend([sentinel, is64 as u8]);
    p
}

/// The encoder's streams of `d` decode to `d` by the whole and by the part module (with a side
/// output of `fields`, too, where given); where the encoder refuses, nothing is asserted.
fn x86_same(what: &str, d: &[u8], origin: u64, is64: bool, fields: Option<x86::Fields>) {
    let e = match fields {
        None => x86::encode(d, origin, is64),
        Some(f) => x86::encode_fields(d, origin, is64, f),
    };
    let Some(e) = e else { return };
    let streams: Vec<&[u8]> = e.streams().iter().map(|s| &s[..]).collect();
    let res = Opcodes::BUILTIN.to_resource();
    let mut p = params(origin, e.sentinel_size(), is64);
    let side = e.side().len() as u64;
    if let Some(f) = fields {
        p.extend(f.base.to_le_bytes());
        p.extend((f.lo as u32).to_le_bytes());
        p.extend((f.hi as u32).to_le_bytes());
    }
    let want = [d, e.side()].concat();
    let whole = module::x86(&p, &[&res], &streams, d.len() as u64 + side);
    assert!(whole.as_deref() == Ok(&want[..]), "{what}: whole");
    let part = module::x86_streamed(&p, &[&res], &streams, [d.len() as u64, side]);
    assert!(part.as_deref() == Ok(&want[..]), "{what}: part");
}

#[test]
fn x86_structured_long() {
    // lengths of 1 to 64 KiB (and either side of the part module's 64 KiB), the origins of a
    // low image, one that ends at 2^32, and ones near 2^63 and the top of the address space
    let origins =
        [0u64, 0x40_1000, 0xffff_f000, 0xffff_ff00, 0x1_0000_0000, (1 << 63) - 0x1000, 1 << 63, u64::MAX - 0x2000];
    for (k, &n) in [1usize, 5, 15, 16, 17, 255, 4097, 20_000, 65_535, 65_536, 65_537, 1 << 17].iter().enumerate() {
        for (j, &origin) in origins.iter().enumerate() {
            let seed = (k * 16 + j) as u64;
            for is64 in [true, false] {
                let d = structured(n, seed, j % 2 == 0);
                x86_same(&format!("{n} bytes, origin {origin:#x}, 64: {is64}"), &d, origin, is64, None);
            }
        }
    }
}

#[test]
fn x86_structured_side_output() {
    let all = |base| x86::Fields { base, lo: 0, hi: u32::MAX as u64 };
    for (k, &n) in [3usize, 100, 5000, 70_000].iter().enumerate() {
        let d = structured(n, 99 + k as u64, false);
        for &origin in &[0x40_1000u64, 0xffff_f000, 0x1_4000_1000, (1 << 63) - 0x100] {
            x86_same(&format!("side {n} at {origin:#x}"), &d, origin, false, Some(all(origin & !0xfff)));
            x86_same(&format!("side {n} at {origin:#x}, 64"), &d, origin, true, Some(all(origin & !0xfff)));
        }
        // a range at the top of 32 bits and one empty
        let top = x86::Fields { base: 0xffff_0000, lo: 0, hi: 0xffff };
        x86_same(&format!("side {n}, top"), &d, 0xffff_1000, false, Some(top));
        let none = x86::Fields { base: 0, lo: 5, hi: 4 };
        x86_same(&format!("side {n}, empty"), &d, 0x1000, false, Some(none));
    }
}

#[test]
fn x86_streams_damaged() {
    let d = structured(3000, 7, false);
    let e = x86::encode(&d, 0x40_1000, true).unwrap();
    let res = Opcodes::BUILTIN.to_resource();
    let p = params(0x40_1000, e.sentinel_size(), true);
    let base: Vec<Vec<u8>> = e.streams().to_vec();
    for st in 0..x86::NUM_STREAMS {
        damaged(&base[st], 6, 64, LIMIT, |v| {
            let mut s = base.clone();
            s[st] = v.to_vec();
            let r: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
            let a = module::x86(&p, &[&res], &r, d.len() as u64);
            let b = module::x86_streamed(&p, &[&res], &r, [d.len() as u64, 0]);
            assert!(a == b, "stream {st}: the whole and the part module differ");
        });
    }
    // params damaged
    damaged(&p, 4, 12, LIMIT, |v| {
        let r: Vec<&[u8]> = base.iter().map(|v| &v[..]).collect();
        let a = module::x86(v, &[&res], &r, d.len() as u64);
        let b = module::x86_streamed(v, &[&res], &r, [d.len() as u64, 0]);
        assert!(a == b, "params: the whole and the part module differ");
    });
}

#[test]
fn arm64_streams_damaged() {
    let mut r = Rng(5);
    let mut d = Vec::new();
    while d.len() < 4000 {
        let w = match r.below(5) {
            0 => 0x9400_0000 | r.next() & 0x03ff_ffff,
            1 => 0x9000_0000 | r.next() & 0x60ff_ffe0,
            2 => 0x1400_0000 | r.next() & 0x03ff_ffff,
            3 => 0x5400_0000 | r.next() & 0x00ff_ffff,
            _ => r.next(),
        };
        d.extend(w.to_le_bytes());
    }
    // origins near 2^63 and the top
    for origin in [0u64, 0xffff_f000, 1 << 63, u64::MAX - 0x1000] {
        let e = arm64::encode(&d, origin);
        let streams: Vec<&[u8]> = e.streams().iter().map(|s| &s[..]).collect();
        assert_eq!(arm64::decode(std::array::from_fn(|i| streams[i]), origin, d.len()).as_deref(), Some(&d[..]));
    }
    let e = arm64::encode(&d, 0x4000);
    let base: Vec<Vec<u8>> = e.streams().to_vec();
    let p = 0x4000u64.to_le_bytes();
    for st in 0..arm64::NUM_STREAMS {
        damaged(&base[st], 6, 64, LIMIT, |v| {
            let mut s = base.clone();
            s[st] = v.to_vec();
            let r: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
            let a = module::arm64(&p, &r, d.len() as u64);
            let b = module::arm64_streamed(&p, &r, d.len() as u64);
            assert!(a == b, "stream {st}: the whole and the part module differ");
        });
    }
}

// ---- relocations ----

/// Base relocations of `pages` pages with random entries, blocks padded to 4 bytes.
fn relocs(seed: u64, pages: usize) -> Vec<u8> {
    let mut r = Rng(seed);
    let mut d = Vec::new();
    let mut page = 0x1000u32;
    for _ in 0..pages {
        let n = 1 + r.below(40) as usize;
        let mut offs: Vec<u16> = (0..n).map(|_| r.below(0x1000) as u16).collect();
        offs.sort_unstable();
        offs.dedup();
        let mut entries: Vec<u16> = offs.iter().map(|&o| if r.below(8) == 0 { o } else { 0xa000 | o }).collect();
        if entries.len() % 2 == 1 {
            entries.push(0);
        }
        d.extend(page.to_le_bytes());
        d.extend((8 + 2 * entries.len() as u32).to_le_bytes());
        d.extend(entries.iter().flat_map(|e| e.to_le_bytes()));
        page += 0x1000 * (1 + r.below(3));
    }
    d
}

#[test]
fn reloc_round_trip_and_damaged() {
    for seed in 0..8 {
        let d = relocs(seed, 1 + seed as usize * 20);
        let Some([rvas, types]) = reloc::encode(&d, 2) else { panic!("seed {seed}") };
        assert_eq!(reloc::decode(&rvas, &types, 2, d.len()).as_deref(), Some(&d[..]), "seed {seed}");
    }
    let d = relocs(3, 30);
    // any damaged relocations: the encoder refuses, or what it makes comes back as it was
    damaged(&d, 4, 64, LIMIT, |v| {
        if let Some([rvas, types]) = reloc::encode(v, 2) {
            assert_eq!(reloc::decode(&rvas, &types, 2, v.len()).as_deref(), Some(v));
        }
    });
    let [rvas, types] = reloc::encode(&d, 2).unwrap();
    damaged(&rvas, 6, 64, LIMIT, |v| {
        let _ = reloc::decode(v, &types, 2, d.len());
    });
    damaged(&types, 6, 64, LIMIT, |v| {
        let _ = reloc::decode(&rvas, v, 2, d.len());
    });
    for align in [0u8, 1, 3, 7, 31, 63, 255] {
        let _ = reloc::encode(&d, align);
        let _ = reloc::decode(&rvas, &types, align, d.len());
    }
}

// ---- symbol tables ----

/// A table of `n` `nlist_64` entries of stabs and symbols of sections, and the strings in the
/// order the encoder wants them: of the entries from `n / 2` on, then the others.
fn symbols(n: usize, seed: u64) -> (Vec<u8>, Vec<u8>, u32) {
    let mut r = Rng(seed);
    let ext = n / 2;
    let mut entries = Vec::new();
    let mut addr = 0x1_0000_0000u64;
    for i in 0..n {
        let named = r.below(6) != 0;
        let (ty, sect) = match r.below(4) {
            0 => (0x2eu8, 1u8),
            1 => (0x24, 0),
            2 => (0x0f, 1),
            _ => (0x0e, 2),
        };
        addr += r.below(200) as u64;
        entries.push((named.then(|| format!("_symbol_{}_{}", i, r.below(50))), ty, sect, r.below(3) as u16, addr));
    }
    let mut strtab = b" ".to_vec();
    let mut strx = vec![0u32; n];
    for i in (ext..n).chain(0..ext) {
        if let Some(name) = &entries[i].0 {
            strx[i] = strtab.len() as u32;
            strtab.extend(name.bytes());
            strtab.push(0);
        }
    }
    let mut tab = Vec::new();
    for (i, (_, ty, sect, desc, addr)) in entries.into_iter().enumerate() {
        tab.extend(strx[i].to_le_bytes());
        tab.extend([ty, sect]);
        tab.extend(desc.to_le_bytes());
        tab.extend(addr.to_le_bytes());
    }
    (tab, strtab, ext as u32)
}

#[test]
fn symtab_round_trip_and_damaged() {
    for (n, seed) in [(0usize, 1u64), (1, 2), (10, 3), (500, 4)] {
        let (tab, strtab, ext) = symbols(n, seed);
        let Some((params, st)) = symtab::encode(&tab, &strtab, ext) else { continue };
        let streams = std::array::from_fn(|i| &st[i][..]);
        assert_eq!(symtab::decode(&params, streams, tab.len() + strtab.len()), Some([&tab[..], &strtab[..]].concat()));
    }
    let (tab, strtab, ext) = symbols(60, 9);
    let (params, st) = symtab::encode(&tab, &strtab, ext).expect("encoded");
    let len = tab.len() + strtab.len();
    for i in 0..symtab::NUM_STREAMS {
        damaged(&st[i], 6, 64, LIMIT, |v| {
            let mut s: [&[u8]; symtab::NUM_STREAMS] = std::array::from_fn(|k| &st[k][..]);
            s[i] = v;
            let _ = symtab::decode(&params, s, len);
        });
    }
    damaged(&params, 6, 64, LIMIT, |v| {
        let _ = symtab::decode(v, std::array::from_fn(|k| &st[k][..]), len);
    });
    // the entries and the strings damaged: the encoder refuses or round-trips
    damaged(&tab, 4, 64, LIMIT, |v| {
        if let Some((p, s)) = symtab::encode(v, &strtab, ext) {
            let got = symtab::decode(&p, std::array::from_fn(|k| &s[k][..]), v.len() + strtab.len());
            assert_eq!(got, Some([v, &strtab[..]].concat()));
        }
    });
    damaged(&strtab, 4, 64, LIMIT, |v| {
        if let Some((p, s)) = symtab::encode(&tab, v, ext) {
            let got = symtab::decode(&p, std::array::from_fn(|k| &s[k][..]), tab.len() + v.len());
            assert_eq!(got, Some([&tab[..], v].concat()));
        }
    });
    // an external count past the entries, and 0
    for e in [0u32, 61, u32::MAX] {
        let _ = symtab::encode(&tab, &strtab, e);
    }
}

// ---- code signatures ----

/// A signature (a SuperBlob of one CodeDirectory of SHA-256 over 4 KiB pages) for `code`,
/// after it.
fn signed(code: &[u8]) -> (Vec<u8>, (usize, usize, usize)) {
    use sha2::{Digest, Sha256};
    let pages = code.len().div_ceil(4096);
    let mut cd = vec![0u8; 44 + 32 * pages];
    cd[0..4].copy_from_slice(&0xfade_0c02u32.to_be_bytes());
    let cdlen = cd.len() as u32;
    cd[4..8].copy_from_slice(&cdlen.to_be_bytes());
    cd[16..20].copy_from_slice(&44u32.to_be_bytes());
    cd[28..32].copy_from_slice(&(pages as u32).to_be_bytes());
    cd[32..36].copy_from_slice(&(code.len() as u32).to_be_bytes());
    cd[36] = 32;
    cd[37] = 2;
    cd[39] = 12;
    for (i, page) in code.chunks(4096).enumerate() {
        cd[44 + 32 * i..76 + 32 * i].copy_from_slice(&Sha256::digest(page));
    }
    let mut sig = vec![0u8; 20];
    sig[0..4].copy_from_slice(&0xfade_0cc0u32.to_be_bytes());
    sig[8..12].copy_from_slice(&1u32.to_be_bytes());
    sig[16..20].copy_from_slice(&20u32.to_be_bytes());
    sig.extend(cd);
    let n = sig.len();
    sig[4..8].copy_from_slice(&(n as u32).to_be_bytes());
    ([code, &sig[..]].concat(), (0, code.len(), n))
}

#[test]
fn codesign_damaged() {
    let code: Vec<u8> = (0..9000u32).map(|i| ((i * 31) >> 2) as u8).collect();
    let (d, sig) = signed(&code);
    let s = codesign::encode(&d, &[sig]).expect("encoded");
    let body = [&d[..sig.1]].concat();
    assert_eq!(codesign::decode(&s.params, &body, &s.sigs, d.len()).as_deref(), Some(&d[..]));
    damaged(&s.params, 8, 64, LIMIT, |v| {
        let _ = codesign::decode(v, &body, &s.sigs, d.len());
    });
    damaged(&s.sigs, 8, 64, LIMIT, |v| {
        let _ = codesign::decode(&s.params, &body, v, body.len() + v.len());
    });
    // the signatures of damaged files: refused, or a round trip
    damaged(&d, 6, 128, LIMIT, |v| {
        let at = v.len().min(sig.1);
        let n = v.len() - at;
        if let Some(s) = codesign::encode(v, &[(0, at, n)]) {
            let mut body = Vec::new();
            let mut p = 0;
            for r in &s.ranges {
                body.extend_from_slice(&v[p..r.start]);
                p = r.end;
            }
            body.extend_from_slice(&v[p..]);
            assert_eq!(codesign::decode(&s.params, &body, &s.sigs, v.len()).as_deref(), Some(v));
        }
    });
}

// ---- CIL ----

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

const ORIGIN: u32 = 0x2000;

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

fn cil_streams(d: &[u8], meta: usize) -> Option<Vec<Vec<u8>>> {
    cil::encode(d, ORIGIN, meta).map(|e| e.streams().to_vec())
}

fn cil_decode(s: &[Vec<u8>], meta: usize, len: usize) -> Option<Vec<u8>> {
    cil::decode(std::array::from_fn(|i| &s[i][..]), ORIGIN, meta, len)
}

/// The section as the encoder takes it comes back; where the encoder refuses, nothing is said.
fn cil_round_trip(what: &str, d: &[u8], meta: usize) -> bool {
    let Some(s) = cil_streams(d, meta) else { return false };
    assert_eq!(cil_decode(&s, meta, d.len()).as_deref(), Some(d), "{what}");
    true
}

#[test]
fn cil_heap_sizes_and_tables() {
    // every combination of 4-byte heap indices
    for heap_sizes in 0..8u8 {
        let (d, meta) = section(heap_sizes, 3, true, b"#~", 0);
        assert!(cil_round_trip(&format!("heap sizes {heap_sizes}"), &d, meta), "heap sizes {heap_sizes} refused");
    }
    // no bodies at all, and fat sections of clauses (1, many)
    let (d, meta) = section(0, 3, false, b"#~", 0);
    assert!(cil_round_trip("no bodies", &d, meta));
    for clauses in [1usize, 2, 10, 200] {
        let (d, meta) = section(0, 2, true, b"#~", clauses);
        assert!(cil_round_trip(&format!("{clauses} fat clauses"), &d, meta), "{clauses} fat clauses refused");
    }
    // tables over 65535 rows (coded indices switch to 4 bytes) and an empty one
    for rows in [0u32, 1, 0xfffe, 0xffff, 0x1_0000, 70_000] {
        let (d, meta) = section(0, rows, true, b"#~", 0);
        assert!(cil_round_trip(&format!("{rows} rows"), &d, meta), "{rows} rows refused");
    }
    // an uncompressed metadata stream (#-)
    let (d, meta) = section(0, 3, true, b"#-", 0);
    assert!(cil_round_trip("#-", &d, meta), "#- refused");
}

#[test]
fn cil_damaged() {
    for (heap_sizes, eh) in [(0u8, 0usize), (7, 0), (0, 3)] {
        let (d, meta) = section(heap_sizes, 6, true, b"#~", eh);
        assert!(cil_round_trip("base", &d, meta));
        // a damaged section: refused, or a round trip
        damaged(&d, 3, d.len(), LIMIT, |v| {
            let _ = cil_round_trip("damaged section", v, meta);
        });
        // damaged streams: no panic
        let base = cil_streams(&d, meta).unwrap();
        for st in 0..cil::NUM_STREAMS {
            damaged(&base[st], 6, 64, LIMIT, |v| {
                let mut s = base.clone();
                s[st] = v.to_vec();
                let _ = cil_decode(&s, meta, d.len());
            });
        }
    }
}

/// The alignment of the blocks is a parameter of the archive: any value is refused or taken,
/// never a shift out of range nor a block padded past the output's length.
#[test]
fn reloc_alignments_at_the_extremes() {
    let d = relocs(1, 3);
    let [rvas, types] = reloc::encode(&d, 2).unwrap();
    for align in 0..=255u8 {
        aqr_testkit::reset();
        let r = reloc::decode(&rvas, &types, align, d.len());
        assert!(align != 2 || r.as_deref() == Some(&d[..]), "align {align}");
        assert!(r.is_none() || align <= 2, "align {align}: blocks of 4 bytes are no blocks of a larger unit");
        assert!(aqr_testkit::most() < 1 << 28, "align {align}: {} bytes reserved", aqr_testkit::most());
        let _ = reloc::encode(&d, align);
    }
}
