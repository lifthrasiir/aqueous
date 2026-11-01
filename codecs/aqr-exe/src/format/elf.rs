//! ELF images (executables, shared objects, relocatable objects), 32- and 64-bit: the ranges
//! of each that hold instructions, and its sections.
//!
//! Only little-endian images are looked into; big-endian ones are images without code. The
//! code is that of the allocated sections of instructions (`SHF_ALLOC | SHF_EXECINSTR`),
//! those that touch (but for padding) taken as one range; of an image without section
//! headers, its executable `PT_LOAD` segments. A relocatable object's sections have no
//! addresses: their file offsets stand for them.

use alloc::vec::Vec;

pub const EM_386: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;

pub const ET_REL: u16 = 1;

pub const SHT_PROGBITS: u32 = 1;
pub const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u64 = 0x2;
const SHF_EXECINSTR: u64 = 0x4;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;

/// Code sections further apart than this are taken as ranges of their own.
const MAX_GAP: usize = 4096;

/// An ELF image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `EM_*` of the header.
    pub machine: u16,
    /// `ET_*` of the header.
    pub kind: u16,
    /// ELFCLASS64.
    pub wide: bool,
    /// `(virtual address, file offset, size)` of each range of code, cut to the data, in file
    /// order, none overlapping.
    pub code: Vec<(u64, usize, usize)>,
    /// The sections with data in the file, cut to it, in the order of the section headers.
    pub sections: Vec<Section>,
}

/// A section of an image, of data in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Its name, empty where the string table does not hold it.
    pub name: Vec<u8>,
    /// `SHT_*`.
    pub kind: u32,
    pub flags: u64,
    pub addr: u64,
    pub off: usize,
    pub size: usize,
    /// The size of its entries, 0 where it has none.
    pub entsize: u64,
}

impl Image {
    /// Whether the image holds x86 or x86-64 code.
    pub fn is_x86(&self) -> bool {
        matches!(self.machine, EM_386 | EM_X86_64)
    }

    /// Whether its code is 64-bit: x86-64 (x32's too) or ARM64.
    pub fn is64(&self) -> bool {
        matches!(self.machine, EM_X86_64 | EM_AARCH64)
    }
}

