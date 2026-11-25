//! Blobs: native libraries digested into just what it takes to place them in another image
//! (`aqr-ld blob`, `aqr-ld blobs`): the image laid out from the first section, its base
//! relocations as a list, the RVAs of its entries, and its imports from the other libraries of the
//! set it was digested with, bound by name ahead ([`Bind`]). `aqr-ld merge` places blobs into a
//! stub executable as sections (the self-extracting programs); [`load`] places them in memory at
//! run time (Windows x86-64), for a program that carries them as data (aqr, the interpreter blob).

use std::fmt;

pub mod dispatch;
#[cfg(feature = "runner")]
pub mod interp;
#[cfg(all(windows, target_arch = "x86_64"))]
mod load;
#[cfg(all(windows, target_arch = "x86_64"))]
pub use load::{Image, Placed, load};

pub type Result<T> = std::result::Result<T, String>;

/// An import bound to another blob of the set: the RVA of the slot of the import address table
/// (in the importing blob), the library imported from (its name, lowercase) and the RVA of the
/// export there. Whoever places the blobs writes the export's address into the slot.
#[derive(Debug, Clone)]
pub struct Bind {
    pub slot: u32,
    pub lib: String,
    pub rva: u32,
}

/// A native library digested. RVAs are the library's own (over its image base); the image holds
/// the bytes of `[first, first + image.len())`, indexed by `rva - first`. A shared library (no
/// entry of a module) has `apply_rva` 0. The import directory (`import_dir_rva`) is the imports
/// from outside the set only (of system libraries, bound where the blob is placed).
#[derive(Clone)]
pub struct Blob {
    pub apply_rva: u32,
    pub info_rva: u32,
    pub pdata_rva: u32,
    pub pdata_count: u32,
    pub import_dir_rva: u32,
    pub first: u32,
    pub image_base: u64,
    /// Its own name as a library (lowercase), what the others' imports name; empty where it
    /// exports nothing.
    pub name: String,
    pub binds: Vec<Bind>,
    pub relocs: Vec<u32>,
    /// The library's sections in the image, for whoever places it to protect each as the loader
    /// would (none: a blob of the earlier version, [`MAGIC_V2`], placed all writable and
    /// executable).
    pub sections: Vec<Section>,
    pub image: Vec<u8>,
}

/// A section of a blob's image: its RVA, its bytes in memory, and the access it is given (of
/// its PE characteristics, [`SCN_EXECUTE`], [`SCN_READ`], [`SCN_WRITE`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section {
    pub rva: u32,
    pub len: u32,
    pub flags: u32,
}

/// The access of a section (`IMAGE_SCN_MEM_*`).
pub const SCN_EXECUTE: u32 = 0x2000_0000;
pub const SCN_READ: u32 = 0x4000_0000;
pub const SCN_WRITE: u32 = 0x8000_0000;

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob({:?}, {} bytes, {} binds)", self.name, self.image.len(), self.binds.len())
    }
}

pub const MAGIC: &[u8; 4] = b"AQB3";
/// The magic of the blobs of no sections (read still: placed as before, all of the image
/// writable and executable).
pub const MAGIC_V2: &[u8; 4] = b"AQB2";

/// The size of an import descriptor of a PE image's import directory (the directory ends with
/// an all-zero one).
pub const IMPORT_DESCRIPTOR_LEN: usize = 20;

/// The flag of a thunk of the import tables (64-bit images): the import is by ordinal, not by
/// the RVA of a hint and name.
pub const IMPORT_BY_ORDINAL: u64 = 0x8000_0000_0000_0000;

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

// ---- the C runtime of blobs

/// The library of the Visual C++ runtime (lowercase), which blobs import from but no Windows
/// carries (the redistributable installs it). A blob could not link it statically itself: its
/// copy would be left uninitialised (no DllMain runs for a blob), and its exception handling,
/// which panics go through, aborts so.
pub const VCRUNTIME: &str = "vcruntime140.dll";

