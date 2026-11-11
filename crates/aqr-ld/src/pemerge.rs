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
//! works, and its imports are merged into the stub's import table. The modules' images are in
//! one section, each at an RVA of its own (the stub's headers have room for few more section
//! headers). A dispatch table naming each module and the RVA of its entry is added too, and its
//! place written into the stub's anchor (its magic). Validated by `crates/aqr-ld/pemerge-test.sh`
//! on trivial modules, `crates/aqr-dll/merge-test.sh` on the codec modules.
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

    /// The RVA of the exported function `name`, or 0 for `-`.
    fn export_opt(&self, name: &str) -> Result<u32> {
        match name {
            "-" => Ok(0),
            _ => self.export(name),
        }
    }

    /// The stub's import descriptors (20 bytes each), without the zero terminator.
    fn descriptors(&self) -> Result<Vec<u8>> {
        let (rva, _) = self.dir(1);
        if rva == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut d = self.off(rva)?;
        while self.buf[d..d + 20].iter().any(|&b| b != 0) {
            out.extend_from_slice(&self.buf[d..d + 20]);
            d += 20;
        }
        Ok(out)
    }
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
    while img[idx(d)..idx(d) + 20].iter().any(|&b| b != 0) {
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
                if e & 0x8000_0000_0000_0000 == 0 {
                    img[t..t + 8].copy_from_slice(&(e + c as u64).to_le_bytes());
                }
                t += 8;
            }
        }
        let mut nd = img[o..o + 20].to_vec();
        if oft != 0 {
            put_u32(&mut nd, 0, oft + c);
        }
        put_u32(&mut nd, 12, name + c);
        if ft != 0 {
            put_u32(&mut nd, 16, ft + c);
        }
        out.extend_from_slice(&nd);
        d += 20;
    }
    Ok(out)
}

/// A module digested from a library into just what the merge needs: its image laid out from the
/// first section (no PE headers, no `.reloc` section, no header gap), its base relocations as a
/// list, and the RVAs the merge resolves. RVAs are the module's own (over its image base); the
/// image holds the bytes of `[first, last)`, indexed by `rva - first`.
struct Blob {
    apply_rva: u32,
    info_rva: u32,
    pdata_rva: u32,
    pdata_count: u32,
    import_dir_rva: u32,
    first: u32,
    image_base: u64,
    relocs: Vec<u32>,
    image: Vec<u8>,
}

const BLOB_MAGIC: &[u8; 4] = b"AQRB";

impl Blob {
    /// Digests a parsed library, resolving `apply_name` and (unless `-`) `info_name`.
    fn of(pe: &Pe, apply_name: &str, info_name: &str) -> Result<Blob> {
        // the image's span: the sections but `.reloc` (read into the relocation list instead)
        let reloc_va = pe.dir(5).0;
        let kept: Vec<&Section> = pe.sections.iter().filter(|s| s.va != reloc_va).collect();
        let first = kept.iter().map(|s| s.va).min().ok_or("no sections")?;
        let last = kept.iter().map(|s| s.va + s.vsize.max(s.raw_size)).max().unwrap();
        let mut image = vec![0u8; (last - first) as usize];
        for s in &kept {
            let n = (s.raw_size as usize).min(pe.buf.len().saturating_sub(s.raw_ptr as usize));
            let at = (s.va - first) as usize;
            image[at..at + n].copy_from_slice(&pe.buf[s.raw_ptr as usize..s.raw_ptr as usize + n]);
        }
        // the dead directories' data, zeroed (the loader reads only the stub's)
        let zero = |image: &mut [u8], (rva, size): (u32, u32)| {
            if rva >= first && rva + size <= last {
                image[(rva - first) as usize..(rva + size - first) as usize].fill(0);
            }
        };
        for dir in [0, 6, 9, 10] {
            // export, debug, thread storage, load config
            zero(&mut image, pe.dir(dir));
        }
        Ok(Blob {
            apply_rva: pe.export(apply_name)?,
            info_rva: pe.export_opt(info_name)?,
            pdata_rva: pe.dir(3).0,
            pdata_count: pe.dir(3).1 / 12,
            import_dir_rva: pe.dir(1).0,
            first,
            image_base: pe.image_base(),
            relocs: pe.relocs()?,
            image,
        })
    }

