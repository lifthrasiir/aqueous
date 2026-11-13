//! Applies the modules built as native libraries (crates/aqr-dll, `target/aqr-dll/NAME.dll`)
//! through their interface (`aqr_api::dll`), loaded at run time:
//!
//! - `record DIR [--only NAME]... ARCHIVE...` decodes the archives with the modules run natively
//!   as aqr-decompress runs them, keeping each application in a directory of its own under
//!   `DIR` (`00001-lz4-recons/`: `params`, `resourceI`, `inputK`, `outputJ`, and `info`).
//! - `run [--module NAME|PATH] CASE...` applies the library of each case's module (or the one
//!   given) to the case, checks the outputs against those recorded, and compares the times.
//! - `apply NAME|PATH [--params FILE] [--resource FILE]... [--input FILE]... (--size N
//!   [--output FILE])...` applies the library to the files given, writing its outputs.
//!
//! A module is named by its binary in aqr-modules (`lz4-recons`), its library then looked for in
//! `aqr-dll/` beside the directory of this program (`target/aqr-dll/` for `target/release/`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use aqr_api::dll::{ApplyFn, Info, InfoFn, Slice, SliceMut, VERSION};
use aqr_decompress::Native;
use aqr_formula::Resource;
use aqr_formula::aqr::{self, Decoded, Loaded, PartIn, PartOut, Runner};
use sha2::{Digest, Sha512_256};

type Result<T> = std::result::Result<T, String>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("record") => record(&args[1..]),
        Some("run") => run(&args[1..]),
        Some("apply") => apply(&args[1..]),
        Some("interp") if args.len() >= 2 => interp(&args[1..]),
        _ => Err("usage: aqr-dll-shim record DIR [--only NAME]... ARCHIVE...\n       \
                  aqr-dll-shim run [--module NAME|PATH] CASE...\n       \
                  aqr-dll-shim apply NAME|PATH [--params FILE] [--resource FILE]... [--input FILE]... \
                  (--size N [--output FILE])..."
            .into()),
    };
    if let Err(e) = r {
        eprintln!("aqr-dll-shim: {e}");
        std::process::exit(1);
    }
}

// ---- the libraries

/// A module's library, loaded (never unloaded).
struct Lib {
    info: Info,
    apply: ApplyFn,
}

/// What `aqr-ld merge` fills: the place of the dispatch table of the modules merged into this
/// executable (its magic, then the table's RVA and length). Empty until something is merged in.
#[repr(C)]
struct Anchor {
    magic: [u8; 16],
    dispatch_rva: u32,
    dispatch_len: u32,
}

#[used]
#[unsafe(no_mangle)]
static AQR_PEMERGE_ANCHOR: Anchor =
    Anchor { magic: *b"\x89AQRPEMERGEv2\x00\x00\x00", dispatch_rva: 0, dispatch_len: 0 };

#[cfg(windows)]
unsafe extern "C" {
    /// The image base, a symbol the MSVC linker defines.
    static __ImageBase: u8;
}

