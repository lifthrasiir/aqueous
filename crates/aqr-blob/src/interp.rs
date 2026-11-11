//! The interpreter blob (aqr-interp, Wasmtime with no codecs) as its host sees it: its entry
//! points ([`Vtable`], `aqr_interp_vtable`'s), and [`Interpreter`], the [`Runner`] that sends
//! modules to it, whether the blob is merged into a stub (aqr-sfx) or placed at run time
//! ([`crate::load`], aqr-wasmrt for aqr). Failures cross the C ABI as their kind's byte and
//! their message ([`failure_bytes`]), so a module the blob cannot run is a resource failure
//! still, not an invalid archive.

use core::ffi::c_void;

use aqr_api::dll::{PartIn as PartInC, PartOut as PartOutC, Slice, SliceMut};
use aqr_formula::Resource;
use aqr_formula::aqr::{Decoded, Failure, Loaded, PartIn, PartOut, Runner};

/// The interpreter blob's entry points, all through one export (`aqr_interp_vtable`).
#[repr(C)]
pub struct Vtable {
    pub load: unsafe extern "C" fn(Slice, SliceMut, *mut usize) -> *mut c_void,
    pub is_part: unsafe extern "C" fn(*mut c_void) -> u32,
    pub apply: unsafe extern "C" fn(
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
    pub apply_part: unsafe extern "C" fn(
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
    pub drop: unsafe extern "C" fn(*mut c_void),
    // (after the five a stub uses: what a program running its modules by the blob uses too)
    /// The module's bytes, its SHA-512/256 (32 bytes; null: computed), the bound of its
    /// applications' memories of its own (`u64::MAX`: none), whether as a module of a release.
    pub load_with: unsafe extern "C" fn(Slice, *const u8, u64, u32, SliceMut, *mut usize) -> *mut c_void,
    pub set_application_bound: extern "C" fn(u64),
    pub warm_up: extern "C" fn(),
    /// Serves the decoder that started this process as a worker of its sandbox, then exits
    /// (`aqr_wasmrt::sandbox::worker_main`).
    pub worker_main: extern "C" fn() -> !,
}

/// What a failure's bytes start with ([`failure_bytes`]), which no plain message does.
const MARK: u8 = 0x01;

/// The bytes a failure crosses the C ABI as: [`MARK`], its kind's byte, then its message.
pub fn failure_bytes(f: &Failure) -> Vec<u8> {
    let (kind, msg) = match f {
        Failure::Invalid(m) => (b'I', m),
        Failure::Decryption(m) => (b'D', m),
        Failure::Resource(m) => (b'R', m),
        Failure::Unsupported(m) => (b'U', m),
    };
    [&[MARK, kind][..], msg.as_bytes()].concat()
}

/// The failure of [`failure_bytes`] `b`; a plain message (a native module blob's) is invalid.
pub fn failure_of(b: &[u8]) -> Failure {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    match b {
        [MARK, b'I', m @ ..] => Failure::Invalid(text(m)),
        [MARK, b'D', m @ ..] => Failure::Decryption(text(m)),
        [MARK, b'R', m @ ..] => Failure::Resource(text(m)),
        [MARK, b'U', m @ ..] => Failure::Unsupported(text(m)),
        _ => Failure::Invalid(text(b)),
    }
}

/// A [`Runner`] sending every module to the interpreter blob of `vtable`.
#[derive(Clone, Copy)]
pub struct Interpreter {
    vtable: &'static Vtable,
}

impl Interpreter {
    /// The interpreter of the blob whose `aqr_interp_vtable` gave `vtable`.
    ///
    /// # Safety
    /// `vtable` is the blob's, which stays for the life of the process.
    pub unsafe fn new(vtable: *const Vtable) -> Interpreter {
        Interpreter { vtable: unsafe { &*vtable } }
    }

    /// `wasm` made ready to apply (`aqr_wasmrt::wasm::load` and the like): of SHA-512/256 `hash`
    /// (`None`: computed), its applications' memories bounded by `bound` of its own (`None`: the
    /// process's), or as a module of a release.
    pub fn load_with(
        &self,
        wasm: &[u8],
        hash: Option<[u8; 32]>,
        bound: Option<u64>,
        released: bool,
    ) -> Decoded<Box<dyn Loaded>> {
        let mut error = [0u8; 1024];
        let mut len = 0;
        let hash_ptr = hash.as_ref().map_or(std::ptr::null(), |h| h.as_ptr());
        // SAFETY: the module's bytes, its hash where given, and a place for a message
        let handle = unsafe {
            (self.vtable.load_with)(
                Slice::of(wasm),
                hash_ptr,
                bound.unwrap_or(u64::MAX),
                released as u32,
                SliceMut::of(&mut error),
                &mut len,
            )
        };
        if handle.is_null() {
            return Err(failure_of(&error[..len]));
        }
        Ok(Box::new(Handle { vtable: self.vtable, handle }))
    }

    pub fn set_application_bound(&self, bytes: u64) {
        (self.vtable.set_application_bound)(bytes)
    }

    pub fn warm_up(&self) {
        (self.vtable.warm_up)()
    }

    pub fn worker_main(&self) -> ! {
        (self.vtable.worker_main)()
    }
}

impl Runner for Interpreter {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        let mut error = [0u8; 1024];
        let mut len = 0;
        // SAFETY: the interpreter's load takes the module's bytes and a place for a message
        let handle = unsafe { (self.vtable.load)(Slice::of(wasm), SliceMut::of(&mut error), &mut len) };
        if handle.is_null() {
            return Err(failure_of(&error[..len]));
        }
        Ok(Box::new(Handle { vtable: self.vtable, handle }))
    }
}

/// A module loaded in the interpreter blob, behind its handle.
struct Handle {
    vtable: &'static Vtable,
    handle: *mut c_void,
}

// SAFETY: the handle's module is run through the interpreter, whose applications are thread-safe.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is live and used no more
        unsafe { (self.vtable.drop)(self.handle) };
    }
}

