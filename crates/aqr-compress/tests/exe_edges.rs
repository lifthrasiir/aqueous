//! Executables of odd structure through the whole of `compress` and `decompress`: PE, ELF and
//! Mach-O images whose sections overlap, lie past the end or at offset 0, whose directories or
//! counts are out of the ordinary, and universal files with architectures at one offset. Each is
//! archived and given back byte for byte (or stored as it is), none of it a panic or a hang.

use std::time::Duration;

use aqr_compress::{Options, compress};
use aqr_decompress::decompress;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(120);

/// `d` archived and given back, as the default options choose and as recompressed whether or not
/// that is smaller (the recompressors' formulas are then those of the tests).
fn round_trip(d: &[u8]) -> bool {
    [Options::default(), Options { smaller: false, ..Options::default() }].iter().all(|o| {
        let c = compress(d, o);
        decompress(&c, 1).is_ok_and(|b| b == d)
    })
}

fn given_back(name: &str, d: &[u8]) {
    aqr_testkit::case(name);
    let r = aqr_testkit::within(LIMIT, || round_trip(d));
    match r {
        Ok(true) => {}
        Ok(false) => panic!("{name}: {} bytes not given back", d.len()),
        Err(e) => panic!("{name}: {e}"),
    }
}

/// x86-64 code of calls and jumps (`n` bytes): what the filter takes.
fn code(n: usize, seed: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 8);
    let mut s = seed | 1;
    while v.len() < n {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        match s >> 29 {
            0..=2 => {
                v.push(0xe8);
                // (to one of 16 targets: absolute addresses repeat, relative ones do not)
                let target = 0x10000 + 0x137 * ((s >> 8) % 16);
                v.extend_from_slice(&target.wrapping_sub(v.len() as u32 + 4).to_le_bytes());
            }
            3 => v.extend_from_slice(&[0x48, 0x8b, 0x45, (s >> 8) as u8]),
            4 => v.extend_from_slice(&[0x55, 0x48, 0x89, 0xe5]),
            5 => v.extend_from_slice(&[0x0f, 0x84, (s >> 8) as u8, 0, 0, 0]),
            _ => v.push(0x90),
        }
    }
    v.truncate(n);
    v
}

fn put(v: &mut Vec<u8>, at: usize, x: &[u8]) {
    if v.len() < at + x.len() {
        v.resize(at + x.len(), 0);
    }
    v[at..at + x.len()].copy_from_slice(x);
}

struct Sec {
    rva: u32,
    raw_size: u32,
    raw_off: u32,
    flags: u32,
}

/// A PE image of `len` bytes: `machine`, optional header `magic`, `ndirs` as the directory
/// count, `sections` in the table (code at those within the file).
fn pe(machine: u16, magic: u16, ndirs: u32, sections: &[Sec], len: usize) -> Vec<u8> {
    let mut v = vec![0u8; 0x400];
    put(&mut v, 0, b"MZ");
    put(&mut v, 0x3c, &0x80u32.to_le_bytes());
    let p = 0x80;
    put(&mut v, p, b"PE\0\0");
    let opt_size: usize = if magic == 0x20b { 112 + 16 * 8 } else { 96 + 16 * 8 };
    put(&mut v, p + 4, &machine.to_le_bytes());
    put(&mut v, p + 6, &(sections.len() as u16).to_le_bytes());
    put(&mut v, p + 20, &(opt_size as u16).to_le_bytes());
    put(&mut v, p + 22, &0x22u16.to_le_bytes());
    let o = p + 24;
    put(&mut v, o, &magic.to_le_bytes());
    put(&mut v, o + 16, &0x1000u32.to_le_bytes());
    put(&mut v, o + 32, &0x1000u32.to_le_bytes());
    put(&mut v, o + 36, &0x200u32.to_le_bytes());
    put(&mut v, o + 56, &0x10000u32.to_le_bytes());
    put(&mut v, o + 60, &0x400u32.to_le_bytes());
    let dcount = if magic == 0x20b { o + 108 } else { o + 92 };
    put(&mut v, dcount, &ndirs.to_le_bytes());
    let table = o + opt_size;
    for (i, s) in sections.iter().enumerate() {
        let h = table + 40 * i;
        put(&mut v, h, format!(".s{i}").as_bytes());
        put(&mut v, h + 8, &s.raw_size.to_le_bytes());
        put(&mut v, h + 12, &s.rva.to_le_bytes());
        put(&mut v, h + 16, &s.raw_size.to_le_bytes());
        put(&mut v, h + 20, &s.raw_off.to_le_bytes());
        put(&mut v, h + 36, &s.flags.to_le_bytes());
    }
    v.resize(len.max(0x400), 0);
    for (i, s) in sections.iter().enumerate() {
        let off = s.raw_off as usize;
        if off < v.len() && off >= 0x400 {
            let n = (s.raw_size as usize).min(v.len() - off);
            let c = code(n, i as u32 + 7);
            put(&mut v, off, &c);
        }
    }
    v
}