impl Lib {
    /// The library of module `name` merged into this executable (`aqr-ld merge`), if any: its
    /// `aqr_dll_apply` and `aqr_dll_info` called by the RVAs in the dispatch table. The images
    /// merged are registered (`aqr_blob::register_image`: their panics' type information is
    /// relative to their RVA 0); their exception data is this executable's (the merge's).
    #[cfg(windows)]
    fn embedded(name: &str) -> Option<Result<Lib>> {
        let base = &raw const __ImageBase as usize;
        let read = |rva: u32, n: usize| unsafe { std::slice::from_raw_parts((base + rva as usize) as *const u8, n) };
        let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let (rva, len) = unsafe {
            (
                std::ptr::read_volatile(&AQR_PEMERGE_ANCHOR.dispatch_rva),
                std::ptr::read_volatile(&AQR_PEMERGE_ANCHOR.dispatch_len),
            )
        };
        if rva == 0 || len < 4 {
            return None;
        }
        let table = read(rva, len as usize);
        // every image merged (the modules and the shared libraries they link against), once
        static IMAGES: std::sync::Once = std::sync::Once::new();
        IMAGES.call_once(|| {
            #[cfg(target_env = "msvc")]
            aqr_blob::keep_vcruntime();
            for i in 0..u32_at(table, 0) as usize {
                let e = 4 + i * 24;
                let (image_rva, image_len, mod_base) =
                    (u32_at(table, e + 8), u32_at(table, e + 12), u32_at(table, e + 16));
                aqr_blob::register_image(base + image_rva as usize, image_len as usize, base + mod_base as usize);
            }
        });
        for i in 0..u32_at(table, 0) as usize {
            // entry: apply_rva, info_rva, image_rva, image_len, mod_base, name_off (6 u32)
            let e = 4 + i * 24;
            let off = u32_at(table, e + 20) as usize;
            let nlen = u32_at(table, off) as usize;
            if &table[off + 4..off + 4 + nlen] != name.as_bytes() {
                continue;
            }
            let (apply_rva, info_rva) = (u32_at(table, e), u32_at(table, e + 4));
            if info_rva == 0 {
                return Some(Err(format!("{name}: no info in the merged module")));
            }
            // SAFETY: the merge wrote the RVAs of the module's aqr_dll_info and aqr_dll_apply
            let info = unsafe { std::mem::transmute::<usize, InfoFn>(base + info_rva as usize)() };
            let apply = unsafe { std::mem::transmute::<usize, ApplyFn>(base + apply_rva as usize) };
            if info.version != VERSION {
                return Some(Err(format!("{name}: merged interface {}, not {VERSION}", info.version)));
            }
            return Some(Ok(Lib { info, apply }));
        }
        None
    }

    #[cfg(not(windows))]
    fn embedded(_name: &str) -> Option<Result<Lib>> {
        None
    }

    /// The library of module `name`: the one merged into this executable, else the one in
    /// `aqr-dll/`, or at path `name`.
    fn find(name: &str) -> Result<Lib> {
        match Lib::embedded(name) {
            Some(r) => r,
            None => Lib::open(name),
        }
    }

    /// The library of module `name` (in `aqr-dll/`), or at path `name`.
    fn open(name: &str) -> Result<Lib> {
        let path = match name.contains(['/', '\\']) || name.ends_with(std::env::consts::DLL_SUFFIX) {
            true => PathBuf::from(name),
            false => {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let dir = exe.parent().and_then(Path::parent).ok_or("no directory of the program")?;
                dir.join("aqr-dll").join(format!("{name}{}", std::env::consts::DLL_SUFFIX))
            }
        };
        let (info, apply) = sys::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let info = unsafe { info() };
        if info.version != VERSION {
            return Err(format!("{}: interface {}, not {VERSION}", path.display(), info.version));
        }
        Ok(Lib { info, apply })
    }

    fn name(&self) -> String {
        String::from_utf8_lossy(unsafe { self.info.name.get() }).into_owned()
    }