fn u16le(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

fn u32le(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

fn u64le(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

/// The fields of a little-endian ELF header.
struct Header {
    wide: bool,
    kind: u16,
    machine: u16,
    phoff: u64,
    phentsize: usize,
    phnum: usize,
    shoff: u64,
    shentsize: usize,
    shnum: usize,
    shstrndx: usize,
}

/// A word of the image's class at `p`.
fn word(d: &[u8], wide: bool, p: usize) -> Option<u64> {
    if wide {
        u64le(d, p)
    } else {
        u32le(d, p).map(u64::from)
    }
}

fn header(d: &[u8]) -> Option<Header> {
    let ident = d.get(..16)?;
    if ident[..4] != *b"\x7fELF" || ident[5] != 1 || ident[6] != 1 {
        return None;
    }
    let wide = match ident[4] {
        1 => false,
        2 => true,
        _ => return None,
    };
    let (kind, machine) = (u16le(d, 16)?, u16le(d, 18)?);
    if u32le(d, 20)? != 1 || kind == 0 || kind > 4 && kind < 0xfe00 {
        return None;
    }
    let (w, at) = if wide { (8, 24) } else { (4, 24) };
    let (phoff, shoff) = (word(d, wide, at + w)?, word(d, wide, at + 2 * w)?);
    let h = at + 3 * w + 4;
    let ehsize = u16le(d, h)? as usize;
    let (phentsize, phnum) = (u16le(d, h + 2)? as usize, u16le(d, h + 4)? as usize);
    let (shentsize, mut shnum) = (u16le(d, h + 6)? as usize, u16le(d, h + 8)? as usize);
    let mut shstrndx = u16le(d, h + 10)? as usize;
    let (eh, ph, sh) = if wide { (64, 56, 64) } else { (52, 32, 40) };
    if ehsize != eh || phnum > 0 && phentsize != ph || shoff != 0 && shentsize != sh {
        return None;
    }
    // more sections than fit, or a string table's index past them: section 0 tells them
    if shoff != 0 && (shnum == 0 || shstrndx == 0xffff) {
        let s0 = usize::try_from(shoff).ok()?;
        if shnum == 0 {
            shnum = usize::try_from(word(d, wide, s0 + if wide { 32 } else { 20 })?).ok()?;
        }
        if shstrndx == 0xffff {
            shstrndx = u32le(d, s0 + if wide { 44 } else { 28 })? as usize;
        }
    }
    Some(Header { wide, kind, machine, phoff, phentsize, phnum, shoff, shentsize, shnum, shstrndx })
}

/// Whether `d` starts with a little-endian ELF header that holds together.
pub fn starts(d: &[u8]) -> bool {
    header(d).is_some()
}

/// The fields of a section header: type, flags, address, offset, size, entry size, name.
fn section_header(d: &[u8], wide: bool, p: usize) -> Option<(u32, u64, u64, u64, u64, u64, u32)> {
    let name = u32le(d, p)?;
    let kind = u32le(d, p + 4)?;
    Some(if wide {
        (kind, u64le(d, p + 8)?, u64le(d, p + 16)?, u64le(d, p + 24)?, u64le(d, p + 32)?, u64le(d, p + 56)?, name)
    } else {
        let f = |i| u32le(d, p + i).map(u64::from);
        (kind, f(8)?, f(12)?, f(16)?, f(20)?, f(36)?, name)
    })
}

/// The fields of a program header: type, flags, offset, virtual address, size in the file.
fn program_header(d: &[u8], wide: bool, p: usize) -> Option<(u32, u32, u64, u64, u64)> {
    let kind = u32le(d, p)?;
    Some(if wide {
        (kind, u32le(d, p + 4)?, u64le(d, p + 8)?, u64le(d, p + 16)?, u64le(d, p + 32)?)
    } else {
        let f = |i| u32le(d, p + i).map(u64::from);
        (kind, u32le(d, p + 24)?, f(4)?, f(8)?, f(16)?)
    })
}

/// The `(offset, size)` cut to `d`, where it starts within it.
fn within(d: &[u8], off: u64, size: u64) -> Option<(usize, usize)> {
    let off = usize::try_from(off).ok().filter(|&o| o < d.len())?;
    let size = usize::try_from(size).map_or(d.len() - off, |s| s.min(d.len() - off));
    (size > 0).then_some((off, size))
}

/// The ELF image at the start of `d`, cut to `d`; `None` where its header does not hold.
pub fn image(d: &[u8]) -> Option<Image> {
    let h = header(d)?;
    let mut sections = Vec::new();
    let mut code = Vec::new();
    let shoff = usize::try_from(h.shoff).ok().filter(|_| h.shoff != 0);
    let headers = shoff.map_or(0, |s| h.shnum.min(d.len().saturating_sub(s) / h.shentsize));
    let strtab = shoff
        .and_then(|s| section_header(d, h.wide, s + h.shstrndx * h.shentsize).filter(|_| h.shstrndx < headers))
        .and_then(|(.., off, size, _, _)| within(d, off, size));
    for i in 0..headers {
        let Some((kind, flags, addr, off, size, entsize, name)) =
            section_header(d, h.wide, shoff.unwrap_or(0) + i * h.shentsize)
        else {
            break;
        };
        if kind == SHT_NOBITS || kind == 0 {
            continue;
        }
        let Some((off, size)) = within(d, off, size) else { continue };
        let name = strtab
            .and_then(|(at, len)| {
                let s = d[at..at + len].get(name as usize..)?;
                Some(s[..s.iter().position(|&b| b == 0)?].to_vec())
            })
            .unwrap_or_default();
        // (a relocatable object's sections are all at 0: their offsets stand for them)
        let addr = if h.kind == ET_REL { off as u64 } else { addr };
        sections.push(Section { name, kind, flags, addr, off, size, entsize });
    }
    let mut ranges: Vec<_> = sections
        .iter()
        .filter(|s| s.kind == SHT_PROGBITS && s.flags & (SHF_ALLOC | SHF_EXECINSTR) == SHF_ALLOC | SHF_EXECINSTR)
        .map(|s| (s.addr, s.off, s.size))
        .collect();
    if ranges.is_empty() && sections.is_empty() {
        // no section headers: the executable segments
        let phoff = usize::try_from(h.phoff).ok().filter(|_| h.phoff != 0);
        for i in 0..phoff.map_or(0, |_| h.phnum) {
            let Some((kind, flags, off, vaddr, filesz)) =
                program_header(d, h.wide, phoff.unwrap_or(0) + i * h.phentsize)
            else {
                break;
            };
            if kind == PT_LOAD && flags & PF_X != 0 {
                ranges.extend(within(d, off, filesz).map(|(off, size)| (vaddr, off, size)));
            }
        }
    }
    ranges.sort_by_key(|r| r.1);
    let mut starts: Vec<usize> = sections.iter().map(|s| s.off).collect();
    starts.sort_unstable();
    for (va, off, size) in ranges {
        // overlapping the last: left out; after it but for a gap of nothing else: joined
        if let Some((lva, loff, lsize)) = code.last_mut() {
            let end = *loff + *lsize;
            if off < end {
                continue;
            }
            // (no section starting in the gap)
            let between = starts.partition_point(|&o| o < end) < starts.partition_point(|&o| o < off);
            if off - end <= MAX_GAP && va.wrapping_sub(*lva) == (off - *loff) as u64 && !between {
                *lsize = off + size - *loff;
                continue;
            }
        }
        code.push((va, off, size));
    }
    Some(Image { machine: h.machine, kind: h.kind, wide: h.wide, code, sections })
}

/// Bytes of the ELF image at the start of `d`, as far as its headers tell: the end of its
/// header tables, of its sections' data and of its segments' data. Not cut to `d`.
pub fn extent(d: &[u8]) -> Option<u64> {
    let h = header(d)?;
    let mut end = if h.wide { 64 } else { 52 };
    if h.phoff != 0 {
        end = end.max(h.phoff.saturating_add((h.phnum * h.phentsize) as u64));
        let phoff = usize::try_from(h.phoff).ok()?;
        for i in 0..h.phnum {
            let Some((_, _, off, _, filesz)) = program_header(d, h.wide, phoff + i * h.phentsize) else { break };
            end = end.max(off.saturating_add(filesz));
        }
    }
    if h.shoff != 0 {
        end = end.max(h.shoff.saturating_add((h.shnum * h.shentsize) as u64));
        let shoff = usize::try_from(h.shoff).ok()?;
        for i in 0..h.shnum {
            let Some((kind, _, _, off, size, ..)) = section_header(d, h.wide, shoff + i * h.shentsize) else { break };
            if kind != SHT_NOBITS && kind != 0 {
                end = end.max(off.saturating_add(size));
            }
        }
    }
    Some(end)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;

    /// A 64-bit executable of `machine` at 0x400000: its header, two program headers (the
    /// headers read-only, the code executable), `.init` (16 bytes), padding to 16, `.text` of
    /// `code`, `.rodata` (32 bytes), `.bss`, `.shstrtab` and the section headers.
    pub(crate) fn exec64(machine: u16, code: &[u8]) -> Vec<u8> {
        let base = 0x40_0000u64;
        let init = 64 + 2 * 56;
        let text = (init + 16usize).next_multiple_of(16);
        let rodata = text + code.len();
        let strtab = rodata + 32;
        let names = b"\0.init\0.text\0.rodata\0.bss\0.shstrtab\0";
        let shoff = (strtab + names.len()).next_multiple_of(8);
        let mut d = vec![0u8; shoff + 6 * 64];
        let put16 = |d: &mut Vec<u8>, p: usize, v: u16| d[p..p + 2].copy_from_slice(&v.to_le_bytes());
        let put32 = |d: &mut Vec<u8>, p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
        let put64 = |d: &mut Vec<u8>, p: usize, v: u64| d[p..p + 8].copy_from_slice(&v.to_le_bytes());
        d[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut d, 16, 2); // ET_EXEC
        put16(&mut d, 18, machine);
        put32(&mut d, 20, 1);
        put64(&mut d, 24, base + text as u64);
        put64(&mut d, 32, 64);
        put64(&mut d, 40, shoff as u64);
        put16(&mut d, 52, 64);
        put16(&mut d, 54, 56);
        put16(&mut d, 56, 2);
        put16(&mut d, 58, 64);
        put16(&mut d, 60, 6);
        put16(&mut d, 62, 5);
        for (i, (flags, off, size)) in [(4, 0, init), (5, init, strtab - init)].into_iter().enumerate() {
            let p = 64 + i * 56;
            put32(&mut d, p, PT_LOAD);
            put32(&mut d, p + 4, flags);
            put64(&mut d, p + 8, off as u64);
            put64(&mut d, p + 16, base + off as u64);
            put64(&mut d, p + 32, size as u64);
            put64(&mut d, p + 40, size as u64);
        }
        d[init..init + 16].fill(0x90);
        d[text..rodata].copy_from_slice(code);
        d[rodata..strtab].fill(0x55);
        d[strtab..strtab + names.len()].copy_from_slice(names);
        let sects = [
            (1, SHT_PROGBITS, 6, init, 16),
            (7, SHT_PROGBITS, 6, text, code.len()),
            (13, SHT_PROGBITS, 2, rodata, 32),
            (21, SHT_NOBITS, 3, strtab, 4096),
            (26, 3, 0, strtab, names.len()),
        ];
        for (i, (name, kind, flags, off, size)) in sects.into_iter().enumerate() {
            let p = shoff + (i + 1) * 64;
            put32(&mut d, p, name);
            put32(&mut d, p + 4, kind);
            put64(&mut d, p + 8, flags);
            put64(&mut d, p + 16, if flags & 2 != 0 { base + off as u64 } else { 0 });
            put64(&mut d, p + 24, off as u64);
            put64(&mut d, p + 32, size as u64);
        }
        d
    }

    #[test]
    fn code_joined_and_extent() {
        let code = [0xc3u8; 1000];
        let d = exec64(EM_X86_64, &code);
        let i = image(&d).unwrap();
        assert!(i.is_x86() && i.is64() && i.wide);
        let init = 64 + 2 * 56;
        let text = 64 + 2 * 56 + 16;
        assert_eq!(i.code, [(0x40_0000 + init as u64, init, text + 1000 - init)]);
        let names: Vec<_> = i.sections.iter().map(|s| &s.name[..]).collect();
        assert_eq!(names, [&b".init"[..], b".text", b".rodata", b".shstrtab"]);
        assert_eq!(extent(&d), Some(d.len() as u64));
        let mut longer = d.clone();
        longer.extend_from_slice(b"trailing");
        assert_eq!(extent(&longer), Some(d.len() as u64));
    }

    /// Code sections with another between them are ranges of their own.
    #[test]
    fn code_apart() {
        let mut d = exec64(EM_X86_64, &[0xc3; 1000]);
        let shoff = u64le(&d, 40).unwrap() as usize;
        // .text not code, .rodata code
        d[shoff + 2 * 64 + 8] = 2;
        d[shoff + 3 * 64 + 8] = 6;
        let i = image(&d).unwrap();
        let (init, text) = (64 + 2 * 56, 64 + 2 * 56 + 16);
        assert_eq!(i.code, [(0x40_0000 + init as u64, init, 16), (0x40_0000 + text as u64 + 1000, text + 1000, 32)]);
    }

    #[test]
    fn without_section_headers() {
        let mut d = exec64(EM_AARCH64, &[0x1f, 0x20, 0x03, 0xd5].repeat(100));
        d[40..48].fill(0); // e_shoff
        d[60..64].fill(0); // e_shnum, e_shstrndx
        let i = image(&d).unwrap();
        assert!(i.sections.is_empty());
        // the executable segment, .rodata in it
        let init = 64 + 2 * 56;
        assert_eq!(i.code, [(0x40_0000 + init as u64, init, 16 + 400 + 32)]);
    }

    #[test]
    fn not_elf() {
        let d = exec64(EM_X86_64, &[0xc3; 100]);
        assert!(starts(&d));
        for (p, v) in [(4, 3), (5, 2), (6, 0), (20, 2), (52, 63)] {
            let mut e = d.clone();
            e[p] = v;
            assert!(!starts(&e), "{p}");
        }
        assert!(!starts(&d[..63]));
    }

    #[test]
    fn cut_short() {
        let d = exec64(EM_X86_64, &[0xc3; 1000]);
        // the section headers cut off: no sections, the executable segment instead
        let cut = &d[..900];
        let i = image(cut).unwrap();
        assert!(i.sections.is_empty());
        assert_eq!(i.code[0].1 + i.code[0].2, 900);
        assert_eq!(extent(cut), Some(d.len() as u64));
    }
}