/// Whether library `lib` is the C runtime's: the Visual C++ runtime ([`VCRUNTIME`]) or the
/// UCRT (`api-ms-win-crt-*.dll`, `ucrtbase.dll`). The programs placing blobs bind the blobs'
/// imports from these to their own functions ([`CRT_TABLE`]), so that none of them is loaded:
/// no Windows carries vcruntime140.dll, and Windows 7 and 8 carry the UCRT only by an update.
/// A program links them statically or not (`.cargo/config.toml`: for Windows 10 the UCRT the
/// system's DLL, for the target of Windows 7 on all static).
pub fn is_crt(lib: &[u8]) -> bool {
    let lib = lib.to_ascii_lowercase();
    lib == VCRUNTIME.as_bytes() || lib == b"ucrtbase.dll" || lib.starts_with(b"api-ms-win-crt-")
}

/// The functions of the C runtime ([`is_crt`]) that blobs import, in the order of
/// [`CRT_TABLE`]'s (where `_CxxThrowException` is one of its own for blobs). Those of the UCRT
/// that only a DLL's startup calls (`_initterm` and the like) are bound too, though no DllMain
/// runs for a blob. `aqr-ld blob` and `blobs` fail for a blob importing another.
pub const CRT_NAMES: [&str; CRT_COUNT] = [
    // the Visual C++ runtime's
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "__C_specific_handler",
    "__CxxFrameHandler3",
    "_CxxThrowException",
    "__std_type_info_destroy_list",
    // the UCRT's
    "strlen",
    "malloc",
    "free",
    "abort",
    "ceil",
    "ceilf",
    "cos",
    "floor",
    "floorf",
    "fma",
    "fmaf",
    "fmod",
    "fmodf",
    "log2",
    "pow",
    "round",
    "roundf",
    "trunc",
    "_cexit",
    "_configure_narrow_argv",
    "_execute_onexit_table",
    "_initialize_narrow_environment",
    "_initialize_onexit_table",
    "_initterm",
    "_initterm_e",
    "_seh_filter_dll",
    // (of WAH's C)
    "strcmp",
    "strncmp",
    "realloc",
    "qsort",
    "_dclass",
    "_fdclass",
    "truncf",
    "copysign",
    "copysignf",
];

const CRT_COUNT: usize = 43;

/// The magic of [`CRT_TABLE`], by which `aqr-ld merge` finds it in a stub's file.
pub const CRT_MAGIC: &[u8; 16] = b"\x89AQRCRTTABLEv3\x00\x00";

/// The program's own functions of [`CRT_NAMES`], after [`CRT_MAGIC`].
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
#[repr(C)]
pub struct CrtTable {
    pub magic: [u8; 16],
    pub functions: [unsafe extern "C" fn(); CRT_COUNT],
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
mod crt {
    use core::ffi::c_void;

    // (only their addresses are taken: the signatures matter only to rustc, which checks those
    // that std declares too, given theirs; the others none)
    unsafe extern "C" {
        fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void;
        fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32;
        fn __C_specific_handler();
        fn __CxxFrameHandler3();
        fn _CxxThrowException(object: *mut c_void, info: *const c_void) -> !;
        fn __std_type_info_destroy_list();
        fn strlen(s: *const core::ffi::c_char) -> usize;
        fn malloc(n: usize) -> *mut c_void;
        fn free(p: *mut c_void);
        fn abort() -> !;
        fn ceil();
        fn ceilf();
        fn cos();
        fn floor();
        fn floorf();
        fn fma();
        fn fmaf();
        fn fmod();
        fn fmodf();
        fn log2();
        fn pow();
        fn round();
        fn roundf();
        fn trunc();
        fn _cexit();
        fn _configure_narrow_argv();
        fn _execute_onexit_table();
        fn _initialize_narrow_environment();
        fn _initialize_onexit_table();
        fn _initterm();
        fn _initterm_e();
        fn _seh_filter_dll();
        fn strcmp();
        fn strncmp();
        fn realloc();
        fn qsort();
        fn _dclass();
        fn _fdclass();
        fn truncf();
        fn copysign();
        fn copysignf();
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn RaiseException(code: u32, flags: u32, count: u32, arguments: *const usize) -> !;
    }