    /// The outputs of an application, of `sizes` bytes.
    fn apply(&self, params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], sizes: &[u64]) -> Result<Vec<Vec<u8>>> {
        let arity = 1 + resources.len() + inputs.len() + sizes.len();
        if arity != self.info.arity as usize {
            return Err(format!("{}: {arity} arguments, not {}", self.name(), self.info.arity));
        }
        let mut outs: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&s| usize::try_from(s).map(|s| vec![0; s]))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| "an output too large")?;
        let res: Vec<Slice> = resources.iter().map(|r| Slice::of(r)).collect();
        let ins: Vec<Slice> = inputs.iter().map(|i| Slice::of(i)).collect();
        let os: Vec<SliceMut> = outs.iter_mut().map(|o| SliceMut::of(o)).collect();
        let mut error = [0u8; 1024];
        let mut error_len = 0;
        let r = unsafe {
            (self.apply)(
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                ins.as_ptr(),
                ins.len(),
                os.as_ptr(),
                os.len(),
                SliceMut::of(&mut error),
                &mut error_len,
            )
        };
        match r {
            0 => Ok(outs),
            _ => Err(format!("{}: {}", self.name(), String::from_utf8_lossy(&error[..error_len]))),
        }
    }

    /// [`Lib::apply`] by the module's native decoding (`aqr_dll_decode`, on `threads`); `None`
    /// where it has none, or the application is of several outputs.
    fn decode(
        &self,
        params: &[u8],
        resources: &[&[u8]],
        inputs: &[&[u8]],
        sizes: &[u64],
        threads: usize,
    ) -> Option<Result<Vec<Vec<u8>>>> {
        let (Some(decode), [size]) = (self.info.decode, sizes) else { return None };
        // (a module that takes no resources natively is given none)
        let res: Vec<Slice> = match self.info.resources {
            0 => Vec::new(),
            _ => resources.iter().map(|r| Slice::of(r)).collect(),
        };
        let ins: Vec<Slice> = inputs.iter().map(|i| Slice::of(i)).collect();
        let mut out = vec![0u8; *size as usize];
        let mut error = [0u8; 1024];
        let mut error_len = 0;
        // SAFETY: each pointer is to as many items as its count, the output apart
        let r = unsafe {
            decode(
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                ins.as_ptr(),
                ins.len(),
                SliceMut::of(&mut out),
                threads,
                SliceMut::of(&mut error),
                &mut error_len,
            )
        };
        Some(match r {
            0 => Ok(vec![out]),
            _ => Err(format!("{}: {}", self.name(), String::from_utf8_lossy(&error[..error_len]))),
        })
    }

    /// [`Lib::apply`] of a part module streamed (`aqr_dll_apply_part`): its inputs pulled from
    /// cursors, its outputs pushed to buffers; `None` for a whole module.
    fn apply_part(
        &self,
        params: &[u8],
        resources: &[&[u8]],
        inputs: &[&[u8]],
        sizes: &[u64],
    ) -> Option<Result<Vec<Vec<u8>>>> {
        let apply_part = self.info.apply_part?;
        let res: Vec<Slice> = resources.iter().map(|r| Slice::of(r)).collect();
        let ins: Vec<In> = inputs.iter().map(|i| In { data: i.to_vec(), cursor: Default::default() }).collect();
        let outs: Vec<std::cell::RefCell<Vec<u8>>> = sizes.iter().map(|_| Default::default()).collect();
        let in_c: Vec<PartInC> =
            ins.iter().map(|i| PartInC { ctx: i as *const In as Ptr, len: i.data.len() as u64, pull }).collect();
        let out_c: Vec<PartOutC> = outs.iter().map(|o| PartOutC { ctx: o as *const _ as Ptr, push }).collect();
        let mut error = [0u8; 1024];
        let mut error_len = 0;
        // SAFETY: the inputs and outputs live across the call, their callbacks driven from here
        let r = unsafe {
            apply_part(
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                in_c.as_ptr(),
                in_c.len(),
                out_c.as_ptr(),
                out_c.len(),
                sizes.as_ptr(),
                SliceMut::of(&mut error),
                &mut error_len,
            )
        };
        Some(match r {
            0 => Ok(outs.into_iter().map(std::cell::RefCell::into_inner).collect()),
            _ => Err(format!("{}: {}", self.name(), String::from_utf8_lossy(&error[..error_len]))),
        })
    }
}

#[cfg(windows)]
mod sys {
    use super::{ApplyFn, InfoFn};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};

    type Sym = unsafe extern "system" fn() -> isize;

    pub fn open(path: &std::path::Path) -> Result<(InfoFn, ApplyFn), String> {
        // (the shared libraries it links against looked for beside it, as build.sh puts them)
        let path = std::path::absolute(path).map_err(|e| e.to_string())?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let h = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
        if h.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let info = unsafe { GetProcAddress(h, c"aqr_dll_info".as_ptr().cast()) }.ok_or("no aqr_dll_info")?;
        let apply = unsafe { GetProcAddress(h, c"aqr_dll_apply".as_ptr().cast()) }.ok_or("no aqr_dll_apply")?;
        // SAFETY: the functions of the interface, as aqr-dll exports them
        Ok(unsafe { (std::mem::transmute::<Sym, InfoFn>(info), std::mem::transmute::<Sym, ApplyFn>(apply)) })
    }
}

