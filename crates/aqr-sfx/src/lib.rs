//! The self-extracting apply logic, shared by the stub (`aqr-sfx-shim`) and the tool that builds
//! self-extracting archives (`aqr --format sfx`): [`Sfx`] finds the blobs merged into the program
//! through the dispatch table `aqr-ld merge` wrote into the anchor -- the native module blobs
//! (aqr-dll), each named by its module's hash ([`native_name`]), and the interpreter blob
//! (`aqr-interp`, [`INTERP`]) for any other module -- and is the [`Runner`] the decoder runs each
//! module through; [`extract`] writes a decoded archive's files out. The merged blobs are Windows
//! x86-64 only; elsewhere [`Sfx::find`] finds none.

mod native;

use aqr_blob::interp::Interpreter;
#[cfg(windows)]
use aqr_blob::interp::Vtable;
use aqr_formula::aqr::{Decoded, Failure, Loaded, Runner};
#[cfg(windows)]
use native::Fns;
pub use native::native_name;
use native::{Native, NativeLoaded};

/// The name of the interpreter blob in the dispatch table.
pub const INTERP: &str = "aqr-interp";

/// What the name of a shared library that native blobs link against (crates/aqr-dll/layout.py)
/// starts with in the dispatch table: its code is reached through the blobs' imports alone (the
/// merge binds them), its exception table registered as theirs are.
pub const LIB: &str = "lib:";

/// The runner of a self-extracting program: the modules with native blobs merged into it run
/// natively, any other through the interpreter blob, where it is merged.
pub struct Sfx {
    interp: Option<Interpreter>,
    natives: Vec<Native>,
    /// The threads the native blobs decode whole inputs on.
    threads: usize,
}

impl Sfx {
    /// The threads the native blobs decode whole inputs on, as many as the machine has.
    pub fn threads(&self) -> usize {
        self.threads
    }
}

impl Runner for Sfx {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        if let Some(hash) = native::stand_in_hash(wasm) {
            let n = self.natives.iter().find(|n| n.hash == hash);
            let n = n.ok_or_else(|| Failure::Resource("a native module blob not merged".into()))?;
            return Ok(Box::new(NativeLoaded { native: n.fns, threads: self.threads }));
        }
        let interp = self.interp.as_ref().ok_or_else(|| {
            Failure::Resource("a module with no native blob merged, and no interpreter blob either".into())
        })?;
        interp.load(wasm)
    }

    fn registered(&self, hash: &[u8; 32]) -> Decoded<Vec<u8>> {
        match self.natives.iter().find(|n| n.hash == *hash) {
            Some(n) => Ok(native::stand_in(hash, &n.slots)),
            None => Err(Failure::Resource("a registered module with no native blob merged".into())),
        }
    }
}

// ---- finding the merged blobs

#[used]
#[unsafe(no_mangle)]
static AQR_PEMERGE_ANCHOR: aqr_blob::dispatch::Anchor = aqr_blob::dispatch::Anchor::EMPTY;

#[cfg(windows)]
impl Sfx {
    /// The blobs merged into this program, as the dispatch table names them: the interpreter
    /// blob ([`INTERP`], the RVA of its `aqr_interp_vtable`) and the native module blobs
    /// ([`native_name`], the RVA of each `aqr_dll_apply`), each one's image registered here
    /// (`aqr_blob::register_image`: its panics' type information is relative to its RVA 0). Their
    /// exception data is in this program's own directory (the merge put it there: the unwinder
    /// reads no other for the addresses of an image).
    pub fn find() -> Sfx {
        unsafe extern "C" {
            static __ImageBase: u8;
        }
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        let mut sfx = Sfx {
            interp: None,
            natives: Vec::new(),
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        };
        // (the merged blobs' imports from the Visual C++ runtime are bound to this program's)
        #[cfg(target_env = "msvc")]
        aqr_blob::keep_vcruntime();
        let base = &raw const __ImageBase as usize;
        let Some((rva, len)) = AQR_PEMERGE_ANCHOR.table() else { return sfx };
        let table = unsafe { std::slice::from_raw_parts((base + rva as usize) as *const u8, len as usize) };
        REGISTERED.call_once(|| {
            for e in aqr_blob::dispatch::entries(table) {
                aqr_blob::register_image(base + e.image_rva as usize, e.image_len as usize, base + e.mod_base as usize);
            }
        });
        for e in aqr_blob::dispatch::entries(table) {
            let (apply_rva, info_rva, name) = (e.apply_rva as usize, e.info_rva as usize, e.name);
            let at = base + apply_rva;
            if name == INTERP.as_bytes() {
                // SAFETY: the merge wrote the RVA of aqr_interp_vtable, which returns the static vtable
                let vtable_fn: extern "C" fn() -> *const Vtable = unsafe { std::mem::transmute(at) };
                sfx.interp = Some(unsafe { Interpreter::new(vtable_fn()) });
            } else if let Some((hash, slots)) = native::parse_native_name(name)
                && info_rva != 0
            {
                // SAFETY: the merge wrote the RVAs of the blob's aqr_dll_apply and aqr_dll_info
                let apply: aqr_api::dll::ApplyFn = unsafe { std::mem::transmute(at) };
                let info: aqr_api::dll::InfoFn = unsafe { std::mem::transmute(base + info_rva) };
                // (a blob of another version of the interface left out: its info is not this)
                let info = unsafe { info() };
                if info.version == aqr_api::dll::VERSION {
                    let fns = Fns {
                        apply,
                        apply_part: info.apply_part,
                        run: info.run,
                        resources: info.resources != 0,
                        decode: info.decode,
                        streamed: info.streamed,
                    };
                    sfx.natives.push(Native { hash, slots, fns });
                }
            }
        }
        sfx
    }
}

#[cfg(not(windows))]
impl Sfx {
    /// None: the merged blobs are Windows x86-64 only.
    pub fn find() -> Sfx {
        Sfx { interp: None, natives: Vec::new(), threads: std::thread::available_parallelism().map_or(1, |n| n.get()) }
    }
}

// ---- writing the decoded files out

use std::path::Path;

use aqr_formula::aqr::{EntryNode, Value};

/// Writes the files of `value` under `dest`, returning how many were written.
pub fn extract(value: Value, dest: &Path) -> Result<usize, String> {
    match value {
        Value::File(data) => {
            std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            write(&dest.join("aqr-sfx-output"), &data)?;
            Ok(1)
        }
        Value::Tree(tree, contents) => {
            let mut paths: Vec<std::path::PathBuf> = Vec::with_capacity(tree.entries.len());
            let mut files = 0;
            for e in &tree.entries {
                // the entries come each after its parent, so the parent's path is known
                let base = match e.parent {
                    Some(p) => paths[p].clone(),
                    None => dest.to_path_buf(),
                };
                let path = base.join(sanitized(&e.name));
                match &e.node {
                    EntryNode::Dir => std::fs::create_dir_all(&path).map_err(|x| format!("{}: {x}", path.display()))?,
                    EntryNode::File(c) => {
                        let data = contents.get(*c).ok_or_else(|| format!("{}: no content", path.display()))?;
                        write(&path, data)?;
                        files += 1;
                    }
                    // links and special entries are left out of this first stub
                    _ => {}
                }
                paths.push(path);
            }
            Ok(files)
        }
    }
}

/// Writes `data` to `path`, making its parent directory.
fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

/// An entry name (WTF-8) as a path component, lossily, with any separators of its own dropped so
/// that it cannot escape its directory.
fn sanitized(name: &[u8]) -> String {
    String::from_utf8_lossy(name).replace(['/', '\\'], "_").replace("..", "_")
}
