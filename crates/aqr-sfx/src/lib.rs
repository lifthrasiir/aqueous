//! The self-extracting apply logic, shared by the stub (`aqr-sfx-shim`) and the tool that builds
//! self-extracting archives (`aqr --format sfx`): [`Sfx`] finds the blobs merged into the program
//! through the dispatch table `aqr-ld merge` wrote into the anchor -- the native module blobs
//! (aqr-dll), each named by its module's hash ([`native_name`]), and the interpreter blob
//! (`aqr-interp`, [`INTERP`]) for any other module -- and is the [`Runner`] the decoder runs each
//! module through; [`extract`] writes a decoded archive's files out. The merged blobs are Windows
//! x86-64 only; elsewhere [`Sfx::find`] finds none.

mod native;

use core::ffi::c_void;

use aqr_api::dll::{Slice, SliceMut};
use aqr_formula::Resource;
use aqr_formula::aqr::{Decoded, Failure, Loaded, PartIn, PartOut, Runner};
pub use native::native_name;
use native::{Fns, Native, NativeLoaded};

// (an input of a part application as the blob pulls it, an output as it commits it: the same
// for the interpreter blob and the native ones)
use aqr_api::dll::{PartIn as PartInC, PartOut as PartOutC};

/// The interpreter blob's entry points ([`aqr_interp::Vtable`]).
#[repr(C)]
pub struct Vtable {
    load: unsafe extern "C" fn(Slice, SliceMut, *mut usize) -> *mut c_void,
    is_part: unsafe extern "C" fn(*mut c_void) -> u32,
    apply: unsafe extern "C" fn(
        *mut c_void,
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
        *mut c_void,
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
    drop: unsafe extern "C" fn(*mut c_void),
}

/// The name of the interpreter blob in the dispatch table.
pub const INTERP: &str = "aqr-interp";

/// What the name of a shared library that native blobs link against (crates/aqr-dll/layout.py)
/// starts with in the dispatch table: its code is reached through the blobs' imports alone (the
/// merge binds them), its exception table registered as theirs are.
pub const LIB: &str = "lib:";

/// The runner of a self-extracting program: the modules with native blobs merged into it run
/// natively, any other through the interpreter blob, where it is merged.
pub struct Sfx {
    interp: Option<&'static Vtable>,
    natives: Vec<Native>,
    /// The threads the native blobs decode whole inputs on.
    threads: usize,
}

// SAFETY: the vtable is in the image, static; the interpreter's own state is thread-safe, and so
// is each native blob's (its applications share nothing).
unsafe impl Sync for Sfx {}

impl Sfx {
    /// How many native module blobs are merged, and whether the interpreter blob is.
    pub fn merged(&self) -> (usize, bool) {
        (self.natives.len(), self.interp.is_some())
    }
}

impl Runner for Sfx {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        if let Some(hash) = native::stand_in_hash(wasm) {
            let n = self.natives.iter().find(|n| n.hash == hash);
            let n = n.ok_or_else(|| Failure::Resource("a native module blob not merged".into()))?;
            return Ok(Box::new(NativeLoaded { native: n.fns, threads: self.threads }));
        }
        let vtable = self.interp.ok_or_else(|| {
            Failure::Resource("a module with no native blob merged, and no interpreter blob either".into())
        })?;
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: the interpreter's load takes the module's bytes and a place for a message
        let handle = unsafe { (vtable.load)(Slice::of(wasm), SliceMut::of(&mut error), &mut len) };
        if handle.is_null() {
            return Err(Failure::Invalid(String::from_utf8_lossy(&error[..len]).into_owned()));
        }
        Ok(Box::new(Loaded_ { vtable, handle }))
    }

    fn registered(&self, hash: &[u8; 32]) -> Decoded<Vec<u8>> {
        match self.natives.iter().find(|n| n.hash == *hash) {
            Some(n) => Ok(native::stand_in(hash, &n.slots)),
            None => Err(Failure::Resource("a registered module with no native blob merged".into())),
        }
    }
}