#[cfg(unix)]
mod sys {
    use super::{ApplyFn, InfoFn};
    use std::os::unix::ffi::OsStrExt;

    type Sym = *mut libc::c_void;

    pub fn open(path: &std::path::Path) -> Result<(InfoFn, ApplyFn), String> {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if h.is_null() {
            return Err(unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) }.to_string_lossy().into_owned());
        }
        let sym = |n: &std::ffi::CStr| match unsafe { libc::dlsym(h, n.as_ptr()) } {
            p if p.is_null() => Err(format!("no {}", n.to_string_lossy())),
            p => Ok(p),
        };
        let (info, apply) = (sym(c"aqr_dll_info")?, sym(c"aqr_dll_apply")?);
        // SAFETY: the functions of the interface, as aqr-dll exports them
        Ok(unsafe { (std::mem::transmute::<Sym, InfoFn>(info), std::mem::transmute::<Sym, ApplyFn>(apply)) })
    }
}

// ---- record

/// Runs the modules as [`Native`] does, keeping each application.
struct Recorder {
    native: Native,
    dir: PathBuf,
    only: Vec<String>,
    n: AtomicUsize,
    failed: Mutex<Option<String>>,
}

struct Recording<'a> {
    rec: &'a Recorder,
    inner: Box<dyn Loaded + 'a>,
    /// The binary's name, where aqr-modules built the module.
    name: Option<String>,
}

impl Runner for Recorder {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        let hash: [u8; 32] = Sha512_256::digest(wasm).into();
        let name = aqr_modules::KNOWN.iter().find(|k| k.sha512_256 == hash).map(|k| k.name.replace('/', "-"));
        let name = name.filter(|n| self.only.is_empty() || self.only.contains(n));
        Ok(Box::new(Recording { rec: self, inner: self.native.load(wasm)?, name }))
    }
}

impl Recording<'_> {
    #[allow(clippy::too_many_arguments)]
    fn keep(
        &self,
        name: &str,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        outputs: &[Vec<u8>],
        limits: (u64, &[u64]),
        secs: f64,
    ) {
        let n = self.rec.n.fetch_add(1, Ordering::Relaxed) + 1;
        let dir = self.rec.dir.join(format!("{n:05}-{name}"));
        let r = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("params"), params)?;
            for (i, r) in resources.iter().enumerate() {
                std::fs::write(dir.join(format!("resource{i}")), &r[..])?;
            }
            for (k, d) in inputs.iter().enumerate() {
                std::fs::write(dir.join(format!("input{k}")), d)?;
            }
            for (j, d) in outputs.iter().enumerate() {
                std::fs::write(dir.join(format!("output{j}")), d)?;
            }
            let info = format!(
                "module {name}\nresources {}\ninputs {}\noutputs {}\nmemory_limit {}\ntask_limits {:?}\nnative {secs:.6}\n",
                resources.len(),
                inputs.len(),
                outputs.len(),
                limits.0,
                limits.1,
            );
            std::fs::write(dir.join("info"), info)
        })();
        let total: usize = inputs.iter().map(|i| i.len()).sum();
        match r {
            Ok(()) => eprintln!("{}: {name}, {total} bytes in, {secs:.3} s", dir.display()),
            Err(e) => {
                self.rec.failed.lock().unwrap().get_or_insert(format!("{}: {e}", dir.display()));
            }
        }
    }
}

impl Loaded for Recording<'_> {
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>> {
        let t = Instant::now();
        let out = self.inner.apply(params, resources, inputs, sizes, memory_limit, task_limits)?;
        let secs = t.elapsed().as_secs_f64();
        if let Some(name) = &self.name {
            self.keep(name, params, resources, inputs, &out, (memory_limit, task_limits), secs);
        }
        Ok(out)
    }

    fn is_part(&self) -> bool {
        self.inner.is_part()
    }

    // (the inputs pulled whole and the module applied whole, to be kept)
    fn apply_part(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        if self.name.is_none() {
            return self.inner.apply_part(params, resources, inputs, outputs, sizes, memory_limit, task_limits);
        }
        let mut data = Vec::with_capacity(inputs.len());
        for i in inputs.iter_mut() {
            let mut d = Vec::with_capacity(i.len() as usize);
            let len = i.len();
            i.pull(len, &mut |b| d.extend_from_slice(b))?;
            data.push(d);
        }
        let ins: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let out = self.apply(params, resources, &ins, sizes, memory_limit, task_limits)?;
        for (o, d) in outputs.iter_mut().zip(&out) {
            o.push(d)?;
        }
        Ok(())
    }
}

