use alloc::{vec, vec::Vec};

use crate::util::{to_u16, to_u32, to_u64};

/// Executable sections of a PE image, as located by [`locate_code`].
#[derive(Debug, Clone)]
pub struct Code {
    /// `IMAGE_FILE_MACHINE_*` of the COFF header (0x14c x86, 0x8664 x86-64, 0xaa64 ARM64).
    pub machine: u16,
    /// True for PE32+ (64-bit) images.
    pub is64: bool,
    /// `(virtual address, file offset, stored size)` of each executable section: of a
    /// ReadyToRun image (.NET, x86-64), the extents of its native code instead,
    /// and of a .NET image of IL alone, not the section holding its metadata.
    pub sections: Vec<(u64, u64, usize)>,
    /// The metadata of a .NET image of IL alone or ReadyToRun, if it is one.
    pub cli: Option<Cli>,
}

/// The metadata of a .NET image, as located by [`locate_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cli {
    /// `(RVA, file offset, stored size)` of the section holding the metadata.
    pub section: (u32, u64, usize),
    /// The offset of the metadata root in that section.
    pub metadata: usize,
}

aqr_common::zopt! {
    /// Runtime functions closer than this are taken in one extent, the padding between them
    /// with them.
    static FUNCTION_GAP: Opt<u32> = Opt::value("exe:pe-function-gap", 64, "PE runtime functions closer than this are taken in one extent");
}

/// Locates the executable sections of the PE image in `f`.
pub fn locate_code<R: std::io::Read + std::io::Seek>(f: &mut R) -> std::io::Result<Code> {
    use std::io::{self, BufReader, Read, SeekFrom};

    f.seek(SeekFrom::Start(0))?;
    let mut r = BufReader::new(f);

    // MZ header
    let mut buf = [0u8; 0x40];
    r.read_exact(&mut buf)?;
    let sig = to_u16(&buf[0..2]);
    if sig != 0x5a4d {
        return Err(io::Error::other(format!("bad MZ signature {sig:#x}")));
    }
    let pe_offset = to_u32(&buf[0x3c..0x40]);
    if pe_offset < 0x40 {
        return Err(io::Error::other(format!("too low offset to PE header {pe_offset:#x}")));
    }

    r.seek_relative(pe_offset as i64 - 0x40)?;

    // PE header
    let mut buf = [0u8; 0x18];
    r.read_exact(&mut buf)?;
    let sig = to_u32(&buf[0..4]);
    if sig != 0x4550 {
        return Err(io::Error::other(format!("bad PE signature {sig:#x}")));
    }
    let machine = to_u16(&buf[4..6]);
    let num_sections = to_u16(&buf[6..8]);
    let opt_header_size = to_u16(&buf[0x14..0x16]) as usize;

    // PE optional header
    let mut opt_header = vec![0u8; opt_header_size];
    r.read_exact(&mut opt_header)?;
    let Some(magic) = opt_header.first_chunk().map(|m| u16::from_le_bytes(*m)) else {
        return Err(io::Error::other(format!("PE optional header too small ({opt_header_size:#x} < 0x2)")));
    };
    // offsets to ImageBase and NumberOfRvaAndSizes; data directories follow the latter
    let (is64, image_base_offset, num_data_dirs_offset) = match magic {
        0x10b => (false, 0x1c, 0x5c), // IMAGE_OPTIONAL_HEADER32
        0x20b => (true, 0x18, 0x6c),  // IMAGE_OPTIONAL_HEADER64
        _ => return Err(io::Error::other(format!("bad PE optional header magic {magic:#x}"))),
    };

    let dirs_offset = num_data_dirs_offset + 4;
    if opt_header_size < dirs_offset {
        return Err(io::Error::other(format!(
            "PE optional header too small ({opt_header_size:#x} < {dirs_offset:#x})"
        )));
    }
    let image_base = if is64 {
        to_u64(&opt_header[image_base_offset..image_base_offset + 8])
    } else {
        to_u32(&opt_header[image_base_offset..image_base_offset + 4]) as u64
    };
    // (an address is an RVA from it)
    if image_base.checked_add(u32::MAX as u64).is_none() {
        return Err(io::Error::other(format!("bad PE image base {image_base:#x}")));
    }
    let num_data_dirs = to_u32(&opt_header[num_data_dirs_offset..dirs_offset]) as usize;
    let min_size = dirs_offset + num_data_dirs * 8;
    if opt_header_size < min_size {
        return Err(io::Error::other(format!("PE optional header too small ({opt_header_size:#x} < {min_size:#x})")));
    }

    // data directories:
    // EXPORT, IMPORT, RESOURCE, EXCEPTION, SECURITY, BASERELOC, DEBUG, COPYRIGHT,
    // GLOBALPTR, TLS, LOAD_CONFIG, BOUND_IMPORT, IAT, DELAY_IMPORT, COM_DESCRIPTOR, -

    // section headers
    let mut sections = Vec::new();
    let mut headers = Vec::new();
    for _ in 0..num_sections {
        let mut buf = [0u8; 40];
        r.read_exact(&mut buf)?;
        let rva = to_u32(&buf[12..16]);
        let stored_size = to_u32(&buf[16..20]);
        let stored_offset = to_u32(&buf[20..24]);
        let flags = to_u32(&buf[36..40]);
        headers.push((rva, stored_offset, stored_size));
        if flags & 0x20 != 0 {
            sections.push((image_base + rva as u64, stored_offset as u64, stored_size as usize));
        }
    }

    let mut cli = None;
    if num_data_dirs > 14 {
        let dir = &opt_header[dirs_offset + 14 * 8..dirs_offset + 15 * 8];
        let mut image = Image { r: &mut r, headers: &headers };
        if let Some(clr) = image.read(to_u32(&dir[0..4]), to_u32(&dir[4..8])).filter(|c| c.len() >= 72) {
            cli = image.cli(&clr);
            if machine == 0x8664 {
                if let Some(native) = image.ready_to_run_code(&clr) {
                    let va = |(rva, off, size)| (image_base + rva as u64, off, size);
                    sections = native.into_iter().map(va).collect();
                }
            }
        }
    }
    if let Some(Cli { section: (rva, ..), .. }) = cli {
        sections.retain(|s| s.0 != image_base + rva as u64);
    }

    Ok(Code { machine, is64, sections, cli })
}

