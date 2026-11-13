//! Blobs placed in memory at run time (Windows x86-64): as `aqr-ld merge` places them in a stub,
//! but into memory allocated for them, by the program that carries them as data. Each blob's
//! relocations are applied for where it lands, its imports from the other blobs of the set
//! written as their addresses, its imports from the Visual C++ runtime bound to this program's
//! own ([`crate::VCRUNTIME`]), those from system libraries bound (`LoadLibraryA`,
//! `GetProcAddress`), its exception table registered for the unwinder, and its image for its
//! panics ([`crate::register_image`]). No DllMain or TLS
//! callback runs, as none does in a merge. What is placed stays for the life of the process.

use core::ffi::c_void;

use crate::{Blob, IMPORT_BY_ORDINAL, IMPORT_DESCRIPTOR_LEN, Result, u32_at, u64_at};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, protect: u32, old: *mut u32) -> i32;
    fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn LoadLibraryA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn RtlAddFunctionTable(table: *const c_void, count: u32, base: u64) -> i32;
}

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const PAGE_READWRITE: u32 = 0x04;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;

/// A blob placed: its name as a library, and the address of its RVA 0 (an RVA `x` of it is at
/// `base + x`), with its entries' RVAs.
#[derive(Debug, Clone)]
pub struct Placed {
    pub name: String,
    pub base: usize,
    pub apply_rva: u32,
    pub info_rva: u32,
}

impl Placed {
    /// The address of RVA `rva` of the blob (0 for RVA 0: an entry it has not).
    pub fn at(&self, rva: u32) -> usize {
        if rva == 0 { 0 } else { self.base + rva as usize }
    }
}

/// The blobs of a set placed in memory, in the order given.
#[derive(Debug)]
pub struct Image {
    pub blobs: Vec<Placed>,
}

/// Places the blobs of `set` (each its bytes, [`Blob::to_bytes`]) in memory, the imports between
/// them bound by their names as libraries.
pub fn load(set: &[&[u8]]) -> Result<Image> {
    let blobs = set.iter().map(|b| Blob::from_bytes(b)).collect::<Result<Vec<_>>>()?;
    // each at a page of its own, in one allocation
    let page = 0x1000usize;
    let mut offsets = Vec::with_capacity(blobs.len());
    let mut total = 0usize;
    for b in &blobs {
        offsets.push(total);
        total += b.image.len().div_ceil(page) * page;
    }
    // SAFETY: a fresh allocation, written only here before it runs
    let mem = unsafe { VirtualAlloc(std::ptr::null_mut(), total.max(page), MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
    if mem.is_null() {
        return Err(format!("{} bytes for blobs not allocated: {}", total, std::io::Error::last_os_error()));
    }
    let region = unsafe { std::slice::from_raw_parts_mut(mem as *mut u8, total) };
    // (where RVA 0 of each lands)
    let bases: Vec<usize> = blobs.iter().zip(&offsets).map(|(b, &o)| mem as usize + o - b.first as usize).collect();
    for ((b, &o), &base) in blobs.iter().zip(&offsets).zip(&bases) {
        let img = &mut region[o..o + b.image.len()];
        img.copy_from_slice(&b.image);
        let at = |rva: u32| (rva - b.first) as usize;
        // rebased: an absolute address in it (image_base + x) becomes base + x
        let delta = (base as u64).wrapping_sub(b.image_base);
        for &loc in &b.relocs {
            let v = u64_at(img, at(loc)).wrapping_add(delta);
            img[at(loc)..at(loc) + 8].copy_from_slice(&v.to_le_bytes());
        }
        // the imports from the others of the set
        for bind in &b.binds {
            let j = blobs.iter().position(|o| !o.name.is_empty() && o.name == bind.lib);
            let j = j.ok_or_else(|| format!("{}: {} is not loaded, which it imports from", b.name, bind.lib))?;
            let v = (bases[j] + bind.rva as usize) as u64;
            img[at(bind.slot)..at(bind.slot) + 8].copy_from_slice(&v.to_le_bytes());
        }
        // the imports from the Visual C++ runtime: this program's own copy (crate::VCRUNTIME)
        #[cfg(target_env = "msvc")]
        for (slot, i) in
            crate::take_vcruntime(img, b.first, b.import_dir_rva).map_err(|e| format!("{}: {e}", b.name))?
        {
            let v = crate::VCRUNTIME_TABLE.functions[i] as usize as u64;
            img[at(slot)..at(slot) + 8].copy_from_slice(&v.to_le_bytes());
        }
        // the imports from system libraries, as the loader binds them
        if b.import_dir_rva != 0 {
            let mut d = at(b.import_dir_rva);
            while img[d..d + IMPORT_DESCRIPTOR_LEN].iter().any(|&x| x != 0) {
                let (oft, name, ft) = (u32_at(img, d), u32_at(img, d + 12), u32_at(img, d + 16));
                let cstr = |img: &[u8], rva: u32| -> Vec<u8> {
                    let s = &img[at(rva)..];
                    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
                    let mut v = s[..end].to_vec();
                    v.push(0);
                    v
                };
                let lib = cstr(img, name);
                // SAFETY: a NUL-terminated name
                let h = unsafe { LoadLibraryA(lib.as_ptr()) };
                let shown = || String::from_utf8_lossy(&lib[..lib.len() - 1]).into_owned();
                if h.is_null() {
                    return Err(format!("{}: {} not loaded: {}", b.name, shown(), std::io::Error::last_os_error()));
                }
                let lookup = if oft != 0 { oft } else { ft };
                for k in 0.. {
                    let v = u64_at(img, at(lookup) + k * 8);
                    if v == 0 {
                        break;
                    }
                    let (f, what) = match v & IMPORT_BY_ORDINAL != 0 {
                        // (by ordinal: the ordinal as the name's pointer, as GetProcAddress takes it)
                        true => (
                            unsafe { GetProcAddress(h, (v & 0xffff) as usize as *const u8) },
                            format!("#{}", v & 0xffff),
                        ),
                        false => {
                            let n = cstr(img, v as u32 + 2);
                            let f = unsafe { GetProcAddress(h, n.as_ptr()) };
                            (f, String::from_utf8_lossy(&n[..n.len() - 1]).into_owned())
                        }
                    };
                    if f.is_null() {
                        return Err(format!("{}: {} has no {what}", b.name, shown()));
                    }
                    let slot = at(ft) + k * 8;
                    img[slot..slot + 8].copy_from_slice(&(f as u64).to_le_bytes());
                }
                d += IMPORT_DESCRIPTOR_LEN;
            }
        }
    }
    let mut old = 0;
    // SAFETY: the region allocated above; nothing runs in it until this returns
    unsafe {
        if VirtualProtect(mem, total, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return Err(format!("blobs not made executable: {}", std::io::Error::last_os_error()));
        }
        FlushInstructionCache(GetCurrentProcess(), mem, total);
    }
    for ((b, &base), &o) in blobs.iter().zip(&bases).zip(&offsets) {
        crate::register_image(mem as usize + o, b.image.len(), base);
        if b.pdata_count > 0 {
            // SAFETY: the blob's exception table, in the region, of entries over its base
            unsafe { RtlAddFunctionTable((base + b.pdata_rva as usize) as *const c_void, b.pdata_count, base as u64) };
        }
    }
    let blobs = blobs
        .into_iter()
        .zip(bases)
        .map(|(b, base)| Placed { name: b.name, base, apply_rva: b.apply_rva, info_rva: b.info_rva })
        .collect();
    Ok(Image { blobs })
}