    /// `_CxxThrowException` for blobs. A panic is a C++ exception whose type information is
    /// given relative to the image of its code: Rust's `panic_unwind` takes the offsets from its
    /// `__ImageBase`, the blob's RVA 0. The runtime's own finds that base by `RtlPcToFileHeader`
    /// of the throw info, which knows only the images the system loaded: none for a blob placed
    /// by [`crate::load`] (the throw then taken for a pure one, which nothing catches: fail-fast),
    /// the stub's for one merged into it (offset from the blob's: no catch matches). This one
    /// takes the base of the blob holding the throw info ([`crate::register_image`]) and raises
    /// the exception as the runtime does with it (WinRT and pure throws, which Rust has not,
    /// aside).
    unsafe extern "C" fn throw_from_blob(object: *mut c_void, info: *const c_void) -> ! {
        let Some(base) = super::image_base_of(info as usize) else {
            // (not a blob's: the runtime's way, then)
            unsafe { _CxxThrowException(object, info) }
        };
        // EH_MAGIC_NUMBER1, the object, its throw info, the image base of its type information
        let arguments = [0x1993_0520, object as usize, info as usize, base];
        // SAFETY: the C++ exception as the runtime raises it (EH_EXCEPTION_NUMBER, noncontinuable)
        unsafe { RaiseException(0xe06d_7363, 1, arguments.len() as u32, arguments.as_ptr()) }
    }

    type Mem = unsafe extern "C" fn(*mut c_void, *const c_void, usize) -> *mut c_void;

    /// A function's address as the table holds it.
    const fn erase<F: Copy>(f: F) -> unsafe extern "C" fn() {
        assert!(size_of::<F>() == size_of::<unsafe extern "C" fn()>());
        // SAFETY: a function pointer of another signature, never called through this one
        unsafe { core::mem::transmute_copy(&f) }
    }

    /// The program's functions of the C runtime that blobs import, found by its magic
    /// in a stub's file by `aqr-ld merge` and used by [`crate::load`]. A program into which blobs
    /// are merged must reference it ([`keep_crt`]), or the linker drops it.
    #[used]
    #[unsafe(no_mangle)]
    pub static CRT_TABLE: super::CrtTable = super::CrtTable {
        magic: *super::CRT_MAGIC,
        functions: [
            erase::<Mem>(memcpy),
            erase::<Mem>(memmove),
            erase::<unsafe extern "C" fn(*mut c_void, i32, usize) -> *mut c_void>(memset),
            erase::<unsafe extern "C" fn(*const c_void, *const c_void, usize) -> i32>(memcmp),
            __C_specific_handler,
            __CxxFrameHandler3,
            erase::<unsafe extern "C" fn(*mut c_void, *const c_void) -> !>(throw_from_blob),
            __std_type_info_destroy_list,
            erase::<unsafe extern "C" fn(*const core::ffi::c_char) -> usize>(strlen),
            erase::<unsafe extern "C" fn(usize) -> *mut c_void>(malloc),
            erase::<unsafe extern "C" fn(*mut c_void)>(free),
            erase::<unsafe extern "C" fn() -> !>(abort),
            ceil,
            ceilf,
            cos,
            floor,
            floorf,
            fma,
            fmaf,
            fmod,
            fmodf,
            log2,
            pow,
            round,
            roundf,
            trunc,
            _cexit,
            _configure_narrow_argv,
            _execute_onexit_table,
            _initialize_narrow_environment,
            _initialize_onexit_table,
            _initterm,
            _initterm_e,
            _seh_filter_dll,
            strcmp,
            strncmp,
            realloc,
            qsort,
            _dclass,
            _fdclass,
            truncf,
            copysign,
            copysignf,
        ],
    };