const CODE: u32 = 0x6000_0020;
const DATA: u32 = 0xc000_0040;

fn s(rva: u32, raw_size: u32, raw_off: u32, flags: u32) -> Sec {
    Sec { rva, raw_size, raw_off, flags }
}

#[test]
fn pe_sections_odd() {
    let len = 0x6000;
    let one = || [s(0x1000, 0x4000, 0x400, CODE)];
    let many: Vec<_> = (0..40).map(|i| s(0x1000 * (i + 1), 0x100, 0x400 + 0x100 * i, CODE)).collect();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("plain", pe(0x8664, 0x20b, 16, &one(), len)),
        ("overlap", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x2000, 0x400, CODE), s(0x3000, 0x2000, 0x1400, CODE)], len)),
        ("same offset", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x2000, 0x400, CODE), s(0x3000, 0x2000, 0x400, CODE)], len)),
        ("nested", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x4000, 0x400, CODE), s(0x2000, 0x100, 0x800, CODE)], len)),
        ("past the file", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x4000, 0x5000, CODE)], len)),
        ("starts past the file", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x4000, 0x7000, CODE)], len)),
        ("size past the file", pe(0x8664, 0x20b, 16, &[s(0x1000, 0xffff_0000, 0x400, CODE)], len)),
        ("offset max", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x100, 0xffff_ffff, CODE)], len)),
        ("size and offset max", pe(0x8664, 0x20b, 16, &[s(0x1000, 0xffff_ffff, 0xffff_ffff, CODE)], len)),
        ("zero offset", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x1000, 0, CODE)], len)),
        ("zero offset and size", pe(0x8664, 0x20b, 16, &[s(0, 0, 0, CODE), s(0x1000, 0x1000, 0x400, CODE)], len)),
        ("headers", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x400, 0, CODE), s(0x2000, 0x1000, 0x40, CODE)], len)),
        ("no sections", pe(0x8664, 0x20b, 16, &[], len)),
        ("many sections", pe(0x8664, 0x20b, 16, &many, len)),
        ("dirs 0", pe(0x8664, 0x20b, 0, &one(), len)),
        ("dirs 17", pe(0x8664, 0x20b, 17, &one(), len)),
        ("dirs max", pe(0x8664, 0x20b, u32::MAX, &one(), len)),
        ("dirs max pe32", pe(0x14c, 0x10b, u32::MAX, &one(), len)),
        ("pe32 x64", pe(0x8664, 0x10b, 16, &one(), len)),
        ("pe32+ x86", pe(0x14c, 0x20b, 16, &one(), len)),
        ("arm64", pe(0xaa64, 0x20b, 16, &one(), len)),
        ("arm64ec", pe(0xa641, 0x20b, 16, &one(), len)),
        ("arm64x", pe(0xa64e, 0x20b, 16, &one(), len)),
        ("machine 0", pe(0, 0x20b, 16, &one(), len)),
        ("data only", pe(0x8664, 0x20b, 16, &[s(0x1000, 0x4000, 0x400, DATA)], len)),
        ("short file", pe(0x8664, 0x20b, 16, &one(), 0x500)),
    ];
    for (name, d) in &cases {
        given_back(name, d);
        // with an overlay, and embedded among other data
        let mut o = d.clone();
        o.extend(code(3000, 5));
        given_back(&format!("{name} + overlay"), &o);
        let mut e = b"prefix of some other data, text text text\n".repeat(50);
        e.extend_from_slice(d);
        e.extend_from_slice(&b"tail of the file\n".repeat(50));
        given_back(&format!("{name} embedded"), &e);
    }
}

