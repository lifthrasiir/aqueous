//! Blobs: native libraries digested into just what it takes to place them in another image
//! (`aqr-ld blob`, `aqr-ld blobs`): the image laid out from the first section, its base
//! relocations as a list, the RVAs of its entries, and its imports from the other libraries of the
//! set it was digested with, bound by name ahead ([`Bind`]). `aqr-ld merge` places blobs into a
//! stub executable as sections (the self-extracting programs); [`load`] places them in memory at
//! run time (Windows x86-64), for a program that carries them as data (aqr, the interpreter blob).

use std::fmt;

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
    pub image: Vec<u8>,
}

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob({:?}, {} bytes, {} binds)", self.name, self.image.len(), self.binds.len())
    }
}

pub const MAGIC: &[u8; 4] = b"AQB2";

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

// ---- the Visual C++ runtime of blobs

/// The library of the Visual C++ runtime (lowercase), which blobs import from but no Windows
/// carries (the redistributable installs it). The programs placing blobs link it statically
/// (`.cargo/config.toml`: the C runtime static, the UCRT of Windows 10 on not), and bind the
/// blobs' imports from it to their own copy ([`VCRUNTIME_TABLE`]), so none is loaded. A blob
/// could not link it statically itself: its copy would be left uninitialised (no DllMain runs
/// for a blob), and its exception handling, which panics go through, aborts so.
pub const VCRUNTIME: &str = "vcruntime140.dll";

/// The functions of [`VCRUNTIME`] that blobs import, in the order of [`VCRUNTIME_TABLE`]'s (where
/// `_CxxThrowException` is one of its own for blobs).
pub const VCRUNTIME_NAMES: [&str; 8] = [
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "__C_specific_handler",
    "__CxxFrameHandler3",
    "_CxxThrowException",
    "__std_type_info_destroy_list",
];

/// The magic of [`VCRUNTIME_TABLE`], by which `aqr-ld merge` finds it in a stub's file.
pub const VCRUNTIME_MAGIC: &[u8; 16] = b"\x89AQRVCRUNTIMEv1\x00";

/// The program's own functions of [`VCRUNTIME_NAMES`], after [`VCRUNTIME_MAGIC`].
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
#[repr(C)]
pub struct VcruntimeTable {
    pub magic: [u8; 16],
    pub functions: [unsafe extern "C" fn(); 8],
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
mod vcruntime {
    use core::ffi::c_void;

    // (only their addresses are taken: the signatures matter only to rustc, which checks the
    // ones that std uses)
    unsafe extern "C" {
        fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void;
        fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32;
        fn __C_specific_handler();
        fn __CxxFrameHandler3();
        fn _CxxThrowException(object: *mut c_void, info: *const c_void) -> !;
        fn __std_type_info_destroy_list();
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

    /// The program's functions of the Visual C++ runtime that blobs import, found by its magic
    /// in a stub's file by `aqr-ld merge` and used by [`crate::load`]. A program into which blobs
    /// are merged must reference it ([`keep_vcruntime`]), or the linker drops it.
    #[used]
    #[unsafe(no_mangle)]
    pub static VCRUNTIME_TABLE: super::VcruntimeTable = super::VcruntimeTable {
        magic: *super::VCRUNTIME_MAGIC,
        functions: [
            erase::<Mem>(memcpy),
            erase::<Mem>(memmove),
            erase::<unsafe extern "C" fn(*mut c_void, i32, usize) -> *mut c_void>(memset),
            erase::<unsafe extern "C" fn(*const c_void, *const c_void, usize) -> i32>(memcmp),
            __C_specific_handler,
            __CxxFrameHandler3,
            erase::<unsafe extern "C" fn(*mut c_void, *const c_void) -> !>(throw_from_blob),
            __std_type_info_destroy_list,
        ],
    };

    /// Keeps [`VCRUNTIME_TABLE`] in the program (for `aqr-ld merge` to find).
    pub fn keep_vcruntime() {
        std::hint::black_box(&VCRUNTIME_TABLE);
    }
}
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
pub use vcruntime::{VCRUNTIME_TABLE, keep_vcruntime};

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

/// Takes the import descriptor of [`VCRUNTIME`] out of the import directory at `dir_rva` of image
/// `img` (its bytes from RVA `first`), the others moved up, and gives the slots of its import
/// address table with the index of each function in [`VCRUNTIME_NAMES`]: for whoever places the
/// image to write the program's own functions there.
pub fn take_vcruntime(img: &mut [u8], first: u32, dir_rva: u32) -> Result<Vec<(u32, usize)>> {
    if dir_rva == 0 {
        return Ok(Vec::new());
    }
    let at = |rva: u32| rva.checked_sub(first).map(|x| x as usize).filter(|&x| x < img.len());
    let cstr = |img: &[u8], rva: u32| -> Result<Vec<u8>> {
        let s = img.get(at(rva).ok_or("an import name out of the image")?..).unwrap_or_default();
        let end = s.iter().position(|&c| c == 0).ok_or("unterminated import name")?;
        Ok(s[..end].to_vec())
    };
    let u64_at = |img: &[u8], o: usize| u64::from_le_bytes(img[o..o + 8].try_into().unwrap());
    let start = at(dir_rva).ok_or("the import directory out of the image")?;
    let (mut slots, mut kept) = (Vec::new(), Vec::new());
    let mut d = start;
    while img[d..d + 20].iter().any(|&b| b != 0) {
        let (oft, name, ft) = (u32_at(img, d), u32_at(img, d + 12), u32_at(img, d + 16));
        if !cstr(img, name)?.eq_ignore_ascii_case(VCRUNTIME.as_bytes()) {
            kept.extend_from_slice(&img[d..d + 20]);
            d += 20;
            continue;
        }
        let lookup = if oft != 0 { oft } else { ft };
        let lookup = at(lookup).ok_or("an import table out of the image")?;
        for k in 0.. {
            let v = u64_at(img, lookup + k * 8);
            if v == 0 {
                break;
            }
            let n = match v & 0x8000_0000_0000_0000 {
                0 => cstr(img, v as u32 + 2)?,
                _ => return Err(format!("{VCRUNTIME}: an import by ordinal")),
            };
            let i = VCRUNTIME_NAMES.iter().position(|f| f.as_bytes() == n).ok_or_else(|| {
                let n = String::from_utf8_lossy(&n);
                format!("{VCRUNTIME}: {n} is imported, which aqr_blob::VCRUNTIME_NAMES does not have")
            })?;
            slots.push((ft + k as u32 * 8, i));
        }
        d += 20;
    }
    img[start..start + kept.len()].copy_from_slice(&kept);
    img[start + kept.len()..d + 20].fill(0);
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
    /// binds (slot, library by index, RVA), the relocations, then the image.
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
        put(&mut b, self.image.len() as u32);
        b.extend_from_slice(&self.image);
        b
    }