/// A table of a PE image that aqr-compress codes by its layout: `len` bytes at `off` in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table {
    pub off: usize,
    pub len: usize,
    pub kind: TableKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    /// The base relocations (data directory 5), for [`crate::reloc`].
    Reloc,
    /// Records of `fields` little-endian numbers of `width` bytes each, which mostly rise from
    /// one record to the next: RVAs and ordinals.
    Rising { width: usize, fields: usize },
}

/// The headers of a PE image read from its bytes, for [`tables`] and [`resources`].
struct Layout<'a> {
    d: &'a [u8],
    machine: u16,
    is64: bool,
    /// `(RVA, size)` of each data directory, `None` where empty.
    dirs: Vec<Option<(u32, u32)>>,
    /// `(RVA, file offset, stored size)` of each section.
    headers: Vec<(u32, u32, u32)>,
}

impl<'a> Layout<'a> {
    /// The headers of `d`, `None` where it is no PE image.
    fn of(d: &'a [u8]) -> Option<Self> {
        let get = |at: usize, n: usize| d.get(at..at.checked_add(n)?);
        let u16_at = |at: usize| get(at, 2).map(to_u16);
        let u32_at = |at: usize| get(at, 4).map(to_u32);
        let pe = u32_at(0x3c)? as usize;
        if get(0, 2)? != b"MZ" || get(pe, 4)? != b"PE\0\0" {
            return None;
        }
        let machine = u16_at(pe + 4)?;
        let num_sections = u16_at(pe + 6)? as usize;
        let opt = pe + 0x18;
        let opt_size = u16_at(pe + 0x14)? as usize;
        let (is64, num_dirs_at) = match u16_at(opt)? {
            0x10b => (false, 0x5c),
            0x20b => (true, 0x6c),
            _ => return None,
        };
        let num_dirs = (u32_at(opt + num_dirs_at)? as usize).min(16).min(opt_size.saturating_sub(num_dirs_at + 4) / 8);
        let dirs = (0..num_dirs)
            .map(|i| {
                let at = opt + num_dirs_at + 4 + 8 * i;
                Some((u32_at(at)?, u32_at(at + 4)?)).filter(|&(rva, size)| rva != 0 && size != 0)
            })
            .collect();
        let headers = (0..num_sections)
            .map_while(|i| {
                let h = get(opt + opt_size + 40 * i, 40)?;
                Some((to_u32(&h[12..16]), to_u32(&h[20..24]), to_u32(&h[16..20])))
            })
            .collect();
        Some(Layout { d, machine, is64, dirs, headers })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        self.d.get(at..at.checked_add(4)?).map(to_u32)
    }