fn record(args: &[String]) -> Result<()> {
    let (dir, rest) = args.split_first().ok_or("record: no directory")?;
    let mut only = Vec::new();
    let mut archives = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--only" => only.push(it.next().ok_or("--only: no module")?.clone()),
            _ => archives.push(a),
        }
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let rec = Recorder {
        native: Native { threads },
        dir: PathBuf::from(dir),
        only,
        n: AtomicUsize::new(0),
        failed: Mutex::new(None),
    };
    for a in archives {
        let c = std::fs::read(a).map_err(|e| format!("{a}: {e}"))?;
        aqr::decode_value_with(&c, &rec, threads).map_err(|e| format!("{a}: {e:?}"))?;
        if let Some(e) = rec.failed.lock().unwrap().take() {
            return Err(e);
        }
    }
    eprintln!("{} applications kept", rec.n.load(Ordering::Relaxed));
    Ok(())
}

// ---- run

/// A case of [`record`].
struct Case {
    module: String,
    params: Vec<u8>,
    resources: Vec<Vec<u8>>,
    inputs: Vec<Vec<u8>>,
    outputs: Vec<Vec<u8>>,
    memory_limit: u64,
    task_limits: Vec<u64>,
    native: Option<f64>,
}

impl Case {
    fn read(dir: &Path) -> Result<Case> {
        let file = |n: &str| std::fs::read(dir.join(n)).map_err(|e| format!("{}: {e}", dir.join(n).display()));
        let info = String::from_utf8(file("info")?).map_err(|e| e.to_string())?;
        let field = |k: &str| info.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(' ')).map(str::to_owned);
        let count = |k: &str| field(k).and_then(|v| v.parse::<usize>().ok()).ok_or(format!("info: no {k}"));
        let many = |kind: &str, n: usize| (0..n).map(|i| file(&format!("{kind}{i}"))).collect::<Result<Vec<_>>>();
        Ok(Case {
            module: field("module").ok_or("info: no module")?,
            params: file("params")?,
            resources: many("resource", count("resources")?)?,
            inputs: many("input", count("inputs")?)?,
            outputs: many("output", count("outputs")?)?,
            memory_limit: field("memory_limit").and_then(|v| v.parse().ok()).unwrap_or(u64::MAX),
            task_limits: field("task_limits")
                .map(|v| v.trim_matches(['[', ']']).split(',').filter_map(|s| s.trim().parse().ok()).collect())
                .unwrap_or_default(),
            native: field("native").and_then(|v| v.parse().ok()),
        })
    }
}