    /// Keeps [`CRT_TABLE`] in the program (for `aqr-ld merge` to find).
    pub fn keep_crt() {
        std::hint::black_box(&CRT_TABLE);
    }
}
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
pub use crt::{CRT_TABLE, keep_crt};

/// The images of the blobs placed in this process: start, end, and the address of RVA 0 of each.
/// Only added to (what is placed stays for the life of the process), and read without a lock:
/// an entry is written before the count that covers it.
#[cfg(all(windows, target_arch = "x86_64"))]
mod images {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// (a set of blobs is about 120; a process places a few)
    const MAX: usize = 4096;
    static IMAGES: [[AtomicUsize; 3]; MAX] = [const { [const { AtomicUsize::new(0) }; 3] }; MAX];
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static ADDING: Mutex<()> = Mutex::new(());

    /// Registers the image of a blob placed at `[start, start + len)`, its RVA 0 at `base`: the
    /// throws of its code (its panics) take their type information relative to `base`. Placing
    /// blobs ([`crate::load`], or a stub finding those merged into it) registers each; past
    /// 4096 images, the rest are left out (their panics then abort).
    pub fn register_image(start: usize, len: usize, base: usize) {
        let _adding = ADDING.lock().unwrap_or_else(|e| e.into_inner());
        let n = COUNT.load(Ordering::Relaxed);
        if n < MAX {
            for (slot, v) in IMAGES[n].iter().zip([start, start + len, base]) {
                slot.store(v, Ordering::Relaxed);
            }
            COUNT.store(n + 1, Ordering::Release);
        }
    }

    /// The base of the registered image holding `addr`, if any.
    pub fn image_base_of(addr: usize) -> Option<usize> {
        let n = COUNT.load(Ordering::Acquire);
        IMAGES[..n].iter().find_map(|[start, end, base]| {
            let range = start.load(Ordering::Relaxed)..end.load(Ordering::Relaxed);
            range.contains(&addr).then(|| base.load(Ordering::Relaxed))
        })
    }
}
#[cfg(all(windows, target_arch = "x86_64"))]
pub use images::{image_base_of, register_image};

/// Takes the import descriptors of the C runtime ([`is_crt`]) out of the import directory at
/// `dir_rva` of image `img` (its bytes from RVA `first`), the others moved up, and gives the slots
/// of their import address tables with the index of each function in [`CRT_NAMES`]: for whoever
/// places the image to write the program's own functions there.
pub fn take_crt(img: &mut [u8], first: u32, dir_rva: u32) -> Result<Vec<(u32, usize)>> {
    if dir_rva == 0 {
        return Ok(Vec::new());
    }
    let at = |rva: u32| rva.checked_sub(first).map(|x| x as usize).filter(|&x| x < img.len());
    let cstr = |img: &[u8], rva: u32| -> Result<Vec<u8>> {
        let s = img.get(at(rva).ok_or("an import name out of the image")?..).unwrap_or_default();
        let end = s.iter().position(|&c| c == 0).ok_or("unterminated import name")?;
        Ok(s[..end].to_vec())
    };
    let start = at(dir_rva).ok_or("the import directory out of the image")?;
    let (mut slots, mut kept) = (Vec::new(), Vec::new());
    let mut d = start;
    while img[d..d + IMPORT_DESCRIPTOR_LEN].iter().any(|&b| b != 0) {
        let (oft, name, ft) = (u32_at(img, d), u32_at(img, d + 12), u32_at(img, d + 16));
        let lib = cstr(img, name)?;
        if !is_crt(&lib) {
            kept.extend_from_slice(&img[d..d + IMPORT_DESCRIPTOR_LEN]);
            d += IMPORT_DESCRIPTOR_LEN;
            continue;
        }
        let lookup = if oft != 0 { oft } else { ft };
        let lookup = at(lookup).ok_or("an import table out of the image")?;
        for k in 0.. {
            let v = u64_at(img, lookup + k * 8);
            if v == 0 {
                break;
            }
            let n = match v & IMPORT_BY_ORDINAL {
                0 => cstr(img, v as u32 + 2)?,
                _ => return Err(format!("{}: an import by ordinal", String::from_utf8_lossy(&lib))),
            };
            let i = CRT_NAMES.iter().position(|f| f.as_bytes() == n).ok_or_else(|| {
                let n = String::from_utf8_lossy(&n);
                format!("{}: {n} is imported, which aqr_blob::CRT_NAMES does not have", String::from_utf8_lossy(&lib))
            })?;
            slots.push((ft + k as u32 * 8, i));
        }
        d += IMPORT_DESCRIPTOR_LEN;
    }
    img[start..start + kept.len()].copy_from_slice(&kept);
    img[start + kept.len()..d + IMPORT_DESCRIPTOR_LEN].fill(0);
    Ok(slots)
}

impl Blob {
    /// The libraries it binds imports from, each once.
    pub fn libs(&self) -> Vec<String> {
        let mut libs: Vec<String> = Vec::new();
        for b in &self.binds {
            if !libs.contains(&b.lib) {
                libs.push(b.lib.clone());
            }
        }
        libs
    }

