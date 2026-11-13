//! The interpreter blob: a module library that runs any Wasm module handed to it, by Wasmtime
//! (`aqr_wasmrt`), with no native codecs linked. A self-extracting stub merges it in and sends it
//! the modules it has no native code for; Wasmtime sandboxes each module (its own memory, no host
//! access), and the engine is made on first use.
//!
//! It exports, over the C ABI:
//! - `aqr_interp_load` and `aqr_interp_load_with`: a Wasm module made ready to apply, returning an
//!   opaque handle (null on failure, with a message in `error`), freed with `aqr_interp_drop`.
//! - `aqr_interp_is_part`: whether the handle's module is a part (streaming) module.
//! - `aqr_interp_apply`: an application of the handle's module, its outputs written into the
//!   caller's buffers (whole modules).
//! - `aqr_interp_apply_part`: an application of a part module, its inputs pulled and its outputs
//!   pushed as it runs.
//! - `aqr_interp_set_application_bound`, `aqr_interp_warm_up` and `aqr_interp_worker_main`:
//!   `aqr_wasmrt`'s, for the programs that have no Wasmtime of their own.
//! - `aqr_interp_vtable`: all of these, in one [`Vtable`] (what the stub merges in looks for).

use core::ffi::c_void;

use aqr_api::dll::{Slice, SliceMut};
use aqr_formula::Resource;
use aqr_formula::aqr::{Decoded, Failure, Loaded, PartIn, PartOut};

/// A module loaded, behind the handle (`Box<dyn Loaded>` is a fat pointer, so it is boxed again
/// to pass one thin pointer).
type Handle = Box<Box<dyn Loaded>>;

/// Writes failure `f` into `error` as it crosses the C ABI (its kind's byte and its message,
/// `aqr_blob::interp::failure_bytes`), as much as fits.
fn fail_with(f: &Failure, error: SliceMut, error_len: *mut usize) {
    fail_bytes(&aqr_blob::interp::failure_bytes(f), error, error_len)
}

/// Writes `msg` into `error` (as much as fits) and `*error_len`.
fn fail_bytes(msg: &[u8], error: SliceMut, error_len: *mut usize) {
    // SAFETY: the caller gives `error` as a buffer and `error_len` a place for the length
    let buf = unsafe { error.get() };
    let n = msg.len().min(buf.len());
    buf[..n].copy_from_slice(&msg[..n]);
    if !error_len.is_null() {
        unsafe { *error_len = n };
    }
}

/// The `n` items at `p` (an empty list for none, whatever `p` is: it may be null).
///
/// # Safety
/// `p` points to `n` items that live as long as the result is used.
unsafe fn items<'a, T>(p: *const T, n: usize) -> &'a [T] {
    match n {
        0 => &[],
        n => unsafe { std::slice::from_raw_parts(p, n) },
    }
}

/// Makes the Wasm module `wasm` ready to apply, returning a handle (null on failure).
///
/// # Safety
/// `wasm` points to its bytes; `error`/`error_len` take a failure's message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_interp_load(
    wasm: Slice,
    error: SliceMut,
    error_len: *mut usize,
) -> *mut core::ffi::c_void {
    unsafe { aqr_interp_load_with(wasm, core::ptr::null(), u64::MAX, 0, error, error_len) }
}

/// Whether the handle's module is a part (streaming) module.
///
/// # Safety
/// `handle` is a live handle from [`aqr_interp_load`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_interp_is_part(handle: *mut core::ffi::c_void) -> u32 {
    let loaded: &dyn Loaded = unsafe { &**(handle as *const Box<dyn Loaded>) };
    loaded.is_part() as u32
}

/// Frees a handle from [`aqr_interp_load`].
///
/// # Safety
/// `handle` is a live handle from [`aqr_interp_load`], not used afterward.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_interp_drop(handle: *mut core::ffi::c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut Box<dyn Loaded>) });
    }
}

