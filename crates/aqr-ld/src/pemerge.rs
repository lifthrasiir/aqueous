//! Merges native module libraries into a stub executable as new sections, so that the stub
//! carries the modules it needs with no loader and no separate files: a first, minimal step
//! toward self-extracting archives (`aqr-ld merge --stub S --out O NAME=MODULE.dll ...`).
//!
//! Each module is a position-independent library whose code uses neither thread-local storage
//! (one per image, its offsets baked into instructions) nor imports (its IAT would be unbound):
//! the whole of its image is copied into the stub at a chosen RVA with its layout kept, so that
//! every RIP-relative reference stays valid, and its base relocations (`DIR64` only) are applied
//! for the new address and emitted into the stub's relocations so that ASLR still works. A
//! dispatch table naming each module and the RVA of its `aqr_mod_apply` is added too, and its
//! place written into the stub's anchor (its magic). This is validated on trivial modules by
//! `crates/aqr-ld/pemerge-test.sh`; the real codec modules (aqr-dll) use the standard library,
//! so they are not mergeable this way yet (they need to be made free of it first).
//!
//! x86-64 only. The structures are the PE/COFF ones (Microsoft's "PE Format").

use std::path::Path;

const MAGIC: &[u8; 16] = b"\x89AQRPEMERGEv1\x00\x00\x00";

/// The characteristics of a merged section: code and data, readable, writable and executable
/// (a module holds all three; writable, as its relocations are applied where it loads).
const SEC_FLAGS: u32 = 0x0000_0020 | 0x0000_0040 | 0x2000_0000 | 0x4000_0000 | 0x8000_0000;
/// The characteristics of the dispatch and relocation sections: initialised data, read-only.
const DATA_FLAGS: u32 = 0x0000_0040 | 0x4000_0000;
const RELOC_FLAGS: u32 = 0x0000_0040 | 0x0200_0000 | 0x4000_0000; // + discardable

type Error = String;
type Result<T> = std::result::Result<T, Error>;

fn u16(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as u32
}
fn u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn put_u16(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
}
fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn align(n: u32, a: u32) -> u32 {
    n.div_ceil(a) * a
}

struct Section {
    va: u32,
    vsize: u32,
    raw_ptr: u32,
    raw_size: u32,
}

/// A parsed PE image (PE32+).
struct Pe {
    buf: Vec<u8>,
    /// The offset of the PE signature.
    pe: usize,
    /// The offset of the optional header.
    opt: usize,
    sections: Vec<Section>,
}

impl Pe {
    fn parse(buf: Vec<u8>) -> Result<Pe> {
        if buf.len() < 0x40 || &buf[0..2] != b"MZ" {
            return Err("not an MZ image".into());
        }
        let pe = u32(&buf, 0x3c) as usize;
        if buf.len() < pe + 24 || &buf[pe..pe + 4] != b"PE\0\0" {
            return Err("no PE signature".into());
        }
        if u16(&buf, pe + 4) != 0x8664 {
            return Err("not x86-64".into());
        }
        let opt = pe + 24;
        if u16(&buf, opt) != 0x20b {
            return Err("not PE32+".into());
        }
        let nsec = u16(&buf, pe + 6) as usize;
        let table = opt + u16(&buf, pe + 20) as usize;
        let sections = (0..nsec)
            .map(|i| {
                let s = table + i * 40;
                Section {
                    vsize: u32(&buf, s + 8),
                    va: u32(&buf, s + 12),
                    raw_size: u32(&buf, s + 16),
                    raw_ptr: u32(&buf, s + 20),
                }
            })
            .collect();
        Ok(Pe { buf, pe, opt, sections })
    }

    fn image_base(&self) -> u64 {
        u64(&self.buf, self.opt + 24)
    }
    fn section_align(&self) -> u32 {
        u32(&self.buf, self.opt + 32)
    }
    fn file_align(&self) -> u32 {
        u32(&self.buf, self.opt + 36)
    }
    fn size_of_image(&self) -> u32 {
        u32(&self.buf, self.opt + 56)
    }
    fn size_of_headers(&self) -> u32 {
        u32(&self.buf, self.opt + 60)
    }
    /// The RVA and size of the `i`-th data directory.
    fn dir(&self, i: usize) -> (u32, u32) {
        let d = self.opt + 112 + i * 8;
        (u32(&self.buf, d), u32(&self.buf, d + 4))
    }

    /// The file offset of RVA `rva`.
    fn off(&self, rva: u32) -> Result<usize> {
        for s in &self.sections {
            if rva >= s.va && rva < s.va + s.vsize.max(s.raw_size) {
                return Ok((s.raw_ptr + (rva - s.va)) as usize);
            }
        }
        Err(format!("RVA {rva:#x} in no section"))
    }