impl Loaded for Handle {
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
        let mut outs = Vec::with_capacity(sizes.len());
        for &s in sizes {
            outs.push(vec![0u8; usize::try_from(s).map_err(|_| Failure::Resource("an output too long".into()))?]);
        }
        let os: Vec<SliceMut> = outs.iter_mut().map(|o: &mut Vec<u8>| SliceMut::of(o)).collect();
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
            _ => Err(failure_of(&error[..len])),
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
        let mut failed = None;
        let (in_c, out_c, _ctx) = part_c(inputs, outputs, &mut failed);
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
        match (r, failed) {
            (0, _) => Ok(()),
            // (an input's or an output's own failure, rather than the module's of it)
            (_, Some(f)) => Err(f),
            _ => Err(failure_of(&error[..len])),
        }
    }
}

/// What a callback of a part application reaches: the place of the decoder's input or output
/// (a `&mut dyn PartIn` or `&mut dyn PartOut`), and where the first failure of one goes.
pub struct Ctx {
    item: *mut c_void,
    failed: *mut Option<Failure>,
}

/// The decoder's inputs and outputs of a part application as a blob's C callbacks drive them
/// ([`pull`], [`push`]), and their contexts, which must stay alive across the application (as
/// must `inputs`, `outputs` and `failed`, where they are); the first failure of an input or
/// output goes to `failed`.
pub fn part_c(
    inputs: &mut [&mut dyn PartIn],
    outputs: &mut [&mut dyn PartOut],
    failed: &mut Option<Failure>,
) -> (Vec<PartInC>, Vec<PartOutC>, Vec<Box<Ctx>>) {
    let failed: *mut Option<Failure> = failed;
    let mut ctx = Vec::with_capacity(inputs.len() + outputs.len());
    let mut in_c = Vec::with_capacity(inputs.len());
    for i in inputs.iter_mut() {
        let len = i.len();
        let c = Box::new(Ctx { item: i as *mut &mut dyn PartIn as *mut c_void, failed });
        in_c.push(PartInC { len, ctx: &*c as *const Ctx as *mut c_void, pull });
        ctx.push(c);
    }
    let mut out_c = Vec::with_capacity(outputs.len());
    for o in outputs.iter_mut() {
        let c = Box::new(Ctx { item: o as *mut &mut dyn PartOut as *mut c_void, failed });
        out_c.push(PartOutC { ctx: &*c as *const Ctx as *mut c_void, push });
        ctx.push(c);
    }
    (in_c, out_c, ctx)
}

/// A blob pulls input `ctx` (a [`Ctx`] of a `&mut dyn PartIn`) up to `hi`, its bytes given to
/// `put`.
unsafe extern "C" fn pull(
    ctx: *mut c_void,
    hi: u64,
    put_ctx: *mut c_void,
    put: unsafe extern "C" fn(*mut c_void, *const u8, usize),
) -> u32 {
    let c = unsafe { &*(ctx as *const Ctx) };
    let pi = unsafe { &mut *(c.item as *mut &mut dyn PartIn) };
    let mut give = |bytes: &[u8]| unsafe { put(put_ctx, bytes.as_ptr(), bytes.len()) };
    match pi.pull(hi, &mut give) {
        Ok(()) => 0,
        Err(f) => {
            unsafe { (*c.failed).get_or_insert(f) };
            1
        }
    }
}

/// A blob commits `len` bytes of output `ctx` (a [`Ctx`] of a `&mut dyn PartOut`).
unsafe extern "C" fn push(ctx: *mut c_void, ptr: *const u8, len: usize) -> u32 {
    let c = unsafe { &*(ctx as *const Ctx) };
    let po = unsafe { &mut *(c.item as *mut &mut dyn PartOut) };
    let bytes = match len {
        0 => &[][..],
        n => unsafe { std::slice::from_raw_parts(ptr, n) },
    };
    match po.push(bytes) {
        Ok(()) => 0,
        Err(f) => {
            unsafe { (*c.failed).get_or_insert(f) };
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_cross_with_their_kinds() {
        for f in [
            Failure::Invalid("bad".into()),
            Failure::Decryption("key".into()),
            Failure::Resource("limit".into()),
            Failure::Unsupported("critical".into()),
        ] {
            assert_eq!(failure_of(&failure_bytes(&f)), f);
        }
        assert_eq!(failure_of(b"Range of no input"), Failure::Invalid("Range of no input".into()));
    }
}