/// Applies the handle's module: the parameters, the resources, the inputs, and the outputs (of
/// the sizes declared), with the memory and task limits. 0 on success, else 1 with a message in
/// `error`.
///
/// # Safety
/// As the pointers are described: each points to as many items as its count, the outputs and
/// `error` apart from everything else.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn aqr_interp_apply(
    handle: *mut core::ffi::c_void,
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const Slice,
    n_inputs: usize,
    memory_limit: u64,
    task_limits: *const u64,
    n_task_limits: usize,
    outputs: *const SliceMut,
    n_outputs: usize,
    error: SliceMut,
    error_len: *mut usize,
) -> u32 {
    let loaded: &dyn Loaded = unsafe { &**(handle as *const Box<dyn Loaded>) };
    let slices =
        |p: *const Slice, n: usize| unsafe { items(p, n) }.iter().map(|s| unsafe { s.get() }).collect::<Vec<&[u8]>>();
    let params = unsafe { params.get() };
    let resources: Vec<Resource> = slices(resources, n_resources).into_iter().map(Resource::new).collect();
    let inputs = slices(inputs, n_inputs);
    let task_limits = unsafe { items(task_limits, n_task_limits) };
    let outs = unsafe { items(outputs, n_outputs) };
    let sizes: Vec<u64> = outs.iter().map(|o| o.len as u64).collect();
    match loaded.apply(params, &resources, &inputs, &sizes, memory_limit, task_limits) {
        Ok(got) => {
            for (o, g) in outs.iter().zip(got) {
                unsafe { o.get() }.copy_from_slice(&g);
            }
            0
        }
        Err(e) => {
            fail_with(&e, error, error_len);
            1
        }
    }
}

// ---- part modules: streamed, so a big stream's memory stays bounded

// (an input of a part application, pulled as the module requests it so it is not held whole, and
// an output, taking bytes as the module commits them: aqr-api's, as the native blobs have them)
pub use aqr_api::dll::{PartIn as PartInC, PartOut as PartOutC};

struct FfiIn(PartInC);

// SAFETY: the callbacks are driven from the instance thread (where the module requests and
// commits), not the module's task threads; the caller keeps `ctx` valid across the application.
unsafe impl Send for FfiIn {}
unsafe impl Send for FfiOut {}

impl PartIn for FfiIn {
    fn len(&self) -> u64 {
        self.0.len
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Decoded<()> {
        // the module's `put` closure, reached through a C callback by a pointer to it
        unsafe extern "C" fn trampoline(ctx: *mut c_void, ptr: *const u8, len: usize) {
            let put = unsafe { &mut *(ctx as *mut &mut dyn FnMut(&[u8])) };
            put(unsafe { std::slice::from_raw_parts(ptr, len) });
        }
        let mut put: &mut dyn FnMut(&[u8]) = put;
        let put_ctx = (&mut put) as *mut &mut dyn FnMut(&[u8]) as *mut c_void;
        match unsafe { (self.0.pull)(self.0.ctx, hi, put_ctx, trampoline) } {
            0 => Ok(()),
            _ => Err(Failure::Invalid("interp: an input could not be pulled".into())),
        }
    }
}

struct FfiOut(PartOutC);

impl PartOut for FfiOut {
    fn push(&mut self, d: &[u8]) -> Decoded<()> {
        match unsafe { (self.0.push)(self.0.ctx, d.as_ptr(), d.len()) } {
            0 => Ok(()),
            _ => Err(Failure::Invalid("interp: an output could not be committed".into())),
        }
    }
}

/// Applies the handle's module as a part module: its inputs pulled as it requests them and its
/// outputs pushed as it commits them (of `sizes` bytes), so a big stream stays within the memory
/// limit. 0 on success, else 1 with a message in `error`.
///
/// # Safety
/// As the pointers are described; the outputs and `error` apart from everything else.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn aqr_interp_apply_part(
    handle: *mut c_void,
    params: Slice,
    resources: *const Slice,
    n_resources: usize,
    inputs: *const PartInC,
    n_inputs: usize,
    outputs: *const PartOutC,
    n_outputs: usize,
    sizes: *const u64,
    n_sizes: usize,
    memory_limit: u64,
    task_limits: *const u64,
    n_task_limits: usize,
    error: SliceMut,
    error_len: *mut usize,
) -> u32 {
    let loaded: &dyn Loaded = unsafe { &**(handle as *const Box<dyn Loaded>) };
    let resources: Vec<Resource> =
        unsafe { items(resources, n_resources) }.iter().map(|s| Resource::new(unsafe { s.get() })).collect();
    let mut ins: Vec<FfiIn> = unsafe { items(inputs, n_inputs) }.iter().map(|&c| FfiIn(c)).collect();
    let mut outs: Vec<FfiOut> = unsafe { items(outputs, n_outputs) }.iter().map(|&c| FfiOut(c)).collect();
    let sizes = unsafe { items(sizes, n_sizes) };
    let task_limits = unsafe { items(task_limits, n_task_limits) };
    let mut in_refs: Vec<&mut dyn PartIn> = ins.iter_mut().map(|x| x as &mut dyn PartIn).collect();
    let mut out_refs: Vec<&mut dyn PartOut> = outs.iter_mut().map(|x| x as &mut dyn PartOut).collect();
    match loaded.apply_part(
        unsafe { params.get() },
        &resources,
        &mut in_refs,
        &mut out_refs,
        sizes,
        memory_limit,
        task_limits,
    ) {
        Ok(()) => 0,
        Err(e) => {
            fail_with(&e, error, error_len);
            1
        }
    }
}