    /// The module's memory image: a buffer of `size_of_image` bytes, each section at its RVA.
    fn image(&self) -> Vec<u8> {
        let mut img = vec![0u8; self.size_of_image() as usize];
        for s in &self.sections {
            let n = s.raw_size.min(s.vsize.max(s.raw_size)) as usize;
            let (src, dst) = (s.raw_ptr as usize, s.va as usize);
            let n = n.min(self.buf.len().saturating_sub(src));
            img[dst..dst + n].copy_from_slice(&self.buf[src..src + n]);
        }
        img
    }

    /// The RVAs of every `DIR64` base relocation (type 10); an error on any other type.
    fn relocs(&self) -> Result<Vec<u32>> {
        let (rva, size) = self.dir(5);
        if rva == 0 {
            return Ok(Vec::new());
        }
        let base = self.off(rva)?;
        let bytes = &self.buf[base..base + size as usize];
        let mut out = Vec::new();
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let page = u32(bytes, at);
            let block = u32(bytes, at + 4) as usize;
            if block < 8 {
                break;
            }
            for e in (at + 8..at + block).step_by(2) {
                let entry = u16(bytes, e);
                match entry >> 12 {
                    0 => {} // padding
                    10 => out.push(page + (entry & 0xfff)),
                    t => return Err(format!("relocation of type {t}, not DIR64")),
                }
            }
            at += block;
        }
        Ok(out)
    }

    /// The RVA of the exported function `name`.
    fn export(&self, name: &str) -> Result<u32> {
        let (rva, _) = self.dir(0);
        if rva == 0 {
            return Err("no export directory".into());
        }
        let e = self.off(rva)?;
        let n_names = u32(&self.buf, e + 24) as usize;
        let functions = self.off(u32(&self.buf, e + 28))?;
        let names = self.off(u32(&self.buf, e + 32))?;
        let ordinals = self.off(u32(&self.buf, e + 36))?;
        for i in 0..n_names {
            let name_rva = u32(&self.buf, names + i * 4);
            let at = self.off(name_rva)?;
            let end = self.buf[at..].iter().position(|&b| b == 0).ok_or("unterminated export name")?;
            if &self.buf[at..at + end] == name.as_bytes() {
                let ord = u16(&self.buf, ordinals + i * 2) as usize;
                return Ok(u32(&self.buf, functions + ord * 4));
            }
        }
        Err(format!("no export {name}"))
    }
}

/// A new section to append to the stub.
struct New {
    name: [u8; 8],
    va: u32,
    vsize: u32,
    flags: u32,
    data: Vec<u8>,
}