    /// The blob serialized: the header, its name, the libraries it binds imports from, the
    /// binds (slot, library by index, RVA), the relocations, the sections (RVA, length, flags),
    /// then the image.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        let put_str = |b: &mut Vec<u8>, s: &str| {
            put(b, s.len() as u32);
            b.extend_from_slice(s.as_bytes());
        };
        b.extend_from_slice(MAGIC);
        for v in [self.apply_rva, self.info_rva, self.pdata_rva, self.pdata_count, self.import_dir_rva, self.first] {
            put(&mut b, v);
        }
        b.extend_from_slice(&self.image_base.to_le_bytes());
        put_str(&mut b, &self.name);
        let libs = self.libs();
        put(&mut b, libs.len() as u32);
        for l in &libs {
            put_str(&mut b, l);
        }
        put(&mut b, self.binds.len() as u32);
        for bind in &self.binds {
            put(&mut b, bind.slot);
            put(&mut b, libs.iter().position(|l| *l == bind.lib).unwrap() as u32);
            put(&mut b, bind.rva);
        }
        put(&mut b, self.relocs.len() as u32);
        for &r in &self.relocs {
            put(&mut b, r);
        }
        put(&mut b, self.sections.len() as u32);
        for s in &self.sections {
            [s.rva, s.len, s.flags].into_iter().for_each(|v| put(&mut b, v));
        }
        put(&mut b, self.image.len() as u32);
        b.extend_from_slice(&self.image);
        b
    }

    /// A blob parsed from bytes.
    pub fn from_bytes(b: &[u8]) -> Result<Blob> {
        let v2 = b.get(0..4) == Some(MAGIC_V2);
        if b.len() < 36 || !(v2 || &b[0..4] == MAGIC) {
            return Err("not a module blob (of this version)".into());
        }
        let mut r = Reader { b, at: MAGIC.len() };
        let (apply_rva, info_rva, pdata_rva, pdata_count, import_dir_rva, first) =
            (r.word()?, r.word()?, r.word()?, r.word()?, r.word()?, r.word()?);
        let image_base = u64::from_le_bytes(r.take(8)?.try_into().unwrap());
        let name = r.string()?;
        let libs = (0..r.word()?).map(|_| r.string()).collect::<Result<Vec<_>>>()?;
        let mut binds = Vec::new();
        for _ in 0..r.word()? {
            let (slot, lib, rva) = (r.word()?, r.word()? as usize, r.word()?);
            let lib = libs.get(lib).ok_or("a bind of no library")?.clone();
            binds.push(Bind { slot, lib, rva });
        }
        let relocs = (0..r.word()?).map(|_| r.word()).collect::<Result<Vec<_>>>()?;
        let sections = match v2 {
            true => Vec::new(),
            false => (0..r.word()?)
                .map(|_| Ok(Section { rva: r.word()?, len: r.word()?, flags: r.word()? }))
                .collect::<Result<Vec<_>>>()?,
        };
        let n = r.word()? as usize;
        let image = r.take(n)?.to_vec();
        Ok(Blob {
            apply_rva,
            info_rva,
            pdata_rva,
            pdata_count,
            import_dir_rva,
            first,
            image_base,
            name,
            binds,
            relocs,
            sections,
            image,
        })
    }
}