    fn dir(&self, i: usize) -> Option<(u32, u32)> {
        self.dirs.get(i).copied().flatten()
    }

    /// The file offset of `size` bytes at `rva`, where whole within a section's stored data
    /// and within the file.
    fn at(&self, rva: u32, size: usize) -> Option<usize> {
        let &(s_rva, s_off, _) = self.headers.iter().find(|&&(s_rva, _, s_size)| {
            rva.checked_sub(s_rva).is_some_and(|o| (o as usize).checked_add(size).is_some_and(|e| e <= s_size as usize))
        })?;
        let off = s_off as usize + (rva - s_rva) as usize;
        self.d.get(off..off.checked_add(size)?).map(|_| off)
    }
}

/// The tables of the PE image `d` that [`Table`] describes, those in its stored data: the base
/// relocations; the export address table, the name pointers and the ordinals; the import
/// lookup and address tables (those that touch in the file as one); the runtime functions
/// (data directory 3) of x86-64 and ARM64. Empty where `d` is no PE image. In no order; they may
/// overlap each other or the code.
pub fn tables(d: &[u8]) -> Vec<Table> {
    let mut t = Vec::new();
    let _ = (|| -> Option<()> {
        let l = Layout::of(d)?;
        let mut push = |off: Option<usize>, len: usize, kind: TableKind| {
            if let Some(off) = off.filter(|_| len > 0) {
                t.push(Table { off, len, kind });
            }
        };

        if let Some((rva, size)) = l.dir(5) {
            push(l.at(rva, size as usize), size as usize, TableKind::Reloc);
        }
        if let Some((rva, _)) = l.dir(0) {
            if let Some(e) = l.at(rva, 40) {
                let (functions, names) = (l.u32_at(e + 20)? as usize, l.u32_at(e + 24)? as usize);
                let rising = |width| TableKind::Rising { width, fields: 1 };
                push(l.at(l.u32_at(e + 28)?, functions * 4), functions * 4, rising(4));
                push(l.at(l.u32_at(e + 32)?, names * 4), names * 4, rising(4));
                push(l.at(l.u32_at(e + 36)?, names * 2), names * 2, rising(2));
            }
        }
        if let Some((rva, _)) = l.dir(1) {
            let w = if l.is64 { 8 } else { 4 };
            let mut thunks: Vec<(usize, usize)> = Vec::new();
            for k in 0.. {
                let Some(i) = l.at(rva.wrapping_add(20 * k), 20) else { break };
                let (lookup, address) = (l.u32_at(i)?, l.u32_at(i + 16)?);
                if lookup == 0 && address == 0 {
                    break;
                }
                // each array of thunks up to its zero, with it
                for rva in [lookup, address].into_iter().filter(|&r| r != 0) {
                    let Some(start) = l.at(rva, w) else { continue };
                    let n = d[start..].chunks_exact(w).position(|c| c.iter().all(|&b| b == 0));
                    if let Some(n) = n.filter(|&n| l.at(rva, (n + 1) * w).is_some()) {
                        thunks.push((start, start + (n + 1) * w));
                    }
                }
            }
            thunks.sort_unstable();
            thunks.dedup();
            let mut merged: Vec<(usize, usize)> = Vec::new();
            for (s, e) in thunks {
                match merged.last_mut() {
                    Some(last) if s <= last.1 && (s - last.0) % w == 0 => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            for (s, e) in merged {
                push(Some(s), e - s, TableKind::Rising { width: w, fields: 1 });
            }
        }
        if let Some((rva, size)) = l.dir(3) {
            let k = match l.machine {
                0x8664 => 12,
                0xaa64 => 8,
                _ => 0,
            };
            if let Some(n) = (size as usize).checked_div(k) {
                let len = n * k;
                push(l.at(rva, len), len, TableKind::Rising { width: 4, fields: k / 4 });
            }
        }
        Some(())
    })();
    t
}

/// The most entries of a resource directory that [`resources`] takes.
const MOST_RESOURCES: usize = 1 << 16;

/// The data of the resources of the PE image `d` (data directory 2): where each data entry's
/// bytes are in the file, those whole within a section's stored data, in file order, none
/// overlapping another (of two that overlap, the first is taken; the same data told by several
/// entries is taken once). Empty where `d` is no PE image or has no resources. The directory is
/// walked three levels deep (type, name, language), as Windows reads it, a directory reached
/// twice walked once; at most [`MOST_RESOURCES`] entries.
pub fn resources(d: &[u8]) -> Vec<core::ops::Range<usize>> {
    let mut out = Vec::new();
    let _ = (|| -> Option<()> {
        let l = Layout::of(d)?;
        let (rva, size) = l.dir(2)?;
        let base = l.at(rva, size as usize).or_else(|| l.at(rva, 16))?;
        let u16_at = |at: usize| d.get(at..at.checked_add(2)?).map(to_u16);
        // (directories by their offset from the base, with their depth)
        let (mut todo, mut seen, mut entries) = (vec![(0usize, 0u8)], alloc::collections::BTreeSet::new(), 0usize);
        while let Some((dir, depth)) = todo.pop() {
            if !seen.insert(dir) {
                continue;
            }
            let at = base.checked_add(dir)?;
            let n = u16_at(at + 12)? as usize + u16_at(at + 14)? as usize;
            for k in 0..n {
                entries += 1;
                if entries > MOST_RESOURCES {
                    return None;
                }
                let Some(to) = l.u32_at(at + 16 + 8 * k + 4) else { break };
                let off = (to & 0x7fff_ffff) as usize;
                match to & 0x8000_0000 != 0 {
                    true if depth < 2 => todo.push((off, depth + 1)),
                    true => {}
                    false => {
                        let Some(e) = base.checked_add(off) else { continue };
                        let (Some(rva), Some(len)) = (l.u32_at(e), l.u32_at(e + 4)) else { continue };
                        if let Some(start) = l.at(rva, len as usize).filter(|_| len > 0) {
                            out.push(start..start + len as usize);
                        }
                    }
                }
            }
        }
        Some(())
    })();
    out.sort_by_key(|r| (r.start, core::cmp::Reverse(r.end)));
    let mut end = 0;
    out.retain(|r| {
        let keep = r.start >= end;
        end = end.max(r.end);
        keep
    });
    out
}

/// A PE image being read, its sections' `(RVA, file offset, stored size)` known.
struct Image<'a, R> {
    r: &'a mut R,
    headers: &'a [(u32, u32, u32)],
}

impl<R: std::io::Read + std::io::Seek> Image<'_, R> {
    /// The section holding `rva..rva + size` in its stored data.
    fn find(&self, rva: u32, size: u32) -> Option<(u32, u32, u32)> {
        self.headers.iter().copied().find(|&(s_rva, _, s_size)| {
            rva.checked_sub(s_rva).is_some_and(|o| o.checked_add(size).is_some_and(|e| e <= s_size))
        })
    }

    fn read(&mut self, rva: u32, size: u32) -> Option<Vec<u8>> {
        let (s_rva, s_off, _) = self.find(rva, size)?;
        self.r.seek(std::io::SeekFrom::Start(s_off as u64 + (rva - s_rva) as u64)).ok()?;
        // (read as far as the file goes: `size` is a header's, and the stored data may lie past it)
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(&mut *self.r, size as u64), &mut buf).ok()?;
        (buf.len() == size as usize).then_some(buf)
    }

    /// The metadata that the CLR header `clr` (`IMAGE_COR20_HEADER`) points to, where the
    /// image is of IL alone (`COMIMAGE_FLAGS_ILONLY`) or ReadyToRun.
    fn cli(&mut self, clr: &[u8]) -> Option<Cli> {
        let (rva, size, flags) = (to_u32(&clr[8..12]), to_u32(&clr[12..16]), to_u32(&clr[16..20]));
        if flags & 1 == 0 && self.ready_to_run(clr).is_none() {
            return None;
        }
        let (s_rva, s_off, s_size) = self.find(rva, size)?;
        Some(Cli { section: (s_rva, s_off as u64, s_size as usize), metadata: (rva - s_rva) as usize })
    }

    /// The sections `(type, RVA, size)` of the ReadyToRun header that the CLR header `clr`
    /// points to (its ManagedNativeHeader), if any.
    fn ready_to_run(&mut self, clr: &[u8]) -> Option<Vec<(u32, u32, u32)>> {
        let (rtr_rva, rtr_size) = (to_u32(&clr[64..68]), to_u32(&clr[68..72]));
        // READYTORUN_HEADER: signature, version, flags, number of sections, then sections
        if rtr_rva == 0 || rtr_size < 16 {
            return None;
        }
        let rtr = self.read(rtr_rva, 16)?;
        if &rtr[0..4] != b"RTR\0" {
            return None;
        }
        let num_sections = to_u32(&rtr[12..16]);
        let sections = self.read(rtr_rva.checked_add(16)?, num_sections.checked_mul(12)?)?;
        Some(
            sections
                .as_chunks::<12>()
                .0
                .iter()
                .map(|s| (to_u32(&s[0..4]), to_u32(&s[4..8]), to_u32(&s[8..12])))
                .collect(),
        )
    }

    /// The extents `(RVA, file offset, size)` of the native code of a ReadyToRun image of
    /// x86-64, from the runtime functions of its ReadyToRun header, which the CLR header
    /// `clr` points to; `None` where there are none, or one is not within the stored data of
    /// a section.
    fn ready_to_run_code(&mut self, clr: &[u8]) -> Option<Vec<(u32, u64, usize)>> {
        // READYTORUN_SECTION_RUNTIME_FUNCTIONS; RUNTIME_FUNCTION of x86-64: begin, end, unwind data
        let (_, funcs_rva, funcs_size) = self.ready_to_run(clr)?.into_iter().find(|s| s.0 == 102)?;
        let funcs = self.read(funcs_rva, funcs_size / 12 * 12)?;
        let mut funcs: Vec<(u32, u32)> =
            funcs.as_chunks::<12>().0.iter().map(|f| (to_u32(&f[0..4]), to_u32(&f[4..8]))).collect();
        funcs.sort_unstable();

        let mut extents: Vec<(u32, u32)> = Vec::new();
        let gap = FUNCTION_GAP.get();
        for (begin, end) in funcs {
            if end <= begin {
                return None;
            }
            match extents.last_mut() {
                Some(last) if begin <= last.1.saturating_add(gap) => last.1 = last.1.max(end),
                _ => extents.push((begin, end)),
            }
        }
        if extents.is_empty() {
            return None;
        }
        extents
            .into_iter()
            .map(|(begin, end)| {
                let (s_rva, s_off, _) = self.find(begin, end - begin)?;
                Some((begin, s_off as u64 + (begin - s_rva) as u64, (end - begin) as usize))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(d: &mut [u8], at: usize, v: &[u8]) {
        d[at..at + v.len()].copy_from_slice(v);
    }

    #[test]
    fn optional_header_too_small_for_its_magic() {
        for size in 0..2u16 {
            let mut d = vec![0u8; 0x60];
            put(&mut d, 0, b"MZ");
            put(&mut d, 0x3c, &0x40u32.to_le_bytes());
            put(&mut d, 0x40, b"PE\0\0");
            put(&mut d, 0x44, &0x8664u16.to_le_bytes());
            put(&mut d, 0x54, &size.to_le_bytes());
            assert!(locate_code(&mut std::io::Cursor::new(&d)).is_err(), "size {size}");
        }
    }

    /// A PE32+ image with one section, `.text` (RVA 0x1000, at 0x200, 0x1000 bytes), holding
    /// a CLR header at its start; with `functions`, a ReadyToRun header with those runtime
    /// functions `(begin, end)`.
    fn image(functions: Option<&[(u32, u32)]>) -> Vec<u8> {
        let mut d = vec![0u8; 0x1200];
        put(&mut d, 0, b"MZ");
        put(&mut d, 0x3c, &0x40u32.to_le_bytes());
        put(&mut d, 0x40, b"PE\0\0");
        put(&mut d, 0x44, &0x8664u16.to_le_bytes());
        put(&mut d, 0x46, &1u16.to_le_bytes());
        put(&mut d, 0x54, &0xf0u16.to_le_bytes());
        let opt = 0x58;
        put(&mut d, opt, &0x20bu16.to_le_bytes());
        put(&mut d, opt + 0x18, &0x1_8000_0000u64.to_le_bytes());
        put(&mut d, opt + 0x6c, &16u32.to_le_bytes());
        put(&mut d, opt + 0x70 + 14 * 8, &[0x1000u32.to_le_bytes(), 72u32.to_le_bytes()].concat());
        let sh = opt + 0xf0;
        put(&mut d, sh, b".text\0\0\0");
        for (i, v) in [0x1000u32, 0x1000, 0x1000, 0x200].into_iter().enumerate() {
            put(&mut d, sh + 8 + 4 * i, &v.to_le_bytes());
        }
        put(&mut d, sh + 36, &0x6000_0020u32.to_le_bytes());
        // CLR header at RVA 0x1000; ManagedNativeHeader at RVA 0x1100
        put(&mut d, 0x200, &72u32.to_le_bytes());
        if let Some(functions) = functions {
            put(&mut d, 0x200 + 64, &[0x1100u32.to_le_bytes(), 16u32.to_le_bytes()].concat());
            put(&mut d, 0x300, b"RTR\0");
            put(&mut d, 0x304, &[5u16.to_le_bytes(), 0u16.to_le_bytes()].concat());
            put(&mut d, 0x30c, &1u32.to_le_bytes());
            let size = 12 * functions.len() as u32;
            put(&mut d, 0x310, &[102u32, 0x1200, size].map(u32::to_le_bytes).concat());
            for (i, &(b, e)) in functions.iter().enumerate() {
                put(&mut d, 0x400 + 12 * i, &[b, e, 0x1f00].map(u32::to_le_bytes).concat());
            }
        }
        d
    }

    #[test]
    fn il_only_whole_section() {
        let code = locate_code(&mut std::io::Cursor::new(image(None))).unwrap();
        assert_eq!(code.sections, [(0x1_8000_1000, 0x200, 0x1000)]);
    }

    #[test]
    fn il_only_metadata() {
        let mut d = image(None);
        // metadata at RVA 0x1500, COMIMAGE_FLAGS_ILONLY
        put(&mut d, 0x208, &[0x1500u32, 0x100, 1].map(u32::to_le_bytes).concat());
        let code = locate_code(&mut std::io::Cursor::new(d)).unwrap();
        assert_eq!(code.cli, Some(Cli { section: (0x1000, 0x200, 0x1000), metadata: 0x500 }));
        assert_eq!(code.sections, []);
    }

    #[test]
    fn ready_to_run_native_code() {
        // out of order, the first two merged over their gap
        let functions = [(0x1800, 0x1900), (0x1400, 0x1480), (0x1490, 0x1500)];
        let code = locate_code(&mut std::io::Cursor::new(image(Some(&functions)))).unwrap();
        assert_eq!(code.sections, [(0x1_8000_1400, 0x600, 0x100), (0x1_8000_1800, 0xa00, 0x100)]);
        assert_eq!(code.cli, None);
    }

    #[test]
    fn ready_to_run_metadata() {
        let functions = [(0x1400, 0x1480)];
        let mut d = image(Some(&functions));
        put(&mut d, 0x208, &[0x1500u32, 0x100, 0].map(u32::to_le_bytes).concat());
        let code = locate_code(&mut std::io::Cursor::new(d)).unwrap();
        assert_eq!(code.cli, Some(Cli { section: (0x1000, 0x200, 0x1000), metadata: 0x500 }));
        assert_eq!(code.sections, [(0x1_8000_1400, 0x600, 0x80)]);
    }

    #[test]
    fn ready_to_run_out_of_section() {
        // a function past the section's stored data: the section is taken whole
        let functions = [(0x1400, 0x1480), (0x1f00, 0x2100)];
        let code = locate_code(&mut std::io::Cursor::new(image(Some(&functions)))).unwrap();
        assert_eq!(code.sections, [(0x1_8000_1000, 0x200, 0x1000)]);
    }

    /// A PE32+ image of one section, `.rsrc` (RVA 0x1000, at 0x400, 0x1000 bytes), holding the
    /// resource directory `dir` at its start (data directory 2), the rest of it of 0x5a.
    fn resource_image(dir: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 0x1400];
        put(&mut d, 0, b"MZ");
        put(&mut d, 0x3c, &0x40u32.to_le_bytes());
        put(&mut d, 0x40, b"PE\0\0");
        put(&mut d, 0x44, &0x8664u16.to_le_bytes());
        put(&mut d, 0x46, &1u16.to_le_bytes());
        put(&mut d, 0x54, &0xf0u16.to_le_bytes());
        let opt = 0x58;
        put(&mut d, opt, &0x20bu16.to_le_bytes());
        put(&mut d, opt + 0x6c, &16u32.to_le_bytes());
        put(&mut d, opt + 0x70 + 16, &[0x1000u32, 0x1000].map(u32::to_le_bytes).concat());
        let sec = opt + 0xf0;
        put(&mut d, sec, b".rsrc");
        put(&mut d, sec + 8, &[0x1000u32, 0x1000, 0x1000, 0x400].map(u32::to_le_bytes).concat());
        d[0x400..].fill(0x5a);
        put(&mut d, 0x400, dir);
        d
    }

    /// A resource directory of `entries` `(name, offset)` at its start.
    fn directory(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut d = vec![0u8; 16];
        put(&mut d, 14, &(entries.len() as u16).to_le_bytes());
        entries.iter().for_each(|&(n, to)| d.extend([n, to].map(u32::to_le_bytes).concat()));
        d
    }

    #[test]
    fn resources_in_file_order() {
        // types 3 and 10 (subdirectories at 0x100 and 0x200); type 3's names 1 (its languages
        // at 0x300) and 2 (a data entry at the depth of names, taken still), type 10's one name
        // whose languages point to the same data twice and to data past the section; a
        // directory pointing back to the root
        let mut dir = vec![0u8; 0x400];
        let at = |dir: &mut Vec<u8>, off: usize, v: &[u8]| dir[off..off + v.len()].copy_from_slice(v);
        at(&mut dir, 0, &directory(&[(3, 0x8000_0100), (10, 0x8000_0200)]));
        at(&mut dir, 0x100, &directory(&[(1, 0x8000_0300), (2, 0x3a0)]));
        at(&mut dir, 0x200, &directory(&[(1, 0x8000_0340)]));
        at(&mut dir, 0x300, &directory(&[(1033, 0x380), (1038, 0x8000_0000)]));
        at(&mut dir, 0x340, &directory(&[(1033, 0x390), (1038, 0x390), (1040, 0x3b0)]));
        // (data entries: RVA and size)
        at(&mut dir, 0x380, &[0x1800u32, 0x100].map(u32::to_le_bytes).concat());
        at(&mut dir, 0x390, &[0x1500u32, 0x80].map(u32::to_le_bytes).concat());
        at(&mut dir, 0x3a0, &[0x1600u32, 0x40].map(u32::to_le_bytes).concat());
        at(&mut dir, 0x3b0, &[0x1f80u32, 0x100].map(u32::to_le_bytes).concat());
        let d = resource_image(&dir);
        assert_eq!(resources(&d), [0x900..0x980, 0xa00..0xa40, 0xc00..0xd00]);
        // overlapping data: the first, the longest of those starting alike
        at(&mut dir, 0x3a0, &[0x1520u32, 0x100].map(u32::to_le_bytes).concat());
        assert_eq!(resources(&resource_image(&dir)), [0x900..0x980, 0xc00..0xd00]);
        // no resources, and no PE image
        assert_eq!(resources(&resource_image(&directory(&[]))), []);
        assert_eq!(resources(&d[..0x200]), []);
        assert_eq!(resources(b"MZ"), []);
    }
}
