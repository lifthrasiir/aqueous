//! Merges native module libraries into a stub executable as new sections, so that the stub
//! carries the modules it needs with no loader and no separate files: the self-extracting
//! archives of `aqr --format sfx` ([`merge_bytes`]), and `aqr-ld merge --stub S --out O
//! NAME=MODULE ...`.
//!
//! Each module is a position-independent library whose code uses no thread-local storage of its
//! own image (its offsets baked into instructions; aqr-dll's no-TLS target has std's use
//! TlsAlloc): the whole of its image is copied into the stub at a chosen RVA with its layout
//! kept, so that every RIP-relative reference stays valid, its base relocations (`DIR64` only)
//! are applied for the new address and emitted into the stub's relocations so that ASLR still
//! works, and its imports are merged into the stub's import table. Its function entries go into
//! the stub's exception directory, its unwind data rebased ([`crate::unwind`]): the unwinder
//! reads no other table for an address in an image. The modules' images are in one section, each
//! at an RVA of its own (the stub's headers have room for few more section headers). A dispatch
//! table naming each module, the RVA of its entry and where its image is (for the stub to
//! register it, `aqr_blob::register_image`, for its panics) is added too, and its place written
//! into the stub's anchor (its magic). Validated by `crates/aqr-ld/pemerge-test.sh`
//! on trivial modules, `crates/aqr-dll/merge-test.sh` on the codec modules.
//!
//! x86-64 only. The structures are the PE/COFF ones (Microsoft's "PE Format").

use std::path::Path;

use aqr_blob::dispatch::{self, ANCHOR_MAGIC, Entry};
use aqr_blob::{Bind, Blob};
use aqr_blob::{IMPORT_BY_ORDINAL, IMPORT_DESCRIPTOR_LEN};

/// The data directories of the optional header that are used here, by index.
const DIR_EXPORT: usize = 0;
const DIR_IMPORT: usize = 1;
const DIR_EXCEPTION: usize = 3;
const DIR_RELOC: usize = 5;
const DIR_DEBUG: usize = 6;
const DIR_TLS: usize = 9;
const DIR_LOAD_CONFIG: usize = 10;
const DIR_BOUND_IMPORT: usize = 11;