    /// The blob serialized (`aqr-ld blob`). The header, then the relocations, then the image.
    fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(BLOB_MAGIC);
        for v in [self.apply_rva, self.info_rva, self.pdata_rva, self.pdata_count, self.import_dir_rva, self.first] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&self.image_base.to_le_bytes());
        b.extend_from_slice(&(self.relocs.len() as u32).to_le_bytes());
        for &r in &self.relocs {
            b.extend_from_slice(&r.to_le_bytes());
        }
        b.extend_from_slice(&(self.image.len() as u32).to_le_bytes());
        b.extend_from_slice(&self.image);
        b
    }

    /// A blob parsed from bytes.
    fn from_bytes(b: &[u8]) -> Result<Blob> {
        if b.len() < 40 || &b[0..4] != BLOB_MAGIC {
            return Err("not a module blob".into());
        }
        let n_relocs = u32(b, 36) as usize;
        let relocs = (0..n_relocs).map(|i| u32(b, 40 + i * 4)).collect();
        let at = 40 + n_relocs * 4;
        let image_len = u32(b, at) as usize;
        Ok(Blob {
            apply_rva: u32(b, 4),
            info_rva: u32(b, 8),
            pdata_rva: u32(b, 12),
            pdata_count: u32(b, 16),
            import_dir_rva: u32(b, 20),
            first: u32(b, 24),
            image_base: u64(b, 28),
            relocs,
            image: b[at + 4..at + 4 + image_len].to_vec(),
        })
    }

    /// A blob from the bytes of a library or a blob file (by its first bytes).
    fn parse(bytes: Vec<u8>, apply_name: &str, info_name: &str) -> Result<Blob> {
        match bytes.first_chunk::<4>() {
            Some(BLOB_MAGIC) => Blob::from_bytes(&bytes),
            _ => Blob::of(&Pe::parse(bytes)?, apply_name, info_name),
        }
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

/// A module merged, as the dispatch table records it.
struct Entry {
    name: String,
    apply_rva: u32,
    info_rva: u32,
    /// The merged RVA of the module's exception table, its count, and the module's base RVA (`r`,
    /// what the stub passes `RtlAddFunctionTable` so its entries' RVAs resolve).
    pdata_rva: u32,
    pdata_count: u32,
    mod_base: u32,
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
    // the modules' images, each at an RVA of its own, section-aligned, in one section (the stub's
    // headers have room for few more section headers)
    let modules_va = next_va;
    let mut images: Vec<u8> = Vec::new();

    for (name, bytes) in modules {
        let b = Blob::parse(bytes.clone(), apply_name, info_name)?;
        // the image (bytes from RVA `first`) lands at `va`, so RVA `x` is at `c + x`
        let va = next_va;
        let c = va.wrapping_sub(b.first);
        let mut img = b.image;
        // rebase: an absolute address in the module (module_base + x) becomes stub_base + c + x
        let delta = base.wrapping_add(c as u64).wrapping_sub(b.image_base);
        for &loc in &b.relocs {
            let o = (loc - b.first) as usize;
            let v = u64::from_le_bytes(img[o..o + 8].try_into().unwrap()).wrapping_add(delta);
            img[o..o + 8].copy_from_slice(&v.to_le_bytes());
            new_relocs.push(c.wrapping_add(loc));
        }
        descriptors.extend_from_slice(&fix_imports(&mut img, b.first, b.import_dir_rva, c)?);
        entries.push(Entry {
            name: name.clone(),
            apply_rva: c.wrapping_add(b.apply_rva),
            info_rva: if b.info_rva == 0 { 0 } else { c.wrapping_add(b.info_rva) },
            pdata_rva: if b.pdata_rva == 0 { 0 } else { c.wrapping_add(b.pdata_rva) },
            pdata_count: b.pdata_count,
            mod_base: c,
        });
        images.resize((va - modules_va) as usize, 0);
        images.extend_from_slice(&img);
        next_va = align(va + img.len() as u32, salign);
    }
    if !images.is_empty() {
        let vsize = images.len() as u32;
        news.push(New { name: *b".aqrm\0\0\0", va: modules_va, vsize, flags: SEC_FLAGS, data: images });
    }

    // the import directory: the descriptors gathered, zero-terminated
    descriptors.extend_from_slice(&[0u8; 20]);
    let imp_va = next_va;
    let imp_len = descriptors.len() as u32;
    news.push(New { name: *b".aqri\0\0\0", va: imp_va, vsize: imp_len, flags: DATA_FLAGS, data: descriptors });
    next_va = align(imp_va + imp_len, salign);

    // the dispatch table: count, then an entry each (6 u32), then the names
    let mut disp = Vec::new();
    disp.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let names_at = 4 + entries.len() * 24;
    let mut names = Vec::new();
    for e in &entries {
        for v in [e.apply_rva, e.info_rva, e.pdata_rva, e.pdata_count, e.mod_base, (names_at + names.len()) as u32] {
            disp.extend_from_slice(&v.to_le_bytes());
        }
        names.extend_from_slice(&(e.name.len() as u32).to_le_bytes());
        names.extend_from_slice(e.name.as_bytes());
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

    // the header fields: section count, image size, the import and relocation directories
    put_u16(&mut pe.buf, pe.pe + 6, (old_n + news.len()) as u32);
    put_u32(&mut pe.buf, pe.opt + 56, align(next_va, salign));
    put_u32(&mut pe.buf, pe.opt + 112 + 8, imp_va); // directory 1 (import)
    put_u32(&mut pe.buf, pe.opt + 112 + 8 + 4, imp_len);
    // the bound-import directory (11) would pin the stub's imports to stale addresses; drop it
    put_u32(&mut pe.buf, pe.opt + 112 + 11 * 8, 0);
    put_u32(&mut pe.buf, pe.opt + 112 + 11 * 8 + 4, 0);
    put_u32(&mut pe.buf, pe.opt + 112 + 5 * 8, reloc_va);
    put_u32(&mut pe.buf, pe.opt + 112 + 5 * 8 + 4, reloc_len);

    // the anchor: the dispatch table's place, written after its magic
    let anchor = find(&pe.buf, MAGIC).ok_or("the stub has no anchor (its magic)")?;
    put_u32(&mut pe.buf, anchor + 16, disp_va);
    put_u32(&mut pe.buf, anchor + 16 + 4, disp_len);

    Ok(pe.buf)
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
            it.next().and_then(|s| s.to_str().map(str::to_owned)).ok_or(format!("{what}: no value"))
        };
        match a.to_str() {
            Some("--stub") => stub = Some(it.next().ok_or("--stub: no path")?.into()),
            Some("--out") => out = Some(it.next().ok_or("--out: no path")?.into()),
            Some("--apply") => apply_name = next(&mut it, "--apply")?,
            Some("--info") => info_name = next(&mut it, "--info")?,
            _ => {
                let a = a.to_str().ok_or("a module argument is not UTF-8")?;
                let (name, path) = a.split_once('=').ok_or(format!("{a}: not NAME=MODULE.dll"))?;
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
            it.next().and_then(|s| s.to_str().map(str::to_owned)).ok_or(format!("{w}: no value"))
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
    let blob = Blob::of(&pe, &apply_name, &info_name)?;
    std::fs::write(&out, blob.to_bytes()).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(())
}