#[test]
fn pe_damaged() {
    let d = pe(0x8664, 0x20b, 16, &[s(0x1000, 0x2000, 0x400, CODE), s(0x3000, 0x2000, 0x2400, DATA)], 0x4400);
    aqr_testkit::damaged(&d, 12, 0x200, LIMIT, |x| assert!(round_trip(x)));
}

// --- ELF ---

struct Sh {
    kind: u32,
    flags: u64,
    addr: u64,
    off: u64,
    size: u64,
}

/// An ELF file of `len` bytes (code from 0x200) and section headers `sh` after it; `shnum` and
/// `shstrndx` as the header says.
fn elf(wide: bool, ty: u16, machine: u16, sh: &[Sh], shnum: u16, shstrndx: u16, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len.max(0x200)];
    put(&mut v, 0, &[0x7f, b'E', b'L', b'F', if wide { 2 } else { 1 }, 1, 1, 0]);
    put(&mut v, 16, &ty.to_le_bytes());
    put(&mut v, 18, &machine.to_le_bytes());
    put(&mut v, 20, &1u32.to_le_bytes());
    let shoff = v.len();
    if wide {
        put(&mut v, 40, &(shoff as u64).to_le_bytes());
        put(&mut v, 52, &64u16.to_le_bytes());
        put(&mut v, 58, &64u16.to_le_bytes());
        put(&mut v, 60, &shnum.to_le_bytes());
        put(&mut v, 62, &shstrndx.to_le_bytes());
    } else {
        put(&mut v, 32, &(shoff as u32).to_le_bytes());
        put(&mut v, 40, &52u16.to_le_bytes());
        put(&mut v, 46, &40u16.to_le_bytes());
        put(&mut v, 48, &shnum.to_le_bytes());
        put(&mut v, 50, &shstrndx.to_le_bytes());
    }
    let c = code(len.saturating_sub(0x200), 3);
    put(&mut v, 0x200, &c);
    for (i, h) in sh.iter().enumerate() {
        if wide {
            let p = shoff + 64 * i;
            put(&mut v, p + 4, &h.kind.to_le_bytes());
            put(&mut v, p + 8, &h.flags.to_le_bytes());
            put(&mut v, p + 16, &h.addr.to_le_bytes());
            put(&mut v, p + 24, &h.off.to_le_bytes());
            put(&mut v, p + 32, &h.size.to_le_bytes());
            put(&mut v, p + 56, &16u64.to_le_bytes());
        } else {
            let p = shoff + 40 * i;
            put(&mut v, p + 4, &h.kind.to_le_bytes());
            put(&mut v, p + 8, &(h.flags as u32).to_le_bytes());
            put(&mut v, p + 12, &(h.addr as u32).to_le_bytes());
            put(&mut v, p + 16, &(h.off as u32).to_le_bytes());
            put(&mut v, p + 20, &(h.size as u32).to_le_bytes());
            put(&mut v, p + 36, &16u32.to_le_bytes());
        }
    }
    v
}

const AX: u64 = 6;

fn x(addr: u64, off: u64, size: u64) -> Sh {
    Sh { kind: 1, flags: AX, addr, off, size }
}