/// The bytes of a blob read in order.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n).ok_or("a module blob cut short")?);
        let s = s.ok_or("a module blob cut short")?;
        self.at += n;
        Ok(s)
    }
    fn word(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String> {
        let n = self.word()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip() {
        let b = Blob {
            apply_rva: 0x1010,
            info_rva: 0x1020,
            pdata_rva: 0x3000,
            pdata_count: 2,
            import_dir_rva: 0x2000,
            first: 0x1000,
            image_base: 0x1_8000_0000,
            name: "aqr_dll_rt.dll".into(),
            binds: vec![
                Bind { slot: 0x2100, lib: "a.dll".into(), rva: 0x1234 },
                Bind { slot: 0x2108, lib: "b.dll".into(), rva: 0x5678 },
                Bind { slot: 0x2110, lib: "a.dll".into(), rva: 0x9abc },
            ],
            relocs: vec![0x1100, 0x1108],
            sections: vec![
                Section { rva: 0x1000, len: 0x1800, flags: SCN_EXECUTE | SCN_READ },
                Section { rva: 0x3000, len: 0x1000, flags: SCN_READ | SCN_WRITE },
            ],
            image: (0..0x3000u32).map(|i| i as u8).collect(),
        };
        let c = Blob::from_bytes(&b.to_bytes()).unwrap();
        assert_eq!(c.to_bytes(), b.to_bytes());
        assert_eq!(c.libs(), ["a.dll", "b.dll"]);
        assert!(Blob::from_bytes(&b.to_bytes()[..100]).is_err());
    }

    /// An image (from RVA 0x1000) importing from kernel32.dll and `crt`.
    fn image(crt: &str, crt_functions: &[&str]) -> Vec<u8> {
        let mut img = vec![0u8; 0x1000];
        let put = |img: &mut [u8], o: usize, v: u64, n: usize| img[o..o + n].copy_from_slice(&v.to_le_bytes()[..n]);
        let name = |img: &mut [u8], o: usize, s: &str| img[o..o + s.len()].copy_from_slice(s.as_bytes());
        // descriptors at 0x1000 (lookup table, name, address table), then the terminator
        for (k, (lib, lookup, ft)) in [("kernel32.dll", 0x1200, 0x1300), (crt, 0x1400, 0x1500)].iter().enumerate() {
            put(&mut img, k * 20, *lookup, 4);
            put(&mut img, k * 20 + 12, 0x1100 + k as u64 * 0x20, 4);
            put(&mut img, k * 20 + 16, *ft, 4);
            name(&mut img, 0x100 + k * 0x20, lib);
        }
        let funcs: [&[&str]; 2] = [&["GetLastError"], crt_functions];
        for (k, fs) in funcs.iter().enumerate() {
            for (i, f) in fs.iter().enumerate() {
                let hint = 0x600 + k * 0x200 + i * 0x40;
                name(&mut img, hint + 2, f);
                for table in [0x200 + k * 0x200, 0x300 + k * 0x200] {
                    put(&mut img, table + i * 8, 0x1000 + hint as u64, 8);
                }
            }
        }
        img
    }

    #[test]
    fn crt_taken() {
        let mut img = image("VCRUNTIME140.dll", &["memset", "__CxxFrameHandler3"]);
        let slots = take_crt(&mut img, 0x1000, 0x1000).unwrap();
        assert_eq!(slots, [(0x1500, 2), (0x1508, 5)]);
        // kernel32.dll's descriptor is left alone, then the terminator
        assert_eq!(u32_at(&img, 12), 0x1100);
        assert!(img[20..40].iter().all(|&b| b == 0));
        // none left to take
        assert_eq!(take_crt(&mut img, 0x1000, 0x1000).unwrap(), []);

        // the UCRT's too
        let mut img = image("api-ms-win-crt-math-l1-1-0.dll", &["pow", "floor"]);
        let slots = take_crt(&mut img, 0x1000, 0x1000).unwrap();
        let at = |n| CRT_NAMES.iter().position(|&f| f == n).unwrap();
        assert_eq!(slots, [(0x1500, at("pow")), (0x1508, at("floor"))]);
        assert!(img[20..40].iter().all(|&b| b == 0));

        let mut img = image("api-ms-win-crt-math-l1-1-0.dll", &["floor", "sinh"]);
        let e = take_crt(&mut img, 0x1000, 0x1000).unwrap_err();
        assert!(e.contains("sinh"), "{e}");
    }

    /// A blob placed as its sections are: its code executable and not writable, its data
    /// writable and not executable, its read-only data neither; one of no sections (of the
    /// earlier version) all writable and executable, as before.
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn placed_as_its_sections() {
        #[repr(C)]
        struct Info {
            base: usize,
            alloc_base: usize,
            alloc_protect: u32,
            partition: u16,
            size: usize,
            state: u32,
            protect: u32,
            ty: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn VirtualQuery(addr: usize, info: *mut Info, len: usize) -> usize;
        }
        let protect = |addr: usize| {
            let mut i = std::mem::MaybeUninit::<Info>::zeroed();
            assert_ne!(unsafe { VirtualQuery(addr, i.as_mut_ptr(), size_of::<Info>()) }, 0);
            unsafe { i.assume_init() }.protect
        };
        // (code: `mov eax, [rip + data]; ret`, of the data's first word, 42)
        let mut image = vec![0u8; 0x3000];
        image[..7].copy_from_slice(&[0x8b, 0x05, 0xfa, 0x1f, 0x00, 0x00, 0xc3]);
        image[0x2000..0x2004].copy_from_slice(&42u32.to_le_bytes());
        let mut b = Blob {
            apply_rva: 0x1000,
            info_rva: 0,
            pdata_rva: 0,
            pdata_count: 0,
            import_dir_rva: 0,
            first: 0x1000,
            image_base: 0x1_8000_0000,
            name: String::new(),
            binds: Vec::new(),
            relocs: Vec::new(),
            sections: vec![
                Section { rva: 0x1000, len: 7, flags: SCN_EXECUTE | SCN_READ },
                Section { rva: 0x2000, len: 0x800, flags: SCN_READ },
                Section { rva: 0x3000, len: 4, flags: SCN_READ | SCN_WRITE },
            ],
            image,
        };
        let (rx, ro, rw, rwx) = (0x20, 0x02, 0x04, 0x40);
        let placed = load(&[&b.to_bytes()]).unwrap().blobs.remove(0);
        let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(placed.at(0x1000)) };
        assert_eq!(f(), 42);
        assert_eq!([0x1000, 0x2000, 0x3000].map(|r| protect(placed.at(r))), [rx, ro, rw]);
        // (of the earlier version: no sections)
        b.sections.clear();
        let mut v2 = b.to_bytes();
        v2[..4].copy_from_slice(MAGIC_V2);
        v2.drain(v2.len() - b.image.len() - 8..v2.len() - b.image.len() - 4);
        let placed = load(&[&v2]).unwrap().blobs.remove(0);
        assert_eq!([0x1000, 0x2000, 0x3000].map(|r| protect(placed.at(r))), [rwx; 3]);
    }
}