/// Merges `modules` (name, path) into the stub at `stub` and writes the result to `out`.
pub fn merge(stub: &Path, out: &Path, modules: &[(String, std::path::PathBuf)]) -> Result<()> {
    let mut pe = Pe::parse(std::fs::read(stub).map_err(|e| format!("{}: {e}", stub.display()))?)?;
    let salign = pe.section_align();
    let falign = pe.file_align();
    let base = pe.image_base();

    // where the next merged section lands, after the stub's image
    let mut next_va = align(pe.size_of_image(), salign);
    let mut news: Vec<New> = Vec::new();
    let mut new_relocs: Vec<u32> = Vec::new();
    // the dispatch entries: (name, apply_rva in the merged image)
    let mut entries: Vec<(String, u32)> = Vec::new();

    for (name, path) in modules {
        let m = Pe::parse(std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)?;
        if m.dir(9).0 != 0 {
            return Err(format!("{name}: the module uses thread-local storage"));
        }
        let r = next_va;
        let mut img = m.image();
        // rebase: an absolute address in the module (module_base + x) becomes stub_base + r + x
        let delta = base.wrapping_add(r as u64).wrapping_sub(m.image_base());
        for &loc in &m.relocs()? {
            let o = loc as usize;
            let v = u64::from_le_bytes(img[o..o + 8].try_into().unwrap()).wrapping_add(delta);
            img[o..o + 8].copy_from_slice(&v.to_le_bytes());
            new_relocs.push(r + loc);
        }
        entries.push((name.clone(), r + m.export("aqr_mod_apply")?));
        let vsize = img.len() as u32;
        let mut sname = [0u8; 8];
        let tag = format!(".aqrm{:02}", news.len());
        sname[..tag.len().min(8)].copy_from_slice(&tag.as_bytes()[..tag.len().min(8)]);
        news.push(New { name: sname, va: r, vsize, flags: SEC_FLAGS, data: img });
        next_va = align(r + vsize, salign);
    }

    // the dispatch table: count, then (apply_rva, name_off, name_len) each, then the names
    let mut disp = Vec::new();
    disp.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let names_at = 4 + entries.len() * 12;
    let mut names = Vec::new();
    for (name, apply) in &entries {
        disp.extend_from_slice(&apply.to_le_bytes());
        disp.extend_from_slice(&((names_at + names.len()) as u32).to_le_bytes());
        disp.extend_from_slice(&(name.len() as u32).to_le_bytes());
        names.extend_from_slice(name.as_bytes());
    }
    disp.extend_from_slice(&names);
    let disp_va = next_va;
    let disp_len = disp.len() as u32;
    news.push(New { name: *b".aqrd\0\0\0", va: disp_va, vsize: disp_len, flags: DATA_FLAGS, data: disp });
    next_va = align(disp_va + disp_len, salign);

    // the relocations: the stub's, then one block per 0x1000 page of the merged ones
    let mut reloc = {
        let (rva, size) = pe.dir(5);
        match rva {
            0 => Vec::new(),
            _ => pe.buf[pe.off(rva)?..pe.off(rva)? + size as usize].to_vec(),
        }
    };
    new_relocs.sort_unstable();
    let mut i = 0;
    while i < new_relocs.len() {
        let page = new_relocs[i] & !0xfff;
        let mut block: Vec<u8> = Vec::new();
        while i < new_relocs.len() && new_relocs[i] & !0xfff == page {
            block.extend_from_slice(&((10 << 12 | (new_relocs[i] & 0xfff)) as u16).to_le_bytes());
            i += 1;
        }
        if !block.len().is_multiple_of(4) {
            block.extend_from_slice(&0u16.to_le_bytes()); // pad to 4 bytes
        }
        reloc.extend_from_slice(&page.to_le_bytes());
        reloc.extend_from_slice(&((block.len() + 8) as u32).to_le_bytes());
        reloc.extend_from_slice(&block);
    }
    let reloc_va = next_va;
    let reloc_len = reloc.len() as u32;
    news.push(New { name: *b".aqrr\0\0\0", va: reloc_va, vsize: reloc_len, flags: RELOC_FLAGS, data: reloc });
    next_va = align(reloc_va + reloc_len, salign);

    // room for the new section headers, below the first section's raw data
    let table = pe.opt + u16(&pe.buf, pe.pe + 20) as usize;
    let old_n = u16(&pe.buf, pe.pe + 6) as usize;
    let headers_end = table + (old_n + news.len()) * 40;
    if headers_end as u32 > pe.size_of_headers() {
        return Err("no room for the new section headers".into());
    }

    // append each new section's data to the file, file-aligned
    for (k, s) in news.iter().enumerate() {
        let ptr = align(pe.buf.len() as u32, falign);
        pe.buf.resize(ptr as usize, 0);
        let h = table + (old_n + k) * 40;
        pe.buf[h..h + 8].copy_from_slice(&s.name);
        put_u32(&mut pe.buf, h + 8, s.vsize);
        put_u32(&mut pe.buf, h + 12, s.va);
        put_u32(&mut pe.buf, h + 16, align(s.data.len() as u32, falign));
        put_u32(&mut pe.buf, h + 20, ptr);
        put_u32(&mut pe.buf, h + 36, s.flags);
        pe.buf.extend_from_slice(&s.data);
        let pad = align(pe.buf.len() as u32, falign) as usize;
        pe.buf.resize(pad, 0);
    }

    // the header fields: section count, image size, the relocation directory
    put_u16(&mut pe.buf, pe.pe + 6, (old_n + news.len()) as u32);
    put_u32(&mut pe.buf, pe.opt + 56, align(next_va, salign));
    put_u32(&mut pe.buf, pe.opt + 112 + 5 * 8, reloc_va);
    put_u32(&mut pe.buf, pe.opt + 112 + 5 * 8 + 4, reloc_len);

    // the anchor: the dispatch table's place, written after its magic
    let anchor = find(&pe.buf, MAGIC).ok_or("the stub has no anchor (its magic)")?;
    put_u32(&mut pe.buf, anchor + 16, disp_va);
    put_u32(&mut pe.buf, anchor + 16 + 4, disp_len);

    std::fs::write(out, &pe.buf).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `aqr-ld merge --stub S --out O NAME=MODULE.dll ...`.
pub fn main(args: &[std::ffi::OsString]) -> Result<()> {
    let mut stub = None;
    let mut out = None;
    let mut modules = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--stub") => stub = Some(it.next().ok_or("--stub: no path")?.into()),
            Some("--out") => out = Some(it.next().ok_or("--out: no path")?.into()),
            _ => {
                let a = a.to_str().ok_or("a module argument is not UTF-8")?;
                let (name, path) = a.split_once('=').ok_or(format!("{a}: not NAME=MODULE.dll"))?;
                modules.push((name.to_owned(), std::path::PathBuf::from(path)));
            }
        }
    }
    let stub: std::path::PathBuf = stub.ok_or("merge: no --stub")?;
    let out: std::path::PathBuf = out.ok_or("merge: no --out")?;
    merge(&stub, &out, &modules)
}