    /// A blob parsed from bytes.
    pub fn from_bytes(b: &[u8]) -> Result<Blob> {
        if b.len() < 36 || &b[0..4] != MAGIC {
            return Err("not a module blob (of this version)".into());
        }
        let mut r = Reader { b, at: 36 };
        let name = r.string()?;
        let libs = (0..r.word()?).map(|_| r.string()).collect::<Result<Vec<_>>>()?;
        let mut binds = Vec::new();
        for _ in 0..r.word()? {
            let (slot, lib, rva) = (r.word()?, r.word()? as usize, r.word()?);
            let lib = libs.get(lib).ok_or("a bind of no library")?.clone();
            binds.push(Bind { slot, lib, rva });
        }
        let relocs = (0..r.word()?).map(|_| r.word()).collect::<Result<Vec<_>>>()?;
        let n = r.word()? as usize;
        let image = r.take(n)?.to_vec();
        Ok(Blob {
            apply_rva: u32_at(b, 4),
            info_rva: u32_at(b, 8),
            pdata_rva: u32_at(b, 12),
            pdata_count: u32_at(b, 16),
            import_dir_rva: u32_at(b, 20),
            first: u32_at(b, 24),
            image_base: u64::from_le_bytes(b[28..36].try_into().unwrap()),
            name,
            binds,
            relocs,
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
            image: (0..0x3000u32).map(|i| i as u8).collect(),
        };
        let c = Blob::from_bytes(&b.to_bytes()).unwrap();
        assert_eq!(c.to_bytes(), b.to_bytes());
        assert_eq!(c.libs(), ["a.dll", "b.dll"]);
        assert!(Blob::from_bytes(&b.to_bytes()[..100]).is_err());
    }

    /// An image (from RVA 0x1000) importing from kernel32.dll and VCRUNTIME140.dll.
    fn image(vcrt: &[&str]) -> Vec<u8> {
        let mut img = vec![0u8; 0x1000];
        let put = |img: &mut [u8], o: usize, v: u64, n: usize| img[o..o + n].copy_from_slice(&v.to_le_bytes()[..n]);
        let name = |img: &mut [u8], o: usize, s: &str| img[o..o + s.len()].copy_from_slice(s.as_bytes());
        // descriptors at 0x1000 (lookup table, name, address table), then the terminator
        for (k, (lib, lookup, ft)) in
            [("kernel32.dll", 0x1200, 0x1300), ("VCRUNTIME140.dll", 0x1400, 0x1500)].iter().enumerate()
        {
            put(&mut img, k * 20, *lookup, 4);
            put(&mut img, k * 20 + 12, 0x1100 + k as u64 * 0x20, 4);
            put(&mut img, k * 20 + 16, *ft, 4);
            name(&mut img, 0x100 + k * 0x20, lib);
        }
        let funcs: [&[&str]; 2] = [&["GetLastError"], vcrt];
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
    fn vcruntime_taken() {
        let mut img = image(&["memset", "__CxxFrameHandler3"]);
        let slots = take_vcruntime(&mut img, 0x1000, 0x1000).unwrap();
        assert_eq!(slots, [(0x1500, 2), (0x1508, 5)]);
        // kernel32.dll's descriptor is left alone, then the terminator
        assert_eq!(u32_at(&img, 12), 0x1100);
        assert!(img[20..40].iter().all(|&b| b == 0));
        // none left to take
        assert_eq!(take_vcruntime(&mut img, 0x1000, 0x1000).unwrap(), []);

        let mut img = image(&["memset", "strlen"]);
        let e = take_vcruntime(&mut img, 0x1000, 0x1000).unwrap_err();
        assert!(e.contains("strlen"), "{e}");
    }
}