fn run(args: &[String]) -> Result<()> {
    let mut module = None;
    let mut cases = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--module" => module = Some(it.next().ok_or("--module: no module")?.clone()),
            _ => cases.push(PathBuf::from(a)),
        }
    }
    let mut libs: HashMap<String, Lib> = HashMap::new();
    let mut bad = 0;
    let (mut total_dll, mut total_native) = (0.0, 0.0);
    for dir in &cases {
        let case = Case::read(dir)?;
        let name = module.clone().unwrap_or_else(|| case.module.clone());
        if !libs.contains_key(&name) {
            // a path given is opened; a bare name prefers the module merged into this executable
            let lib = if module.is_some() { Lib::open(&name)? } else { Lib::find(&name)? };
            libs.insert(name.clone(), lib);
        }
        let lib = &libs[&name];
        let res: Vec<&[u8]> = case.resources.iter().map(Vec::as_slice).collect();
        let ins: Vec<&[u8]> = case.inputs.iter().map(Vec::as_slice).collect();
        let sizes: Vec<u64> = case.outputs.iter().map(|o| o.len() as u64).collect();
        let t = Instant::now();
        let got = lib.apply(&case.params, &res, &ins, &sizes);
        let secs = t.elapsed().as_secs_f64();
        let judge = |got: &Result<Vec<Vec<u8>>>| match got {
            Ok(o) if *o == case.outputs => "same".to_owned(),
            Ok(_) => "DIFFERENT".to_owned(),
            Err(e) => format!("FAILED: {e}"),
        };
        let mut verdict = judge(&got);
        // a part module streamed too (`aqr_dll_apply_part`): the same, or what is wrong with it
        let t = Instant::now();
        let streamed = lib.apply_part(&case.params, &res, &ins, &sizes);
        let streamed_secs = t.elapsed().as_secs_f64();
        if let Some(s) = &streamed
            && verdict == "same"
        {
            verdict = match judge(s) {
                v if v == "same" => v,
                v => format!("streamed {v}"),
            };
        }
        let streamed = match streamed {
            Some(_) => format!(", streamed {streamed_secs:.3} s"),
            None => String::new(),
        };
        // by its native decoding too (`aqr_dll_decode`): the same
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let t = Instant::now();
        let decoded = lib.decode(&case.params, &res, &ins, &sizes, threads);
        let decoded_secs = t.elapsed().as_secs_f64();
        if let Some(d) = &decoded
            && verdict == "same"
        {
            verdict = match judge(d) {
                v if v == "same" => v,
                v => format!("decoded {v}"),
            };
        }
        let streamed = match decoded {
            Some(_) => format!("{streamed}, decoded {decoded_secs:.3} s"),
            None => streamed,
        };
        bad += (verdict != "same") as usize;
        let total: usize = case.inputs.iter().map(Vec::len).sum();
        let native = case.native.map_or(String::new(), |n| format!(", native {n:.3} s"));
        println!("{}: {name}, {total} bytes in, library {secs:.3} s{native}{streamed}: {verdict}", dir.display());
        total_dll += secs;
        total_native += case.native.unwrap_or(0.0);
    }
    if cases.len() > 1 {
        println!("{} cases: library {total_dll:.3} s, native {total_native:.3} s, {bad} not the same", cases.len());
    }
    match bad {
        0 => Ok(()),
        n => Err(format!("{n} cases not the same")),
    }
}

// ---- apply

fn apply(args: &[String]) -> Result<()> {
    let (name, rest) = args.split_first().ok_or("apply: no module")?;
    let read = |f: &String| std::fs::read(f).map_err(|e| format!("{f}: {e}"));
    let (mut params, mut resources, mut inputs, mut sizes, mut files) = (Vec::new(), vec![], vec![], vec![], vec![]);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("{a}: no value"));
        match a.as_str() {
            "--params" => params = read(value()?)?,
            "--resource" => resources.push(read(value()?)?),
            "--input" => inputs.push(read(value()?)?),
            "--size" => {
                let v = value()?;
                sizes.push(v.parse::<u64>().map_err(|e| format!("--size {v}: {e}"))?);
                files.push(None);
            }
            "--output" => {
                let v = value()?.clone();
                *files.last_mut().ok_or("--output before its --size")? = Some(v);
            }
            _ => return Err(format!("apply: {a}?")),
        }
    }
    let lib = Lib::open(name)?;
    let res: Vec<&[u8]> = resources.iter().map(Vec::as_slice).collect();
    let ins: Vec<&[u8]> = inputs.iter().map(Vec::as_slice).collect();
    let t = Instant::now();
    let outs = lib.apply(&params, &res, &ins, &sizes)?;
    eprintln!("{}: {:.3} s", lib.name(), t.elapsed().as_secs_f64());
    for (j, (o, f)) in outs.iter().zip(&files).enumerate() {
        match f {
            Some(f) => std::fs::write(f, o).map_err(|e| format!("{f}: {e}"))?,
            None => println!("output {j}: {} bytes, SHA-512/256 {:x}", o.len(), Sha512_256::digest(o)),
        }
    }
    Ok(())
}