fn null() -> Sh {
    Sh { kind: 0, flags: 0, addr: 0, off: 0, size: 0 }
}

#[test]
fn elf_odd() {
    let len = 0x3000;
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    for (wide, machine) in [(true, 62u16), (false, 3), (true, 183), (false, 40)] {
        for ty in [2u16, 3, 1] {
            let t = format!("{}-{machine}-{ty}", if wide { 64 } else { 32 });
            let secs: [(&str, Vec<Sh>); 8] = [
                ("one", vec![null(), x(0x1000, 0x200, 0x2000)]),
                ("overlap", vec![null(), x(0x1000, 0x200, 0x1000), x(0x1800, 0x800, 0x1000)]),
                ("same", vec![null(), x(0x1000, 0x200, 0x1000), x(0x2000, 0x200, 0x1000)]),
                ("past", vec![null(), x(0x1000, 0x5000, 0x1000)]),
                ("zero off", vec![null(), x(0x1000, 0, 0x1000)]),
                ("huge", vec![null(), x(0x1000, 0x200, u64::MAX)]),
                ("huge off", vec![null(), x(0x1000, u64::MAX, 0x100), x(0x1000, u64::MAX - 1, u64::MAX)]),
                ("wrap", vec![null(), x(u64::MAX - 0x10, 0x200, 0x800), x(0x10, 0xa00, 0x800)]),
            ];
            for (n, sh) in secs {
                let k = sh.len() as u16;
                cases.push((format!("{t} {n}"), elf(wide, ty, machine, &sh, k, 0, len)));
                // header counts: zero (SHN_XINDEX's escape), more than the file holds, and a
                // name table that is none
                cases.push((format!("{t} {n} shnum 0"), elf(wide, ty, machine, &sh, 0, 0, len)));
                cases.push((format!("{t} {n} shnum 0xffff"), elf(wide, ty, machine, &sh, 0xffff, 0xffff, len)));
                cases.push((format!("{t} {n} strndx"), elf(wide, ty, machine, &sh, k, k + 3, len)));
            }
        }
    }
    // (x32: ELF32 class of x86-64)
    cases.push(("x32".into(), elf(false, 2, 62, &[null(), x(0x1000, 0x200, 0x2000)], 2, 0, len)));
    for (name, d) in &cases {
        given_back(name, d);
    }
}

#[test]
fn elf_xindex() {
    // section 0's size holds the real count, its link the string table's index
    let sh = [Sh { kind: 0, flags: 0, addr: 0, off: 0, size: 3 }, x(0, 0x200, 0x1000), x(0, 0x1200, 0x800)];
    let mut d = elf(true, 1, 62, &sh, 0, 0xffff, 0x3000);
    given_back("xindex", &d);
    let at = d.len() - 3 * 64;
    d[at + 40..at + 44].copy_from_slice(&2u32.to_le_bytes());
    given_back("xindex link", &d);
    aqr_testkit::damaged(&d, 12, 0x100, LIMIT, |y| assert!(round_trip(y)));
}

// --- Mach-O ---

struct Ms {
    addr: u64,
    size: u64,
    off: u32,
    flags: u32,
}