/// The size of an entry of the exception directory (a `RUNTIME_FUNCTION`).
pub(crate) const RUNTIME_FUNCTION_LEN: usize = 12;

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
    /// Sets the RVA and size of the `i`-th data directory.
    fn set_dir(&mut self, i: usize, rva: u32, size: u32) {
        let d = self.opt + 112 + i * 8;
        put_u32(&mut self.buf, d, rva);
        put_u32(&mut self.buf, d + 4, size);
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

    /// The RVAs of every `DIR64` base relocation (type 10); an error on any other type.
    fn relocs(&self) -> Result<Vec<u32>> {
        let (rva, size) = self.dir(DIR_RELOC);
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
        let (rva, _) = self.dir(DIR_EXPORT);
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

    /// The RVA of the exported function `name`, or 0 for `-`.
    fn export_opt(&self, name: &str) -> Result<u32> {
        match name {
            "-" => Ok(0),
            _ => self.export(name),
        }
    }

    /// The library's own name (its export directory's, lowercase) and its exports, by name and
    /// by ordinal; `None` for an image that exports nothing.
    fn exports(&self) -> Result<Option<Exports>> {
        let (rva, size) = self.dir(DIR_EXPORT);
        if rva == 0 {
            return Ok(None);
        }
        let e = self.off(rva)?;
        let cstr = |at: usize| -> Result<Vec<u8>> {
            let end = self.buf[at..].iter().position(|&b| b == 0).ok_or("unterminated export name")?;
            Ok(self.buf[at..at + end].to_vec())
        };
        let name = String::from_utf8_lossy(&cstr(self.off(u32(&self.buf, e + 12))?)?).to_lowercase();
        let base = u32(&self.buf, e + 16);
        let (n_functions, n_names) = (u32(&self.buf, e + 20) as usize, u32(&self.buf, e + 24) as usize);
        let functions = self.off(u32(&self.buf, e + 28))?;
        let mut by_ordinal = std::collections::HashMap::new();
        for i in 0..n_functions {
            let f = u32(&self.buf, functions + i * 4);
            if f >= rva && f < rva + size {
                return Err(format!("{name}: a forwarded export"));
            }
            if f != 0 {
                by_ordinal.insert(base + i as u32, f);
            }
        }
        let mut by_name = std::collections::HashMap::new();
        if n_names > 0 {
            let (names, ordinals) = (self.off(u32(&self.buf, e + 32))?, self.off(u32(&self.buf, e + 36))?);
            for i in 0..n_names {
                let at = self.off(u32(&self.buf, names + i * 4))?;
                let ord = u16(&self.buf, ordinals + i * 2) as usize;
                by_name.insert(cstr(at)?, u32(&self.buf, functions + ord * 4));
            }
        }
        Ok(Some(Exports { name, by_name, by_ordinal }))
    }

    /// The stub's import descriptors (20 bytes each), without the zero terminator.
    fn descriptors(&self) -> Result<Vec<u8>> {
        let (rva, _) = self.dir(DIR_IMPORT);
        if rva == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut d = self.off(rva)?;
        while self.buf[d..d + IMPORT_DESCRIPTOR_LEN].iter().any(|&b| b != 0) {
            out.extend_from_slice(&self.buf[d..d + IMPORT_DESCRIPTOR_LEN]);
            d += IMPORT_DESCRIPTOR_LEN;
        }
        Ok(out)
    }
}

/// What a library exports ([`Pe::exports`]).
struct Exports {
    name: String,
    by_name: std::collections::HashMap<Vec<u8>, u32>,
    by_ordinal: std::collections::HashMap<u32, u32>,
}

/// Binds the imports of a module's image `img` (its bytes from RVA `first`) from the libraries of
/// `set` (by their names): each slot of their import address tables to its export, the import
/// descriptors of those libraries taken out of the import directory (the others moved up). The
/// loader then binds only the imports from outside the set.
fn bind_set(img: &mut [u8], first: u32, dir1_rva: u32, set: &[&Exports]) -> Result<Vec<Bind>> {
    if dir1_rva == 0 {
        return Ok(Vec::new());
    }
    let idx = |rva: u32| (rva - first) as usize;
    let cstr = |img: &[u8], rva: u32| -> Result<Vec<u8>> {
        let at = idx(rva);
        let end = img[at..].iter().position(|&b| b == 0).ok_or("unterminated import name")?;
        Ok(img[at..at + end].to_vec())
    };
    let (mut binds, mut kept) = (Vec::new(), Vec::new());
    let mut d = idx(dir1_rva);
    let start = d;
    while img[d..d + IMPORT_DESCRIPTOR_LEN].iter().any(|&b| b != 0) {
        let (oft, name, ft) = (u32(img, d), u32(img, d + 12), u32(img, d + 16));
        let lib = String::from_utf8_lossy(&cstr(img, name)?).to_lowercase();
        match set.iter().find(|e| e.name == lib) {
            None => kept.extend_from_slice(&img[d..d + IMPORT_DESCRIPTOR_LEN]),
            Some(e) => {
                // (the lookup table names each import; the address table is what the code reads)
                let lookup = if oft != 0 { oft } else { ft };
                for k in 0.. {
                    let v = u64(img, idx(lookup) + k * 8);
                    if v == 0 {
                        break;
                    }
                    let rva = if v & IMPORT_BY_ORDINAL != 0 {
                        e.by_ordinal.get(&((v & 0xffff) as u32)).copied()
                    } else {
                        e.by_name.get(&cstr(img, v as u32 + 2)?).copied()
                    };
                    let what = || String::from_utf8_lossy(&cstr(img, v as u32 + 2).unwrap_or_default()).into_owned();
                    let rva = rva.ok_or_else(|| format!("{lib} does not export {}", what()))?;
                    binds.push(Bind { slot: ft + k as u32 * 8, lib: lib.clone(), rva });
                }
            }
        }
        d += IMPORT_DESCRIPTOR_LEN;
    }
    let end = d + IMPORT_DESCRIPTOR_LEN;
    img[start..start + kept.len()].copy_from_slice(&kept);
    img[start + kept.len()..end].fill(0);
    Ok(binds)
}

/// Fixes the import thunk RVAs in a module's image `img` (its bytes from RVA `first`, placed so
/// that RVA `x` lands at `c + x`) by adding `c`, and returns its import descriptors adjusted the
/// same way (without a terminator): the loader binds them in the merged image as the stub's own.
fn fix_imports(img: &mut [u8], first: u32, dir1_rva: u32, c: u32) -> Result<Vec<u8>> {
    if dir1_rva == 0 {
        return Ok(Vec::new());
    }
    let idx = |rva: u32| (rva - first) as usize;
    let mut out = Vec::new();
    let mut d = dir1_rva;
    while img[idx(d)..idx(d) + IMPORT_DESCRIPTOR_LEN].iter().any(|&b| b != 0) {
        let o = idx(d);
        let (oft, name, ft) = (u32(img, o), u32(img, o + 12), u32(img, o + 16));
        // each distinct thunk array: a by-name entry (high bit clear) is an RVA, so add c
        let mut arrays = vec![oft, ft];
        arrays.retain(|&a| a != 0);
        arrays.dedup();
        for arr in arrays {
            let mut t = idx(arr);
            loop {
                let e = u64(img, t);
                if e == 0 {
                    break;
                }
                if e & IMPORT_BY_ORDINAL == 0 {
                    img[t..t + 8].copy_from_slice(&(e + c as u64).to_le_bytes());
                }
                t += 8;
            }
        }
        let mut nd = img[o..o + IMPORT_DESCRIPTOR_LEN].to_vec();
        if oft != 0 {
            put_u32(&mut nd, 0, oft + c);
        }
        put_u32(&mut nd, 12, name + c);
        if ft != 0 {
            put_u32(&mut nd, 16, ft + c);
        }
        out.extend_from_slice(&nd);
        d += IMPORT_DESCRIPTOR_LEN as u32;
    }
    Ok(out)
}

/// Digests a parsed library into a blob, resolving `apply_name` (unless `optional` and not there: a
/// shared library) and (unless `-`, or `optional`) `info_name`, and binding its imports from
/// the libraries of `set`.
fn blob_of(pe: &Pe, apply_name: &str, info_name: &str, optional: bool, set: &[&Exports]) -> Result<Blob> {
    // the image's span: the sections but `.reloc` (read into the relocation list instead)
    let reloc_va = pe.dir(DIR_RELOC).0;
    let kept: Vec<&Section> = pe.sections.iter().filter(|s| s.va != reloc_va).collect();
    let first = kept.iter().map(|s| s.va).min().ok_or("no sections")?;
    let last = kept.iter().map(|s| s.va + s.vsize.max(s.raw_size)).max().unwrap();
    let mut image = vec![0u8; (last - first) as usize];
    for s in &kept {
        let n = (s.raw_size as usize).min(pe.buf.len().saturating_sub(s.raw_ptr as usize));
        let at = (s.va - first) as usize;
        image[at..at + n].copy_from_slice(&pe.buf[s.raw_ptr as usize..s.raw_ptr as usize + n]);
    }
    let binds = bind_set(&mut image, first, pe.dir(DIR_IMPORT).0, set)?;
    // the dead directories' data, zeroed (the loader reads only the stub's)
    let zero = |image: &mut [u8], (rva, size): (u32, u32)| {
        if rva >= first && rva + size <= last {
            image[(rva - first) as usize..(rva + size - first) as usize].fill(0);
        }
    };
    let name = pe.exports()?.map(|e| e.name).unwrap_or_default();
    for dir in [DIR_EXPORT, DIR_DEBUG, DIR_TLS, DIR_LOAD_CONFIG] {
        zero(&mut image, pe.dir(dir));
    }
    let entry = |n: &str| match optional {
        true => Ok(pe.export_opt(n).unwrap_or(0)),
        false => pe.export_opt(n),
    };
    Ok(Blob {
        apply_rva: match optional {
            true => pe.export(apply_name).unwrap_or(0),
            false => pe.export(apply_name)?,
        },
        info_rva: entry(info_name)?,
        pdata_rva: pe.dir(DIR_EXCEPTION).0,
        pdata_count: pe.dir(DIR_EXCEPTION).1 / RUNTIME_FUNCTION_LEN as u32,
        import_dir_rva: pe.dir(DIR_IMPORT).0,
        first,
        image_base: pe.image_base(),
        name,
        binds,
        relocs: pe.relocs()?,
        image,
    })
}

/// A blob from the bytes of a library or a blob file (by its first bytes).
fn parse_blob(bytes: &[u8], apply_name: &str, info_name: &str) -> Result<Blob> {
    match bytes.first_chunk::<4>() {
        Some(aqr_blob::MAGIC) => Blob::from_bytes(bytes),
        _ => blob_of(&Pe::parse(bytes.to_vec())?, apply_name, info_name, false, &[]),
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

/// Merges `modules` (name, path) into the stub at `stub` and writes the result to `out`. Each
/// module exports its application entry as `apply_name` and, unless `-`, its info as `info_name`.
pub fn merge(
    stub: &Path,
    out: &Path,
    modules: &[(String, std::path::PathBuf)],
    apply_name: &str,
    info_name: &str,
) -> Result<()> {
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    let modules = modules.iter().map(|(n, p)| Ok((n.clone(), read(p)?))).collect::<Result<Vec<_>>>()?;
    let merged = merge_bytes(read(stub)?, &modules, apply_name, info_name)?;
    std::fs::write(out, merged).map_err(|e| format!("{}: {e}", out.display()))
}

/// Merges `modules` (each a name, and the bytes of a library or a blob) into stub executable
/// `stub`, giving the merged executable; the names go in the dispatch table as they are. Each
/// module exports its application entry as `apply_name` and, unless `-`, its info as `info_name`
/// (a blob has them resolved already).
pub fn merge_bytes(stub: Vec<u8>, modules: &[(String, Vec<u8>)], apply_name: &str, info_name: &str) -> Result<Vec<u8>> {
    let mut pe = Pe::parse(stub)?;
    let salign = pe.section_align();
    let falign = pe.file_align();
    let base = pe.image_base();

    // where the next merged section lands, after the stub's image
    let mut next_va = align(pe.size_of_image(), salign);
    let mut news: Vec<New> = Vec::new();
    let mut new_relocs: Vec<u32> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    // the stub's import descriptors, then each module's (its thunks and names in its own image)
    let mut descriptors = pe.descriptors()?;
    // the stub's exception directory, then each module's function entries, rebased (crate::unwind)
    let mut functions: Vec<[u32; 3]> = {
        let (rva, size) = pe.dir(DIR_EXCEPTION);
        let at = if rva == 0 { 0 } else { pe.off(rva)? };
        (0..size as usize / RUNTIME_FUNCTION_LEN)
            .map(|i| [0, 4, 8].map(|k| u32(&pe.buf, at + i * RUNTIME_FUNCTION_LEN + k)))
            .collect()
    };
    // the modules' images, each at an RVA of its own, section-aligned, in one section (the stub's
    // headers have room for few more section headers)
    let modules_va = next_va;
    let mut images: Vec<u8> = Vec::new();

    // where each lands first (its image, bytes from RVA `first`, at `va`, so RVA `x` is at
    // `c + x`), for the imports bound between them
    let mut blobs = Vec::new();
    for (name, bytes) in modules {
        let b = parse_blob(bytes, apply_name, info_name)?;
        let va = next_va;
        next_va = align(va + b.image.len() as u32, salign);
        blobs.push((name, va, va.wrapping_sub(b.first), b));
    }
    let placed: Vec<(String, u32)> = blobs.iter().map(|(_, _, c, b)| (b.name.clone(), *c)).collect();
    let vcrt = vcruntime_table(&pe);

    for (name, va, c, b) in blobs {
        let mut img = b.image;
        // the imports from the other blobs: the export's address in each slot, relocated
        for bind in &b.binds {
            let lc = placed.iter().filter(|(n, _)| !n.is_empty()).find(|(n, _)| *n == bind.lib).map(|p| p.1);
            let lc = lc.ok_or_else(|| format!("{name}: {} is not merged, which it imports from", bind.lib))?;
            let o = (bind.slot - b.first) as usize;
            img[o..o + 8].copy_from_slice(&base.wrapping_add(lc.wrapping_add(bind.rva) as u64).to_le_bytes());
            new_relocs.push(c.wrapping_add(bind.slot));
        }
        // rebase: an absolute address in the module (module_base + x) becomes stub_base + c + x
        let delta = base.wrapping_add(c as u64).wrapping_sub(b.image_base);
        for &loc in &b.relocs {
            let o = (loc - b.first) as usize;
            let v = u64::from_le_bytes(img[o..o + 8].try_into().unwrap()).wrapping_add(delta);
            img[o..o + 8].copy_from_slice(&v.to_le_bytes());
            new_relocs.push(c.wrapping_add(loc));
        }
        // the exception data: the unwinder reads only the stub's directory for its addresses
        let names = crate::unwind::import_names(&img, b.first, b.import_dir_rva)?;
        let rebased = crate::unwind::rebase(&mut img, b.first, b.pdata_rva, b.pdata_count, c, &names);
        functions.extend(rebased.map_err(|e| format!("{name}: {e}"))?);
        // the imports from the Visual C++ runtime: the stub's own copy, where it has its table
        if let Some(vcrt) = &vcrt {
            for (slot, i) in
                aqr_blob::take_vcruntime(&mut img, b.first, b.import_dir_rva).map_err(|e| format!("{name}: {e}"))?
            {
                let o = (slot - b.first) as usize;
                img[o..o + 8].copy_from_slice(&vcrt[i].to_le_bytes());
                new_relocs.push(c.wrapping_add(slot));
            }
        }
        descriptors.extend_from_slice(&fix_imports(&mut img, b.first, b.import_dir_rva, c)?);
        entries.push(Entry {
            name: name.as_bytes(),
            apply_rva: if b.apply_rva == 0 { 0 } else { c.wrapping_add(b.apply_rva) },
            info_rva: if b.info_rva == 0 { 0 } else { c.wrapping_add(b.info_rva) },
            image_rva: va,
            image_len: img.len() as u32,
            mod_base: c,
        });
        images.resize((va - modules_va) as usize, 0);
        images.extend_from_slice(&img);
    }
    if !images.is_empty() {
        let vsize = images.len() as u32;
        news.push(New { name: *b".aqrm\0\0\0", va: modules_va, vsize, flags: SEC_FLAGS, data: images });
    }

    // the import directory: the descriptors gathered, zero-terminated; then the exception
    // directory, in the same section (the stub's headers have room for few more section headers)
    descriptors.extend_from_slice(&[0u8; IMPORT_DESCRIPTOR_LEN]);
    let imp_va = next_va;
    let imp_len = descriptors.len() as u32;
    let mut data = descriptors;
    data.resize(data.len().next_multiple_of(4), 0);
    let exc_va = imp_va + data.len() as u32;
    // (in order of their addresses, as the unwinder searches it: the stub's, then each module's
    // in the order they land)
    if !functions.is_sorted_by_key(|f| f[0]) {
        return Err("the function entries are not in order".into());
    }
    for f in &functions {
        for v in f {
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    let exc_len = (functions.len() * RUNTIME_FUNCTION_LEN) as u32;
    let data_len = data.len() as u32;
    news.push(New { name: *b".aqri\0\0\0", va: imp_va, vsize: data_len, flags: DATA_FLAGS, data });
    next_va = align(imp_va + data_len, salign);

    // the dispatch table
    let disp = dispatch::write(&entries);
    let disp_va = next_va;
    let disp_len = disp.len() as u32;
    news.push(New { name: *b".aqrd\0\0\0", va: disp_va, vsize: disp_len, flags: DATA_FLAGS, data: disp });
    next_va = align(disp_va + disp_len, salign);

    // the relocations: the stub's, then one block per 0x1000 page of the merged ones
    let mut reloc = {
        let (rva, size) = pe.dir(DIR_RELOC);
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

    // the header fields: section count, image size, the import, exception and relocation
    // directories
    put_u16(&mut pe.buf, pe.pe + 6, (old_n + news.len()) as u32);
    put_u32(&mut pe.buf, pe.opt + 56, align(next_va, salign));
    pe.set_dir(DIR_IMPORT, imp_va, imp_len);
    pe.set_dir(DIR_EXCEPTION, exc_va, exc_len);
    // the bound-import directory would pin the stub's imports to stale addresses; drop it
    pe.set_dir(DIR_BOUND_IMPORT, 0, 0);
    pe.set_dir(DIR_RELOC, reloc_va, reloc_len);

    // the anchor: the dispatch table's place, written after its magic
    let anchor = find(&pe.buf, ANCHOR_MAGIC).ok_or("the stub has no anchor (its magic)")?;
    put_u32(&mut pe.buf, anchor + 16, disp_va);
    put_u32(&mut pe.buf, anchor + 16 + 4, disp_len);

    Ok(pe.buf)
}

/// The addresses (at the stub's image base) of the stub's functions of the Visual C++ runtime, as
/// its `aqr_blob::VCRUNTIME_TABLE` has them, if it has one: the modules' imports from the runtime
/// are bound to them, so that the merged program loads no `vcruntime140.dll` (which no Windows
/// carries). A stub without the table (the stub of pemerge-test.sh, aqr-dll-shim) keeps them as
/// imports.
fn vcruntime_table(pe: &Pe) -> Option<Vec<u64>> {
    let (base, size) = (pe.image_base(), pe.size_of_image() as u64);
    let n = aqr_blob::VCRUNTIME_NAMES.len();
    let magic = aqr_blob::VCRUNTIME_MAGIC;
    let mut from = 0;
    while let Some(k) = find(&pe.buf[from..], magic) {
        let at = from + k + magic.len();
        let table: Option<Vec<u64>> =
            (0..n).map(|i| pe.buf.get(at + i * 8..at + i * 8 + 8)).map(|b| b.map(|b| u64(b, 0))).collect();
        // (the table itself, not the magic's bytes elsewhere: each an address in the image)
        if let Some(t) = table.filter(|t| t.iter().all(|&v| v > base && v < base + size)) {
            return Some(t);
        }
        from += k + 1;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `aqr-ld merge --stub S --out O [--apply NAME] [--info NAME] NAME=MODULE.dll ...`. The exports
/// default to `aqr_dll_apply` and `aqr_dll_info`; `--info -` is for a module with no info export.
pub fn main(args: &[std::ffi::OsString]) -> Result<()> {
    let mut stub = None;
    let mut out = None;
    let mut apply_name = "aqr_dll_apply".to_owned();
    let mut info_name = "aqr_dll_info".to_owned();
    let mut modules = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let next = |it: &mut std::slice::Iter<std::ffi::OsString>, what: &str| {
            it.next().and_then(|s| s.to_str().map(str::to_owned)).ok_or_else(|| format!("{what}: no value"))
        };
        match a.to_str() {
            Some("--stub") => stub = Some(it.next().ok_or("--stub: no path")?.into()),
            Some("--out") => out = Some(it.next().ok_or("--out: no path")?.into()),
            Some("--apply") => apply_name = next(&mut it, "--apply")?,
            Some("--info") => info_name = next(&mut it, "--info")?,
            _ => {
                let a = a.to_str().ok_or("a module argument is not UTF-8")?;
                let (name, path) = a.split_once('=').ok_or_else(|| format!("{a}: not NAME=MODULE.dll"))?;
                modules.push((name.to_owned(), std::path::PathBuf::from(path)));
            }
        }
    }
    let stub: std::path::PathBuf = stub.ok_or("merge: no --stub")?;
    let out: std::path::PathBuf = out.ok_or("merge: no --out")?;
    merge(&stub, &out, &modules, &apply_name, &info_name)
}

/// `aqr-ld blob --out M.blob [--apply NAME] [--info NAME] MODULE.dll`: digests a module library
/// into a blob (just the bytes and the facts the merge needs, no PE format).
pub fn blob_main(args: &[std::ffi::OsString]) -> Result<()> {
    let mut out = None;
    let mut apply_name = "aqr_dll_apply".to_owned();
    let mut info_name = "aqr_dll_info".to_owned();
    let mut dll = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let val = |it: &mut std::slice::Iter<std::ffi::OsString>, w: &str| {
            it.next().and_then(|s| s.to_str().map(str::to_owned)).ok_or_else(|| format!("{w}: no value"))
        };
        match a.to_str() {
            Some("--out") => out = Some(it.next().ok_or("--out: no path")?.into()),
            Some("--apply") => apply_name = val(&mut it, "--apply")?,
            Some("--info") => info_name = val(&mut it, "--info")?,
            _ => dll = Some(std::path::PathBuf::from(a)),
        }
    }
    let out: std::path::PathBuf = out.ok_or("blob: no --out")?;
    let dll = dll.ok_or("blob: no module library")?;
    let pe = Pe::parse(std::fs::read(&dll).map_err(|e| format!("{}: {e}", dll.display()))?)?;
    let blob = blob_of(&pe, &apply_name, &info_name, false, &[])?;
    std::fs::write(&out, blob.to_bytes()).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(())
}

/// `aqr-ld blobs --out DIR LIBRARY.dll...`: digests a set of libraries, modules (exporting
/// `aqr_dll_apply` and `aqr_dll_info`) and the shared libraries they link against, each into
/// `DIR/STEM.blob`, the imports of each from the others bound ([`Bind`]): merged with them, a
/// blob runs with no loader binding those.
pub fn blobs_main(args: &[std::ffi::OsString]) -> Result<()> {
    let mut out = None;
    let mut dlls = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--out") => out = Some(std::path::PathBuf::from(it.next().ok_or("--out: no directory")?)),
            _ => dlls.push(std::path::PathBuf::from(a)),
        }
    }
    let out = out.ok_or("blobs: no --out")?;
    let pes = dlls
        .iter()
        .map(|d| Pe::parse(std::fs::read(d).map_err(|e| format!("{}: {e}", d.display()))?))
        .collect::<Result<Vec<_>>>()?;
    let exports: Vec<Exports> =
        pes.iter().map(Pe::exports).collect::<Result<Vec<_>>>()?.into_iter().flatten().collect();
    let set: Vec<&Exports> = exports.iter().collect();
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (dll, pe) in dlls.iter().zip(&pes) {
        let blob =
            blob_of(pe, "aqr_dll_apply", "aqr_dll_info", true, &set).map_err(|e| format!("{}: {e}", dll.display()))?;
        let to = out.join(dll.file_stem().ok_or("a library of no name")?).with_extension("blob");
        std::fs::write(&to, blob.to_bytes()).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

/// The imports by name of library (or executable) `buf`: each the library's name (lowercase)
/// and the symbol's.
pub fn imports(buf: Vec<u8>) -> Result<Vec<(String, String)>> {
    let pe = Pe::parse(buf)?;
    let (rva, _) = pe.dir(DIR_IMPORT);
    if rva == 0 {
        return Ok(Vec::new());
    }
    let cstr = |rva: u32| -> Result<String> {
        let at = pe.off(rva)?;
        let end = pe.buf[at..].iter().position(|&b| b == 0).ok_or("unterminated import name")?;
        Ok(String::from_utf8_lossy(&pe.buf[at..at + end]).into_owned())
    };
    let mut out = Vec::new();
    let mut d = pe.off(rva)?;
    while pe.buf[d..d + IMPORT_DESCRIPTOR_LEN].iter().any(|&b| b != 0) {
        let (oft, name, ft) = (u32(&pe.buf, d), u32(&pe.buf, d + 12), u32(&pe.buf, d + 16));
        let lib = cstr(name)?.to_lowercase();
        let mut t = pe.off(if oft != 0 { oft } else { ft })?;
        loop {
            let v = u64(&pe.buf, t);
            if v == 0 {
                break;
            }
            if v & IMPORT_BY_ORDINAL == 0 {
                out.push((lib.clone(), cstr(v as u32 + 2)?));
            }
            t += 8;
        }
        d += IMPORT_DESCRIPTOR_LEN;
    }
    Ok(out)
}

/// Of the bytes of a blob: its name as a library (lowercase; empty where it exports nothing),
/// whether it is a module's (it has an application entry), and the libraries it imports from
/// (which a merge must have too).
pub fn blob_facts(bytes: &[u8]) -> Result<(String, bool, Vec<String>)> {
    let b = Blob::from_bytes(bytes)?;
    Ok((b.name.clone(), b.apply_rva != 0, b.libs()))
}
