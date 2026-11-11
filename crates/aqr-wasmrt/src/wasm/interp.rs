//! Modules run by the interpreter blob (aqr-interp: this crate's [`super::engine`] built as a
//! library of its own, with Wasmtime), which the program gives as bytes ([`set_interpreter`])
//! and this places in memory on first use (`aqr_blob::load`, Windows x86-64): the program links no
//! Wasmtime. What is loaded and applied goes through the blob's entry points; the bound of the
//! applications' memories is the program's, given to the blob as it is set.

use std::sync::OnceLock;

use aqr_blob::interp::{Interpreter, Vtable};
use aqr_formula::aqr::{Failure, Loaded};

use super::Decoded;

/// Gives the interpreter blob's bytes (its [`aqr_blob::Blob`]).
type Provide = fn() -> Result<Vec<u8>, String>;

static PROVIDE: OnceLock<Provide> = OnceLock::new();
static INTERP: OnceLock<Result<Interpreter, String>> = OnceLock::new();

/// Where the interpreter blob comes from, as the program carries it: asked for once, as the
/// first module is loaded. Before any module is (and, in a worker of the sandbox, before
/// `sandbox::worker_main`).
pub fn set_interpreter(provide: Provide) {
    let _ = PROVIDE.set(provide);
}

/// The interpreter, placed on first use.
fn interpreter() -> Decoded<&'static Interpreter> {
    let i = INTERP.get_or_init(|| {
        let provide = PROVIDE.get().ok_or("this program has no interpreter blob")?;
        place(&provide()?)
    });
    i.as_ref().map_err(|e| Failure::Resource(format!("modules not run as WebAssembly: {e}")))
}

#[cfg(all(windows, target_arch = "x86_64", not(target_vendor = "win7")))]
fn place(bytes: &[u8]) -> Result<Interpreter, String> {
    let image = aqr_blob::load(&[bytes])?;
    let b = &image.blobs[0];
    if b.apply_rva == 0 {
        return Err("the interpreter blob has no entry".into());
    }
    // SAFETY: the blob's entry is its aqr_interp_vtable, which gives its static vtable
    let vtable_fn: extern "C" fn() -> *const Vtable = unsafe { std::mem::transmute(b.at(b.apply_rva)) };
    let i = unsafe { Interpreter::new(vtable_fn()) };
    i.set_application_bound(super::application_bound_set());
    Ok(i)
}

#[cfg(not(all(windows, target_arch = "x86_64", not(target_vendor = "win7"))))]
fn place(_: &[u8]) -> Result<Interpreter, String> {
    Err("the interpreter blob runs on Windows 10 on, x86-64, alone".into())
}

/// `wasm`, of SHA-512/256 `hash`, checked against the profile and compiled (once a process).
pub fn load(wasm: &[u8], hash: [u8; 32]) -> Decoded<Box<dyn Loaded>> {
    interpreter()?.load_with(wasm, Some(hash), None, false)
}

/// [`load`] with the bound of the application's memories `bound` of its own, for tests.
#[doc(hidden)]
pub fn load_bounded(wasm: &[u8], hash: [u8; 32], bound: u64) -> Decoded<Box<dyn Loaded>> {
    interpreter()?.load_with(wasm, Some(hash), Some(bound), false)
}

/// [`load`] of a module as if it were of a release (not kept), for tests of the earlier rules.
#[doc(hidden)]
pub fn load_released(wasm: &[u8]) -> Decoded<Box<dyn Loaded>> {
    interpreter()?.load_with(wasm, None, None, true)
}

/// Whether the program has the interpreter given (it placed here): the failure, where it has not.
#[cfg(feature = "sandbox")]
pub(crate) fn interpreter_given() -> Decoded<()> {
    interpreter().map(|_| ())
}

/// The bound of the applications' memories set: the blob's too, where it is placed.
pub(crate) fn bound_set(bytes: u64) {
    if let Some(Ok(i)) = INTERP.get() {
        i.set_application_bound(bytes);
    }
}

/// The interpreter placed, and its engine made (before a worker of the sandbox is locked down).
pub fn warm_up() {
    if let Ok(i) = interpreter() {
        i.warm_up();
    }
}

/// A worker of the sandbox: served by the interpreter blob's own (its engine's), then exits.
#[cfg(feature = "sandbox")]
pub(crate) fn worker_main() -> ! {
    match interpreter() {
        Ok(i) => i.worker_main(),
        // (said where the decoder's standard error goes: what the decoder tells of the worker is
        // only that it did not start)
        Err(e) => {
            eprintln!("aqr: a sandbox worker: {e:?}");
            std::process::exit(1)
        }
    }
}