/// A module loaded in the interpreter blob, behind its handle.
struct Loaded_ {
    vtable: &'static Vtable,
    handle: *mut c_void,
}

// SAFETY: the handle's module is run through the interpreter, whose applications are thread-safe.
unsafe impl Send for Loaded_ {}
unsafe impl Sync for Loaded_ {}

impl Drop for Loaded_ {
    fn drop(&mut self) {
        // SAFETY: the handle is live and used no more
        unsafe { (self.vtable.drop)(self.handle) };
    }
}

fn message(error: &[u8], len: usize) -> Failure {
    Failure::Invalid(String::from_utf8_lossy(&error[..len]).into_owned())
}

impl Loaded for Loaded_ {
    fn is_part(&self) -> bool {
        // SAFETY: a live handle
        unsafe { (self.vtable.is_part)(self.handle) != 0 }
    }

    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>> {
        let res: Vec<Slice> = resources.iter().map(|r| Slice::of(r)).collect();
        let ins: Vec<Slice> = inputs.iter().map(|i| Slice::of(i)).collect();
        let mut outs: Vec<Vec<u8>> = sizes.iter().map(|&s| vec![0u8; s as usize]).collect();
        let os: Vec<SliceMut> = outs.iter_mut().map(|o| SliceMut::of(o)).collect();
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: each pointer is to as many items as its count, the outputs apart
        let r = unsafe {
            (self.vtable.apply)(
                self.handle,
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                ins.as_ptr(),
                ins.len(),
                memory_limit,
                task_limits.as_ptr(),
                task_limits.len(),
                os.as_ptr(),
                os.len(),
                SliceMut::of(&mut error),
                &mut len,
            )
        };
        match r {
            0 => Ok(outs),
            _ => Err(message(&error, len)),
        }
    }

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
        let res: Vec<Slice> = resources.iter().map(|r| Slice::of(r)).collect();
        let (in_c, out_c) = part_c(inputs, outputs);
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: the inputs and outputs live across the call, their callbacks driven from here
        let r = unsafe {
            (self.vtable.apply_part)(
                self.handle,
                Slice::of(params),
                res.as_ptr(),
                res.len(),
                in_c.as_ptr(),
                in_c.len(),
                out_c.as_ptr(),
                out_c.len(),
                sizes.as_ptr(),
                sizes.len(),
                memory_limit,
                task_limits.as_ptr(),
                task_limits.len(),
                SliceMut::of(&mut error),
                &mut len,
            )
        };
        match r {
            0 => Ok(()),
            _ => Err(message(&error, len)),
        }
    }
}

/// The decoder's inputs and outputs of a part application as a blob's C callbacks drive them
/// ([`pull`], [`push`]): each callback's context is the item's place in `inputs` or `outputs`,
/// which must stay where it is across the application.
pub(crate) fn part_c(
    inputs: &mut [&mut dyn PartIn],
    outputs: &mut [&mut dyn PartOut],
) -> (Vec<PartInC>, Vec<PartOutC>) {
    let ins = inputs
        .iter_mut()
        .map(|pi| PartInC { len: pi.len(), ctx: pi as *mut &mut dyn PartIn as *mut c_void, pull })
        .collect();
    let outs =
        outputs.iter_mut().map(|po| PartOutC { ctx: po as *mut &mut dyn PartOut as *mut c_void, push }).collect();
    (ins, outs)
}