// ---- the vtable: one export the merge records, holding every entry point

/// The interpreter blob's entry points, so a stub that merges the blob in (or a program that
/// places it, `aqr_blob::load`) finds them all through one export ([`aqr_interp_vtable`]) rather
/// than by name: aqr-blob's, which its hosts read.
pub use aqr_blob::interp::Vtable;

static VTABLE: Vtable = Vtable {
    load: aqr_interp_load,
    is_part: aqr_interp_is_part,
    apply: aqr_interp_apply,
    apply_part: aqr_interp_apply_part,
    drop: aqr_interp_drop,
    load_with: aqr_interp_load_with,
    set_application_bound: aqr_interp_set_application_bound,
    warm_up: aqr_interp_warm_up,
    worker_main: aqr_interp_worker_main,
};

/// [`aqr_interp_load`] as `aqr_wasmrt::wasm` loads: of the module's SHA-512/256 `hash` (32 bytes;
/// null: computed), with the bound of its applications' memories `bound` of its own (`u64::MAX`:
/// none), or as a module of a release (`released` 1, for tests of the earlier rules).
///
/// # Safety
/// `wasm` points to its bytes, `hash` to 32 bytes or is null; `error`/`error_len` take a
/// failure's message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn aqr_interp_load_with(
    wasm: Slice,
    hash: *const u8,
    bound: u64,
    released: u32,
    error: SliceMut,
    error_len: *mut usize,
) -> *mut c_void {
    use sha2::{Digest, Sha512_256};
    let bytes = unsafe { wasm.get() };
    let hash: [u8; 32] = match hash.is_null() {
        true => Sha512_256::digest(bytes).into(),
        false => unsafe { std::ptr::read_unaligned(hash.cast::<[u8; 32]>()) },
    };
    let loaded = match (released, bound) {
        (0, u64::MAX) => aqr_wasmrt::wasm::load(bytes, hash),
        (0, b) => aqr_wasmrt::wasm::load_bounded(bytes, hash, b),
        _ => aqr_wasmrt::wasm::load_released(bytes),
    };
    match loaded {
        Ok(loaded) => {
            let handle: Handle = Box::new(loaded);
            Box::into_raw(handle).cast()
        }
        Err(e) => {
            fail_with(&e, error, error_len);
            core::ptr::null_mut()
        }
    }
}

extern "C" fn aqr_interp_set_application_bound(bytes: u64) {
    aqr_wasmrt::wasm::set_application_bound(bytes);
}

extern "C" fn aqr_interp_warm_up() {
    aqr_wasmrt::wasm::warm_up();
}

extern "C" fn aqr_interp_worker_main() -> ! {
    aqr_wasmrt::sandbox::worker_main()
}

/// The blob's [`Vtable`], its only export the stub needs: its entry points, by a pointer the
/// merge relocates.
#[unsafe(no_mangle)]
pub extern "C" fn aqr_interp_vtable() -> *const Vtable {
    &VTABLE
}