/// A thin Mach-O file of `len` bytes (code from 0x400) with one segment of `sects`, its count
/// given as `nsects`; `wide` for 64-bit.
fn macho_thin(wide: bool, cputype: u32, cpusub: u32, sects: &[Ms], nsects: u32, len: usize) -> Vec<u8> {
    let (head, seg, sect) = if wide { (32, 72, 80) } else { (28, 56, 68) };
    let cmdsize = seg + sect * sects.len();
    let mut v = vec![0u8; len.max(0x400)];
    put(&mut v, 0, &(if wide { 0xfeed_facfu32 } else { 0xfeed_face }).to_le_bytes());
    put(&mut v, 4, &cputype.to_le_bytes());
    put(&mut v, 8, &cpusub.to_le_bytes());
    put(&mut v, 12, &2u32.to_le_bytes());
    put(&mut v, 16, &1u32.to_le_bytes());
    put(&mut v, 20, &(cmdsize as u32).to_le_bytes());
    let p = head;
    put(&mut v, p, &(if wide { 0x19u32 } else { 1 }).to_le_bytes());
    put(&mut v, p + 4, &(cmdsize as u32).to_le_bytes());
    put(&mut v, p + 8, b"__TEXT");
    // (the segment holds the whole file, so that the file's extent is all of it)
    let n = v.len();
    if wide {
        put(&mut v, p + 48, &(n as u64).to_le_bytes());
    } else {
        put(&mut v, p + 36, &(n as u32).to_le_bytes());
    }
    put(&mut v, p + seg - 8, &nsects.to_le_bytes());
    let c = code(len.saturating_sub(0x400), 11);
    put(&mut v, 0x400, &c);
    for (i, m) in sects.iter().enumerate() {
        let s = p + seg + sect * i;
        put(&mut v, s, b"__text");
        put(&mut v, s + 16, b"__TEXT");
        if wide {
            put(&mut v, s + 32, &m.addr.to_le_bytes());
            put(&mut v, s + 40, &m.size.to_le_bytes());
            put(&mut v, s + 48, &m.off.to_le_bytes());
            put(&mut v, s + 64, &m.flags.to_le_bytes());
        } else {
            put(&mut v, s + 32, &(m.addr as u32).to_le_bytes());
            put(&mut v, s + 36, &(m.size as u32).to_le_bytes());
            put(&mut v, s + 40, &m.off.to_le_bytes());
            put(&mut v, s + 56, &m.flags.to_le_bytes());
        }
    }
    v
}

const PURE: u32 = 0x8000_0400;

fn m(addr: u64, size: u64, off: u32) -> Ms {
    Ms { addr, size, off, flags: PURE }
}

