//! Edge cases of the image parsers and the decoders: sections past the end of the file,
//! overlapping or at offset 0, counts of tables and directories at their extremes, the other
//! variants of each container (PE32 of 0x8664, ARM64EC, ET_REL, x32, SHN_XINDEX, 32-bit and fat
//! Mach-O), and every parser run on damaged inputs: no panic, no hang, no allocation the data
//! cannot hold.

use aqr_exe::format::{elf, macho, pe};
use aqr_testkit::{damaged, Refusing};
use std::io::Cursor;
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(20);

fn put(d: &mut [u8], at: usize, v: &[u8]) {
    d[at..at + v.len()].copy_from_slice(v);
}

// ---- PE ----

/// A section of a PE image: name, RVA, stored size, file offset, flags.
type Sect = (&'static [u8], u32, u32, u32, u32);

/// A PE image of `len` bytes: of `machine` and optional header `magic`, with `dirs` data
/// directories of which `set` are `(index, rva, size)`, and the sections.
fn pe_image(len: usize, machine: u16, magic: u16, dirs: u32, set: &[(usize, u32, u32)], sects: &[Sect]) -> Vec<u8> {
    let mut d = vec![0u8; len];
    put(&mut d, 0, b"MZ");
    put(&mut d, 0x3c, &0x40u32.to_le_bytes());
    put(&mut d, 0x40, b"PE\0\0");
    put(&mut d, 0x44, &machine.to_le_bytes());
    put(&mut d, 0x46, &(sects.len() as u16).to_le_bytes());
    let (base_at, dirs_at, head) = if magic == 0x20b { (0x18, 0x6c, 0x70) } else { (0x1c, 0x5c, 0x60) };
    let opt_size = head + 8 * dirs as usize;
    put(&mut d, 0x54, &(opt_size as u16).to_le_bytes());
    let opt = 0x58;
    put(&mut d, opt, &magic.to_le_bytes());
    if magic == 0x20b {
        put(&mut d, opt + base_at, &0x1_4000_0000u64.to_le_bytes());
    } else {
        put(&mut d, opt + base_at, &0x40_0000u32.to_le_bytes());
    }
    put(&mut d, opt + dirs_at, &dirs.to_le_bytes());
    for &(i, rva, size) in set {
        if i < dirs as usize {
            put(&mut d, opt + head + 8 * i, &[rva.to_le_bytes(), size.to_le_bytes()].concat());
        }
    }
    for (i, &(name, rva, size, off, flags)) in sects.iter().enumerate() {
        let sh = opt + opt_size + 40 * i;
        put(&mut d, sh, name);
        put(&mut d, sh + 8, &size.to_le_bytes());
        put(&mut d, sh + 12, &rva.to_le_bytes());
        put(&mut d, sh + 16, &size.to_le_bytes());
        put(&mut d, sh + 20, &off.to_le_bytes());
        put(&mut d, sh + 36, &flags.to_le_bytes());
    }
    d
}

const CODE: u32 = 0x6000_0020;
const DATA: u32 = 0x4000_0040;

fn locate(d: &[u8]) -> std::io::Result<pe::Code> {
    pe::locate_code(&mut Cursor::new(d))
}

/// Every PE reader on `d`: none panics, the tables and resources lie within the file.
fn pe_all(d: &[u8]) {
    let _ = locate(d);
    for t in pe::tables(d) {
        assert!(t.off + t.len <= d.len(), "{t:?}");
    }
    for r in pe::resources(d) {
        assert!(r.start <= r.end && r.end <= d.len(), "{r:?}");
    }
}

#[test]
fn pe_sections_past_the_file_overlapping_or_at_zero() {
    // the locator reports the stored ranges as the headers give them (the consumer cuts
    // them to the file)
    let past = pe_image(0x400, 0x8664, 0x20b, 16, &[], &[(b".text", 0x1000, 0x10_0000, 0x200, CODE)]);
    assert_eq!(locate(&past).unwrap().sections, [(0x1_4000_1000, 0x200, 0x10_0000)]);
    pe_all(&past);

    let beyond = pe_image(0x400, 0x8664, 0x20b, 16, &[], &[(b".text", 0x1000, 0x1000, 0xffff_f000, CODE)]);
    assert_eq!(locate(&beyond).unwrap().sections, [(0x1_4000_1000, 0xffff_f000, 0x1000)]);
    pe_all(&beyond);

    let zero = pe_image(0x400, 0x8664, 0x20b, 16, &[], &[(b".text", 0x1000, 0x200, 0, CODE)]);
    assert_eq!(locate(&zero).unwrap().sections, [(0x1_4000_1000, 0, 0x200)]);
    pe_all(&zero);

    let overlap = pe_image(
        0x800,
        0x8664,
        0x20b,
        16,
        &[(2, 0x1000, 0x100), (5, 0x1000, 0x100), (3, 0x1000, 0x100)],
        &[(b".text", 0x1000, 0x400, 0x200, CODE), (b".text2", 0x1100, 0x400, 0x300, CODE)],
    );
    assert_eq!(locate(&overlap).unwrap().sections.len(), 2);
    pe_all(&overlap);

    // 65535 sections' headers, past the file
    let mut many = pe_image(0x200, 0x8664, 0x20b, 16, &[], &[]);
    put(&mut many, 0x46, &0xffffu16.to_le_bytes());
    assert!(locate(&many).is_err());
    pe_all(&many);
}

#[test]
fn pe_data_directories_none_or_many() {
    for dirs in [0u32, 1, 2, 3, 5, 14, 15, 16, 17, 100, 0xffff_ffff] {
        // (a count the optional header cannot hold is an error, never an allocation)
        let mut d = pe_image(
            0x600,
            0x8664,
            0x20b,
            dirs.min(16),
            &[(14, 0x1000, 72), (2, 0x1000, 0x40)],
            &[(b".text", 0x1000, 0x400, 0x200, CODE)],
        );
        if dirs > 16 {
            put(&mut d, 0x58 + 0x6c, &dirs.to_le_bytes());
        }
        assert_eq!(locate(&d).is_err(), dirs > 16, "{dirs} directories");
        pe_all(&d);
    }
    for dirs in [0u32, 14, 15, 16] {
        let d = pe_image(0x600, 0x14c, 0x10b, dirs, &[(14, 0x1000, 72)], &[(b".text", 0x1000, 0x400, 0x200, CODE)]);
        assert!(!locate(&d).unwrap().is64);
        pe_all(&d);
    }
}

#[test]
fn pe_machine_and_magic_combinations() {
    // PE32 of an x86-64 machine, PE32+ of x86, ARM64EC (0xa641), ARM64X, ARM64
    for (machine, magic) in [(0x8664u16, 0x10bu16), (0x14c, 0x20b), (0xa641, 0x20b), (0xa64e, 0x20b), (0xaa64, 0x20b)] {
        let d = pe_image(0x600, machine, magic, 16, &[(14, 0x1000, 72)], &[(b".text", 0x1000, 0x400, 0x200, CODE)]);
        let c = locate(&d).unwrap();
        assert_eq!((c.machine, c.is64), (machine, magic == 0x20b));
        assert_eq!(c.sections.len(), 1, "{machine:#x} {magic:#x}");
        pe_all(&d);
    }
    // packed: the code in a writable section (UPX1), the rest of no code
    let d = pe_image(
        0x600,
        0x14c,
        0x10b,
        16,
        &[],
        &[(b"UPX0", 0x1000, 0, 0, 0xe000_0080), (b"UPX1", 0x2000, 0x400, 0x200, 0xe000_0020)],
    );
    assert_eq!(locate(&d).unwrap().sections, [(0x40_2000, 0x200, 0x400)]);
    let d = pe_image(0x600, 0x14c, 0x10b, 16, &[], &[(b".data", 0x1000, 0x400, 0x200, DATA)]);
    assert_eq!(locate(&d).unwrap().sections, []);
}

#[test]
fn pe_tables_of_extreme_counts() {
    // export directory with 2^32-1 functions and names, runtime functions and relocations of
    // a size of 0xffffffff
    let mut d = pe_image(
        0x800,
        0x8664,
        0x20b,
        16,
        &[(0, 0x1000, 40), (1, 0x1100, 40), (3, 0x1000, 0xffff_ffff), (5, 0x1000, 0xffff_ffff), (2, 0x1000, 0x400)],
        &[(b".rdata", 0x1000, 0x400, 0x200, DATA)],
    );
    put(&mut d, 0x200 + 20, &[0xffu8; 8]);
    put(&mut d, 0x200 + 28, &[0x00, 0x10, 0, 0, 0x00, 0x10, 0, 0, 0x00, 0x10, 0, 0]);
    pe_all(&d);
    // import descriptors all pointing to one array of thunks
    let mut d = pe_image(0x800, 0x8664, 0x20b, 16, &[(1, 0x1000, 0x400)], &[(b".idata", 0x1000, 0x400, 0x200, DATA)]);
    for k in 0..0x400 / 20 {
        put(&mut d, 0x200 + 20 * k, &0x1000u32.to_le_bytes());
        put(&mut d, 0x200 + 20 * k + 16, &0x1000u32.to_le_bytes());
    }
    pe_all(&d);
    // a resource directory of 0xffff entries each of which is the directory itself
    let mut d = pe_image(0x800, 0x8664, 0x20b, 16, &[(2, 0x1000, 0x400)], &[(b".rsrc", 0x1000, 0x400, 0x200, DATA)]);
    put(&mut d, 0x200 + 12, &[0xff, 0xff, 0xff, 0xff]);
    for k in 0..0x30 {
        put(&mut d, 0x200 + 16 + 8 * k + 4, &0x8000_0000u32.to_le_bytes());
    }
    aqr_testkit::within(LIMIT, || pe_all(&d)).unwrap();
}

#[test]
fn pe_damaged() {
    let d = pe_image(
        0x700,
        0x8664,
        0x20b,
        16,
        &[(0, 0x1000, 40), (1, 0x1100, 40), (2, 0x1200, 0x100), (3, 0x1000, 24), (5, 0x1300, 16), (14, 0x1000, 72)],
        &[(b".text", 0x1000, 0x400, 0x200, CODE), (b".rdata", 0x1400, 0x100, 0x600, DATA)],
    );
    damaged(&d, 6, 0x200, LIMIT, pe_all);
    // (the resources too: three levels, one leaf)
    let mut r = pe_image(0x700, 0x8664, 0x20b, 16, &[(2, 0x1000, 0x80)], &[(b".rsrc", 0x1000, 0x400, 0x200, DATA)]);
    put(&mut r, 0x200 + 14, &[1, 0]);
    put(&mut r, 0x200 + 20, &0x8000_0020u32.to_le_bytes());
    put(&mut r, 0x220 + 14, &[1, 0]);
    put(&mut r, 0x220 + 20, &0x8000_0040u32.to_le_bytes());
    put(&mut r, 0x240 + 14, &[1, 0]);
    put(&mut r, 0x240 + 20, &0x60u32.to_le_bytes());
    put(&mut r, 0x260, &[0x70, 0x10, 0, 0, 8, 0, 0, 0]);
    put(&mut r, 0x270, &[1; 8]);
    assert_eq!(pe::resources(&r), vec![0x270..0x278]);
    damaged(&r, 6, 0x300, LIMIT, pe_all);
}

// ---- ELF ----

/// An ELF of the layout the fields say: `sections` as `(name, type, flags, addr, offset,
/// size)`, `segments` as `(type, flags, offset, vaddr, filesz)`.
struct Elf {
    wide: bool,
    kind: u16,
    machine: u16,
    /// e_shnum and e_shstrndx as the header holds them.
    shnum: u16,
    shstrndx: u16,
    sections: Vec<(u32, u32, u64, u64, u64, u64)>,
    segments: Vec<(u32, u32, u64, u64, u64)>,
}

impl Elf {
    fn sizes(&self) -> (usize, usize, usize) {
        if self.wide {
            (64, 56, 64)
        } else {
            (52, 32, 40)
        }
    }

    fn shoff(&self) -> usize {
        let (eh, ph, _) = self.sizes();
        (eh + ph * self.segments.len()).next_multiple_of(8)
    }

    fn build(&self, len: usize) -> Vec<u8> {
        let (eh, ph, sh) = self.sizes();
        let (phoff, shoff) = (eh, self.shoff());
        let mut d = vec![0u8; len.max(shoff + sh * self.sections.len())];
        d[..4].copy_from_slice(b"\x7fELF");
        d[4] = if self.wide { 2 } else { 1 };
        d[5] = 1;
        d[6] = 1;
        put(&mut d, 16, &self.kind.to_le_bytes());
        put(&mut d, 18, &self.machine.to_le_bytes());
        put(&mut d, 20, &1u32.to_le_bytes());
        let ws = if self.wide { 8 } else { 4 };
        let w = |d: &mut Vec<u8>, at: usize, v: u64| put(d, at, &v.to_le_bytes()[..ws]);
        w(&mut d, 24 + ws, if self.segments.is_empty() { 0 } else { phoff as u64 });
        w(&mut d, 24 + 2 * ws, if self.sections.is_empty() { 0 } else { shoff as u64 });
        let h = 24 + 3 * ws + 4;
        put(&mut d, h, &(eh as u16).to_le_bytes());
        put(&mut d, h + 2, &(ph as u16).to_le_bytes());
        put(&mut d, h + 4, &(self.segments.len() as u16).to_le_bytes());
        put(&mut d, h + 6, &(sh as u16).to_le_bytes());
        put(&mut d, h + 8, &self.shnum.to_le_bytes());
        put(&mut d, h + 10, &self.shstrndx.to_le_bytes());
        for (i, &(kind, flags, off, vaddr, filesz)) in self.segments.iter().enumerate() {
            let p = phoff + i * ph;
            put(&mut d, p, &kind.to_le_bytes());
            if self.wide {
                put(&mut d, p + 4, &flags.to_le_bytes());
                w(&mut d, p + 8, off);
                w(&mut d, p + 16, vaddr);
                w(&mut d, p + 32, filesz);
            } else {
                w(&mut d, p + 4, off);
                w(&mut d, p + 8, vaddr);
                w(&mut d, p + 16, filesz);
                put(&mut d, p + 24, &flags.to_le_bytes());
            }
        }
        for (i, &(name, kind, flags, addr, off, size)) in self.sections.iter().enumerate() {
            let p = shoff + i * sh;
            put(&mut d, p, &name.to_le_bytes());
            put(&mut d, p + 4, &kind.to_le_bytes());
            let at = if self.wide { [8, 16, 24, 32] } else { [8, 12, 16, 20] };
            for (a, v) in at.into_iter().zip([flags, addr, off, size]) {
                w(&mut d, p + a, v);
            }
        }
        put(&mut d, 0x300, b"\0.text\0.shstr");
        d
    }
}

const AX: u64 = 6; // SHF_ALLOC | SHF_EXECINSTR

fn text_elf(wide: bool, kind: u16, machine: u16) -> Elf {
    Elf {
        wide,
        kind,
        machine,
        shnum: 3,
        shstrndx: 2,
        sections: vec![(0, 0, 0, 0, 0, 0), (1, 1, AX, 0x401000, 0x200, 0x100), (7, 3, 0, 0, 0x300, 16)],
        segments: vec![(1, 5, 0x200, 0x401000, 0x100)],
    }
}

fn elf_all(d: &[u8]) {
    let _ = elf::starts(d);
    let _ = elf::extent(d);
    if let Some(i) = elf::image(d) {
        for &(_, off, size) in &i.code {
            assert!(off + size <= d.len());
        }
        for s in &i.sections {
            assert!(s.off + s.size <= d.len());
        }
    }
}

#[test]
fn elf_variants() {
    // ET_REL: sections all at address 0, their offsets stand for them
    let mut e = text_elf(true, 1, 62);
    e.sections[1].3 = 0;
    e.segments.clear();
    let d = e.build(0x400);
    let i = elf::image(&d).unwrap();
    assert_eq!(i.kind, 1);
    assert_eq!(i.code, [(0x200, 0x200, 0x100)]);
    elf_all(&d);
    // x32: ELFCLASS32 of x86-64
    let d = text_elf(false, 2, 62).build(0x400);
    let i = elf::image(&d).unwrap();
    assert!(i.is_x86() && i.is64() && !i.wide);
    assert_eq!(i.code, [(0x401000, 0x200, 0x100)]);
    elf_all(&d);
    // ARM64, ARM, i386, RISC-V
    for (wide, m) in [(true, 183u16), (false, 40), (false, 3), (true, 243)] {
        let d = text_elf(wide, 3, m).build(0x400);
        assert_eq!(elf::image(&d).unwrap().code.len(), 1, "machine {m}");
        elf_all(&d);
    }
}

#[test]
fn elf_extended_section_numbers() {
    // SHN_XINDEX: e_shstrndx is 0xffff and section 0's sh_link tells it; e_shnum is 0 and
    // section 0's sh_size tells it
    for wide in [true, false] {
        let mut e = text_elf(wide, 2, 62);
        e.shnum = 0;
        e.shstrndx = 0xffff;
        e.sections[0].5 = 3;
        let mut d = e.build(0x400);
        let shoff = e.shoff();
        put(&mut d, shoff + if wide { 40 } else { 24 }, &2u32.to_le_bytes()); // sh_link
        let i = elf::image(&d).unwrap();
        assert_eq!(i.sections.len(), 2, "wide {wide}");
        assert_eq!(i.sections[0].name, b".text", "wide {wide}: the string table by sh_link");
        assert_eq!(i.code.len(), 1);
        elf_all(&d);
        // a count of 2^32-1 and of 2^64-1 sections, an index past them
        for (count, index) in [(u32::MAX as u64, 2u32), (3, u32::MAX), (u64::MAX, 0)] {
            let mut x = d.clone();
            put(&mut x, shoff + if wide { 32 } else { 20 }, &count.to_le_bytes()[..if wide { 8 } else { 4 }]);
            put(&mut x, shoff + if wide { 40 } else { 24 }, &index.to_le_bytes()[..4]);
            elf_all(&x);
        }
    }
}

#[test]
fn elf_sections_past_overlapping_or_at_zero() {
    // the offset past the end
    let mut e = text_elf(true, 2, 62);
    e.sections[1].4 = 0x10_0000;
    let d = e.build(0x400);
    assert!(elf::image(&d).unwrap().code.is_empty());
    elf_all(&d);
    // the size past the end: cut to it
    let mut e = text_elf(true, 2, 62);
    e.sections[1].5 = 0xffff_ffff_ffff_ff00;
    let d = e.build(0x400);
    assert_eq!(elf::image(&d).unwrap().code, [(0x401000, 0x200, 0x400 - 0x200)]);
    elf_all(&d);
    // the offset and the size overflowing
    let mut e = text_elf(true, 2, 62);
    e.sections[1].4 = u64::MAX - 8;
    elf_all(&e.build(0x400));
    // offset 0: the header itself as code
    let mut e = text_elf(true, 2, 62);
    e.sections[1].4 = 0;
    let d = e.build(0x400);
    assert_eq!(elf::image(&d).unwrap().code, [(0x401000, 0, 0x100)]);
    elf_all(&d);
    // overlapping sections: the second left out
    let mut e = text_elf(true, 2, 62);
    e.shnum = 4;
    e.sections.push((1, 1, AX, 0x401080, 0x280, 0x100));
    let d = e.build(0x400);
    assert_eq!(elf::image(&d).unwrap().code, [(0x401000, 0x200, 0x100)]);
    elf_all(&d);
    // a name offset past the string table
    let mut e = text_elf(true, 2, 62);
    e.sections[1].0 = 0xffff_fff0;
    elf_all(&e.build(0x400));
    // no section headers: the segments, one overlapping the first, one past the end
    let mut e = text_elf(true, 2, 62);
    e.sections.clear();
    e.shnum = 0;
    e.shstrndx = 0;
    e.segments = vec![(1, 5, 0x200, 0x401000, 0x100), (1, 5, 0x280, 0x401080, 0x100), (1, 1, 0x10_0000, 0, 8)];
    let d = e.build(0x400);
    assert_eq!(elf::image(&d).unwrap().code, [(0x401000, 0x200, 0x100)]);
    elf_all(&d);
    // 2^16-1 program headers
    let mut e = text_elf(true, 2, 62);
    e.sections.clear();
    e.shnum = 0;
    let mut d = e.build(0x400);
    put(&mut d, 24 + 24 + 4 + 4, &0xffffu16.to_le_bytes());
    elf_all(&d);
}

#[test]
fn elf_damaged() {
    let mut e = text_elf(true, 2, 62);
    e.shnum = 4;
    e.sections.push((1, 1, AX, 0x401080, 0x280, 0x100));
    damaged(&e.build(0x400), 5, 0x100, LIMIT, elf_all);
    damaged(&text_elf(false, 2, 3).build(0x400), 5, 0x100, LIMIT, elf_all);
}

// ---- Mach-O ----

fn macho_all(d: &[u8]) {
    let _ = macho::starts(d);
    let _ = macho::extent(d);
    let _ = macho::fat_archs(d);
    if let Some(images) = macho::images(d) {
        for i in &images {
            assert!(i.range.end <= d.len());
            let ranges = i
                .sections
                .iter()
                .map(|&(_, o, s)| (o, s))
                .chain(i.debug.iter().map(|&(_, o, s)| (o, s)))
                .chain(i.data.iter().map(|&(_, o, s)| (o, s)));
            for (off, size) in ranges {
                assert!(off + size <= d.len());
            }
        }
    }
}

/// A thin Mach-O image of `cputype` (and `subtype`), `wide` or not, with one segment command
/// holding `sects` as `(segname, sectname, addr, size, offset, flags)` and saying `declared`.
fn thin_macho(wide: bool, cputype: u32, subtype: u32, declared: u32, sects: &[MSect], len: usize) -> Vec<u8> {
    let (header, seg, ssz) = if wide { (32, 72, 80) } else { (28, 56, 68) };
    let cmdsize = seg + sects.len() * ssz;
    let mut d = vec![0u8; len.max(header + cmdsize)];
    put(&mut d, 0, &(if wide { 0xfeed_facfu32 } else { 0xfeed_face }).to_le_bytes());
    put(&mut d, 4, &cputype.to_le_bytes());
    put(&mut d, 8, &subtype.to_le_bytes());
    put(&mut d, 12, &2u32.to_le_bytes());
    put(&mut d, 16, &1u32.to_le_bytes());
    put(&mut d, 20, &(cmdsize as u32).to_le_bytes());
    put(&mut d, header, &(if wide { 0x19u32 } else { 1 }).to_le_bytes());
    put(&mut d, header + 4, &(cmdsize as u32).to_le_bytes());
    put(&mut d, header + 8, b"__TEXT");
    put(&mut d, header + seg - 8, &declared.to_le_bytes());
    for (i, &(sg, sn, addr, size, off, flags)) in sects.iter().enumerate() {
        let s = header + seg + i * ssz;
        put(&mut d, s, sn);
        put(&mut d, s + 16, sg);
        if wide {
            put(&mut d, s + 32, &addr.to_le_bytes());
            put(&mut d, s + 40, &size.to_le_bytes());
            put(&mut d, s + 48, &off.to_le_bytes());
            put(&mut d, s + 64, &flags.to_le_bytes());
        } else {
            put(&mut d, s + 32, &(addr as u32).to_le_bytes());
            put(&mut d, s + 36, &(size as u32).to_le_bytes());
            put(&mut d, s + 40, &off.to_le_bytes());
            put(&mut d, s + 56, &flags.to_le_bytes());
        }
    }
    d
}

const PURE: u32 = 0x8000_0000;

/// A section of a Mach-O image: segment, name, address, size, offset, flags.
type MSect = (&'static [u8], &'static [u8], u64, u64, u32, u32);
const TEXT: MSect = (b"__TEXT", b"__text", 0x1000, 0x80, 0x100, PURE | 0x400);

#[test]
fn macho_variants() {
    // 32-bit i386 and ARM
    for cpu in [7u32, 12] {
        let d = thin_macho(false, cpu, 3, 1, &[TEXT], 0x200);
        let i = macho::images(&d).unwrap();
        assert_eq!(i[0].sections, [(0x1000, 0x100, 0x80)], "cpu {cpu}");
        assert!(!i[0].is64());
        macho_all(&d);
    }
    // arm64e (the subtype's pointer-authentication bit), arm64_32, x86-64h
    for (cpu, sub) in [(0x0100_000cu32, 2u32), (0x0100_000c, 0x8000_0002), (0x0200_000c, 1), (0x0100_0007, 8)] {
        let d = thin_macho(true, cpu, sub, 1, &[TEXT], 0x200);
        let i = macho::images(&d).unwrap();
        assert_eq!(i[0].sections, [(0x1000, 0x100, 0x80)], "cpu {cpu:#x}");
        assert_eq!(i[0].cputype, cpu);
        macho_all(&d);
    }
}

#[test]
fn macho_section_counts_at_the_extremes() {
    // `nsects * size` past 32 bits (a 32-bit multiplication would wrap to a count that fits)
    for declared in [2u32, 0x3c3c_3c3d, 0x6000_0000, 0xffff_ffff, 0x8000_0001] {
        for wide in [true, false] {
            let d = thin_macho(wide, if wide { 0x0100_0007 } else { 7 }, 3, declared, &[TEXT], 0x200);
            // more sections than the command holds: no image
            assert!(macho::images(&d).is_none(), "declared {declared:#x}, wide {wide}");
            macho_all(&d);
        }
    }
    // a section's offset and size past the file, at its end, and overflowing 32 bits
    for (size, off) in
        [(0xffff_ffff_ffffu64, 0x100u32), (0x80, 0xffff_ffff), (0x80, 0x1ff), (u64::MAX, 0x1ff), (0x80, 0)]
    {
        let d = thin_macho(true, 0x0100_0007, 3, 1, &[(b"__TEXT", b"__text", 0x1000, size, off, PURE)], 0x200);
        macho_all(&d);
    }
    // 2^32-1 load commands, and a size of them
    let mut d = thin_macho(true, 0x0100_0007, 3, 1, &[TEXT], 0x200);
    put(&mut d, 16, &u32::MAX.to_le_bytes());
    macho_all(&d);
    put(&mut d, 20, &u32::MAX.to_le_bytes());
    macho_all(&d);
}

fn fat(wide: bool, archs: &[(u32, u64, u64)], len: usize) -> Vec<u8> {
    let entry = if wide { 32 } else { 20 };
    let mut d = vec![0u8; len.max(8 + entry * archs.len())];
    put(&mut d, 0, &(if wide { 0xcafe_babfu32 } else { 0xcafe_babe }).to_be_bytes());
    put(&mut d, 4, &(archs.len() as u32).to_be_bytes());
    for (i, &(cpu, off, size)) in archs.iter().enumerate() {
        let e = 8 + i * entry;
        put(&mut d, e, &cpu.to_be_bytes());
        if wide {
            put(&mut d, e + 8, &off.to_be_bytes());
            put(&mut d, e + 16, &size.to_be_bytes());
        } else {
            put(&mut d, e + 8, &(off as u32).to_be_bytes());
            put(&mut d, e + 12, &(size as u32).to_be_bytes());
        }
    }
    d
}

#[test]
fn macho_fat_archs_at_one_offset() {
    let thin = thin_macho(true, 0x0100_0007, 3, 1, &[TEXT], 0x200);
    for wide in [false, true] {
        // two architectures sharing one image
        let mut d = fat(wide, &[(0x0100_0007, 0x1000, 0x200), (0x0100_000c, 0x1000, 0x200)], 0x1000);
        d.extend(&thin);
        assert_eq!(macho::fat_archs(&d).unwrap().len(), 2);
        let images = macho::images(&d).unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].sections, images[1].sections);
        assert_eq!(macho::extent(&d), Some(0x1200));
        macho_all(&d);
        // one inside the other
        let mut d2 = fat(wide, &[(7, 0x1000, 0x200), (12, 0x1100, 0x100)], 0x1000);
        d2.extend(&thin);
        macho_all(&d2);
        // the same offset, the file cut before it
        let cut = fat(wide, &[(7, 0x1000, 0x200), (12, 0x1000, 0x200)], 0x100);
        assert!(macho::images(&cut).is_some());
        macho_all(&cut);
        // the offset inside the table of the architectures
        for offset in [0u64, 8, 0x13, 0x27] {
            let bad = fat(wide, &[(7, offset, 0x200)], 0x1000);
            assert!(macho::fat_archs(&bad).is_none(), "{offset}");
            macho_all(&bad);
        }
        // a count of 2^32-1
        let mut many = d.clone();
        put(&mut many, 4, &u32::MAX.to_be_bytes());
        macho_all(&many);
    }
}

#[test]
fn macho_damaged() {
    let d = thin_macho(
        true,
        0x0100_0007,
        3,
        2,
        &[(b"__TEXT", b"__text", 0x1000, 0x40, 0x100, PURE), (b"__TEXT", b"__const", 0x1100, 0x20, 0x140, 0)],
        0x200,
    );
    damaged(&d, 6, 0x100, LIMIT, macho_all);
    damaged(&thin_macho(false, 7, 3, 1, &[TEXT], 0x200), 6, 0x100, LIMIT, macho_all);
    let mut f = fat(true, &[(0x0100_0007, 0x1000, 0x200), (0x0100_000c, 0x1000, 0x200)], 0x1000);
    f.extend(thin_macho(true, 0x0100_0007, 3, 1, &[TEXT], 0x200));
    damaged(&f, 6, 0x80, LIMIT, macho_all);
}
