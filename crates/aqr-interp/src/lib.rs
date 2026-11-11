//! The interpreter blob: a module library that runs any Wasm module handed to it, by Wasmtime
//! (`aqr_wasmrt`), with no native codecs linked. A self-extracting stub merges it in and sends it
//! the modules it has no native code for; Wasmtime sandboxes each module (its own memory, no host
//! access), and the engine is made on first use.
//!
//! It exports, over the C ABI:
//! - `aqr_interp_load`: a Wasm module made ready to apply, returning an opaque handle (null on
//!   failure, with a message in `error`), freed with `aqr_interp_drop`.
//! - `aqr_interp_is_part`: whether the handle's module is a part (streaming) module.
//! - `aqr_interp_apply`: an application of the handle's module, its outputs written into the
//!   caller's buffers (whole modules; a part module is run whole here too).

use aqr_api::dll::{Slice, SliceMut};
use aqr_formula::Resource;
use aqr_formula::aqr::Loaded;

/// A module loaded, behind the handle (`Box<dyn Loaded>` is a fat pointer, so it is boxed again
/// to pass one thin pointer).
type Handle = Box<Box<dyn Loaded>>;

/// Writes `msg` into `error` (as much as fits) and `*error_len`.
fn fail(msg: &str, error: SliceMut, error_len: *mut usize) {
    // SAFETY: the caller gives `error` as a buffer and `error_len` a place for the length
    let buf = unsafe { error.get() };
    let n = msg.len().min(buf.len());
    buf[..n].copy_from_slice(&msg.as_bytes()[..n]);
    if !error_len.is_null() {
        unsafe { *error_len = n };
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
    use sha2::{Digest, Sha512_256};
    let bytes = unsafe { wasm.get() };
    let hash: [u8; 32] = Sha512_256::digest(bytes).into();
    match aqr_wasmrt::wasm::load(bytes, hash) {
        // the inner box is the fat `dyn Loaded` pointer; the outer one is passed as a thin handle
        Ok(loaded) => {
            let handle: Handle = Box::new(loaded);
            Box::into_raw(handle).cast()
        }
        Err(e) => {
            fail(&format!("{e:?}"), error, error_len);
            core::ptr::null_mut()
        }
    }
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
    let slices = |p: *const Slice, n: usize| match n {
        0 => Vec::new(),
        n => unsafe { std::slice::from_raw_parts(p, n) }.iter().map(|s| unsafe { s.get() }).collect::<Vec<&[u8]>>(),
    };
    let params = unsafe { params.get() };
    let resources: Vec<Resource> =
        slices(resources, n_resources).into_iter().map(|r| Resource::new(r.to_vec())).collect();
    let inputs = slices(inputs, n_inputs);
    let task_limits = match n_task_limits {
        0 => &[][..],
        n => unsafe { std::slice::from_raw_parts(task_limits, n) },
    };
    let outs: &[SliceMut] = match n_outputs {
        0 => &[],
        n => unsafe { std::slice::from_raw_parts(outputs, n) },
    };
    let sizes: Vec<u64> = outs.iter().map(|o| o.len as u64).collect();
    match loaded.apply(params, &resources, &inputs, &sizes, memory_limit, task_limits) {
        Ok(got) => {
            for (o, g) in outs.iter().zip(got) {
                unsafe { o.get() }.copy_from_slice(&g);
            }
            0
        }
        Err(e) => {
            fail(&format!("{e:?}"), error, error_len);
            1
        }
    }
}