// ---- interp: run cases through the interpreter blob (aqr-interp), whole and streamed

type Ptr = *mut core::ffi::c_void;
type PutFn = unsafe extern "C" fn(Ptr, *const u8, usize);

// (the callbacks of a part application, as aqr-api gives them: the interpreter's and the
// libraries')
use aqr_api::dll::{PartIn as PartInC, PartOut as PartOutC};

#[allow(clippy::type_complexity)]
struct Interp {
    load: unsafe extern "C" fn(Slice, SliceMut, *mut usize) -> Ptr,
    is_part: unsafe extern "C" fn(Ptr) -> u32,
    apply: unsafe extern "C" fn(
        Ptr,
        Slice,
        *const Slice,
        usize,
        *const Slice,
        usize,
        u64,
        *const u64,
        usize,
        *const SliceMut,
        usize,
        SliceMut,
        *mut usize,
    ) -> u32,
    apply_part: unsafe extern "C" fn(
        Ptr,
        Slice,
        *const Slice,
        usize,
        *const PartInC,
        usize,
        *const PartOutC,
        usize,
        *const u64,
        usize,
        u64,
        *const u64,
        usize,
        SliceMut,
        *mut usize,
    ) -> u32,
    drop: unsafe extern "C" fn(Ptr),
}

/// An input kept whole for the test, pulled in pieces from a cursor.
struct In {
    data: Vec<u8>,
    cursor: std::cell::Cell<u64>,
}

unsafe extern "C" fn pull(ctx: Ptr, hi: u64, put_ctx: Ptr, put: PutFn) -> u32 {
    let input = unsafe { &*(ctx as *const In) };
    let (lo, hi) = (input.cursor.get() as usize, hi as usize);
    if hi > input.data.len() {
        return 1;
    }
    unsafe { put(put_ctx, input.data[lo..hi].as_ptr(), hi - lo) };
    input.cursor.set(hi as u64);
    0
}

unsafe extern "C" fn push(ctx: Ptr, ptr: *const u8, len: usize) -> u32 {
    let out = unsafe { &*(ctx as *const std::cell::RefCell<Vec<u8>>) };
    out.borrow_mut().extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, len) });
    0
}