/// A blob pulls input `ctx` (a `&mut dyn PartIn`) up to `hi`, its bytes given to `put`.
unsafe extern "C" fn pull(
    ctx: *mut c_void,
    hi: u64,
    put_ctx: *mut c_void,
    put: unsafe extern "C" fn(*mut c_void, *const u8, usize),
) -> u32 {
    let pi = unsafe { &mut *(ctx as *mut &mut dyn PartIn) };
    let mut give = |bytes: &[u8]| unsafe { put(put_ctx, bytes.as_ptr(), bytes.len()) };
    match pi.pull(hi, &mut give) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// A blob commits `len` bytes of output `ctx` (a `&mut dyn PartOut`).
unsafe extern "C" fn push(ctx: *mut c_void, ptr: *const u8, len: usize) -> u32 {
    let po = unsafe { &mut *(ctx as *mut &mut dyn PartOut) };
    match po.push(unsafe { std::slice::from_raw_parts(ptr, len) }) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

// ---- finding the merged blobs

/// What `aqr-ld merge` fills: the dispatch table's place in the image (its magic, then RVA and
/// length).
#[repr(C)]
struct Anchor {
    magic: [u8; 16],
    dispatch_rva: u32,
    dispatch_len: u32,
}

#[used]
#[unsafe(no_mangle)]
static AQR_PEMERGE_ANCHOR: Anchor =
    Anchor { magic: *b"\x89AQRPEMERGEv1\x00\x00\x00", dispatch_rva: 0, dispatch_len: 0 };

#[cfg(windows)]
impl Sfx {
    /// The blobs merged into this program, as the dispatch table names them: the interpreter
    /// blob ([`INTERP`], the RVA of its `aqr_interp_vtable`) and the native module blobs
    /// ([`native_name`], the RVA of each `aqr_dll_apply`), each one's exception table registered
    /// here (for the unwinder: traps, panics).
    pub fn find() -> Sfx {
        unsafe extern "C" {
            static __ImageBase: u8;
        }
        unsafe extern "system" {
            fn RtlAddFunctionTable(table: *const u8, count: u32, base: u64) -> i32;
        }
        let mut sfx = Sfx {
            interp: None,
            natives: Vec::new(),
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        };
        let base = &raw const __ImageBase as usize;
        let u32_at = |b: &[u8], o: usize| b.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let (rva, len) = unsafe {
            (
                std::ptr::read_volatile(&AQR_PEMERGE_ANCHOR.dispatch_rva),
                std::ptr::read_volatile(&AQR_PEMERGE_ANCHOR.dispatch_len),
            )
        };
        if rva == 0 || len < 4 {
            return sfx;
        }
        let table = unsafe { std::slice::from_raw_parts((base + rva as usize) as *const u8, len as usize) };
        // the count, then an entry each (apply_rva, info_rva, pdata_rva, pdata_count, mod_base,
        // name_off: 6 u32), then the names (each its length, u32, and its bytes)
        for k in 0..u32_at(table, 0).unwrap_or(0) as usize {
            let field = |i: usize| u32_at(table, 4 + k * 24 + i * 4).unwrap_or(0);
            let (apply_rva, info_rva, pdata_rva, pdata_count, mod_base, name_at) =
                (field(0), field(1), field(2), field(3), field(4), field(5) as usize);
            let Some(n) = u32_at(table, name_at) else { continue };
            let Some(name) = table.get(name_at + 4..name_at + 4 + n as usize) else { continue };
            if pdata_count > 0 {
                unsafe {
                    RtlAddFunctionTable(
                        (base + pdata_rva as usize) as *const u8,
                        pdata_count,
                        (base + mod_base as usize) as u64,
                    )
                };
            }
            let at = base + apply_rva as usize;
            if name == INTERP.as_bytes() {
                // SAFETY: the merge wrote the RVA of aqr_interp_vtable, which returns the static vtable
                let vtable_fn: extern "C" fn() -> *const Vtable = unsafe { std::mem::transmute(at) };
                sfx.interp = Some(unsafe { &*vtable_fn() });
            } else if let Some((hash, slots)) = native::parse_native_name(name)
                && info_rva != 0
            {
                // SAFETY: the merge wrote the RVAs of the blob's aqr_dll_apply and aqr_dll_info
                let apply: aqr_api::dll::ApplyFn = unsafe { std::mem::transmute(at) };
                let info: aqr_api::dll::InfoFn = unsafe { std::mem::transmute(base + info_rva as usize) };
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