fn fat(wide: bool, archs: &[(u32, u64, u64, &[u8])]) -> Vec<u8> {
    let entry = if wide { 32 } else { 20 };
    let mut v = Vec::new();
    v.extend_from_slice(&(if wide { 0xcafe_babfu32 } else { 0xcafe_babe }).to_be_bytes());
    v.extend_from_slice(&(archs.len() as u32).to_be_bytes());
    for &(cpu, off, size, _) in archs {
        v.extend_from_slice(&cpu.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        if wide {
            v.extend_from_slice(&off.to_be_bytes());
            v.extend_from_slice(&size.to_be_bytes());
            v.extend_from_slice(&0u64.to_be_bytes());
        } else {
            v.extend_from_slice(&(off as u32).to_be_bytes());
            v.extend_from_slice(&(size as u32).to_be_bytes());
            v.extend_from_slice(&0u32.to_be_bytes());
        }
    }
    assert_eq!(v.len(), 8 + entry * archs.len());
    for &(_, off, _, data) in archs {
        put(&mut v, off as usize, data);
    }
    v
}

const X64: u32 = 0x0100_0007;
const ARM64: u32 = 0x0100_000c;

#[test]
fn macho_odd() {
    let len = 0x3000;
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    let kinds = [
        (true, X64, 3, "x64"),
        (false, 7, 3, "x86"),
        (true, ARM64, 0, "arm64"),
        (true, ARM64, 0x8000_0002, "arm64e"),
        (false, 12, 9, "arm"),
    ];
    for (wide, cpu, sub, t) in kinds {
        let secs: [(&str, Vec<Ms>); 9] = [
            ("one", vec![m(0x1000, 0x2000, 0x400)]),
            ("overlap", vec![m(0x1000, 0x1000, 0x400), m(0x1800, 0x1000, 0x800)]),
            ("same", vec![m(0x1000, 0x1000, 0x400), m(0x2000, 0x1000, 0x400)]),
            ("past", vec![m(0x1000, 0x1000, 0x5000)]),
            ("zero off", vec![m(0x1000, 0x1000, 0)]),
            ("huge", vec![m(0x1000, u64::MAX, 0x400)]),
            ("off max", vec![m(0x1000, 0x100, u32::MAX), m(0x1000, u64::MAX, u32::MAX - 1)]),
            ("wrap", vec![m(u64::MAX - 0x10, 0x800, 0x400), m(0x10, 0x800, 0xc00)]),
            ("zero size", vec![m(0x1000, 0, 0x400), m(0x1000, 0x100, 0x400)]),
        ];
        for (n, sc) in secs {
            let k = sc.len() as u32;
            cases.push((format!("{t} {n}"), macho_thin(wide, cpu, sub, &sc, k, len)));
            cases.push((format!("{t} {n} nsects 0"), macho_thin(wide, cpu, sub, &sc, 0, len)));
            cases.push((format!("{t} {n} nsects max"), macho_thin(wide, cpu, sub, &sc, u32::MAX, len)));
            cases.push((format!("{t} {n} nsects 2^31"), macho_thin(wide, cpu, sub, &sc, 1 << 31, len)));
        }
    }
    for (name, d) in &cases {
        given_back(name, d);
    }
}

#[test]
fn macho_fat() {
    let a = macho_thin(true, X64, 3, &[m(0x1000, 0x1000, 0x400)], 1, 0x2000);
    let b = macho_thin(true, ARM64, 0, &[m(0x1000, 0x1000, 0x400)], 1, 0x2000);
    for wide in [false, true] {
        let n = if wide { 8 + 64 } else { 8 + 40 };
        let at = 0x4000u64;
        let (al, bl) = (a.len() as u64, b.len() as u64);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("two", fat(wide, &[(X64, at, al, &a), (ARM64, at + 0x4000, bl, &b)])),
            ("one offset", fat(wide, &[(X64, at, al, &a), (ARM64, at, bl, &b)])),
            ("same data", fat(wide, &[(X64, at, al, &a), (X64, at, al, &a)])),
            ("overlapping", fat(wide, &[(X64, at, al, &a), (ARM64, at + 0x800, bl, &b)])),
            ("nested", fat(wide, &[(X64, at, al, &a), (ARM64, at + 0x100, 0x40, &b[..0x40])])),
            ("past the file", fat(wide, &[(X64, at, al, &a), (ARM64, 0x10_0000, 0x2000, &[])])),
            ("size past", fat(wide, &[(X64, at, 1 << 40, &a)])),
            ("size max", fat(wide, &[(X64, at, u64::MAX, &a)])),
            ("at header end", fat(wide, &[(X64, n as u64, al, &a)])),
        ];
        for (name, d) in &cases {
            given_back(&format!("fat{} {name}", if wide { 64 } else { 32 }), d);
        }
        aqr_testkit::damaged(&cases[1].1, 12, 0x100, LIMIT, |y| assert!(round_trip(y)));
    }
}

/// The images above are recognised: each is recompressed by the executable filter, so the odd
/// ones test it, not stored data.
#[test]
fn plain_images_are_filtered() {
    let p = pe(0x8664, 0x20b, 16, &[s(0x1000, 0x4000, 0x400, CODE)], 0x6000);
    let e = elf(true, 2, 62, &[null(), x(0x1000, 0x200, 0x2000)], 2, 0, 0x3000);
    let mo = macho_thin(true, X64, 3, &[m(0x1000, 0x2000, 0x400)], 1, 0x3000);
    let o = Options { smaller: false, ..Options::default() };
    for (name, d) in [("pe", p), ("elf", e), ("macho", mo)] {
        let f = aqr_compress::recompress(&d, &o);
        let modules: Vec<_> = f.applies().iter().map(|a| a.module.name).collect();
        assert!(modules.iter().any(|m| m.starts_with("exe-recons")), "{name}: {modules:?}");
    }
}