fn interp(args: &[String]) -> Result<()> {
    let (dll, cases) = (&args[0], &args[1..]);
    let it = interp_sys::open(Path::new(dll)).map_err(|e| format!("{dll}: {e}"))?;
    let (mut total, mut bad) = (0.0, 0);
    for dir in cases {
        let case = Case::read(Path::new(dir))?;
        let known = aqr_modules::KNOWN
            .iter()
            .find(|k| k.name == case.module)
            .ok_or_else(|| format!("{}: not a module of aqr-modules", case.module))?;
        let wasm = known.read_wasm().map_err(|e| format!("{}: {e}", case.module))?;
        let mut err = [0u8; 1024];
        let mut el = 0;
        let handle = unsafe { (it.load)(Slice::of(&wasm), SliceMut::of(&mut err), &mut el) };
        if handle.is_null() {
            return Err(format!("{}: load: {}", case.module, String::from_utf8_lossy(&err[..el])));
        }
        let res: Vec<Slice> = case.resources.iter().map(|r| Slice::of(r)).collect();
        let sizes: Vec<u64> = case.outputs.iter().map(|o| o.len() as u64).collect();
        let t = Instant::now();
        let got: Result<Vec<Vec<u8>>> = if unsafe { (it.is_part)(handle) } != 0 {
            // streamed: inputs pulled as requested, outputs collected as committed
            let ins: Vec<In> = case.inputs.iter().map(|d| In { data: d.clone(), cursor: 0.into() }).collect();
            let in_c: Vec<PartInC> =
                ins.iter().map(|i| PartInC { ctx: i as *const In as Ptr, len: i.data.len() as u64, pull }).collect();
            let outs: Vec<std::cell::RefCell<Vec<u8>>> =
                (0..case.outputs.len()).map(|_| std::cell::RefCell::new(Vec::new())).collect();
            let out_c: Vec<PartOutC> = outs.iter().map(|o| PartOutC { ctx: o as *const _ as Ptr, push }).collect();
            let r = unsafe {
                (it.apply_part)(
                    handle,
                    Slice::of(&case.params),
                    res.as_ptr(),
                    res.len(),
                    in_c.as_ptr(),
                    in_c.len(),
                    out_c.as_ptr(),
                    out_c.len(),
                    sizes.as_ptr(),
                    sizes.len(),
                    case.memory_limit,
                    case.task_limits.as_ptr(),
                    case.task_limits.len(),
                    SliceMut::of(&mut err),
                    &mut el,
                )
            };
            match r {
                0 => Ok(outs.into_iter().map(|o| o.into_inner()).collect()),
                _ => Err(String::from_utf8_lossy(&err[..el]).into_owned()),
            }
        } else {
            let ins: Vec<Slice> = case.inputs.iter().map(|i| Slice::of(i)).collect();
            let mut bufs: Vec<Vec<u8>> = sizes.iter().map(|&s| vec![0u8; s as usize]).collect();
            let os: Vec<SliceMut> = bufs.iter_mut().map(|o| SliceMut::of(o)).collect();
            let r = unsafe {
                (it.apply)(
                    handle,
                    Slice::of(&case.params),
                    res.as_ptr(),
                    res.len(),
                    ins.as_ptr(),
                    ins.len(),
                    case.memory_limit,
                    case.task_limits.as_ptr(),
                    case.task_limits.len(),
                    os.as_ptr(),
                    os.len(),
                    SliceMut::of(&mut err),
                    &mut el,
                )
            };
            match r {
                0 => Ok(bufs),
                _ => Err(String::from_utf8_lossy(&err[..el]).into_owned()),
            }
        };
        let secs = t.elapsed().as_secs_f64();
        unsafe { (it.drop)(handle) };
        total += secs;
        let verdict = match &got {
            Ok(o) if *o == case.outputs => "same".to_owned(),
            Ok(_) => "DIFFERENT".to_owned(),
            Err(e) => format!("FAILED: {e}"),
        };
        bad += (verdict != "same") as usize;
        let n: usize = case.inputs.iter().map(Vec::len).sum();
        println!("{dir}: {}, {n} bytes in, interp {secs:.3} s: {verdict}", case.module);
    }
    if cases.len() > 1 {
        println!("{} cases: interp {total:.3} s, {bad} not the same", cases.len());
    }
    match bad {
        0 => Ok(()),
        n => Err(format!("{n} cases not the same")),
    }
}

#[cfg(windows)]
mod interp_sys {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    use super::Interp;

    type Sym = unsafe extern "system" fn() -> isize;

    #[allow(clippy::missing_transmute_annotations)]
    pub fn open(path: &std::path::Path) -> Result<Interp, String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let h = unsafe { LoadLibraryW(wide.as_ptr()) };
        if h.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let sym =
            |n: &std::ffi::CStr| unsafe { GetProcAddress(h, n.as_ptr().cast()) }.ok_or_else(|| format!("no {n:?}"));
        Ok(Interp {
            load: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_load")?) },
            is_part: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_is_part")?) },
            apply: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_apply")?) },
            apply_part: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_apply_part")?) },
            drop: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_drop")?) },
        })
    }
}

#[cfg(unix)]
mod interp_sys {
    use std::os::unix::ffi::OsStrExt;

    use super::Interp;

    type Sym = *mut libc::c_void;

    #[allow(clippy::missing_transmute_annotations)]
    pub fn open(path: &std::path::Path) -> Result<Interp, String> {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if h.is_null() {
            return Err(unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) }.to_string_lossy().into_owned());
        }
        let sym = |n: &std::ffi::CStr| match unsafe { libc::dlsym(h, n.as_ptr()) } {
            p if p.is_null() => Err(format!("no {n:?}")),
            p => Ok(p),
        };
        Ok(Interp {
            load: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_load")?) },
            is_part: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_is_part")?) },
            apply: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_apply")?) },
            apply_part: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_apply_part")?) },
            drop: unsafe { std::mem::transmute::<Sym, _>(sym(c"aqr_interp_drop")?) },
        })
    }
}
